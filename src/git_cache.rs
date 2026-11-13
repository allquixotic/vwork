//! Derived Git history shared by every temporary view of one repository.
//!
//! Fjall stays authoritative. These packs are a rebuildable cache: history is
//! transcoded once, reusing the deltas already stored rather than searching
//! for new ones, and every lease then names the same immutable files through
//! hard links. Git and libgit2 replace packs by rename and never rewrite one
//! in place, so a shared inode cannot carry one lease's activity to another.
use crate::{git_delta, git_store, kv::Database, model::Repository};
use anyhow::{Context, Result, ensure};
use git2::{ObjectType, Oid};
use libgit2_sys as raw;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::Write,
    path::{Path, PathBuf},
};

/// Each incremental export adds one pack. Consolidate before lookups, which
/// probe packs in turn, start paying for the history of how objects arrived.
const MAX_PACKS: usize = 24;
const STATE: &str = "state.json";

#[derive(Default, Serialize, Deserialize)]
struct State {
    version: u32,
    /// Digest of the reference set whose complete history the packs hold.
    refs: String,
    /// Finished files and their lengths. Anything else here is debris.
    files: BTreeMap<String, u64>,
}

/// What one request cost. `scanned` false means no storage scan happened.
#[derive(Default, Debug, Clone, Serialize)]
pub struct Report {
    pub scanned: bool,
    pub exported_objects: usize,
    pub reused_deltas: usize,
    pub packs: usize,
    pub bytes: u64,
    #[serde(skip)]
    pub files: Vec<String>,
}

pub(crate) fn pack_directory(root: &Path) -> PathBuf {
    root.join("objects").join("pack")
}

/// Libgit2 marks packs read-only; Windows refuses to delete those directly.
fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            #[cfg(windows)]
            // Clears only the Windows READONLY attribute of our own cache file.
            #[allow(clippy::permissions_set_readonly_false)]
            {
                let mut permissions = std::fs::metadata(path)?.permissions();
                permissions.set_readonly(false);
                std::fs::set_permissions(path, permissions)?;
                std::fs::remove_file(path)
            }
            #[cfg(not(windows))]
            Err(error)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Derived data: dropping it only costs the next request one export.
pub(crate) fn discard(root: &Path) -> Result<()> {
    if root.exists() {
        std::fs::remove_dir_all(root)?;
    }
    Ok(())
}

fn digest(repo: &Repository) -> String {
    let mut hasher = blake3::Hasher::new();
    for (name, oid) in &repo.refs {
        hasher.update(name.as_bytes());
        hasher.update(&[0]);
        hasher.update(oid.as_bytes());
        hasher.update(b"\n");
    }
    hasher.finalize().to_hex().to_string()
}

/// Trust only files a completed export recorded. A crash between renames or a
/// truncated pack leaves a name the state does not vouch for; remove it.
fn load(root: &Path) -> Result<State> {
    let directory = pack_directory(root);
    std::fs::create_dir_all(&directory)?;
    let mut state = std::fs::read(root.join(STATE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<State>(&bytes).ok())
        .filter(|state| state.version == 1)
        .unwrap_or_default();
    let intact = state.files.iter().all(|(name, length)| {
        std::fs::metadata(directory.join(name)).is_ok_and(|meta| meta.len() == *length)
    });
    if !intact {
        state = State::default();
    }
    state.version = 1;
    for entry in std::fs::read_dir(&directory)? {
        let entry = entry?;
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| state.files.contains_key(name))
        {
            // Best effort: Windows refuses while any link to the file is open
            // elsewhere. Unrecorded names are never linked into a lease, and
            // exports that replace a set never consult what is present.
            let _ = remove(&entry.path());
        }
    }
    Ok(state)
}

fn save(root: &Path, state: &State) -> Result<()> {
    let pending = root.join(format!("{STATE}.new"));
    std::fs::write(&pending, serde_json::to_vec(state)?)?;
    std::fs::rename(pending, root.join(STATE))?;
    Ok(())
}

