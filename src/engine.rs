use crate::{
    git, git_store,
    model::*,
    packages::{self, Project, Step},
};
use anyhow::{Context, Result, bail, ensure};
use base64::Engine as _;
use redb::{Database, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, RwLock};
use wait_timeout::ChildExt;

enum BuildRequest {
    Task(Task),
    Package(Project, String),
}

pub(crate) const RECORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("records_v1");

pub struct Engine {
    pub db: Arc<Database>,
    pub home: PathBuf,
    pub config: Config,
    build_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    pub(crate) lifecycle: RwLock<()>,
    pub(crate) temporary_lock: Mutex<()>,
    transfer_lock: Mutex<()>,
    base_cache: Mutex<HashMap<String, Tree>>,
}

fn key(kind: &str, id: &str) -> String {
    format!("{kind}/{id}")
}
fn string<'a>(a: &'a Value, field: &str) -> Result<&'a str> {
    a.get(field)
        .and_then(Value::as_str)
        .with_context(|| format!("missing string argument: {field}"))
}
fn optional<'a>(a: &'a Value, field: &str, fallback: &'a str) -> &'a str {
    a.get(field).and_then(Value::as_str).unwrap_or(fallback)
}
fn number(a: &Value, field: &str, fallback: usize) -> usize {
    a.get(field)
        .and_then(Value::as_u64)
        .map(|x| x as usize)
        .unwrap_or(fallback)
}
fn bool_arg(a: &Value, field: &str) -> bool {
    a.get(field).and_then(Value::as_bool).unwrap_or(false)
}

impl Engine {
    pub fn open(home: impl AsRef<Path>, config: Config) -> Result<Self> {
        config.validate()?;
        std::fs::create_dir_all(home.as_ref())?;
        let home = home.as_ref().canonicalize()?;
        let db = Database::create(home.join("workspaces.redb"))?;
        let txn = db.begin_write()?;
        {
            let mut table = txn.open_table(RECORDS)?;
            if let Some(v) = table.get("schema")? {
                ensure!(
                    [b"1".as_slice(), b"2".as_slice()].contains(&v.value()),
                    "unsupported database schema"
                );
            }
            if table.get("schema")?.is_none() {
                table.insert("schema", b"1".as_slice())?;
            }
        }
        txn.commit()?;
        git_store::initialize(&db)?;
        let engine = Self {
            db: Arc::new(db),
            home,
            config,
            build_locks: Mutex::new(HashMap::new()),
            lifecycle: RwLock::new(()),
            temporary_lock: Mutex::new(()),
            transfer_lock: Mutex::new(()),
            base_cache: Mutex::new(HashMap::new()),
        };
        engine.cleanup_transfers()?;
        engine.migrate_repositories()?;
        engine.migrate_legacy_copies()?;
        engine.reap_temporary()?;
        Ok(engine)
    }

