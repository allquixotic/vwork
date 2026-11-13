//! Physical files of a materialization, and the evidence that they are still
//! what was written. Comparing recorded metadata from one listing per
//! directory replaces opening and reading every file: the rule Git's index
//! uses, and the difference between touching names and touching contents.
use crate::model::Tree;
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
    path::Path,
};

pub(crate) const MARKER: &str = ".vwork-managed.json";
/// The same record while a released tree waits for its next lease. Under
/// this name the tree is nobody's materialization: nothing recovers edits
/// from it, and it must be made pristine before it is handed out again.
pub(crate) const POOLED: &str = ".vwork-pooled.json";

/// What the filesystem reported for a file when vwork finished writing it.
/// Windows has no owner or inode here; its creation time stands in for the
/// change time, as Git for Windows does.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(into = "[u64; 8]", from = "[u64; 8]")]
pub struct FileStat {
    pub size: u64,
    pub modified: (i64, u32),
    pub changed: (i64, u32),
    pub inode: u64,
    pub owner: (u32, u32),
}

impl From<FileStat> for [u64; 8] {
    fn from(stat: FileStat) -> Self {
        [
            stat.size,
            stat.modified.0 as u64,
            stat.modified.1.into(),
            stat.changed.0 as u64,
            stat.changed.1.into(),
            stat.inode,
            stat.owner.0.into(),
            stat.owner.1.into(),
        ]
    }
}
impl From<[u64; 8]> for FileStat {
    fn from(n: [u64; 8]) -> Self {
        Self {
            size: n[0],
            modified: (n[1] as i64, n[2] as u32),
            changed: (n[3] as i64, n[4] as u32),
            inode: n[5],
            owner: (n[6] as u32, n[7] as u32),
        }
    }
}

impl FileStat {
    #[cfg(unix)]
    pub fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            size: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec() as u32),
            changed: (meta.ctime(), meta.ctime_nsec() as u32),
            inode: meta.ino(),
            owner: (meta.uid(), meta.gid()),
        }
    }
    #[cfg(windows)]
    pub fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::windows::fs::MetadataExt;
        // FILETIME: 100 ns units since 1601. Same arithmetic as Git and libgit2.
        let unix = |time: u64| {
            (
                (time / 10_000_000) as i64 - 11_644_473_600,
                ((time % 10_000_000) * 100) as u32,
            )
        };
        Self {
            size: meta.len(),
            modified: unix(meta.last_write_time()),
            changed: unix(meta.creation_time()),
            inode: 0,
            owner: (0, 0),
        }
    }
}

pub(crate) fn is_link(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Any reparse point, including junctions and placeholders.
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    meta.file_type().is_symlink()
}

/// Every directory that holds a tracked file, parents before children.
pub(crate) fn directories(tree: &Tree) -> BTreeSet<&str> {
    let mut all = BTreeSet::new();
    for path in tree.keys() {
        let mut rest = path.as_str();
        while let Some((parent, _)) = rest.rsplit_once('/') {
            // A known directory already brought its own ancestors.
            if !all.insert(parent) {
                break;
            }
            rest = parent;
        }
    }
    all
}

/// What one listing per tracked directory found.
#[derive(Default)]
pub(crate) struct Scan {
    /// Current metadata of tracked files that exist as regular files.
    pub files: BTreeMap<String, FileStat>,
    /// Everything else in those directories, when asked for: generated
    /// files, dependency trees, and anything occupying a tracked name.
    pub foreign: Vec<String>,
}