fn open(root: &Path) -> Result<git2::Odb<'static>> {
    let odb = git2::Odb::new()?;
    odb.add_disk_alternate(
        crate::git::path_argument(&root.join("objects"))
            .to_str()
            .context("non-UTF8 state path")?,
    )?;
    Ok(odb)
}

fn present(odb: &git2::Odb<'_>, oid: Oid) -> bool {
    // A miss must not rescan the pack directory for every absent object.
    odb.exists_ext(oid, git2::OdbLookupFlags::NO_REFRESH)
}

/// Make the cache hold all permanent history of `repo`, including `required`.
///
/// Caller owns the repository lock. An unchanged reference set whose required
/// commits are present proves completeness without scanning storage: imports
/// publish references only after every object, under the same lock.
pub(crate) fn ensure(
    db: &Database,
    repo: &Repository,
    root: &Path,
    required: &[Oid],
) -> Result<Report> {
    let mut state = load(root)?;
    let directory = pack_directory(root);
    let references = digest(repo);
    let mut report = Report::default();
    let complete = state.refs == references && {
        let odb = open(root)?;
        required.iter().all(|oid| present(&odb, *oid))
    };
    if !complete {
        report.scanned = true;
        let members = git_store::permanent_members(db, &repo.id)?;
        let consolidate = state.files.len() / 2 >= MAX_PACKS;
        let missing = if state.files.is_empty() || consolidate {
            members
        } else {
            let odb = open(root)?;
            members
                .into_iter()
                .filter(|oid| !present(&odb, *oid))
                .collect()
        };
        let superseded: Vec<String> = if consolidate {
            std::mem::take(&mut state.files).into_keys().collect()
        } else {
            Vec::new()
        };
        if !missing.is_empty() {
            let exported = export(db, &missing, &directory)?;
            report.exported_objects = missing.len();
            report.reused_deltas = exported.reused_deltas;
            state.files.extend(exported.files);
        }
        state.refs = references;
        // Publish the new set before removing what it replaces: a crash in
        // between leaves unrecorded names, which the next load clears.
        save(root, &state)?;
        for name in superseded {
            let _ = remove(&directory.join(name));
        }
        let odb = open(root)?;
        for oid in required {
            ensure!(present(&odb, *oid), "Git object missing from store: {oid}");
        }
    }
    report.packs = state.files.len() / 2;
    report.bytes = state.files.values().sum();
    report.files = state.files.into_keys().collect();
    Ok(report)
}

/// Give a lease its own names for the shared packs: no bytes are written when
/// the filesystem can link. Returns (linked, copied).
pub(crate) fn link(root: &Path, files: &[String], destination: &Path) -> Result<(usize, usize)> {
    std::fs::create_dir_all(destination)?;
    let source = pack_directory(root);
    let (mut linked, mut copied) = (0, 0);
    // Packs first: a reader that finds an index expects its pack.
    for extension in [".pack", ".idx"] {
        for name in files.iter().filter(|name| name.ends_with(extension)) {
            let target = destination.join(name);
            if target.symlink_metadata().is_ok() {
                continue;
            }
            if std::fs::hard_link(source.join(name), &target).is_ok() {
                linked += 1;
            } else {
                // Another volume or a filesystem without links.
                std::fs::copy(source.join(name), &target)?;
                copied += 1;
            }
        }
    }
    Ok((linked, copied))
}

/// Streams a version 2 pack and its index without libgit2's delta search or a
/// second decoding pass: every identifier, offset and checksum is known here.
pub(crate) struct PackWriter {
    file: std::io::BufWriter<std::fs::File>,
    pending: PathBuf,
    hash: sha1_smol::Sha1,
    offset: u64,
    declared: usize,
    entries: Vec<([u8; 20], u32, u64)>,
    scratch: Vec<u8>,
}