    pub(crate) fn get<T: DeserializeOwned>(&self, k: &str) -> Result<T> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(RECORDS)?;
        let bytes = t.get(k)?.with_context(|| format!("not found: {k}"))?;
        Ok(serde_json::from_slice(bytes.value())?)
    }
    pub(crate) fn put<T: Serialize>(&self, k: &str, value: &T) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            txn.open_table(RECORDS)?
                .insert(k, serde_json::to_vec(value)?.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }
    pub(crate) fn scan(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>> {
        let txn = self.db.begin_read()?;
        let t = txn.open_table(RECORDS)?;
        let mut out = Vec::new();
        for item in t.range(prefix..)? {
            let (k, v) = item?;
            if !k.value().starts_with(prefix) {
                break;
            }
            out.push((k.value().into(), v.value().to_vec()));
        }
        Ok(out)
    }
    pub fn repo(&self, id: &str) -> Result<Repository> {
        valid_id(id)?;
        self.get(&key("r", id))
    }
    pub fn workspace(&self, id: &str) -> Result<Workspace> {
        valid_id(id)?;
        self.get(&key("w", id))
    }
    pub fn revision(&self, id: &str) -> Result<Revision> {
        valid_id(id)?;
        self.get(&key("v", id))
    }

    fn import_base(&self, repo: &Repository, commit: &str) -> Result<()> {
        let prefix = format!("t/{}/{commit}/", repo.id);
        if self.get::<bool>(&format!("m/{}/{commit}", repo.id)).is_ok() {
            return Ok(());
        }
        let tree = git::tree(&self.db, repo, commit)?;
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(RECORDS)?;
            for (p, e) in tree {
                table.insert(
                    format!("{prefix}{p}").as_str(),
                    serde_json::to_vec(&e)?.as_slice(),
                )?;
            }
            table.insert(
                format!("m/{}/{commit}", repo.id).as_str(),
                b"true".as_slice(),
            )?;
        }
        txn.commit()?;
        Ok(())
    }

    pub fn snapshot(&self, ws: &Workspace) -> Result<(Revision, Tree)> {
        let head = self.revision(&ws.head)?;
        ensure!(
            head.repo == ws.repo,
            "revision belongs to another repository"
        );
        let prefix = format!("t/{}/{}/", ws.repo, head.base);
        let cached = self.base_cache.lock().unwrap().get(&prefix).cloned();
        let mut tree = if let Some(tree) = cached {
            tree
        } else {
            let mut tree = Tree::new();
            for (k, v) in self.scan(&prefix)? {
                tree.insert(k[prefix.len()..].into(), serde_json::from_slice(&v)?);
            }
            let mut cache = self.base_cache.lock().unwrap();
            if cache.len() >= self.config.max_cached_bases {
                cache.clear();
            }
            cache.insert(prefix.clone(), tree.clone());
            tree
        };
        let mut rev = head.clone();
        let mut chain = Vec::new();
        loop {
            ensure!(
                rev.repo == ws.repo && rev.base == head.base,
                "invalid revision ancestry"
            );
            chain.push(rev.changes);
            if let Some(parent) = rev.parent {
                rev = self.revision(&parent)?;
            } else {
                break;
            }
            ensure!(
                chain.len() < 100_000,
                "revision chain too deep; create a compacted workspace"
            );
        }
        for changes in chain.into_iter().rev() {
            for (p, e) in changes {
                if let Some(e) = e {
                    tree.insert(p, e);
                } else {
                    tree.remove(&p);
                }
            }
        }
        Ok((head, tree))
    }

    pub(crate) fn bytes(
        &self,
        _repo: &Repository,
        e: &Entry,
        reader: &mut Option<git::Reader>,
    ) -> Result<Vec<u8>> {
        ensure!(
            e.mode != "160000",
            "submodule contents are not in this repository; register the submodule separately"
        );
        ensure!(
            e.size <= self.config.max_file_bytes as u64,
            "file exceeds configured max_file_bytes"
        );
        if e.source == "git" {
            if reader.is_none() {
                *reader = Some(git::Reader::new(&self.db)?);
            }
            return reader
                .as_mut()
                .unwrap()
                .blob(&e.oid, self.config.max_file_bytes);
        }
        ensure!(e.source == "blob", "invalid content source");
        let txn = self.db.begin_read()?;
        let table = txn.open_table(RECORDS)?;
        let stored = table
            .get(key("b", &e.oid).as_str())?
            .context("missing blob")?;
        let decoder = zstd::stream::read::Decoder::new(stored.value())?;
        let mut bytes = Vec::new();
        decoder
            .take(self.config.max_file_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() == e.size as usize && bytes.len() <= self.config.max_file_bytes,
            "blob size mismatch"
        );
        ensure!(
            blake3::hash(&bytes).to_hex().as_str() == e.oid,
            "blob integrity check failed"
        );
        Ok(bytes)
    }

    fn publish(
        &self,
        ws: &Workspace,
        expected: &str,
        changes: BTreeMap<String, Option<Entry>>,
        blobs: BTreeMap<String, Vec<u8>>,
        message: &str,
    ) -> Result<Workspace> {
        self.publish_revision(ws, expected, changes, blobs, message, false)
    }

    fn publish_revision(
        &self,
        ws: &Workspace,
        expected: &str,
        changes: BTreeMap<String, Option<Entry>>,
        blobs: BTreeMap<String, Vec<u8>>,
        message: &str,
        squash: bool,
    ) -> Result<Workspace> {
        let parent = self.revision(&ws.head)?;
        let next = Revision {
            id: id(),
            repo: ws.repo.clone(),
            parent: if squash { None } else { Some(ws.head.clone()) },
            base: parent.base,
            changes,
            message: message.into(),
            timestamp: now(),
        };
        let txn = self.db.begin_write()?;
        let updated;
        {
            let mut table = txn.open_table(RECORDS)?;
            let current: Workspace = serde_json::from_slice(
                table
                    .get(key("w", &ws.id).as_str())?
                    .context("workspace was deleted")?
                    .value(),
            )?;
            ensure!(
                current.head == expected && ws.head == expected,
                "revision conflict: expected {expected}, current {}",
                current.head
            );
            for (hash, bytes) in blobs {
                table.insert(key("b", &hash).as_str(), bytes.as_slice())?;
            }
            table.insert(
                key("v", &next.id).as_str(),
                serde_json::to_vec(&next)?.as_slice(),
            )?;
            updated = Workspace {
                head: next.id.clone(),
                ..current
            };
            table.insert(
                key("w", &updated.id).as_str(),
                serde_json::to_vec(&updated)?.as_slice(),
            )?;
        }
        txn.commit()?;
        Ok(updated)
    }

    pub(crate) fn encoded(&self, content: Vec<u8>, mode: &str) -> Result<(Entry, Vec<u8>)> {
        ensure!(
            content.len() <= self.config.max_file_bytes,
            "file exceeds max_file_bytes"
        );
        ensure!(
            ["100644", "100755", "120000"].contains(&mode),
            "invalid file mode"
        );
        let hash = blake3::hash(&content).to_hex().to_string();
        let compressed =
            zstd::stream::encode_all(content.as_slice(), self.config.compression_level)?;
        Ok((
            Entry {
                source: "blob".into(),
                oid: hash,
                mode: mode.into(),
                size: content.len() as u64,
            },
            compressed,
        ))
    }
    fn content_arg(a: &Value) -> Result<Vec<u8>> {
        match (
            a.get("content").and_then(Value::as_str),
            a.get("base64").and_then(Value::as_str),
        ) {
            (Some(s), None) => Ok(s.as_bytes().to_vec()),
            (None, Some(s)) => Ok(base64::engine::general_purpose::STANDARD.decode(s)?),
            _ => bail!("provide exactly one of content or base64"),
        }
    }

    pub fn call(&self, op: &str, a: &Value) -> Result<Value> {
        if op == "gc" {
            let _guard = self
                .lifecycle
                .write()
                .map_err(|_| anyhow::anyhow!("lifecycle lock poisoned"))?;
            return self.gc();
        }
        let _guard = self
            .lifecycle
            .read()
            .map_err(|_| anyhow::anyhow!("lifecycle lock poisoned"))?;
        self.call_inner(op, a)
    }

    fn repository_id_available(&self, repo_id: &str) -> Result<()> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(RECORDS)?;
        ensure!(
            table.get(key("r", repo_id).as_str())?.is_none(),
            "repository id already registered"
        );
        Ok(())
    }

    fn publish_repository(&self, repo: &Repository) -> Result<()> {
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(RECORDS)?;
            let key = key("r", &repo.id);
            ensure!(
                table.get(key.as_str())?.is_none(),
                "repository id already registered"
            );
            table.insert(key.as_str(), serde_json::to_vec(repo)?.as_slice())?;
        }
        txn.commit()?;
        Ok(())
    }

    fn migrate_repositories(&self) -> Result<()> {
        for (key, bytes) in self.scan("r/")? {
            let mut repo: Repository = serde_json::from_slice(&bytes)?;
            if !repo.stored {
                let source = git2::Repository::open(&repo.git_dir).with_context(|| {
                    format!(
                        "migration needs original repository at {}; restore it and retry",
                        repo.git_dir.display()
                    )
                })?;
                git::import(
                    &self.db,
                    &source,
                    &mut repo,
                    self.config.compression_level,
                    self.config.task_timeout_seconds,
                )?;
                self.put(&key, &repo)?;
            }
        }
        let tx = self.db.begin_write()?;
        tx.open_table(RECORDS)?.insert("schema", b"2".as_slice())?;
        tx.commit()?;
        Ok(())
    }

    fn inspect_repository(&self, repo_id: &str, path: &Path) -> Result<Repository> {
        ensure!(
            path.is_absolute(),
            "registration requires an absolute repository path"
        );
        let source = git2::Repository::open(path)?;
        let root = source.workdir().unwrap_or(source.path()).canonicalize()?;
        ensure!(
            !self.home.starts_with(&root) && !root.starts_with(&self.home),
            "vwork home and registered repository must not overlap"
        );
        let mut repo = Repository {
            id: repo_id.into(),
            path: root,
            git_dir: source.path().canonicalize()?,
            stored: false,
            refs: BTreeMap::new(),
            author_name: String::new(),
            author_email: String::new(),
        };
        git::import(
            &self.db,
            &source,
            &mut repo,
            self.config.compression_level,
            self.config.task_timeout_seconds,
        )?;
        Ok(repo)
    }

    pub(crate) fn cleanup_transfers(&self) -> Result<()> {
        let Ok(_transfer) = self.transfer_lock.try_lock() else {
            return Ok(());
        };
        for (key, _) in self.scan("transfer/")? {
            if let Err(error) = self.remove_transfer(&key[9..]) {
                let mut record: Value = self.get(&key)?;
                record["cleanup_error"] = json!(format!("{error:#}"));
                self.put(&key, &record)?;
            }
        }
        Ok(())
    }
    fn remove_transfer(&self, transfer: &str) -> Result<()> {
        valid_id(transfer)?;
        let path = self.home.join("transfers").join(transfer);
        safe_ancestors(&path)?;
        if path.exists() {
            let marker = path.join("owner");
            if marker.exists() {
                ensure!(
                    std::fs::read_to_string(&marker)? == transfer,
                    "transfer ownership mismatch"
                );
            } else {
                ensure!(
                    std::fs::read_dir(&path)?.next().is_none(),
                    "unmarked transfer contains files"
                );
            }
            std::fs::remove_dir_all(&path)?;
        }
        let tx = self.db.begin_write()?;
        tx.open_table(RECORDS)?
            .remove(format!("transfer/{transfer}").as_str())?;
        tx.commit()?;
        Ok(())
    }
    fn clone_repository(&self, repo_id: &str, source: &str) -> Result<Value> {
        let source = git::clone_source(source)?;
        if Path::new(&source).is_absolute() {
            let mut repo = self.inspect_repository(repo_id, Path::new(&source))?;
            repo.path = PathBuf::new();
            repo.git_dir = PathBuf::new();
            self.publish_repository(&repo)?;
            return Ok(
                json!({"repo":repo.id,"stored":true,"storage":"redb","default_reference":"HEAD"}),
            );
        }
        let _transfer = self
            .transfer_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("transfer lock poisoned"))?;
        ensure!(
            self.scan("transfer/")?.len() < self.config.max_temporary_copies,
            "pending transfer cleanup limit reached; inspect temporary_list"
        );
        let transfer = id();
        let path = self.home.join("transfers").join(&transfer);
        safe_ancestors(&path)?;
        self.put(
            &format!("transfer/{transfer}"),
            &json!({"repo":repo_id,"created":now()}),
        )?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::create_dir(&path)?;
        std::fs::write(path.join("owner"), &transfer)?;
        let result = (|| -> Result<Value> {
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_secs(self.config.task_timeout_seconds);
            let mut callbacks = git2::RemoteCallbacks::new();
            callbacks.transfer_progress(|p| {
                std::time::Instant::now() < deadline && p.received_bytes() <= 8 * 1024 * 1024 * 1024
            });
            let mut credential_attempts = 0;
            callbacks.credentials(|url, user, allowed| {
                credential_attempts += 1;
                if credential_attempts > 3 {
                    return Err(git2::Error::from_str(
                        "noninteractive authentication failed",
                    ));
                }
                if allowed.contains(git2::CredentialType::USERNAME) {
                    git2::Cred::username(user.unwrap_or("git"))
                } else if allowed.contains(git2::CredentialType::SSH_KEY) {
                    git2::Cred::ssh_key_from_agent(user.unwrap_or("git"))
                } else if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
                    git_credentials(url, user).map_err(|_| {
                        git2::Error::from_str("noninteractive Git credential helper failed")
                    })
                } else if allowed.contains(git2::CredentialType::DEFAULT) {
                    git2::Cred::default()
                } else {
                    Err(git2::Error::from_str(
                        "unsupported noninteractive authentication",
                    ))
                }
            });
            let mut fetch = git2::FetchOptions::new();
            fetch.remote_callbacks(callbacks);
            let source = git2::build::RepoBuilder::new()
                .bare(true)
                .fetch_options(fetch)
                .clone(&source, &path.join("pack"))?;
            let mut repo = Repository {
                id: repo_id.into(),
                path: PathBuf::new(),
                git_dir: PathBuf::new(),
                stored: false,
                refs: BTreeMap::new(),
                author_name: String::new(),
                author_email: String::new(),
            };
            git::import(
                &self.db,
                &source,
                &mut repo,
                self.config.compression_level,
                self.config.task_timeout_seconds,
            )?;
            self.publish_repository(&repo)?;
            Ok(json!({"repo":repo.id,"stored":true,"storage":"redb","default_reference":"HEAD"}))
        })();
        let cleanup = self.remove_transfer(&transfer);
        match (result, cleanup) {
            (Ok(mut result), Err(error)) => {
                result["cleanup_pending"] = json!(error.to_string());
                Ok(result)
            }
            (Err(error), _) => Err(error),
            (Ok(result), Ok(())) => Ok(result),
        }
    }

    fn call_inner(&self, op: &str, a: &Value) -> Result<Value> {
        if [
            "workspace_temporary",
            "temporary_list",
            "temporary_renew",
            "temporary_release",
        ]
        .contains(&op)
        {
            return self.temporary_call(op, a);
        }
        match op {
            "health" => {
                return Ok(
                    json!({"version":env!("CARGO_PKG_VERSION"),"schema":2,"compression_level":self.config.compression_level}),
                );
            }
            "repo_register" | "repo_clone" => {
                let repo_id = string(a, "repo")?;
                valid_id(repo_id)?;
                let lock = self.build_lock(repo_id);
                let _setup = lock
                    .lock()
                    .map_err(|_| anyhow::anyhow!("repository setup lock poisoned"))?;
                self.repository_id_available(repo_id)?;
                if op == "repo_clone" {
                    return self.clone_repository(repo_id, string(a, "source")?);
                }
                let repo = self.inspect_repository(repo_id, Path::new(string(a, "path")?))?;
                self.publish_repository(&repo)?;
                return Ok(serde_json::to_value(repo)?);
            }
            "repo_list" => {
                return Ok(json!(
                    self.scan("r/")?
                        .into_iter()
                        .map(|(_, v)| serde_json::from_slice::<Value>(&v))
                        .collect::<std::result::Result<Vec<_>, _>>()?
                ));
            }
            "workspace_list" => {
                return Ok(json!(
                    self.scan("w/")?
                        .into_iter()
                        .map(|(_, v)| serde_json::from_slice::<Value>(&v))
                        .collect::<std::result::Result<Vec<_>, _>>()?
                ));
            }
            "workspace_create" => {
                let repo = self.repo(string(a, "repo")?)?;
                let ws_id = string(a, "workspace")?;
                valid_id(ws_id)?;
                let base = git::resolve(&self.db, &repo, optional(a, "reference", "HEAD"))?;
                self.import_base(&repo, &base)?;
                let rev = Revision {
                    id: id(),
                    repo: repo.id.clone(),
                    parent: None,
                    base,
                    changes: BTreeMap::new(),
                    message: "create workspace".into(),
                    timestamp: now(),
                };
                let ws = Workspace {
                    id: ws_id.into(),
                    repo: repo.id,
                    head: rev.id.clone(),
                };
                let txn = self.db.begin_write()?;
                {
                    let mut t = txn.open_table(RECORDS)?;
                    ensure!(
                        t.get(key("w", ws_id).as_str())?.is_none(),
                        "workspace already exists"
                    );
                    t.insert(
                        key("v", &rev.id).as_str(),
                        serde_json::to_vec(&rev)?.as_slice(),
                    )?;
                    t.insert(
                        key("w", ws_id).as_str(),
                        serde_json::to_vec(&ws)?.as_slice(),
                    )?;
                }
                txn.commit()?;
                return Ok(serde_json::to_value(ws)?);
            }
            "workspace_fork" => {
                let from = self.workspace(string(a, "workspace")?)?;
                let name = string(a, "name")?;
                valid_id(name)?;
                let new = Workspace {
                    id: name.into(),
                    ..from
                };
                let txn = self.db.begin_write()?;
                {
                    let mut t = txn.open_table(RECORDS)?;
                    ensure!(
                        t.get(key("w", name).as_str())?.is_none(),
                        "workspace already exists"
                    );
                    t.insert(
                        key("w", name).as_str(),
                        serde_json::to_vec(&new)?.as_slice(),
                    )?;
                }
                txn.commit()?;
                return Ok(serde_json::to_value(new)?);
            }
            "export_forget" => {
                let repo = string(a, "repo")?;
                self.repo(repo)?;
                let revision = string(a, "revision")?;
                valid_id(revision)?;
                let record = format!("export/{repo}/{revision}");
                let pin: Value = self.get(&record)?;
                let txn = self.db.begin_write()?;
                {
                    txn.open_table(RECORDS)?.remove(record.as_str())?;
                }
                txn.commit()?;
                return Ok(
                    json!({"forgotten_revision":revision,"git_commit_retained":pin["commit"],"note":"only the database history pin was removed; Git commit/ref remain"}),
                );
            }
            "package_configure" => {
                let project: Project = serde_json::from_value(a.clone())?;
                project.validate()?;
                self.repo(&project.repo)?;
                self.put(
                    &format!("project/{}/{}", project.repo, project.name),
                    &project,
                )?;
                return Ok(json!({"project":project.name,"actions":project.actions()}));
            }
            "package_list" => {
                let repo = string(a, "repo")?;
                self.repo(repo)?;
                let mut projects = Vec::new();
                for (_, value) in self.scan(&format!("project/{repo}/"))? {
                    let project: Project = serde_json::from_slice(&value)?;
                    projects.push(json!({"project":project,"actions":project.actions()}));
                }
                return Ok(json!({"projects":projects}));
            }
            "task_set" => {
                let task: Task = serde_json::from_value(a.clone())?;
                self.repo(&task.repo)?;
                valid_id(&task.name)?;
                if !task.cwd.is_empty() {
                    path(&task.cwd)?;
                }
                ensure!(!task.program.is_empty(), "empty task executable");
                self.put(&format!("task/{}/{}", task.repo, task.name), &task)?;
                return Ok(json!({"saved":true}));
            }
            "task_list" => {
                let repo = string(a, "repo")?;
                self.repo(repo)?;
                return Ok(json!(
                    self.scan(&format!("task/{repo}/"))?
                        .into_iter()
                        .map(|(_, v)| serde_json::from_slice::<Value>(&v))
                        .collect::<std::result::Result<Vec<_>, _>>()?
                ));
            }
            "gc" => return self.gc(),
            "stats" | "storage_info" => return self.stats(),
            "repo_log" => {
                let repo = self.repo(string(a, "repo")?)?;
                let reference = git::resolve(&self.db, &repo, optional(a, "reference", "HEAD"))?;
                let out = git::log(
                    &self.db,
                    &repo,
                    &reference,
                    number(a, "limit", 20).min(1000),
                )?;
                return Ok(json!({"log":out}));
            }
            _ => {}
        }
        let ws = self.workspace(string(a, "workspace")?)?;
        // Dropping a reference must not load a potentially enormous source manifest.
        if op == "workspace_drop" {
            let expected = string(a, "expected_revision")?;
            let txn = self.db.begin_write()?;
            {
                let mut t = txn.open_table(RECORDS)?;
                let current: Workspace = serde_json::from_slice(
                    t.get(key("w", &ws.id).as_str())?
                        .context("not found")?
                        .value(),
                )?;
                ensure!(current.head == expected, "revision conflict");
                t.remove(key("w", &ws.id).as_str())?;
            }
            txn.commit()?;
            return Ok(
                json!({"dropped":ws.id,"space_reclamation":"run gc; compact database while daemon stopped"}),
            );
        }
        let repo = self.repo(&ws.repo)?;
        let (rev, tree) = self.snapshot(&ws)?;
        let mut reader = None;
        let limit = number(a, "limit", 200).min(self.config.max_results);
        let offset = number(a, "offset", 0);
        match op {
            "workspace_size" => {
                let mut sizes: BTreeMap<String, (u64, usize)> = BTreeMap::new();
                for (p, e) in &tree {
                    let mut dir = p.as_str();
                    loop {
                        dir = dir.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
                        let total = sizes.entry(dir.into()).or_default();
                        total.0 += e.size;
                        total.1 += 1;
                        if dir.is_empty() {
                            break;
                        }
                    }
                }
                let mut directories: Vec<_> = sizes.into_iter().collect();
                directories.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
                let total = directories.len();
                let items: Vec<_> = directories
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .map(
                        |(p, (bytes, files))| json!({"path":p,"logical_bytes":bytes,"files":files}),
                    )
                    .collect();
                Ok(
                    json!({"revision":ws.head,"directories":items,"total":total,"note":"logical snapshot sizes; shared Git/blob bytes are not per-workspace physical allocation"}),
                )
            }
            "workspace_info" => {
                Ok(json!({"workspace":ws,"base":rev.base,"files":tree.len(),"revision":ws.head}))
            }
            "files_list" | "files_glob" | "files_stat" => {
                let pattern = optional(a, "glob", "**");
                let glob = globset::Glob::new(pattern)?.compile_matcher();
                let prefix = optional(a, "path", "");
                if !prefix.is_empty() {
                    path(prefix)?;
                }
                let all: Vec<_> = tree
                    .iter()
                    .filter(|(p, _)| {
                        glob.is_match(p)
                            && (prefix.is_empty()
                                || p.as_str() == prefix
                                || p.starts_with(&format!("{prefix}/")))
                    })
                    .collect();
                let entries:Vec<_>=all.iter().skip(offset).take(limit).map(|(p,e)|json!({"path":p,"size":e.size,"mode":e.mode,"source":e.source,"object":e.oid})).collect();
                Ok(
                    json!({"revision":ws.head,"total":all.len(),"entries":entries,"next_offset":(offset+entries.len()<all.len()).then_some(offset+entries.len())}),
                )
            }
            "files_read" => {
                let p = path(string(a, "path")?)?;
                let e = tree.get(&p).context("file not found")?;
                let bytes = self.bytes(&repo, e, &mut reader)?;
                let hash = blake3::hash(&bytes).to_hex().to_string();
                if bool_arg(a, "binary") {
                    return Ok(
                        json!({"revision":ws.head,"path":p,"hash":hash,"base64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
                    );
                }
                let content =
                    std::str::from_utf8(&bytes).context("file is binary; request binary=true")?;
                let start = number(a, "start_line", 1).max(1);
                let count = number(a, "line_count", 200).min(10000);
                let lines: Vec<_> = content.lines().collect();
                let text = lines
                    .iter()
                    .skip(start - 1)
                    .take(count)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");
                let (text, truncated) = truncate_utf8(text, self.config.max_output_bytes);
                Ok(
                    json!({"revision":ws.head,"path":p,"hash":hash,"mode":e.mode,"total_lines":lines.len(),"start_line":start,"content":text,"trailing_newline":content.ends_with('\n'),"truncated":truncated}),
                )
            }
            "files_search" => {
                let pattern = string(a, "pattern")?;
                let regex = regex::RegexBuilder::new(pattern)
                    .case_insensitive(bool_arg(a, "ignore_case"))
                    .size_limit(8 * 1024 * 1024)
                    .build()?;
                let glob = globset::Glob::new(optional(a, "glob", "**"))?.compile_matcher();
                let context = number(a, "context", 2).min(20);
                let mut matches = Vec::new();
                let mut seen = 0;
                let mut skipped = 0;
                let mut truncated = false;
                let mut response_bytes = 0;
                'files: for (p, e) in &tree {
                    if !glob.is_match(p) {
                        continue;
                    }
                    if e.mode == "160000" || e.size > self.config.max_file_bytes as u64 {
                        skipped += 1;
                        continue;
                    }
                    let bytes = self.bytes(&repo, e, &mut reader)?;
                    if bytes.contains(&0) {
                        skipped += 1;
                        continue;
                    }
                    let Ok(text) = std::str::from_utf8(&bytes) else {
                        skipped += 1;
                        continue;
                    };
                    let lines: Vec<_> = text.lines().collect();
                    for (i, line) in lines.iter().enumerate() {
                        if regex.is_match(line) {
                            if seen < offset {
                                seen += 1;
                                continue;
                            }
                            if matches.len() >= limit {
                                truncated = true;
                                break 'files;
                            }
                            let (text, line_truncated) = truncate_utf8((*line).to_string(), 8192);
                            let surrounding: Vec<_> = lines
                                [i.saturating_sub(context)..(i + context + 1).min(lines.len())]
                                .iter()
                                .map(|s| truncate_utf8((*s).to_string(), 8192).0)
                                .collect();
                            let item = json!({"path":p,"line":i+1,"text":text,"text_truncated":line_truncated,"context_start":i.saturating_sub(context)+1,"context":surrounding});
                            let size = item.to_string().len();
                            if !matches.is_empty()
                                && response_bytes + size > self.config.max_output_bytes
                            {
                                truncated = true;
                                break 'files;
                            }
                            response_bytes += size;
                            matches.push(item);
                            seen += 1;
                        }
                    }
                }
                Ok(
                    json!({"revision":ws.head,"matches":matches,"skipped_binary_or_large":skipped,"truncated":truncated,"next_offset":truncated.then_some(offset+matches.len())}),
                )
            }
            "files_write" | "files_replace" | "files_apply_patch" | "files_delete"
            | "files_copy" | "files_rename" => {
                let expected = string(a, "expected_revision")?;
                let mut changes = BTreeMap::new();
                let mut blobs = BTreeMap::new();
                match op {
                    "files_write" => {
                        let p = path(string(a, "path")?)?;
                        let mode = optional(
                            a,
                            "mode",
                            tree.get(&p).map(|e| e.mode.as_str()).unwrap_or("100644"),
                        );
                        let (e, b) = self.encoded(Self::content_arg(a)?, mode)?;
                        blobs.insert(e.oid.clone(), b);
                        changes.insert(p, Some(e));
                    }
                    "files_replace" => {
                        let p = path(string(a, "path")?)?;
                        let e = tree.get(&p).context("file not found")?;
                        let bytes = self.bytes(&repo, e, &mut reader)?;
                        let s = std::str::from_utf8(&bytes)?;
                        let before = string(a, "before")?;
                        ensure!(!before.is_empty(), "before must not be empty");
                        let count = s.matches(before).count();
                        let wanted = number(a, "occurrences", 1);
                        ensure!(
                            count == wanted && count > 0,
                            "replacement expected {wanted} matches, found {count}"
                        );
                        let output = s.replace(before, string(a, "after")?);
                        let (e, b) = self.encoded(output.into_bytes(), &e.mode)?;
                        blobs.insert(e.oid.clone(), b);
                        changes.insert(p, Some(e));
                    }
                    "files_apply_patch" => {
                        for edit in a
                            .get("edits")
                            .and_then(Value::as_array)
                            .context("edits must be an array")?
                        {
                            let p = path(string(edit, "path")?)?;
                            ensure!(!changes.contains_key(&p), "duplicate patch path");
                            if bool_arg(edit, "delete") {
                                ensure!(tree.contains_key(&p), "file not found: {p}");
                                changes.insert(p, None);
                            } else {
                                let mode = optional(
                                    edit,
                                    "mode",
                                    tree.get(&p).map(|e| e.mode.as_str()).unwrap_or("100644"),
                                );
                                let (e, b) = self.encoded(Self::content_arg(edit)?, mode)?;
                                blobs.insert(e.oid.clone(), b);
                                changes.insert(p, Some(e));
                            }
                        }
                    }
                    "files_delete" => {
                        let p = path(string(a, "path")?)?;
                        for name in tree.keys().filter(|name| {
                            name.as_str() == p
                                || (bool_arg(a, "recursive") && name.starts_with(&format!("{p}/")))
                        }) {
                            changes.insert(name.clone(), None);
                        }
                        ensure!(!changes.is_empty(), "path not found");
                    }
                    _ => {
                        let p = path(string(a, "path")?)?;
                        let dest = path(string(a, "destination")?)?;
                        let e = tree.get(&p).context("source file not found")?;
                        ensure!(!tree.contains_key(&dest), "destination already exists");
                        changes.insert(dest, Some(e.clone()));
                        if op == "files_rename" {
                            changes.insert(p, None);
                        }
                    }
                }
                let mut resulting = tree.clone();
                for (p, e) in &changes {
                    if let Some(e) = e {
                        resulting.insert(p.clone(), e.clone());
                    } else {
                        resulting.remove(p);
                    }
                }
                Self::validate_tree(&resulting, false)?;
                let updated =
                    self.publish(&ws, expected, changes, blobs, optional(a, "message", op))?;
                Ok(json!({"workspace":updated.id,"revision":updated.head}))
            }
            "workspace_status" | "workspace_diff" => {
                let against = if let Some(r) = a.get("against_revision").and_then(Value::as_str) {
                    let r = self.revision(r)?;
                    ensure!(r.repo == ws.repo, "other repository");
                    self.snapshot(&Workspace {
                        head: r.id,
                        ..ws.clone()
                    })?
                    .1
                } else {
                    let mut t = Tree::new();
                    let prefix = format!("t/{}/{}/", ws.repo, rev.base);
                    for (p, v) in self.scan(&prefix)? {
                        t.insert(p[prefix.len()..].into(), serde_json::from_slice(&v)?);
                    }
                    t
                };
                let paths: BTreeSet<_> = tree.keys().chain(against.keys()).cloned().collect();
                let mut changed = Vec::new();
                let mut diff = String::new();
                for p in paths {
                    let left = against.get(&p);
                    let right = tree.get(&p);
                    if self.equal_entries(&repo, left, right, &mut reader)? {
                        continue;
                    }
                    changed.push(json!({"path":p,"status":if left.is_none(){"A"}else if right.is_none(){"D"}else{"M"}}));
                    if op == "workspace_diff" && diff.len() < self.config.max_output_bytes {
                        let b = left
                            .map(|e| self.bytes(&repo, e, &mut reader))
                            .transpose()?
                            .unwrap_or_default();
                        let c = right
                            .map(|e| self.bytes(&repo, e, &mut reader))
                            .transpose()?
                            .unwrap_or_default();
                        match (std::str::from_utf8(&b), std::str::from_utf8(&c)) {
                            (Ok(b), Ok(c)) if !b.contains('\0') && !c.contains('\0') => diff
                                .push_str(
                                    &similar::TextDiff::from_lines(b, c)
                                        .unified_diff()
                                        .context_radius(3)
                                        .header(&format!("a/{p}"), &format!("b/{p}"))
                                        .to_string(),
                                ),
                            _ => diff.push_str(&format!("Binary files differ: {p}\n")),
                        }
                    }
                }
                let (diff, truncated) = truncate_utf8(diff, self.config.max_output_bytes);
                Ok(
                    json!({"revision":ws.head,"changes":changed.into_iter().skip(offset).take(limit).collect::<Vec<_>>(),"diff":diff,"truncated":truncated}),
                )
            }
            "workspace_history" => {
                let mut current = Some(ws.head.clone());
                let mut out = Vec::new();
                let mut skip = offset;
                while let Some(r) = current {
                    let rev = self.revision(&r)?;
                    current = rev.parent.clone();
                    if skip > 0 {
                        skip -= 1;
                        continue;
                    }
                    if out.len() >= limit {
                        break;
                    }
                    out.push(json!({"revision":rev.id,"parent":rev.parent,"message":rev.message,"timestamp":rev.timestamp,"changed_paths":rev.changes.keys().collect::<Vec<_>>()}));
                }
                Ok(json!({"history":out,"revision":ws.head}))
            }
            "workspace_squash" => {
                let prefix = format!("t/{}/{}/", ws.repo, rev.base);
                let mut base = Tree::new();
                for (p, v) in self.scan(&prefix)? {
                    base.insert(p[prefix.len()..].into(), serde_json::from_slice(&v)?);
                }
                let changes = Self::tree_changes(&base, &tree);
                let updated = self.publish_revision(
                    &ws,
                    string(a, "expected_revision")?,
                    changes,
                    BTreeMap::new(),
                    "squash virtual history",
                    true,
                )?;
                Ok(
                    json!({"revision":updated.head,"note":"history detached for this workspace only; gc reclaims revisions not held by forks or exported commits"}),
                )
            }
            "workspace_restore" => {
                let target = self.revision(string(a, "revision")?)?;
                ensure!(target.repo == ws.repo, "other repository");
                let (_, other) = self.snapshot(&Workspace {
                    head: target.id,
                    ..ws.clone()
                })?;
                let changes = Self::tree_changes(&tree, &other);
                let updated = self.publish(
                    &ws,
                    string(a, "expected_revision")?,
                    changes,
                    BTreeMap::new(),
                    "restore revision",
                )?;
                Ok(json!({"revision":updated.head}))
            }
            "workspace_merge" => self.merge(&ws, a, &tree),
            "workspace_commit" => self.commit(&ws, a, &tree, &rev),
            "workspace_export" => {
                let dest = resolved_physical_path(Path::new(string(a, "destination")?))?;
                ensure!(
                    !dest.starts_with(&self.home),
                    "explicit exports must be outside the vwork home"
                );
                let glob = globset::Glob::new(optional(a, "glob", "**"))?.compile_matcher();
                let selected: Tree = tree
                    .iter()
                    .filter(|(p, _)| glob.is_match(p))
                    .map(|(p, e)| (p.clone(), e.clone()))
                    .collect();
                let lock = self.build_lock(&ws.repo);
                let _guard = lock
                    .lock()
                    .map_err(|_| anyhow::anyhow!("build lock poisoned"))?;
                self.materialize(&ws, &selected, &dest)?;
                Ok(json!({"destination":dest,"revision":ws.head,"files":selected.len()}))
            }
            "workspace_import" => self.import(&ws, a, &tree),
            "workspace_run_task" => self.run_task(&ws, a, &tree),
            "package_run" => {
                let name = string(a, "project")?;
                valid_id(name)?;
                let project: Project = self.get(&format!("project/{}/{name}", ws.repo))?;
                self.run_build(
                    &ws,
                    a,
                    &tree,
                    BuildRequest::Package(project, string(a, "action")?.into()),
                )
            }
            _ => bail!("unknown operation: {op}"),
        }
    }

    fn tree_changes(from: &Tree, to: &Tree) -> BTreeMap<String, Option<Entry>> {
        from.keys()
            .chain(to.keys())
            .filter(|p| from.get(*p) != to.get(*p))
            .map(|p| (p.clone(), to.get(p).cloned()))
            .collect()
    }
    fn equal_entries(
        &self,
        repo: &Repository,
        a: Option<&Entry>,
        b: Option<&Entry>,
        reader: &mut Option<git::Reader>,
    ) -> Result<bool> {
        if a == b {
            return Ok(true);
        }
        let (Some(a), Some(b)) = (a, b) else {
            return Ok(false);
        };
        if a.mode != b.mode || a.size != b.size {
            return Ok(false);
        }
        if a.mode == "160000" {
            return Ok(a.oid == b.oid);
        }
        Ok(self.bytes(repo, a, reader)? == self.bytes(repo, b, reader)?)
    }
    fn validate_tree(tree: &Tree, portable: bool) -> Result<()> {
        let mut cases = BTreeMap::new();
        for (p, e) in tree {
            path(p)?;
            if portable {
                portable_path(p)?;
                let mut prefix = String::new();
                for part in p.split('/') {
                    if !prefix.is_empty() {
                        prefix.push('/');
                    }
                    prefix.push_str(part);
                    if let Some(previous) = cases.insert(prefix.to_lowercase(), prefix.clone()) {
                        ensure!(
                            previous == prefix,
                            "case-insensitive path collision: {previous} and {prefix}"
                        );
                    }
                }
                ensure!(
                    ["100644", "100755"].contains(&e.mode.as_str()),
                    "cannot materialize symlink or submodule: {p}"
                );
            }
            let mut ancestor = p.as_str();
            while let Some((parent, _)) = ancestor.rsplit_once('/') {
                ensure!(
                    !tree.contains_key(parent),
                    "file/directory conflict: {parent}"
                );
                ancestor = parent;
            }
        }
        Ok(())
    }

    fn merge(&self, ws: &Workspace, a: &Value, ours: &Tree) -> Result<Value> {
        let source = self.workspace(string(a, "source")?)?;
        ensure!(
            source.repo == ws.repo,
            "cannot merge different repositories"
        );
        let mut ancestors = BTreeSet::new();
        let mut cursor = Some(ws.head.clone());
        while let Some(r) = cursor {
            ancestors.insert(r.clone());
            cursor = self.revision(&r)?.parent;
        }
        let mut cursor = Some(source.head.clone());
        let mut common = None;
        while let Some(r) = cursor {
            if ancestors.contains(&r) {
                common = Some(r);
                break;
            }
            cursor = self.revision(&r)?.parent;
        }
        let common =
            common.context("no shared virtual ancestor; fork workspaces before merging")?;
        let base = self
            .snapshot(&Workspace {
                head: common,
                ..ws.clone()
            })?
            .1;
        let theirs = self.snapshot(&source)?.1;
        let repo = self.repo(&ws.repo)?;
        let mut reader = None;
        let mut conflicts = Vec::new();
        let mut changes = BTreeMap::new();
        let paths: BTreeSet<_> = base
            .keys()
            .chain(ours.keys())
            .chain(theirs.keys())
            .cloned()
            .collect();
        for p in paths {
            if self.equal_entries(&repo, base.get(&p), theirs.get(&p), &mut reader)? {
                continue;
            }
            if self.equal_entries(&repo, ours.get(&p), theirs.get(&p), &mut reader)? {
                continue;
            }
            if self.equal_entries(&repo, base.get(&p), ours.get(&p), &mut reader)? {
                changes.insert(p.clone(), theirs.get(&p).cloned());
            } else {
                conflicts.push(p);
            }
        }
        if !conflicts.is_empty() {
            return Ok(json!({"merged":false,"conflicts":conflicts,"revision":ws.head}));
        }
        let mut result = ours.clone();
        for (p, e) in &changes {
            if let Some(e) = e {
                result.insert(p.clone(), e.clone());
            } else {
                result.remove(p);
            }
        }
        Self::validate_tree(&result, false)?;
        let updated = self.publish(
            ws,
            string(a, "expected_revision")?,
            changes,
            BTreeMap::new(),
            &format!("merge {} at {}", source.id, source.head),
        )?;
        Ok(json!({"merged":true,"revision":updated.head,"source_revision":source.head}))
    }

    fn commit(&self, ws: &Workspace, a: &Value, tree: &Tree, rev: &Revision) -> Result<Value> {
        ensure!(
            string(a, "expected_revision")? == ws.head,
            "revision conflict"
        );
        let mut repo = self.repo(&ws.repo)?;
        let repository = git::open(&self.db, &repo, self.config.compression_level)?;
        let mut converted = tree.clone();
        let mut reader = None;
        for (p, original) in tree {
            let mut entry = original.clone();
            if entry.source == "blob" {
                entry.oid = repository
                    .blob(&self.bytes(&repo, &entry, &mut reader)?)?
                    .to_string();
                entry.source = "git".into();
            }
            converted.insert(p.clone(), entry);
        }
        let root = git::write_tree(&repository, &converted)?;
        let message = string(a, "message")?;
        ensure!(!message.trim().is_empty(), "empty commit message");
        let signature = git2::Signature::now(&repo.author_name, &repo.author_email)?;
        let oid = repository
            .commit(
                None,
                &signature,
                &signature,
                message,
                &repository.find_tree(root)?,
                &[&repository.find_commit(git2::Oid::from_str(&rev.base)?)?],
            )?
            .to_string();
        let reference = format!("refs/vwork/commits/{}/{}", ws.id, ws.head);
        let transaction = self.db.begin_write()?;
        {
            let mut table = transaction.open_table(RECORDS)?;
            let key = key("r", &repo.id);
            repo = serde_json::from_slice(
                table
                    .get(key.as_str())?
                    .context("repository missing")?
                    .value(),
            )?;
            repo.refs.insert(reference.clone(), oid.clone());
            table.insert(key.as_str(), serde_json::to_vec(&repo)?.as_slice())?;
        }
        transaction.commit()?;
        self.put(
            &format!("export/{}/{}", ws.repo, ws.head),
            &json!({"commit":oid,"reference":reference,"revision":ws.head}),
        )?;
        Ok(
            json!({"commit":oid,"reference":reference,"revision":ws.head,"note":"commit stored in redb; original repository untouched; use a temporary copy for native Git access"}),
        )
    }

    pub(crate) fn build_lock(&self, repo: &str) -> Arc<Mutex<()>> {
        self.build_locks
            .lock()
            .unwrap()
            .entry(repo.into())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub fn materialize(&self, ws: &Workspace, tree: &Tree, dest: &Path) -> Result<()> {
        Self::validate_tree(tree, true)?;
        ensure!(dest.is_absolute(), "destination must be absolute");
        ensure!(
            !dest.components().any(|c| matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )),
            "destination cannot contain dot path components"
        );
        safe_ancestors(dest)?;
        let repo = self.repo(&ws.repo)?;
        let normalized = resolved_physical_path(dest)?;
        let dest = normalized.as_path();
        for (_, value) in self.scan("r/")? {
            let registered: Repository = serde_json::from_slice(&value)?;
            ensure!(
                !registered.path.is_absolute()
                    || (!dest.starts_with(&registered.path) && !registered.path.starts_with(dest)),
                "destination must not overlap any registered checkout"
            );
        }
        std::fs::create_dir_all(dest)?;
        let dest = dest.canonicalize()?;
        let marker = dest.join(".vwork-managed.json");
        safe_ancestors(&marker)?;
        let previous: Tree = if marker.exists() {
            let state: Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
            ensure!(
                state.get("home").and_then(Value::as_str) == self.home.to_str()
                    && state.get("repo").and_then(Value::as_str) == Some(&ws.repo),
                "export belongs to another vwork home or repository"
            );
            serde_json::from_value(state["entries"].clone())?
        } else {
            ensure!(
                std::fs::read_dir(&dest)?.next().is_none(),
                "destination must be empty or a vwork-managed export"
            );
            Tree::new()
        };
        Self::validate_tree(&previous, true)?;
        let mut empty_dirs = BTreeSet::new();
        for p in previous.keys().filter(|p| !tree.contains_key(*p)) {
            let target = dest.join(p);
            safe_ancestors(&target)?;
            if target.exists() {
                ensure!(!target.is_dir(), "tracked path became directory: {p}");
                std::fs::remove_file(&target)?;
            }
            let mut parent = target.parent();
            while let Some(dir) = parent {
                if dir == dest {
                    break;
                }
                empty_dirs.insert(dir.to_path_buf());
                parent = dir.parent();
            }
        }
        for dir in empty_dirs.iter().rev() {
            // Empty tracked directories can go; generated/untracked files must survive.
            if dir.is_dir() && std::fs::read_dir(dir)?.next().is_none() {
                std::fs::remove_dir(dir)?;
            }
        }
        let mut reader = None;
        for (p, e) in tree {
            let target = dest.join(p);
            safe_ancestors(&target)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = self.bytes(&repo, e, &mut reader)?;
            // Recheck physical contents even when manifest is unchanged: tasks may modify source.
            if std::fs::read(&target).ok().as_deref() != Some(bytes.as_slice()) {
                ensure!(
                    previous.contains_key(p) || !target.exists(),
                    "refusing to overwrite untracked physical path: {p}"
                );
                std::fs::write(&target, bytes)?;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &target,
                    std::fs::Permissions::from_mode(if e.mode == "100755" { 0o755 } else { 0o644 }),
                )?;
            }
        }
        std::fs::write(
            marker,
            serde_json::to_vec_pretty(
                &json!({"home":self.home,"repo":ws.repo,"workspace":ws.id,"revision":ws.head,"entries":tree}),
            )?,
        )?;
        Ok(())
    }

    fn import(&self, ws: &Workspace, a: &Value, tree: &Tree) -> Result<Value> {
        let source = PathBuf::from(string(a, "source")?);
        ensure!(source.is_absolute(), "source must be absolute");
        safe_ancestors(&source)?;
        let src = source.canonicalize()?;
        safe_ancestors(&src.join(".vwork-managed.json"))?;
        let marker: Value = serde_json::from_slice(
            &std::fs::read(src.join(".vwork-managed.json"))
                .context("source must be a vwork export")?,
        )?;
        ensure!(
            marker["workspace"] == ws.id
                && marker["revision"] == string(a, "expected_revision")?
                && marker["repo"] == ws.repo
                && marker["home"].as_str() == self.home.to_str(),
            "export/revision mismatch; export again or resolve edits explicitly"
        );
        let repo = self.repo(&ws.repo)?;
        let mut reader = None;
        let mut blobs = BTreeMap::new();
        let mut changes = BTreeMap::new();
        let paths = a["paths"].as_array().context(
            "explicit paths array required; unknown generated files are never auto-imported",
        )?;
        for p in paths {
            let p = path(p.as_str().context("path must be string")?)?;
            let target = src.join(&p);
            safe_ancestors(&target)?;
            if !target.exists() {
                ensure!(tree.contains_key(&p), "missing untracked file: {p}");
                changes.insert(p, None);
                continue;
            }
            ensure!(target.is_file(), "import path is not a regular file");
            let mut bytes = Vec::new();
            std::fs::File::open(&target)?
                .take(self.config.max_file_bytes as u64 + 1)
                .read_to_end(&mut bytes)?;
            if let Some(old) = tree.get(&p)
                && self.bytes(&repo, old, &mut reader)? == bytes
            {
                continue;
            }
            let (e, b) = self.encoded(
                bytes,
                tree.get(&p).map(|e| e.mode.as_str()).unwrap_or("100644"),
            )?;
            blobs.insert(e.oid.clone(), b);
            changes.insert(p, Some(e));
        }
        let mut result = tree.clone();
        for (p, e) in &changes {
            if let Some(e) = e {
                result.insert(p.clone(), e.clone());
            } else {
                result.remove(p);
            }
        }
        Self::validate_tree(&result, false)?;
        let updated = self.publish(
            ws,
            string(a, "expected_revision")?,
            changes,
            blobs,
            "import explicit physical edits",
        )?;
        Ok(json!({"revision":updated.head}))
    }

    fn run_task(&self, ws: &Workspace, a: &Value, tree: &Tree) -> Result<Value> {
        let name = string(a, "task")?;
        valid_id(name)?;
        let task: Task = self.get(&format!("task/{}/{name}", ws.repo))?;
        self.run_build(ws, a, tree, BuildRequest::Task(task))
    }

    fn run_build(
        &self,
        ws: &Workspace,
        a: &Value,
        tree: &Tree,
        request: BuildRequest,
    ) -> Result<Value> {
        ensure!(
            string(a, "expected_revision")? == ws.head,
            "revision conflict"
        );
        let lock = self.build_lock(&ws.repo);
        let _guard = lock
            .lock()
            .map_err(|_| anyhow::anyhow!("build lock poisoned"))?;
        let mut lease = if let Some(lease) = self
            .leases()?
            .into_iter()
            .find(|l| l.build && l.repo == ws.repo && l.state == "ready")
        {
            self.recover_lease(&lease)?;
            lease
        } else {
            self.create_lease(
                ws,
                "configured build/package action",
                true,
                self.config.temporary_ttl_seconds,
            )?
        };
        let view = self.prepare_lease(&mut lease, ws, tree)?;
        let lease_root = self.lease_root(&lease);
        let dest = lease_root.join("tree");
        let admin = lease_root.join("git");
        let revision = self.revision(&ws.head)?;
        let repo = self.repo(&ws.repo)?;
        let mut reader = None;
        let mut before = BTreeMap::new();
        let mut package = None;
        let (name, steps, cwd, mut env, timeout) = match request {
            BuildRequest::Task(task) => (
                task.name,
                vec![Step::Command {
                    program: task.program,
                    args: task.args,
                }],
                dest.join(task.cwd),
                task.env,
                task.timeout_seconds
                    .unwrap_or(self.config.task_timeout_seconds)
                    .min(self.config.task_timeout_seconds),
            ),
            BuildRequest::Package(project, action) => {
                for p in project.scoped_inputs(tree) {
                    let content = tree
                        .get(&p)
                        .map(|e| self.bytes(&repo, e, &mut reader))
                        .transpose()?;
                    before.insert(p, content);
                }
                let fingerprint = packages::fingerprint(&project, &before)?;
                let state_key = format!(
                    "package-state/{}/{}/{}",
                    lease.id, project.repo, project.name
                );
                let installed =
                    self.get::<String>(&state_key).ok().as_deref() == Some(&fingerprint);
                let plan = packages::plan(&project, &action, &lease_root, &dest, tree, installed)?;
                if plan.installs || plan.clears {
                    let txn = self.db.begin_write()?;
                    {
                        txn.open_table(RECORDS)?.remove(state_key.as_str())?;
                    }
                    txn.commit()?;
                }
                let name = format!("{}/{action}", project.name);
                package = Some((
                    project,
                    action,
                    plan.installs,
                    plan.clears,
                    plan.environment,
                    plan.artifacts,
                    state_key,
                ));
                (
                    name,
                    plan.steps,
                    plan.cwd,
                    plan.env,
                    self.config.task_timeout_seconds,
                )
            }
        };
        safe_ancestors(&cwd)?;
        ensure!(timeout > 0, "task timeout must be positive");
        env.insert("VWORK_WORKSPACE".into(), ws.id.clone());
        env.insert("VWORK_REVISION".into(), ws.head.clone());
        env.insert("VWORK_BASE_COMMIT".into(), revision.base.clone());
        env.insert("VWORK_BUILD_ROOT".into(), packages::process_path(&dest));
        env.insert("VWORK_GIT_DIR".into(), packages::process_path(&admin));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(timeout);
        let mut output = ProcessOutput::default();
        for step in steps {
            match step {
                Step::Remove(path) => {
                    safe_ancestors(&path)?;
                    ensure!(
                        path.starts_with(&self.home),
                        "cleanup must stay inside managed state"
                    );
                    if path.exists() {
                        std::fs::remove_dir_all(&path)?;
                    }
                }
                Step::Command { program, args } => {
                    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                    if remaining.is_zero() {
                        output.success = false;
                        output.timed_out = true;
                        output.exit_code = None;
                        break;
                    }
                    let next = execute_process(
                        &program,
                        &args,
                        &cwd,
                        &env,
                        remaining,
                        self.config.max_output_bytes,
                        None,
                    )?;
                    output.append(next, self.config.max_output_bytes);
                    if !output.success {
                        break;
                    }
                }
            }
        }
        let run_id = id();
        let mut result = json!({"run":run_id,"workspace":ws.id,"revision":ws.head,"base_commit":revision.base,"task":name,"success":output.success,"exit_code":output.exit_code,"timed_out":output.timed_out,"stdout":String::from_utf8_lossy(&output.stdout),"stderr":String::from_utf8_lossy(&output.stderr),"stdout_truncated":output.stdout_truncated,"stderr_truncated":output.stderr_truncated,"checkout":dest,"git_dir":admin,"timestamp":now()});
        lease.expires_at = now().saturating_add(self.config.temporary_ttl_seconds);
        self.lease_save(&lease)?;
        result["temporary"] = self.lease_value(&lease);
        let mut physical = BTreeMap::new();
        let mut input_changes = Vec::new();
        for (path, old) in &before {
            let file = dest.join(path);
            safe_ancestors(&file)?;
            let content = if file.exists() {
                ensure!(file.is_file(), "package input became a directory: {path}");
                let mut bytes = Vec::new();
                std::fs::File::open(file)?
                    .take(self.config.max_file_bytes as u64 + 1)
                    .read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= self.config.max_file_bytes,
                    "package input exceeds max_file_bytes: {path}"
                );
                Some(bytes)
            } else {
                None
            };
            if content != *old {
                input_changes.push(path.clone());
            }
            physical.insert(path.clone(), content);
        }
        let mut changed: BTreeSet<String> = git::changed(&view)?.into_iter().collect();
        changed.extend(input_changes.iter().cloned());
        result["source_changes_truncated"] = json!(changed.len() > self.config.max_results);
        result["source_changes"] = json!(
            changed
                .into_iter()
                .take(self.config.max_results)
                .collect::<Vec<_>>()
        );
        if let Some((project, action, installs, _clears, environment, artifacts, state_key)) =
            package
        {
            if installs && output.success {
                self.put(&state_key, &packages::fingerprint(&project, &physical)?)?;
            }
            result["package"] = json!({"project":project.name,"manager":project.manager,"action":action,"environment":environment,"artifacts":artifacts,"input_changes":input_changes,"import_instruction":"Use workspace_import with returned checkout, revision and explicit source_changes paths before another task if changes should be retained."});
        }
        let txn = self.db.begin_write()?;
        {
            let mut table = txn.open_table(RECORDS)?;
            table.insert(
                format!("run/{run_id}").as_str(),
                serde_json::to_vec(&result)?.as_slice(),
            )?;
            let mut runs = Vec::new();
            for item in table.range("run/"..)? {
                let (key, value) = item?;
                if !key.value().starts_with("run/") {
                    break;
                }
                let record: Value = serde_json::from_slice(value.value())?;
                runs.push((
                    record["timestamp"].as_u64().unwrap_or(0),
                    key.value().to_string(),
                ));
            }
            runs.sort();
            let remove = runs.len().saturating_sub(self.config.max_run_history);
            for (_, key) in runs.into_iter().take(remove) {
                table.remove(key.as_str())?;
            }
        }
        txn.commit()?;
        Ok(result)
    }

    fn stats(&self) -> Result<Value> {
        let (mut repositories, mut workspaces, mut revisions) = (0u64, 0u64, 0u64);
        let (mut blobs, mut blob_bytes) = (0u64, 0u64);
        let (mut manifests, mut entries, mut manifest_bytes) = (0u64, 0u64, 0u64);
        let (mut workspace_bytes, mut revision_bytes) = (0u64, 0u64);
        let txn = self.db.begin_read()?;
        let table = txn.open_table(RECORDS)?;
        // One read transaction gives consistent counts without cloning records
        // or reading/decompressing Git/blob content. Byte counts are logical
        // key/value bytes, not redb page allocation or per-workspace disk usage.
        for item in table.iter()? {
            let (key, value) = item?;
            let bytes = (key.value().len() + value.value().len()) as u64;
            match key.value().split('/').next().unwrap_or("") {
                "r" => repositories += 1,
                "w" => {
                    workspaces += 1;
                    workspace_bytes += bytes;
                }
                "v" => {
                    revisions += 1;
                    revision_bytes += bytes;
                }
                "m" => {
                    manifests += 1;
                    manifest_bytes += bytes;
                }
                "t" => {
                    entries += 1;
                    manifest_bytes += bytes;
                }
                "b" => {
                    blobs += 1;
                    blob_bytes += value.value().len() as u64;
                }
                _ => {}
            }
        }
        let git_objects = txn.open_table(git_store::OBJECTS)?;
        let mut git_bytes = 0u64;
        for item in git_objects.iter()? {
            git_bytes += item?.1.value().len() as u64;
        }
        Ok(json!({
            "git_objects":git_objects.len()?, "compressed_git_bytes":git_bytes,
            "scope":"daemon", "repositories":repositories, "workspaces":workspaces,
            "revisions":revisions, "blobs":blobs, "compressed_blob_bytes":blob_bytes,
            "base_manifests":manifests, "base_entries":entries, "base_manifest_bytes":manifest_bytes,
            "workspace_record_bytes":workspace_bytes, "revision_record_bytes":revision_bytes,
            "database_file_bytes":std::fs::metadata(self.home.join("workspaces.redb"))?.len(),
            "compression_level":self.config.compression_level,
            "sharing":{
                "base_manifest_key":"repository_id+commit",
                "git_base_content":"All Git objects and source contents are stored in redb, shared across registrations by Git object ID",
                "edited_blob_scope":"daemon", "edited_blob_key":"BLAKE3 of raw content",
                "remote_url_deduplication":false, "git_object_scope":"daemon", "git_object_key":"Git SHA-1 of typed raw object",
                "new_workspace":"One workspace record and one empty revision; reuses an imported base manifest for the same repository ID and commit",
                "fork":"One workspace record pointing to the existing immutable revision; copies no manifest or content"
            },
            "accounting_notes":[
                "Counts include retained and not-yet-collected records; inspection makes no changes",
                "Record byte counts are serialized keys/values, not physical allocation per workspace",
                "Database file size includes page overhead and reusable space; it is not the sum of live record sizes",
                "Temporary copies, exports and package caches are outside these database sizes; Git objects are included",
                "Different repository registrations have separate base manifests, even for the same path or remote URL"
            ]
        }))
    }
    fn gc(&self) -> Result<Value> {
        // Hold write transaction across mark+sweep so publication cannot race reachability.
        let txn = self.db.begin_write()?;
        let removed;
        {
            let mut t = txn.open_table(RECORDS)?;
            let mut live = BTreeSet::new();
            let mut stack = Vec::new();
            let mut blob_ids = BTreeSet::new();
            for item in t.range("w/"..)? {
                let (k, v) = item?;
                if !k.value().starts_with("w/") {
                    break;
                }
                let ws: Workspace = serde_json::from_slice(v.value())?;
                stack.push(ws.head);
            }
            // Exported commits pin their recorded virtual snapshots for reproducibility.
            for item in t.range("export/"..)? {
                let (k, v) = item?;
                if !k.value().starts_with("export/") {
                    break;
                }
                let v: Value = serde_json::from_slice(v.value())?;
                stack.push(string(&v, "revision")?.into());
            }
            for item in t.range("lease/"..)? {
                let (k, v) = item?;
                if !k.value().starts_with("lease/") {
                    break;
                }
                let lease: crate::leases::Lease = serde_json::from_slice(v.value())?;
                stack.push(lease.revision);
            }
            while let Some(r) = stack.pop() {
                if !live.insert(r.clone()) {
                    continue;
                }
                let rev: Revision = serde_json::from_slice(
                    t.get(key("v", &r).as_str())?
                        .context("missing live revision")?
                        .value(),
                )?;
                if let Some(p) = rev.parent {
                    stack.push(p);
                }
                for e in rev.changes.values().flatten() {
                    if e.source == "blob" {
                        blob_ids.insert(e.oid.clone());
                    }
                }
            }
            let mut remove = Vec::new();
            for item in t.range("v/"..)? {
                let (k, _) = item?;
                if !k.value().starts_with("v/") {
                    break;
                }
                if !live.contains(&k.value()[2..]) {
                    remove.push(k.value().to_string());
                }
            }
            for item in t.range("b/"..)? {
                let (k, _) = item?;
                if !k.value().starts_with("b/") {
                    break;
                }
                if !blob_ids.contains(&k.value()[2..]) {
                    remove.push(k.value().to_string());
                }
            }
            removed = remove.len();
            for k in remove {
                t.remove(k.as_str())?;
            }
        }
        {
            let records = txn.open_table(RECORDS)?;
            let mut members = txn.open_table(git_store::MEMBERS)?;
            let mut live = BTreeSet::new();
            let mut orphaned = Vec::new();
            for item in members.iter()? {
                let (member, _) = item?;
                let (repo, oid) = member
                    .value()
                    .split_once('/')
                    .context("invalid Git membership")?;
                if records.get(format!("r/{repo}").as_str())?.is_some() {
                    live.insert(oid.to_string());
                } else {
                    orphaned.push(member.value().to_string());
                }
            }
            for key in orphaned {
                members.remove(key.as_str())?;
            }
            let mut objects = txn.open_table(git_store::OBJECTS)?;
            let mut orphaned = Vec::new();
            for item in objects.iter()? {
                let (oid, _) = item?;
                if !live.contains(oid.value()) {
                    orphaned.push(oid.value().to_string());
                }
            }
            for oid in orphaned {
                objects.remove(oid.as_str())?;
            }
        }
        txn.commit()?;
        Ok(
            json!({"removed_records":removed,"note":"freed pages are reused; stop daemon and run compact to shrink the database file; Git objects and base manifests remain in redb"}),
        )
    }
}

