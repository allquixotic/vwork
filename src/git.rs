use crate::kv::Database;
use crate::{
    git_store,
    model::{Entry, Repository, Tree},
    physical::FileStat,
};
use anyhow::{Context, Result, bail, ensure};
use git2::{ObjectType, Oid};
use std::{collections::BTreeMap, path::Path, process::Command, sync::Arc};

/// Accept ordinary Git transports or an explicit local path, never helper syntax
/// or command-line options. Git also receives a protocol allowlist after rewrites.
pub fn clone_source(source: &str) -> Result<String> {
    ensure!(
        !source.is_empty()
            && source.len() <= 8192
            && !source.starts_with('-')
            && !source.chars().any(char::is_control),
        "invalid clone source"
    );
    if Path::new(source).is_absolute() {
        return Ok(path_argument(Path::new(source))
            .to_string_lossy()
            .into_owned());
    }
    ensure!(
        !source.chars().any(char::is_whitespace),
        "clone URL cannot contain whitespace"
    );
    if let Some((scheme, rest)) = source.split_once("://") {
        ensure!(
            ["https", "http", "ssh", "git", "file"].contains(&scheme),
            "unsupported Git transport"
        );
        ensure!(
            !rest.contains(['?', '#']),
            "use configured Git credentials, not URL query tokens or fragments"
        );
        let authority = rest.split('/').next().unwrap_or("");
        ensure!(
            scheme == "file" || !authority.is_empty(),
            "clone URL needs a host"
        );
        let host = if let Some((user, host)) = authority.rsplit_once('@') {
            ensure!(
                !user.contains(':'),
                "use configured Git credentials, not a password in the URL"
            );
            host
        } else {
            authority
        };
        ensure!(!host.starts_with('-'), "invalid clone host");
        return Ok(source.into());
    }
    if let Some((host, path)) = source.split_once(':') {
        ensure!(
            !host.is_empty()
                && !host.contains(['/', '\\'])
                && !path.is_empty()
                && !path.starts_with(':')
                && !host.rsplit('@').next().unwrap_or("").starts_with('-'),
            "invalid SSH clone source"
        );
        return Ok(source.into());
    }
    bail!("clone source must be an HTTPS/SSH/Git/file URL, SSH host:path, or absolute local path")
}

/// Git for Windows expects its own normalized path syntax, not Rust's verbatim prefix.
pub fn path_argument(path: &Path) -> std::ffi::OsString {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
        let prefix: Vec<u16> = "\\\\?\\".encode_utf16().collect();
        let unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
        let mut result = if raw.starts_with(&unc) {
            let mut normal: Vec<u16> = "//".encode_utf16().collect();
            normal.extend_from_slice(&raw[unc.len()..]);
            normal
        } else if raw.starts_with(&prefix) {
            raw[prefix.len()..].to_vec()
        } else {
            raw
        };
        for c in &mut result {
            if *c == u16::from(b'\\') {
                *c = u16::from(b'/');
            }
        }
        std::ffi::OsString::from_wide(&result)
    }
    #[cfg(not(windows))]
    {
        path.as_os_str().to_owned()
    }
}

pub fn open(db: &Arc<Database>, repo: &Repository, level: i32) -> Result<git2::Repository> {
    ensure!(repo.stored, "repository has not been imported into Fjall");
    Ok(git2::Repository::from_odb(git_store::odb(
        db.clone(),
        &repo.id,
        level,
    )?)?)
}

/// Import every available object (including history and tags), not just HEAD's tree.
/// Publication is separate and occurs only after the object import succeeds.
pub fn import(
    db: &Arc<Database>,
    source: &git2::Repository,
    repo: &mut Repository,
    level: i32,
    timeout: u64,
) -> Result<()> {
    import_objects(db, source, &repo.id, level, timeout)?;
    repo.refs.clear();
    for reference in source.references()? {
        let reference = reference?.resolve()?;
        if let (Ok(name), Some(id)) = (reference.name(), reference.target()) {
            repo.refs.insert(name.into(), id.to_string());
        }
    }
    if let Ok(head) = source.head() {
        repo.refs
            .insert("HEAD".into(), head.peel_to_commit()?.id().to_string());
    }
    let config = source.config()?;
    repo.author_name = config
        .get_string("user.name")
        .unwrap_or_else(|_| "vwork".into());
    repo.author_email = config
        .get_string("user.email")
        .unwrap_or_else(|_| "vwork@localhost".into());
    repo.stored = true;
    Ok(())
}