impl PackWriter {
    pub(crate) fn create(directory: &Path, objects: usize) -> Result<Self> {
        ensure!(
            u32::try_from(objects).is_ok(),
            "too many objects for one pack"
        );
        std::fs::create_dir_all(directory)?;
        let pending = directory.join(format!("tmp-{}.pack", crate::model::id()));
        let mut writer = Self {
            file: std::io::BufWriter::with_capacity(
                1024 * 1024,
                std::fs::File::create_new(&pending)?,
            ),
            pending,
            hash: sha1_smol::Sha1::new(),
            offset: 0,
            declared: objects,
            entries: Vec::with_capacity(objects),
            scratch: Vec::new(),
        };
        let mut header = b"PACK".to_vec();
        header.extend_from_slice(&2u32.to_be_bytes());
        header.extend_from_slice(&(objects as u32).to_be_bytes());
        writer.emit(&header)?;
        Ok(writer)
    }

    fn emit(&mut self, bytes: &[u8]) -> Result<()> {
        self.hash.update(bytes);
        self.file.write_all(bytes)?;
        self.offset += bytes.len() as u64;
        Ok(())
    }

    fn entry(&mut self, oid: Oid, code: u8, base: Option<Oid>, payload: &[u8]) -> Result<()> {
        let mut out = std::mem::take(&mut self.scratch);
        out.clear();
        let mut size = payload.len();
        let mut byte = (code << 4) | (size & 15) as u8;
        size >>= 4;
        while size != 0 {
            out.push(byte | 0x80);
            byte = (size & 127) as u8;
            size >>= 7;
        }
        out.push(byte);
        if let Some(base) = base {
            out.extend_from_slice(base.as_bytes());
        }
        let mut encoder = flate2::write::ZlibEncoder::new(out, flate2::Compression::fast());
        encoder.write_all(payload)?;
        let out = encoder.finish()?;
        let mut crc = flate2::Crc::new();
        crc.update(&out);
        self.entries
            .push((oid.as_bytes().try_into()?, crc.sum(), self.offset));
        self.emit(&out)?;
        self.scratch = out;
        Ok(())
    }

    pub(crate) fn full(&mut self, oid: Oid, kind: ObjectType, bytes: &[u8]) -> Result<()> {
        let code = match kind {
            ObjectType::Commit => 1,
            ObjectType::Tree => 2,
            ObjectType::Blob => 3,
            ObjectType::Tag => 4,
            _ => anyhow::bail!("invalid object type"),
        };
        self.entry(oid, code, None, bytes)
    }

    /// `delta` is a complete Git delta stream against `base`, which this pack
    /// must also contain: readers resolve a named base inside one pack only.
    fn delta(&mut self, oid: Oid, base: Oid, delta: &[u8]) -> Result<()> {
        self.entry(oid, 7, Some(base), delta)
    }