fn resolved_physical_path(path: &Path) -> Result<PathBuf> {
    ensure!(path.is_absolute(), "physical path must be absolute");
    ensure!(
        !path.components().any(|c| matches!(
            c,
            std::path::Component::ParentDir | std::path::Component::CurDir
        )),
        "physical path cannot contain dot components"
    );
    safe_ancestors(path)?;
    let existing = path
        .ancestors()
        .find(|p| p.exists())
        .context("no existing ancestor")?;
    Ok(existing.canonicalize()?.join(path.strip_prefix(existing)?))
}

pub(crate) fn safe_ancestors(path: &Path) -> Result<()> {
    for p in path.ancestors() {
        if let Ok(meta) = std::fs::symlink_metadata(p) {
            #[cfg(target_os = "macos")]
            let system_alias = (p == Path::new("/var")
                && std::fs::read_link(p).ok() == Some(PathBuf::from("private/var")))
                || (p == Path::new("/tmp")
                    && std::fs::read_link(p).ok() == Some(PathBuf::from("private/tmp")));
            #[cfg(not(target_os = "macos"))]
            let system_alias = false;
            ensure!(
                !meta.file_type().is_symlink() || system_alias,
                "symlink in physical path: {}",
                p.display()
            );
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                ensure!(
                    meta.file_attributes() & 0x400 == 0,
                    "reparse point in physical path: {}",
                    p.display()
                );
            }
        }
    }
    Ok(())
}
fn truncate_utf8(mut s: String, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut n = max;
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    s.truncate(n);
    (s, true)
}
fn capture(mut input: impl Read, max: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    let mut truncated = false;
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let take = n.min(max.saturating_sub(out.len()));
        out.extend_from_slice(&buf[..take]);
        truncated |= take < n;
    }
    Ok((out, truncated))
}

