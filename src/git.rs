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