    /// Durable before visible: the state file vouches for these lengths, so a
    /// power loss must not leave a renamed name over unwritten blocks.
    pub(crate) fn finish(mut self) -> Result<BTreeMap<String, u64>> {
        ensure!(
            self.entries.len() == self.declared,
            "pack object count changed during export"
        );
        let trailer = self.hash.digest().bytes();
        self.file.write_all(&trailer)?;
        let file = self
            .file
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        file.sync_all()?;
        drop(file);
        self.entries.sort_unstable_by_key(|entry| entry.0);
        ensure!(
            self.entries.windows(2).all(|pair| pair[0].0 != pair[1].0),
            "duplicate object in pack"
        );
        let mut index = vec![0xff, b't', b'O', b'c', 0, 0, 0, 2];
        let mut fanout = [0u32; 256];
        for (oid, _, _) in &self.entries {
            fanout[oid[0] as usize] += 1;
        }
        let mut total = 0u32;
        for count in fanout {
            total += count;
            index.extend_from_slice(&total.to_be_bytes());
        }
        for (oid, _, _) in &self.entries {
            index.extend_from_slice(oid);
        }
        for (_, crc, _) in &self.entries {
            index.extend_from_slice(&crc.to_be_bytes());
        }
        let mut large = Vec::new();
        for (_, _, offset) in &self.entries {
            if let Ok(small) = u32::try_from(*offset)
                && small < 0x8000_0000
            {
                index.extend_from_slice(&small.to_be_bytes());
            } else {
                index.extend_from_slice(&(0x8000_0000 | (large.len() / 8) as u32).to_be_bytes());
                large.extend_from_slice(&offset.to_be_bytes());
            }
        }
        index.extend_from_slice(&large);
        index.extend_from_slice(&trailer);
        let checksum = sha1_smol::Sha1::from(&index).digest().bytes();
        index.extend_from_slice(&checksum);

        let directory = self.pending.parent().context("pack directory")?;
        let stem = format!(
            "pack-{}",
            trailer
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
        let staged = self.pending.with_extension("idx");
        let mut file = std::fs::File::create_new(&staged)?;
        file.write_all(&index)?;
        file.sync_all()?;
        drop(file);
        let mut files = BTreeMap::new();
        for (from, extension, length) in [
            (&self.pending, "pack", self.offset + 20),
            (&staged, "idx", index.len() as u64),
        ] {
            let name = format!("{stem}.{extension}");
            let target = directory.join(&name);
            let mut permissions = std::fs::metadata(from)?.permissions();
            permissions.set_readonly(true);
            std::fs::set_permissions(from, permissions)?;
            if target.exists() {
                // Identical content already published under its own hash.
                remove(from)?;
            } else {
                std::fs::rename(from, &target)?;
            }
            files.insert(name, length);
        }
        Ok(files)
    }
}

struct Exported {
    files: BTreeMap<String, u64>,
    reused_deltas: usize,
}

struct Exporter<'a> {
    db: &'a Database,
    objects: crate::kv::Table<'a, &'static str, &'static [u8]>,
    children: HashMap<Oid, Vec<Oid>>,
    writer: PackWriter,
    reused_deltas: usize,
}

impl Exporter<'_> {
    /// Write `oid`, then every stored delta against it while its bytes are in
    /// hand. One reconstruction and one hash per object; a family stays
    /// adjacent in the pack; depth is bounded by the store's delta limit.
    fn emit(&mut self, oid: Oid, base: Option<&[u8]>, depth: u8) -> Result<()> {
        ensure!(depth <= git_delta::MAX_DEPTH, "Git delta chain too deep");
        let guard = self
            .objects
            .get(oid.to_string().as_str())?
            .context("Git object missing from store")?;
        let record = guard.value();
        let parent = git_store::delta_base(record)?;
        let kind =
            ObjectType::from_raw(record[0] as raw::git_object_t).context("invalid object type")?;
        let length = usize::try_from(u64::from_le_bytes(record[1..9].try_into()?))?;
        let rebuilt;
        let bytes: &[u8] = match (parent, base) {
            (None, _) => {
                let payload = &record[11..];
                ensure!(
                    payload.len() == length && Oid::hash_object(kind, payload)? == oid,
                    "Git object integrity failure"
                );
                self.writer.full(oid, kind, payload)?;
                payload
            }
            (Some(parent), Some(source)) => {
                rebuilt = git_delta::apply(source, &record[31..], length)?;
                ensure!(
                    Oid::hash_object(kind, &rebuilt)? == oid,
                    "Git object integrity failure"
                );
                self.writer.delta(oid, parent, &record[31..])?;
                self.reused_deltas += 1;
                &rebuilt
            }
            (Some(_), None) => {
                // Its base lies outside this pack: another repository's
                // object, or one exported earlier. Store it whole.
                let (stored, data) = git_store::read(self.db, oid)?;
                ensure!(stored == kind, "Git object type mismatch");
                rebuilt = data;
                self.writer.full(oid, kind, &rebuilt)?;
                &rebuilt
            }
        };
        if let Some(children) = self.children.remove(&oid) {
            for child in children {
                self.emit(child, Some(bytes), depth + 1)?;
            }
        }
        Ok(())
    }
}