struct ProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_truncated: bool,
    stderr_truncated: bool,
    success: bool,
    exit_code: Option<i32>,
    timed_out: bool,
}
impl Default for ProcessOutput {
    fn default() -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            success: true,
            exit_code: Some(0),
            timed_out: false,
        }
    }
}
impl ProcessOutput {
    fn append(&mut self, next: Self, max: usize) {
        let out = next.stdout.len().min(max.saturating_sub(self.stdout.len()));
        self.stdout.extend_from_slice(&next.stdout[..out]);
        let err = next.stderr.len().min(max.saturating_sub(self.stderr.len()));
        self.stderr.extend_from_slice(&next.stderr[..err]);
        self.stdout_truncated |= next.stdout_truncated || out < next.stdout.len();
        self.stderr_truncated |= next.stderr_truncated || err < next.stderr.len();
        self.success = next.success;
        self.exit_code = next.exit_code;
        self.timed_out = next.timed_out;
    }
}
pub(crate) fn git_credentials(url: &str, user: Option<&str>) -> Result<git2::Cred> {
    ensure!(
        !url.contains(['\n', '\r']) && !user.unwrap_or("").contains(['\n', '\r']),
        "invalid credential request"
    );
    let mut input = format!("url={url}\n");
    if let Some(user) = user {
        input.push_str(&format!("username={user}\n"));
    }
    input.push('\n');
    let args = ["-c", "credential.interactive=false", "credential", "fill"].map(String::from);
    let env = BTreeMap::from([
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("GCM_INTERACTIVE".into(), "never".into()),
    ]);
    let output = execute_process(
        "git",
        &args,
        &std::env::temp_dir(),
        &env,
        std::time::Duration::from_secs(30),
        65536,
        Some(input.as_bytes()),
    )?;
    ensure!(
        output.success && !output.stdout_truncated,
        "noninteractive credential lookup failed"
    );
    let text = std::str::from_utf8(&output.stdout)?;
    let values: BTreeMap<&str, &str> = text.lines().filter_map(|l| l.split_once('=')).collect();
    Ok(git2::Cred::userpass_plaintext(
        values
            .get("username")
            .context("missing credential username")?,
        values
            .get("password")
            .context("missing credential password")?,
    )?)
}

