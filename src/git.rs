use crate::kv::Database;
use crate::{
    git_store,
    model::{Entry, Repository, Tree},
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
    let odb = source.odb()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
    let mut failure = None;
    // Bound writer hold time and amortize durable commits across small batches.
    let mut transaction = Some(db.begin_write()?);
    let mut batch_objects = 0usize;
    let mut batch_bytes = 0usize;
    odb.foreach(|id| {
        let result = (|| -> Result<()> {
            ensure!(std::time::Instant::now() < deadline, "Git import timed out");
            let object = odb.read(*id)?;
            git_store::write_record(
                transaction.as_ref().unwrap(),
                &repo.id,
                *id,
                object.kind(),
                object.data(),
                level,
            )?;
            batch_objects += 1;
            batch_bytes = batch_bytes.saturating_add(object.len());
            if batch_objects >= 256 || batch_bytes >= 64 * 1024 * 1024 {
                transaction.take().unwrap().commit()?;
                transaction = Some(db.begin_write()?);
                batch_objects = 0;
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
    transaction.take().unwrap().commit()?;
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

/// A disposable compatibility repository for external tools that require .git.
/// All authoritative objects remain in Fjall. No alternates or original checkout access.
pub fn prepare_build(
    db: &Arc<Database>,
    source: &Repository,
    admin: &Path,
    checkout: &Path,
    base: &str,
    entries: &Tree,
    mut content: impl FnMut(&Entry) -> Result<Vec<u8>>,
) -> Result<git2::Repository> {
    let mut options = git2::RepositoryInitOptions::new();
    options.bare(true).external_template(false);
    let view = git2::Repository::init_opts(admin, &options)?;
    let mut config = view.config()?;
    for (key, value) in [
        ("core.bare", "false"),
        ("core.autocrlf", "false"),
        ("core.symlinks", "false"),
        ("core.logAllRefUpdates", "false"),
        ("gc.auto", "0"),
        (
            "core.filemode",
            if cfg!(windows) { "false" } else { "true" },
        ),
    ] {
        config.set_str(key, value)?;
    }
    config.set_str("core.worktree", &path_argument(checkout).to_string_lossy())?;
    let hooks = admin.join("disabled-hooks");
    std::fs::create_dir_all(&hooks)?;
    config.set_str("core.hooksPath", &path_argument(&hooks).to_string_lossy())?;
    view.set_workdir(checkout, false)?;
    let odb = view.odb()?;
    export_packs(db, source, &view)?;
    let names: Vec<String> = view
        .references()?
        .filter_map(|r| r.ok()?.name().ok().map(String::from))
        .collect();
    for name in names {
        if !source.refs.contains_key(&name) {
            view.find_reference(&name)?.delete()?;
        }
    }
    for (name, oid) in &source.refs {
        if name != "HEAD" {
            view.reference(name, Oid::from_str(oid)?, true, "vwork temporary view")?;
        }
    }
    view.set_head_detached(Oid::from_str(base)?)?;
    let mut converted = entries.clone();
    for (name, entry) in entries {
        if entry.source != "git" {
            let mut e = entry.clone();
            e.oid = view.blob(&content(entry)?)?.to_string();
            e.source = "git".into();
            converted.insert(name.clone(), e);
        }
    }
    let tree = write_tree(&view, &converted)?;
    let mut index = view.index()?;
    index.read_tree(&view.find_tree(tree)?)?;
    index.write()?;
    std::fs::write(
        checkout.join(".git"),
        format!("gitdir: {}\n", path_argument(admin).to_string_lossy()),
    )?;
    drop(index);
    drop(odb);
    drop(config);
    Ok(view)
}

/// External Git needs a filesystem ODB. Export bounded, streamed packs instead
/// of one loose file (and its metadata/flush work) per historical object. Keep
/// full history, tags and overlays; never borrow the user's source checkout.
fn export_packs(db: &Arc<Database>, source: &Repository, view: &git2::Repository) -> Result<()> {
    use std::io::Write;
    const MAX_OBJECTS: usize = 4096;
    const MAX_BYTES: usize = 64 * 1024 * 1024;
    let repository = open(db, source, 3)?;
    // Bound libgit2's delta-search memory and worker count. PackBuilder does
    // not honor Git CLI's pack.window setting, so bound each batch as well.
    let mut config = view.config()?;
    config.set_i32("pack.windowMemory", 16 * 1024 * 1024)?;
    config.set_i32("pack.deltaCacheSize", 16 * 1024 * 1024)?;
    repository.set_config(&config)?;
    let input = repository.odb()?;
    let output = view.odb()?;
    let mut objects = git_store::members(db, &source.id)?.into_iter().peekable();
    while objects.peek().is_some() {
        let mut builder = repository.packbuilder()?;
        builder.set_threads(1);
        let mut weight = 0usize;
        while let Some(&oid) = objects.peek() {
            if output.exists(oid) {
                objects.next();
                continue;
            }
            let (size, _) = input.read_header(oid)?;
            if builder.object_count() > 0
                && (builder.object_count() >= MAX_OBJECTS
                    || weight.saturating_add(size) > MAX_BYTES)
            {
                break;
            }
            // A single large object is permitted, still bounded by the store's
            // object limit. No whole-pack buffer or loose-object staging.
            builder.insert_object(oid, None)?;
            weight = weight.saturating_add(size);
            objects.next();
        }
        if builder.object_count() == 0 {
            continue;
        }
        let mut writer = output.packwriter()?;
        let mut io_error = None;
        let packed = builder.foreach(|bytes| match writer.write_all(bytes) {
            Ok(()) => true,
            Err(error) => {
                io_error = Some(error);
                false
            }
        });
        if let Some(error) = io_error {
            return Err(error.into());
        }
        packed?;
        writer.commit()?;
    }
    Ok(())
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
