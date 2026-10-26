//! Owned, journaled temporary copies. Repository locks protect running builds;
//! a separate allocation lock bounds the number of physical copies globally.
use crate::{
    engine::{Engine, RECORDS, safe_ancestors},
    git,
    model::*,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::PathBuf,
};

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Lease {
    pub id: String,
    pub workspace: String,
    pub repo: String,
    pub revision: String,
    pub purpose: String,
    pub build: bool,
    pub expires_at: u64,
    pub state: String,
    pub cleanup_error: Option<String>,
}
impl Engine {
    pub(crate) fn lease_root(&self, lease: &Lease) -> PathBuf {
        self.home.join("temporary").join(&lease.id)
    }
    pub(crate) fn lease_value(&self, lease: &Lease) -> Value {
        let mut value = serde_json::to_value(lease).unwrap();
        value["path"] = json!(crate::packages::process_path(
            &self.lease_root(lease).join("tree")
        ));
        value["instruction"] = json!(
            "Use only while necessary. Import wanted source changes explicitly, then call temporary_release with this lease ID. Renew before expiry while still using it. Expiry saves bounded nonignored source edits to a recovery workspace before removal; dependencies/caches are disposable."
        );
        value
    }
    pub(crate) fn leases(&self) -> Result<Vec<Lease>> {
        self.scan("lease/")?
            .into_iter()
            .map(|(_, b)| Ok(serde_json::from_slice(&b)?))
            .collect()
    }
    pub(crate) fn lease_save(&self, lease: &Lease) -> Result<()> {
        self.put(&format!("lease/{}", lease.id), lease)
    }
    fn ttl(&self, value: &Value) -> Result<u64> {
        let ttl = value["ttl_seconds"]
            .as_u64()
            .unwrap_or(self.config.temporary_ttl_seconds);
        ensure!(
            ttl > 0 && ttl <= self.config.temporary_max_ttl_seconds,
            "temporary TTL must be 1..{} seconds",
            self.config.temporary_max_ttl_seconds
        );
        Ok(ttl)
    }
    // Caller owns the repository build lock, including through filesystem creation.
    pub(crate) fn create_lease(
        &self,
        ws: &Workspace,
        purpose: &str,
        build: bool,
        ttl: u64,
    ) -> Result<Lease> {
        ensure!(
            !purpose.trim().is_empty() && purpose.len() <= 1024,
            "give a short reason the tool needs physical files"
        );
        let _allocation = self
            .temporary_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("temporary allocation lock poisoned"))?;
        ensure!(
            self.leases()?.len() < self.config.max_temporary_copies,
            "temporary copy limit reached; release an existing lease before allocating another"
        );
        let lease = Lease {
            id: id(),
            workspace: ws.id.clone(),
            repo: ws.repo.clone(),
            revision: ws.head.clone(),
            purpose: purpose.into(),
            build,
            expires_at: now().saturating_add(ttl),
            state: "creating".into(),
            cleanup_error: None,
        };
        self.lease_save(&lease)?; // Persist before creating any heavy disk content.
        let root = self.lease_root(&lease);
        safe_ancestors(&root)?;
        std::fs::create_dir_all(root.parent().unwrap())?;
        std::fs::create_dir(&root)?;
        std::fs::write(root.join("owner"), &lease.id)?;
        Ok(lease)
    }
    pub(crate) fn prepare_lease(
        &self,
        lease: &mut Lease,
        ws: &Workspace,
        tree: &Tree,
    ) -> Result<git2::Repository> {
        let root = self.lease_root(lease);
        self.check_lease_owner(lease)?;
        self.materialize(ws, tree, &root.join("tree"))?;
        let repo = self.repo(&ws.repo)?;
        let revision = self.revision(&ws.head)?;
        let mut reader = None;
        let view = git::prepare_build(
            &self.db,
            &repo,
            &root.join("git"),
            &root.join("tree"),
            &revision.base,
            tree,
            |entry| self.bytes(&repo, entry, &mut reader),
        )?;
        lease.workspace = ws.id.clone();
        lease.revision = ws.head.clone();
        lease.state = "ready".into();
        lease.cleanup_error = None;
        self.lease_save(lease)?;
        Ok(view)
    }
    fn check_lease_owner(&self, lease: &Lease) -> Result<()> {
        valid_id(&lease.id)?;
        let root = self.lease_root(lease);
        safe_ancestors(&root)?;
        if !root.exists() {
            return Ok(());
        }
        let marker = root.join("owner");
        safe_ancestors(&marker)?;
        if marker.exists() {
            ensure!(
                std::fs::read_to_string(marker)? == lease.id,
                "temporary ownership mismatch"
            );
        } else {
            ensure!(
                std::fs::read_dir(&root)?.next().is_none(),
                "unmarked temporary directory contains files"
            );
        }
        Ok(())
    }
    /// Preserve source changes in a separate virtual workspace, never the live source.
    /// Ignored/generated dependency trees are deliberately not archived.
    pub(crate) fn recover_lease(&self, lease: &Lease) -> Result<Option<String>> {
        let root = self.lease_root(lease).join("tree");
        if !root.join(".vwork-managed.json").exists() {
            return Ok(None);
        }
        let ws = Workspace {
            id: lease.workspace.clone(),
            repo: lease.repo.clone(),
            head: lease.revision.clone(),
        };
        let (base, tree) = self.snapshot(&ws)?;
        let repo = self.repo(&ws.repo)?;
        let mut paths: BTreeSet<String> = tree.keys().cloned().collect();
        if root.join(".git").exists() {
            let view = git2::Repository::open(&root)?;
            let mut options = git2::StatusOptions::new();
            options
                .include_untracked(true)
                .recurse_untracked_dirs(true)
                .include_ignored(false);
            for status in view.statuses(Some(&mut options))?.iter() {
                paths.insert(status.path()?.replace('\\', "/"));
            }
        }
        let mut changes = BTreeMap::new();
        let mut blobs = BTreeMap::new();
        let mut reader = None;
        let mut total = 0u64;
        for path in paths {
            if !tree.contains_key(&path)
                && path.split('/').any(|p| {
                    [
                        ".git",
                        ".vwork-managed.json",
                        "node_modules",
                        ".venv",
                        "target",
                        "cdk.out",
                        "__pycache__",
                    ]
                    .contains(&p)
                })
            {
                continue;
            }
            crate::model::path(&path)?;
            let physical = root.join(&path);
            safe_ancestors(&physical)?;
            let old = tree.get(&path);
            if !physical.exists() {
                if old.is_some() {
                    changes.insert(path, None);
                }
                continue;
            }
            ensure!(
                physical.is_file(),
                "recovery source became a directory: {path}"
            );
            let size = std::fs::metadata(&physical)?.len();
            ensure!(
                size <= self.config.max_file_bytes as u64,
                "recovery file too large: {path}; import or explicitly discard before release"
            );
            let mut content = Vec::new();
            std::fs::File::open(&physical)?
                .take(self.config.max_file_bytes as u64 + 1)
                .read_to_end(&mut content)?;
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                if std::fs::metadata(&physical)?.permissions().mode() & 0o111 != 0 {
                    "100755"
                } else {
                    "100644"
                }
            };
            #[cfg(windows)]
            let mode = old.map_or("100644", |e| e.mode.as_str());
            if let Some(entry) = old
                && entry.mode == mode
                && self.bytes(&repo, entry, &mut reader)? == content
            {
                continue;
            }
            total = total.saturating_add(content.len() as u64);
            ensure!(
                total <= self.config.max_recovery_bytes && changes.len() < self.config.max_results,
                "source recovery limit exceeded; import or explicitly discard before release"
            );
            let (entry, compressed) = self.encoded(content, mode)?;
            blobs.insert(entry.oid.clone(), compressed);
            changes.insert(path, Some(entry));
        }
        ensure!(
            changes.len() <= self.config.max_results,
            "source recovery limit exceeded; import or intentionally discard before release"
        );
        if changes.is_empty() {
            return Ok(None);
        }
        let name = format!("recovery-{}-{}", lease.id, &id()[..8]);
        let revision = Revision {
            id: id(),
            repo: ws.repo.clone(),
            parent: Some(ws.head),
            base: base.base,
            changes,
            message: format!("Recovered temporary copy {} ({})", lease.id, lease.purpose),
            timestamp: now(),
        };
        let recovery = Workspace {
            id: name.clone(),
            repo: ws.repo,
            head: revision.id.clone(),
        };
        let tx = self.db.begin_write()?;
        {
            let mut t = tx.open_table(RECORDS)?;
            for (hash, bytes) in blobs {
                t.insert(format!("b/{hash}").as_str(), bytes.as_slice())?;
            }
            t.insert(
                format!("v/{}", revision.id).as_str(),
                serde_json::to_vec(&revision)?.as_slice(),
            )?;
            t.insert(
                format!("w/{name}").as_str(),
                serde_json::to_vec(&recovery)?.as_slice(),
            )?;
        }
        tx.commit()?;
        Ok(Some(name))
    }
    // Caller owns repository lock and allocation lock. The record remains on any
    // failure, so Windows sharing violations are visible and retried after restart.
    fn release_lease(&self, lease: &mut Lease, discard: bool) -> Result<Value> {
        self.check_lease_owner(lease)?;
        let history_key = format!("lease-history/{}", lease.id);
        let previous = self.get::<Value>(&history_key).ok();
        let recovery = if lease.state == "deleting" {
            previous
                .as_ref()
                .and_then(|v| v["recovery_workspace"].as_str())
                .map(String::from)
        } else if discard {
            None
        } else {
            self.recover_lease(lease)?
        };
        let result = json!({"lease":lease.id,"released":true,"recovery_workspace":recovery,"timestamp":now()});
        let mut pending = result.clone();
        pending["released"] = json!(false);
        self.put(&history_key, &pending)?;
        lease.state = "deleting".into();
        self.lease_save(lease)?;
        let root = self.lease_root(lease);
        if root.exists() {
            std::fs::remove_dir_all(root)?;
        }
        let tx = self.db.begin_write()?;
        {
            let mut table = tx.open_table(RECORDS)?;
            table.insert(
                history_key.as_str(),
                serde_json::to_vec(&result)?.as_slice(),
            )?;
            table.remove(format!("lease/{}", lease.id).as_str())?;
        }
        tx.commit()?;
        Ok(result)
    }
    /// Adopt only legacy build directories whose existing vwork marker proves
    /// ownership. Moving them into the lease root avoids copying heavy outputs.
    pub(crate) fn migrate_legacy_copies(&self) -> Result<()> {
        for (_, value) in self.scan("r/")? {
            let repo: Repository = serde_json::from_slice(&value)?;
            let old = self.home.join("build").join(&repo.id);
            safe_ancestors(&old)?;
            let adopted = self
                .home
                .join("temporary")
                .join(format!("legacy-{}", repo.id))
                .join("tree");
            if !old.exists() && !adopted.exists() {
                continue;
            }
            let marker = if old.exists() {
                old.join(".vwork-managed.json")
            } else {
                adopted.join(".vwork-managed.json")
            };
            safe_ancestors(&marker)?;
            if !marker.is_file() {
                continue;
            }
            let record: Value = serde_json::from_slice(&std::fs::read(&marker)?)?;
            ensure!(
                record["home"].as_str() == self.home.to_str() && record["repo"] == repo.id,
                "legacy build ownership mismatch"
            );
            let ws = Workspace {
                id: record["workspace"]
                    .as_str()
                    .context("legacy workspace missing")?
                    .into(),
                repo: repo.id.clone(),
                head: record["revision"]
                    .as_str()
                    .context("legacy revision missing")?
                    .into(),
            };
            self.revision(&ws.head)?;
            let lease = Lease {
                id: format!("legacy-{}", repo.id),
                workspace: ws.id.clone(),
                repo: repo.id.clone(),
                revision: ws.head.clone(),
                purpose: "reclaim pre-0.3 build outputs".into(),
                build: true,
                expires_at: now(),
                state: "creating".into(),
                cleanup_error: None,
            };
            // Deterministic ID makes interrupted adoption resumable.
            let root = self.lease_root(&lease);
            safe_ancestors(&root)?;
            if root.exists() {
                self.check_lease_owner(&lease)?;
            } else {
                self.lease_save(&lease)?;
                std::fs::create_dir_all(root.parent().unwrap())?;
                std::fs::create_dir(&root)?;
                std::fs::write(root.join("owner"), &lease.id)?;
            }
            if !root.join("tree").exists() {
                std::fs::rename(&old, root.join("tree"))?;
            }
            for category in ["build-git", "targets", "environments", "artifacts"] {
                let from = self.home.join(category).join(&repo.id);
                safe_ancestors(&from)?;
                let to = if category == "build-git" {
                    root.join("git")
                } else {
                    root.join(category).join(&repo.id)
                };
                if from.exists() {
                    std::fs::create_dir_all(to.parent().unwrap())?;
                    std::fs::rename(from, &to)?;
                }
            }
            // Remove old alternates before rebuilding the Git view from redb;
            // physical source edits remain untouched for recovery.
            let alternates = root.join("git/objects/info/alternates");
            safe_ancestors(&alternates)?;
            if alternates.exists() {
                std::fs::remove_file(alternates)?;
            }
            let (revision, tree) = self.snapshot(&ws)?;
            let mut reader = None;
            git::prepare_build(
                &self.db,
                &repo,
                &root.join("git"),
                &root.join("tree"),
                &revision.base,
                &tree,
                |entry| self.bytes(&repo, entry, &mut reader),
            )?;
            let mut ready = lease;
            ready.state = "ready".into();
            self.lease_save(&ready)?;
        }
        Ok(())
    }
    pub fn reap_temporary(&self) -> Result<()> {
        self.cleanup_transfers()?;
        let _lifecycle = self
            .lifecycle
            .read()
            .map_err(|_| anyhow::anyhow!("lifecycle lock poisoned"))?;
        let _allocation = self
            .temporary_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("temporary allocation lock poisoned"))?;
        for mut lease in self.leases()? {
            if lease.expires_at > now() && lease.state == "ready" {
                continue;
            }
            let lock = self.build_lock(&lease.repo);
            let Ok(_job) = lock.try_lock() else {
                continue;
            };
            if let Err(error) = self.release_lease(&mut lease, false) {
                lease.cleanup_error = Some(format!("{error:#}"));
                self.lease_save(&lease)?;
            }
        }
        Ok(())
    }
    pub(crate) fn temporary_call(&self, op: &str, args: &Value) -> Result<Value> {
        if op == "temporary_list" {
            let active: Vec<Value> = self.leases()?.iter().map(|l| self.lease_value(l)).collect();
            let mut recent: Vec<Value> = self
                .scan("lease-history/")?
                .into_iter()
                .map(|(_, v)| serde_json::from_slice(&v))
                .collect::<std::result::Result<_, _>>()?;
            recent.sort_by_key(|v| std::cmp::Reverse(v["timestamp"].as_u64().unwrap_or(0)));
            recent.truncate(self.config.max_run_history);
            let transfers: Vec<Value> = self
                .scan("transfer/")?
                .into_iter()
                .map(|(k, v)| {
                    let mut record: Value = serde_json::from_slice(&v)?;
                    record["id"] = json!(&k[9..]);
                    record["path"] = json!(self.home.join("transfers").join(&k[9..]));
                    Ok(record)
                })
                .collect::<Result<_>>()?;
            return Ok(json!({"active":active,"recent":recent,"transfers":transfers}));
        }
        if op == "workspace_temporary" {
            let ws = self.workspace(args["workspace"].as_str().context("missing workspace")?)?;
            ensure!(
                args["expected_revision"].as_str() == Some(&ws.head),
                "revision conflict"
            );
            let lock = self.build_lock(&ws.repo);
            let _job = lock
                .lock()
                .map_err(|_| anyhow::anyhow!("repository lock poisoned"))?;
            let (_, tree) = self.snapshot(&ws)?;
            let mut lease = self.create_lease(
                &ws,
                args["purpose"].as_str().context("missing purpose")?,
                false,
                self.ttl(args)?,
            )?;
            self.prepare_lease(&mut lease, &ws, &tree)?;
            return Ok(self.lease_value(&lease));
        }
        let id = args["lease"].as_str().context("missing lease")?;
        valid_id(id)?;
        let _allocation = self
            .temporary_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("temporary allocation lock poisoned"))?;
        let mut lease: Lease = self.get(&format!("lease/{id}"))?;
        let lock = self.build_lock(&lease.repo);
        let _job = lock
            .try_lock()
            .map_err(|_| anyhow::anyhow!("repository job is active; retry after it finishes"))?;
        if op == "temporary_renew" {
            ensure!(lease.state == "ready", "temporary copy is not ready");
            lease.expires_at = now().saturating_add(self.ttl(args)?);
            self.lease_save(&lease)?;
            return Ok(self.lease_value(&lease));
        }
        let result = self.release_lease(
            &mut lease,
            args["discard_changes"].as_bool().unwrap_or(false),
        );
        if let Err(error) = &result {
            lease.cleanup_error = Some(format!("{error:#}"));
            self.lease_save(&lease)?;
        }
        result
    }
}