fn execute_process(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &BTreeMap<String, String>,
    timeout: std::time::Duration,
    max: usize,
    input: Option<&[u8]>,
) -> Result<ProcessOutput> {
    let mut cmd = crate::process::command(program);
    cmd.args(args)
        .current_dir(cwd)
        .envs(env)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    git::clear_overrides(&mut cmd);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd
        .spawn()
        .with_context(|| format!("start task executable {program}"))?;
    #[cfg(windows)]
    let job = match crate::job::Job::attach(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    if let Some(input) = input {
        use std::io::Write;
        if let Err(error) = child.stdin.take().unwrap().write_all(input) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error.into());
        }
    }
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let out_thread = std::thread::spawn(move || capture(stdout, max));
    let err_thread = std::thread::spawn(move || capture(stderr, max));
    let status = child.wait_timeout(timeout)?;
    let timed_out = status.is_none();
    #[cfg(windows)]
    drop(job);
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    if timed_out {
        let _ = child.kill();
    }
    let status = if let Some(status) = status {
        status
    } else {
        child.wait()?
    };
    let out = out_thread
        .join()
        .map_err(|_| anyhow::anyhow!("output thread failed"))??;
    let err = err_thread
        .join()
        .map_err(|_| anyhow::anyhow!("output thread failed"))??;
    Ok(ProcessOutput {
        stdout: out.0,
        stderr: err.0,
        stdout_truncated: out.1,
        stderr_truncated: err.1,
        success: status.success() && !timed_out,
        exit_code: status.code(),
        timed_out,
    })
}