fn export(db: &Database, missing: &[Oid], directory: &Path) -> Result<Exported> {
    let transaction = db.begin_read()?;
    let headers = transaction.open_table(git_store::HEADERS)?;
    let wanted: HashSet<Oid> = missing.iter().copied().collect();
    let mut children: HashMap<Oid, Vec<Oid>> = HashMap::new();
    let mut roots = Vec::new();
    for oid in missing {
        let header = headers
            .get(oid.to_string().as_str())?
            .context("Git metadata missing")?;
        let data = header.value();
        match git_store::delta_base(data)? {
            Some(base) if wanted.contains(&base) => children.entry(base).or_default().push(*oid),
            // Commits and tags first: history walks then stay within a few
            // pages instead of touching every blob's neighbourhood.
            _ => roots.push((
                match data[0] as raw::git_object_t {
                    raw::GIT_OBJECT_COMMIT => 0u8,
                    raw::GIT_OBJECT_TAG => 1,
                    raw::GIT_OBJECT_TREE => 2,
                    _ => 3,
                },
                *oid,
            )),
        }
    }
    roots.sort_unstable();
    let mut exporter = Exporter {
        db,
        objects: transaction.open_table(git_store::OBJECTS)?,
        children,
        writer: PackWriter::create(directory, missing.len())?,
        reused_deltas: 0,
    };
    let result = roots
        .into_iter()
        .try_for_each(|(_, oid)| exporter.emit(oid, None, 0));
    let pending = exporter.writer.pending.clone();
    let finished = result.and_then(|()| {
        ensure!(
            exporter.children.is_empty(),
            "cyclic Git delta in shared history export"
        );
        exporter.writer.finish()
    });
    match finished {
        Ok(files) => Ok(Exported {
            files,
            reused_deltas: exporter.reused_deltas,
        }),
        Err(error) => {
            let _ = remove(&pending);
            let _ = remove(&pending.with_extension("idx"));
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v42_pack_writer_matches_git_readers() -> Result<()> {
        let root = tempfile::tempdir()?;
        let directory = pack_directory(root.path());
        let base = "shared source line with stable content\n"
            .repeat(2000)
            .into_bytes();
        let mut edited = base.clone();
        edited[120..124].copy_from_slice(b"edit");
        let delta = git_delta::encode(&base, &edited).context("fixture delta")?;
        let objects = [
            (ObjectType::Blob, base.clone()),
            (ObjectType::Blob, edited.clone()),
            (ObjectType::Blob, Vec::new()),
            (ObjectType::Blob, vec![7; 300_000]),
        ];
        let ids: Vec<Oid> = objects
            .iter()
            .map(|(kind, bytes)| Oid::hash_object(*kind, bytes))
            .collect::<std::result::Result<_, _>>()?;
        let mut writer = PackWriter::create(&directory, objects.len())?;
        writer.full(ids[0], ObjectType::Blob, &base)?;
        writer.delta(ids[1], ids[0], &delta)?;
        writer.full(ids[2], ObjectType::Blob, &[])?;
        writer.full(ids[3], ObjectType::Blob, &objects[3].1)?;
        let files = writer.finish()?;
        assert_eq!(files.len(), 2);
        for (name, length) in &files {
            assert_eq!(std::fs::metadata(directory.join(name))?.len(), *length);
        }
        let odb = open(root.path())?;
        for (id, (kind, bytes)) in ids.iter().zip(&objects) {
            let object = odb.read(*id)?;
            assert_eq!(object.kind(), *kind);
            assert_eq!(object.data(), bytes.as_slice());
        }
        // An independent implementation recomputes every checksum and hash.
        let pack = files.keys().find(|name| name.ends_with(".pack")).unwrap();
        let verified = std::process::Command::new("git")
            .arg("verify-pack")
            .arg(directory.join(pack))
            .output()?;
        assert!(
            verified.status.success(),
            "{}",
            String::from_utf8_lossy(&verified.stderr)
        );
        Ok(())
    }
}
