use crate::model::{Entry, Repository, Tree};
use anyhow::{Context, Result, bail, ensure};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

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

pub fn command(repo: &Repository) -> Command {
    let mut cmd = Command::new("git");
    cmd.args(["-c", "core.longpaths=true"])
        .arg("--git-dir")
        .arg(path_argument(&repo.git_dir));
    // Never inherit caller index/worktree overrides or interactive Git behavior.
    for key in [
        "GIT_INDEX_FILE",
        "GIT_WORK_TREE",
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

pub fn run(repo: &Repository, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut child = command(repo)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start Git (Git must be installed)")?;
    // Drain stdout/stderr while writing, avoiding pipe deadlocks for large objects.
    let bytes = input.unwrap_or_default().to_vec();
    let mut stdin = child.stdin.take().unwrap();
    let writer = std::thread::spawn(move || stdin.write_all(&bytes));
    let result = child.wait_with_output()?;
    writer
        .join()
        .map_err(|_| anyhow::anyhow!("Git input thread panicked"))??;
    ensure!(
        result.status.success(),
        "Git {} failed: {}",
        args.first().unwrap_or(&""),
        String::from_utf8_lossy(&result.stderr)
    );
    Ok(result.stdout)
}

pub fn resolve(repo: &Repository, reference: &str) -> Result<String> {
    ensure!(
        !reference.starts_with('-') && !reference.contains('\0'),
        "invalid revision"
    );
    Ok(String::from_utf8(run(
        repo,
        &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        None,
    )?)?
    .trim()
    .into())
}

pub fn tree(repo: &Repository, commit: &str) -> Result<Tree> {
    let raw = run(repo, &["ls-tree", "-rlz", "--full-tree", commit], None)?;
    let mut result = Tree::new();
    for item in raw.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let tab = item
            .iter()
            .position(|b| *b == b'\t')
            .context("invalid ls-tree record")?;
        let header = std::str::from_utf8(&item[..tab])?;
        let parts: Vec<_> = header.split_whitespace().collect();
        ensure!(parts.len() == 4, "invalid Git tree header");
        let name = String::from_utf8(item[tab + 1..].to_vec())
            .context("non-UTF8 Git paths are not supported")?;
        crate::model::path(&name)?;
        result.insert(
            name,
            Entry {
                source: "git".into(),
                oid: parts[2].into(),
                mode: parts[0].into(),
                size: parts[3].parse().unwrap_or(0),
            },
        );
    }
    Ok(result)
}

pub struct Reader {
    child: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
}
impl Reader {
    pub fn new(repo: &Repository) -> Result<Self> {
        let mut child = command(repo)
            .arg("cat-file")
            .arg("--batch")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self {
            input: child.stdin.take(),
            output: BufReader::new(child.stdout.take().unwrap()),
            child,
        })
    }
    pub fn blob(&mut self, oid: &str, limit: usize) -> Result<Vec<u8>> {
        ensure!(
            oid.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid object id"
        );
        writeln!(self.input.as_mut().context("Git reader closed")?, "{oid}")?;
        self.input.as_mut().unwrap().flush()?;
        let mut header = String::new();
        self.output.read_line(&mut header)?;
        let parts: Vec<_> = header.split_whitespace().collect();
        ensure!(
            parts.len() == 3 && parts[1] == "blob",
            "Git object unavailable or not blob: {oid}"
        );
        let len: usize = parts[2].parse()?;
        ensure!(len <= limit, "file exceeds max_file_bytes ({limit})");
        let mut content = vec![0; len];
        self.output.read_exact(&mut content)?;
        let mut newline = [0];
        self.output.read_exact(&mut newline)?;
        ensure!(newline[0] == b'\n', "invalid cat-file framing");
        Ok(content)
    }
}
impl Drop for Reader {
    fn drop(&mut self) {
        self.input.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn write_tree(repo: &Repository, entries: &Tree) -> Result<String> {
    #[derive(Default)]
    struct Node {
        files: Tree,
        dirs: std::collections::BTreeMap<String, Node>,
    }
    fn insert(node: &mut Node, parts: &[&str], e: Entry) -> Result<()> {
        if parts.len() == 1 {
            ensure!(!node.dirs.contains_key(parts[0]), "file/directory conflict");
            node.files.insert(parts[0].into(), e);
        } else {
            ensure!(
                !node.files.contains_key(parts[0]),
                "file/directory conflict"
            );
            insert(
                node.dirs.entry(parts[0].into()).or_default(),
                &parts[1..],
                e,
            )?;
        }
        Ok(())
    }
    fn emit(repo: &Repository, node: &Node) -> Result<String> {
        let mut records = Vec::new();
        for (name, e) in &node.files {
            let kind = if e.mode == "160000" { "commit" } else { "blob" };
            records.extend_from_slice(format!("{} {kind} {}\t{name}\0", e.mode, e.oid).as_bytes());
        }
        for (name, dir) in &node.dirs {
            records.extend_from_slice(
                format!("040000 tree {}\t{name}\0", emit(repo, dir)?).as_bytes(),
            );
        }
        Ok(
            String::from_utf8(run(repo, &["mktree", "-z"], Some(&records))?)?
                .trim()
                .into(),
        )
    }
    let mut root = Node::default();
    for (name, e) in entries {
        if e.source != "git" {
            bail!("unconverted virtual blob");
        }
        insert(&mut root, &name.split('/').collect::<Vec<_>>(), e.clone())?;
    }
    emit(repo, &root)
}

/// Build a private Git view without cloning objects or touching the source index/refs.
/// The private index represents virtual content; HEAD remains its real Git base.
pub fn prepare_build(
    source: &Repository,
    admin: &Path,
    checkout: &Path,
    base: &str,
    changes: &std::collections::BTreeMap<String, Option<Entry>>,
    mut content: impl FnMut(&Entry) -> Result<Vec<u8>>,
) -> Result<Repository> {
    let object_format =
        String::from_utf8(run(source, &["rev-parse", "--show-object-format"], None)?)?;
    let object_format = object_format.trim();
    ensure!(
        ["sha1", "sha256"].contains(&object_format),
        "unsupported Git object format"
    );
    let view = Repository {
        id: source.id.clone(),
        path: checkout.to_path_buf(),
        git_dir: admin.to_path_buf(),
    };
    if !admin.join("HEAD").exists() {
        std::fs::create_dir_all(admin)?;
        let output = Command::new("git")
            .args([
                "init",
                "--bare",
                "--quiet",
                "--template=",
                &format!("--object-format={object_format}"),
            ])
            .arg(path_argument(admin))
            .output()?;
        ensure!(
            output.status.success(),
            "initialize build Git: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let objects = String::from_utf8(run(
        source,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "objects",
        ],
        None,
    )?)?;
    let object_path = path_argument(Path::new(objects.trim()));
    let object_path = object_path
        .to_str()
        .context("Git object path is not UTF-8")?;
    ensure!(
        !object_path.contains(['\n', '\r']),
        "newline in Git object path"
    );
    std::fs::create_dir_all(admin.join("objects/info"))?;
    std::fs::write(
        admin.join("objects/info/alternates"),
        format!("{object_path}\n"),
    )?;
    for (key, value) in [
        ("core.bare", "false"),
        ("core.autocrlf", "false"),
        ("core.symlinks", "false"),
        ("core.logAllRefUpdates", "false"),
        ("gc.auto", "0"),
        ("maintenance.auto", "false"),
        ("index.version", "4"),
        (
            "core.filemode",
            if cfg!(windows) { "false" } else { "true" },
        ),
    ] {
        run(&view, &["config", "--local", key, value], None)?;
    }
    let worktree = path_argument(checkout);
    run(
        &view,
        &[
            "config",
            "--local",
            "core.worktree",
            worktree.to_str().context("non-UTF8 build path")?,
        ],
        None,
    )?;
    let hooks = admin.join("disabled-hooks");
    std::fs::create_dir_all(&hooks)?;
    let hooks = path_argument(&hooks);
    run(
        &view,
        &[
            "config",
            "--local",
            "core.hooksPath",
            hooks.to_str().context("non-UTF8 hooks path")?,
        ],
        None,
    )?;
    // Only ref names and OIDs are copied. Objects remain in the source's object database.
    let mut refs = std::collections::BTreeMap::new();
    for line in String::from_utf8(run(
        source,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
            "refs/tags",
            "refs/remotes",
        ],
        None,
    )?)?
    .lines()
    {
        let (name, oid) = line.split_once(' ').context("invalid Git ref record")?;
        refs.insert(name.to_string(), oid.to_string());
    }
    let previous = String::from_utf8(run(&view, &["for-each-ref", "--format=%(refname)"], None)?)?;
    let mut updates = String::new();
    for name in previous.lines() {
        if !refs.contains_key(name) {
            updates.push_str(&format!("delete {name}\n"));
        }
    }
    for (name, oid) in refs {
        updates.push_str(&format!("update {name} {oid}\n"));
    }
    if !updates.is_empty() {
        run(&view, &["update-ref", "--stdin"], Some(updates.as_bytes()))?;
    }
    run(&view, &["update-ref", "--no-deref", "HEAD", base], None)?;
    run(&view, &["read-tree", base], None)?;
    let mut index = Vec::new();
    for (path, entry) in changes {
        let (mode, oid) = if let Some(entry) = entry {
            let oid = if entry.source == "git" {
                entry.oid.clone()
            } else {
                String::from_utf8(run(
                    &view,
                    &["hash-object", "-w", "--stdin"],
                    Some(&content(entry)?),
                )?)?
                .trim()
                .to_string()
            };
            (entry.mode.clone(), oid)
        } else {
            (
                "0".into(),
                "0".repeat(if object_format == "sha256" { 64 } else { 40 }),
            )
        };
        index.extend_from_slice(format!("{mode} {oid}\t{path}\0").as_bytes());
    }
    if !index.is_empty() {
        run(&view, &["update-index", "-z", "--index-info"], Some(&index))?;
    }
    let pointer = path_argument(admin);
    std::fs::write(
        checkout.join(".git"),
        format!(
            "gitdir: {}\n",
            pointer.to_str().context("non-UTF8 build Git path")?
        ),
    )?;
    Ok(view)
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