#[derive(Default, serde::Serialize)]
pub(crate) struct ImportStats {
    visited_objects: usize,
    reused_objects: usize,
    decoded_objects: usize,
    decoded_bytes: usize,
}

impl ImportStats {
    fn append(&mut self, other: Self) {
        self.visited_objects += other.visited_objects;
        self.reused_objects += other.reused_objects;
        self.decoded_objects += other.decoded_objects;
        self.decoded_bytes = self.decoded_bytes.saturating_add(other.decoded_bytes);
    }
}

fn import_batch(
    db: &Database,
    odb: &git2::Odb<'_>,
    repo: &str,
    ids: &[Oid],
    level: i32,
    deadline: std::time::Instant,
) -> Result<ImportStats> {
    // Different repositories may receive the same OIDs concurrently. Retry
    // pure bounded storage work against the new snapshot, never the transport.
    // Reprepare deltas too: a competing import may choose another base encoding.
    db.transaction(|transaction| {
        let mut stats = ImportStats::default();
        for id in ids {
            ensure!(std::time::Instant::now() < deadline, "Git import timed out");
            stats.visited_objects += 1;
            if git_store::link_existing(transaction, repo, *id)? {
                stats.reused_objects += 1;
            } else {
                let object = odb.read(*id)?;
                git_store::write_record(
                    transaction,
                    repo,
                    *id,
                    object.kind(),
                    object.data(),
                    level,
                )?;
                stats.decoded_objects += 1;
                stats.decoded_bytes = stats.decoded_bytes.saturating_add(object.len());
            }
        }
        Ok(stats)
    })
}

/// Fetch needs only objects, not another scan of the receiver's refs/config.
pub(crate) fn import_objects(
    db: &Arc<Database>,
    source: &git2::Repository,
    repo: &str,
    level: i32,
    timeout: u64,
) -> Result<ImportStats> {
    let mut stats = ImportStats::default();
    let odb = source.odb()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let mut failure = None;
    // Keep only bounded OID batches so an OCC retry can reread owned packs
    // without retaining another copy of all decoded object bodies in memory.
    let mut ids = Vec::new();
    let mut batch_bytes = 0usize;
    odb.foreach(|id| {
        let result = (|| -> Result<()> {
            ensure!(std::time::Instant::now() < deadline, "Git import timed out");
            ids.push(*id);
            // Existing objects need no source payload/header read at all.
            if !git_store::has_object(db, *id)? {
                batch_bytes = batch_bytes.saturating_add(odb.read_header(*id)?.0);
            }
            if ids.len() >= 256 || batch_bytes >= 64 * 1024 * 1024 {
                stats.append(import_batch(db, &odb, repo, &ids, level, deadline)?);
                ids.clear();
                batch_bytes = 0;
            }
            Ok(())
        })();
        if let Err(error) = result {
            failure = Some(error);
            false
        } else {
            true
        }
    })
    .or_else(|error| {
        if failure.is_some() {
            Ok(())
        } else {
            Err(error)
        }
    })?;
    if let Some(error) = failure {
        return Err(error);
    }
    if !ids.is_empty() {
        stats.append(import_batch(db, &odb, repo, &ids, level, deadline)?);
    }
    Ok(stats)
}