/// List `root` once per tracked directory. Untracked directories
/// (dependencies, build output) are never entered.
///
/// Without `foreign`, untracked entries are ignored and a link or wrong file
/// type where vwork expects its own file or directory is refused: nothing may
/// be written or read through it. With `foreign`, all of those are reported
/// instead, so the caller can move them out of the tree by name.
pub(crate) fn scan(
    root: &Path,
    tree: &Tree,
    directories: &BTreeSet<&str>,
    foreign: bool,
) -> Result<Scan> {
    let mut found = Scan::default();
    for directory in std::iter::once("").chain(directories.iter().copied()) {
        let listing = match std::fs::read_dir(root.join(directory)) {
            Ok(listing) => listing,
            // Absent, or (when collecting) a foreign entry of its parent.
            Err(error)
                if error.kind() == std::io::ErrorKind::NotFound
                    || (foreign && error.kind() == std::io::ErrorKind::NotADirectory) =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        for entry in listing {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(String::from) else {
                ensure!(!foreign, "unreadable name in pooled tree");
                continue;
            };
            let own = directory.is_empty() && [MARKER, POOLED, ".git"].contains(&name.as_str());
            let path = if directory.is_empty() {
                name
            } else {
                format!("{directory}/{name}")
            };
            let file = tree.contains_key(&path);
            let tracked = file || directories.contains(path.as_str());
            if !tracked {
                if foreign && !own {
                    found.foreign.push(path);
                }
                continue;
            }
            // Listing metadata: no handle is opened per file on Windows.
            let meta = entry.metadata()?;
            let expected = !is_link(&meta) && if file { meta.is_file() } else { meta.is_dir() };
            if expected {
                if file {
                    found.files.insert(path, FileStat::of(&meta));
                }
            } else if foreign {
                found.foreign.push(path);
            } else {
                ensure!(!is_link(&meta), "symlink in physical path: {path}");
                ensure!(file, "tracked directory is not a directory: {path}");
            }
        }
    }
    Ok(found)
}

/// Write one file and report its final metadata from the still-open handle:
/// one open per file, no second lookup by name.
///
/// `stamp` gives every file of a brand-new tree one modification time safely
/// in the past. An index written afterwards is then newer than its entries by
/// a whole second, so Git trusts them instead of rehashing each file it
/// considers racily clean, and any later edit still moves the time forward.
pub(crate) fn write(
    target: &Path,
    bytes: &[u8],
    executable: bool,
    replace: bool,
    stamp: Option<std::time::SystemTime>,
) -> std::io::Result<FileStat> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true);
    if replace {
        options.create(true).truncate(true);
    } else {
        // Never adopt a file this materialization did not create.
        options.create_new(true);
    }
    let mut file = options.open(target)?;
    file.write_all(bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(if executable {
            0o755
        } else {
            0o644
        }))?;
    }
    #[cfg(windows)]
    let _ = executable;
    // NTFS otherwise stamps the write time when the handle closes, after this
    // function has reported it. An explicit time is final on every platform.
    #[cfg(windows)]
    let stamp = Some(stamp.unwrap_or_else(std::time::SystemTime::now));
    if let Some(stamp) = stamp {
        file.set_modified(stamp)?;
    }
    Ok(FileStat::of(&file.metadata()?))
}

/// Whole seconds, two of them ago: older than any index or build output that
/// can follow, on filesystems of any timestamp granularity.
pub(crate) fn settled_time() -> std::time::SystemTime {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    std::time::UNIX_EPOCH + std::time::Duration::from_secs(now.saturating_sub(2))
}

/// Ownership record inside every materialized tree. `stat` follows `entries`
/// in path order; trees written before it existed simply have none.
#[derive(Deserialize)]
pub(crate) struct Marker {
    #[serde(default)]
    pub home: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub entries: Tree,
    #[serde(default)]
    pub stat: Vec<FileStat>,
}

impl Marker {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        Ok(serde_json::from_slice(&std::fs::read(path)?)?)
    }
    /// Recorded metadata by path, when it covers the recorded entries.
    pub(crate) fn stats(&self) -> BTreeMap<&str, FileStat> {
        if self.stat.len() != self.entries.len() {
            return BTreeMap::new();
        }
        self.entries
            .keys()
            .map(String::as_str)
            .zip(self.stat.iter().copied())
            .collect()
    }
}

#[derive(Serialize)]
pub(crate) struct MarkerRecord<'a> {
    pub home: &'a Path,
    pub repo: &'a str,
    pub workspace: &'a str,
    pub revision: &'a str,
    pub entries: &'a Tree,
    pub stat: Vec<FileStat>,
}

impl MarkerRecord<'_> {
    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        ensure!(
            self.stat.len() == self.entries.len(),
            "incomplete materialization record"
        );
        std::fs::write(path, serde_json::to_vec(self)?)?;
        Ok(())
    }
}