pub fn resolve(db: &Arc<Database>, repo: &Repository, reference: &str) -> Result<String> {
    ensure!(
        !reference.is_empty() && !reference.starts_with('-') && !reference.contains('\0'),
        "invalid revision"
    );
    let split = reference.find(['~', '^', ':']).unwrap_or(reference.len());
    let name = &reference[..split];
    let mut matches = Vec::new();
    for candidate in [
        name.to_string(),
        format!("refs/{name}"),
        format!("refs/heads/{name}"),
        format!("refs/tags/{name}"),
        format!("refs/remotes/{name}"),
    ] {
        if let Some(oid) = repo.refs.get(&candidate) {
            matches.push(oid.clone());
        }
    }
    matches.sort();
    matches.dedup();
    ensure!(matches.len() <= 1, "ambiguous revision");
    let oid = if let Some(id) = matches.first() {
        id.clone()
    } else {
        ensure!(
            name.len() == 40 && name.bytes().all(|x| x.is_ascii_hexdigit()),
            "unknown reference; use a registered ref or full commit ID"
        );
        name.into()
    };
    let repository = open(db, repo, 3)?;
    Ok(repository
        .revparse_single(&format!("{oid}{}", &reference[split..]))?
        .peel_to_commit()?
        .id()
        .to_string())
}

pub fn tree(db: &Arc<Database>, repo: &Repository, commit: &str) -> Result<Tree> {
    let repository = open(db, repo, 3)?;
    let root = repository.find_commit(Oid::from_str(commit)?)?.tree()?;
    fn visit(
        repository: &git2::Repository,
        tree: git2::Tree<'_>,
        prefix: &str,
        result: &mut Tree,
    ) -> Result<()> {
        for entry in &tree {
            let path = format!(
                "{prefix}{}",
                entry.name().context("non-UTF8 Git paths are unsupported")?
            );
            crate::model::path(&path)?;
            if entry.kind() == Some(ObjectType::Tree) {
                visit(
                    repository,
                    repository.find_tree(entry.id())?,
                    &format!("{path}/"),
                    result,
                )?;
            } else {
                let size = if entry.kind() == Some(ObjectType::Blob) {
                    repository.odb()?.read_header(entry.id())?.0 as u64
                } else {
                    0
                };
                result.insert(
                    path,
                    Entry {
                        source: "git".into(),
                        oid: entry.id().to_string(),
                        mode: format!("{:06o}", entry.filemode()),
                        size,
                    },
                );
            }
        }
        Ok(())
    }
    let mut result = Tree::new();
    visit(&repository, root, "", &mut result)?;
    Ok(result)
}

/// Resolve one path in the immutable Git tree, without walking sibling trees
/// or reading every blob header. All objects come from the shared database.
pub fn entry(
    db: &Arc<Database>,
    repo: &Repository,
    commit: &str,
    path: &str,
) -> Result<Option<Entry>> {
    let repository = open(db, repo, 3)?;
    let root = repository.find_commit(Oid::from_str(commit)?)?.tree()?;
    let entry = match root.get_path(Path::new(path)) {
        Ok(entry) => entry,
        Err(e) if e.code() == git2::ErrorCode::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    if entry.kind() == Some(ObjectType::Tree) {
        return Ok(None);
    }
    Ok(Some(Entry {
        source: "git".into(),
        oid: entry.id().to_string(),
        mode: format!("{:06o}", entry.filemode()),
        size: if entry.kind() == Some(ObjectType::Blob) {
            repository.odb()?.read_header(entry.id())?.0 as u64
        } else {
            0
        },
    }))
}

pub struct Reader {
    db: Arc<Database>,
    repo: String,
}
impl Reader {
    pub fn new(db: &Arc<Database>, repo: &str) -> Result<Self> {
        Ok(Self {
            db: db.clone(),
            repo: repo.into(),
        })
    }
    pub fn blob(&mut self, oid: &str, limit: usize) -> Result<Vec<u8>> {
        let oid = Oid::from_str(oid)?;
        ensure!(
            git_store::contains(&self.db, &self.repo, oid)?,
            "Git object does not belong to repository"
        );
        let (kind, bytes) = git_store::read(&self.db, oid)?;
        ensure!(
            kind == ObjectType::Blob && bytes.len() <= limit,
            "not a blob or exceeds max_file_bytes"
        );
        Ok(bytes)
    }
}

pub fn write_tree(repository: &git2::Repository, entries: &Tree) -> Result<Oid> {
    #[derive(Default)]
    struct Node {
        files: Tree,
        dirs: BTreeMap<String, Node>,
    }
    fn insert(node: &mut Node, parts: &[&str], entry: Entry) -> Result<()> {
        if parts.len() == 1 {
            ensure!(!node.dirs.contains_key(parts[0]), "file/directory conflict");
            node.files.insert(parts[0].into(), entry);
        } else {
            ensure!(
                !node.files.contains_key(parts[0]),
                "file/directory conflict"
            );
            insert(
                node.dirs.entry(parts[0].into()).or_default(),
                &parts[1..],
                entry,
            )?;
        }
        Ok(())
    }
    fn emit(repository: &git2::Repository, node: &Node) -> Result<Oid> {
        let mut builder = repository.treebuilder(None)?;
        for (name, entry) in &node.files {
            builder.insert(
                name,
                Oid::from_str(&entry.oid)?,
                i32::from_str_radix(&entry.mode, 8)?,
            )?;
        }
        for (name, child) in &node.dirs {
            builder.insert(name, emit(repository, child)?, 0o040000)?;
        }
        Ok(builder.write()?)
    }
    let mut node = Node::default();
    for (name, entry) in entries {
        ensure!(entry.source == "git", "unconverted virtual blob");
        insert(
            &mut node,
            &name.split('/').collect::<Vec<_>>(),
            entry.clone(),
        )?;
    }
    emit(repository, &node)
}

pub fn log(db: &Arc<Database>, repo: &Repository, reference: &str, limit: usize) -> Result<String> {
    if limit == 0 {
        return Ok(String::new());
    }
    let repository = open(db, repo, 3)?;
    // libgit2 sets its `limited` flag for *every* non-NONE sorting mode,
    // including TIME, and preprocesses reachable history in limit_list.
    // Maintain our own date-prioritized frontier so short previews only read
    // parents of emitted commits. Both merge parents remain reachable.
    let first = repository.find_commit(Oid::from_str(reference)?)?;
    let mut pending = std::collections::BinaryHeap::from([(first.time().seconds(), first.id())]);
    let mut discovered = std::collections::HashSet::from([first.id()]);
    let mut text = String::new();
    let mut emitted = 0;
    while let Some((_, oid)) = pending.pop() {
        let c = repository.find_commit(oid)?;
        text.push_str(&format!(
            "{}\t{}\t{}\n",
            c.id(),
            c.time().seconds(),
            c.summary()?.unwrap_or("")
        ));
        emitted += 1;
        if emitted == limit {
            break;
        }
        for parent in c.parent_ids() {
            if discovered.insert(parent) {
                let parent = repository.find_commit(parent)?;
                pending.push((parent.time().seconds(), parent.id()));
            }
        }
    }
    Ok(text)
}

/// One sorted packed-refs file instead of a lock, write and rename per ref.
pub(crate) fn write_packed_refs(admin: &Path, refs: &BTreeMap<String, String>) -> Result<()> {
    use std::io::Write;
    let mut file = std::io::BufWriter::new(std::fs::File::create(admin.join("packed-refs"))?);
    writeln!(file, "# pack-refs with: sorted")?;
    for (name, oid) in refs {
        if name == "HEAD" {
            continue;
        }
        ensure!(
            git2::Reference::is_valid_name(name) && name.starts_with("refs/"),
            "invalid stored ref name"
        );
        writeln!(file, "{} {name}", Oid::from_str(oid)?)?;
    }
    file.flush()?;
    Ok(())
}

fn config_value(path: &Path) -> String {
    let mut out = String::from("\"");
    for c in path_argument(path).to_string_lossy().chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

const CONFIG_MARK: &str = "# vwork temporary view\n";

/// Later sections override what libgit2's filesystem probe wrote, so the whole
/// view configuration is one append instead of a rewrite per key.
fn view_config(admin: &Path, checkout: &Path) -> Result<()> {
    let path = admin.join("config");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let block = format!(
        "{CONFIG_MARK}[core]\n\tbare = false\n\tautocrlf = false\n\tsymlinks = false\n\tlogallrefupdates = false\n\tfilemode = {}\n\ttrustctime = false\n\tcheckstat = minimal\n\tworktree = {}\n\thookspath = {}\n[gc]\n\tauto = 0\n",
        !cfg!(windows),
        config_value(checkout),
        config_value(&admin.join("disabled-hooks")),
    );
    let probed = existing.split(CONFIG_MARK).next().unwrap_or_default();
    let wanted = format!("{probed}{block}");
    if existing != wanted {
        std::fs::write(path, wanted)?;
    }
    Ok(())
}

/// Version 2 index written directly: entries are already sorted and their
/// objects already checked, so there is nothing for a tree walk to add.
struct IndexWriter {
    bytes: Vec<u8>,
    count: u32,
}
impl IndexWriter {
    fn new(entries: usize) -> Self {
        let mut bytes = Vec::with_capacity(12 + entries * 104 + 20);
        bytes.extend_from_slice(b"DIRC");
        bytes.extend_from_slice(&2u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        Self { bytes, count: 0 }
    }
    fn push(&mut self, path: &str, oid: Oid, mode: u32, stat: Option<&FileStat>) {
        let zero = FileStat::default();
        let stat = stat.unwrap_or(&zero);
        // Git stores and compares the low 32 bits of every field.
        for field in [
            stat.changed.0 as u32,
            stat.changed.1,
            stat.modified.0 as u32,
            stat.modified.1,
            0,
            stat.inode as u32,
            mode,
            stat.owner.0,
            stat.owner.1,
            stat.size as u32,
        ] {
            self.bytes.extend_from_slice(&field.to_be_bytes());
        }
        self.bytes.extend_from_slice(oid.as_bytes());
        self.bytes
            .extend_from_slice(&(path.len().min(0xfff) as u16).to_be_bytes());
        self.bytes.extend_from_slice(path.as_bytes());
        // NUL-terminated and padded to eight bytes.
        let padding = 8 - (62 + path.len()) % 8;
        self.bytes.extend(std::iter::repeat_n(0, padding));
        self.count += 1;
    }
    fn finish(mut self, admin: &Path) -> Result<()> {
        self.bytes[8..12].copy_from_slice(&self.count.to_be_bytes());
        let checksum = sha1_smol::Sha1::from(&self.bytes).digest().bytes();
        self.bytes.extend_from_slice(&checksum);
        // Readers of an existing view see the old or the new index, never part.
        let pending = admin.join("index.vwork");
        std::fs::write(&pending, &self.bytes)?;
        std::fs::rename(pending, admin.join("index"))?;
        Ok(())
    }
}

#[derive(Default, Debug, Clone, serde::Serialize)]
pub struct ViewReport {
    pub packs_linked: usize,
    pub packs_copied: usize,
    pub private_objects: usize,
}

/// A disposable compatibility repository for external tools that require .git.
/// All authoritative objects remain in Fjall. History is the repository's
/// shared pack cache under this lease's own names; the view writes only what
/// is its own: configuration, refs, index and blobs absent from history.
/// No alternates, no original checkout access.
#[allow(clippy::too_many_arguments)]
pub fn prepare_build(
    source: &Repository,
    history: &Path,
    packs: &[String],
    admin: &Path,
    checkout: &Path,
    base: &str,
    entries: &Tree,
    stats: &BTreeMap<String, FileStat>,
    mut content: impl FnMut(&Entry) -> Result<Vec<u8>>,
) -> Result<(git2::Repository, ViewReport)> {
    let mut report = ViewReport::default();
    if !admin.join("HEAD").exists() {
        let mut options = git2::RepositoryInitOptions::new();
        options.bare(true).external_template(false);
        git2::Repository::init_opts(admin, &options)?;
        std::fs::create_dir_all(admin.join("disabled-hooks"))?;
        // The ownership record lives beside the source, not in it.
        std::fs::write(
            admin.join("info").join("exclude"),
            format!("/{}\n", crate::physical::MARKER),
        )?;
    }
    view_config(admin, checkout)?;
    (report.packs_linked, report.packs_copied) =
        crate::git_cache::link(history, packs, &admin.join("objects").join("pack"))?;
    // Views from earlier versions hold one loose file per ref, which would
    // shadow the packed file. Fresh views have only the two empty directories.
    let refs = admin.join("refs");
    if walkdir::WalkDir::new(&refs)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .any(|entry| entry.file_type().is_file())
    {
        std::fs::remove_dir_all(&refs)?;
        std::fs::create_dir_all(refs.join("heads"))?;
        std::fs::create_dir_all(refs.join("tags"))?;
    }
    write_packed_refs(admin, &source.refs)?;
    let base = Oid::from_str(base)?;
    std::fs::write(admin.join("HEAD"), format!("{base}\n"))?;
    let view = git2::Repository::open(admin)?;
    view.set_workdir(checkout, false)?;
    let odb = view.odb()?;
    ensure!(
        odb.exists(base),
        "base commit missing from temporary Git history"
    );
    let mut index = IndexWriter::new(entries.len());
    let mut seen = std::collections::HashSet::new();
    let mut absent = Vec::new();
    for (name, entry) in entries {
        let (oid, bytes) = if entry.source == "git" {
            (Oid::from_str(&entry.oid)?, None)
        } else {
            let bytes = content(entry)?;
            (Oid::hash_object(ObjectType::Blob, &bytes)?, Some(bytes))
        };
        // Virtual edits are not history. One lookup per distinct blob keeps
        // the view complete whatever the shared packs happen to contain.
        if seen.insert(oid) && !odb.exists_ext(oid, git2::OdbLookupFlags::NO_REFRESH) {
            absent.push((oid, entry, bytes));
        }
        index.push(
            name,
            oid,
            u32::from_str_radix(&entry.mode, 8)?,
            stats.get(name),
        );
    }
    if !absent.is_empty() {
        // One private pack however many files changed: two files, not one
        // loose object and directory per edit.
        let mut writer = crate::git_cache::PackWriter::create(
            &admin.join("objects").join("pack"),
            absent.len(),
        )?;
        for (oid, entry, bytes) in &absent {
            let owned;
            let bytes = match bytes {
                Some(bytes) => bytes,
                None => {
                    owned = content(entry)?;
                    &owned
                }
            };
            ensure!(
                Oid::hash_object(ObjectType::Blob, bytes)? == *oid,
                "Git object integrity failure"
            );
            writer.full(*oid, ObjectType::Blob, bytes)?;
        }
        writer.finish()?;
        report.private_objects = absent.len();
    }
    index.finish(admin)?;
    let pointer = format!("gitdir: {}\n", path_argument(admin).to_string_lossy());
    let dot_git = checkout.join(".git");
    if std::fs::read_to_string(&dot_git).ok().as_deref() != Some(pointer.as_str()) {
        std::fs::write(dot_git, pointer)?;
    }
    drop(odb);
    // Reopen so the returned handle sees the index and private pack just written.
    drop(view);
    let view = git2::Repository::open(admin)?;
    view.set_workdir(checkout, false)?;
    Ok((view, report))
}

pub fn changed(view: &git2::Repository) -> Result<Vec<String>> {
    let mut options = git2::DiffOptions::new();
    options.ignore_submodules(true);
    let diff = view.diff_index_to_workdir(None, Some(&mut options))?;
    Ok(diff
        .deltas()
        .filter_map(|d| {
            d.new_file()
                .path()
                .or(d.old_file().path())
                .map(|p| p.to_string_lossy().replace('\\', "/"))
        })
        .collect())
}

/// Remove inherited Git-directory/index overrides before launching build tools.
pub fn clear_overrides(command: &mut Command) {
    for key in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CONFIG_COUNT",
        "GIT_CONFIG_PARAMETERS",
    ] {
        command.env_remove(key);
    }
}
