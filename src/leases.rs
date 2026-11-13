//! Owned, journaled temporary copies. Repository locks protect running builds;
//! a separate allocation lock bounds the number of physical copies globally.
use crate::{
    engine::{Engine, RECORDS, safe_ancestors},
    git,
    model::*,
    physical,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::{Path, PathBuf},
    time::Instant,
};

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.
}

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
        self.put(&format!("lease/{}", lease.id), lease)?;
        // External temporary directories need durable ownership/recovery state.
        self.db.persist()
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
        let active = self.leases()?.len();
        ensure!(
            active < self.config.max_temporary_copies,
            "temporary copy limit reached; release an existing lease before allocating another"
        );
        // Pooled trees are physical copies too. Leases outrank them, and
        // this lease frees a slot itself when it adopts one of its own.
        let adoptable = usize::from(self.pooled().iter().any(|(repo, _, _)| *repo == ws.repo));
        self.trim_pool(
            self.config.max_temporary_copies - active - 1 + adoptable,
            &ws.repo,
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
    /// Permanent history of `repo` as shared packs holding at least `base`.
    /// Caller owns the repository lock.
    pub(crate) fn shared_history(
        &self,
        repo: &Repository,
        base: &str,
    ) -> Result<(PathBuf, crate::git_cache::Report)> {
        let root = self.home.join("git-cache").join(&repo.id);
        safe_ancestors(&root)?;
        let report =
            crate::git_cache::ensure(&self.db, repo, &root, &[git2::Oid::from_str(base)?])?;
        Ok((root, report))
    }
    /// Returns the Git view and a receipt of the work each phase performed.
    pub(crate) fn prepare_lease(
        &self,
        lease: &mut Lease,
        ws: &Workspace,
        tree: &Tree,
    ) -> Result<(git2::Repository, Value)> {
        let root = self.lease_root(lease);
        self.check_lease_owner(lease)?;
        let repo = self.repo(&ws.repo)?;
        let revision = self.revision(&ws.head)?;
        let started = Instant::now();
        let (history, shared) = self.shared_history(&repo, &revision.base)?;
        let history_ms = milliseconds(started);
        let started = Instant::now();
        let destination = root.join("tree");
        let mut adopted = !destination.exists() && self.adopt_tree(&ws.repo, &destination);
        let files = match self.materialize_into(ws, tree, &destination, adopted) {
            Err(_) if adopted => {
                // A pooled tree that cannot be made pristine (a foreign file
                // still held open, a damaged record) is only a cache miss.
                adopted = false;
                self.discard_tree(&destination)?;
                self.materialize_into(ws, tree, &destination, false)?
            }
            other => other?,
        };
        let files_ms = milliseconds(started);
        let started = Instant::now();
        let mut reader = None;
        let (view, linked) = git::prepare_build(
            &repo,
            &history,
            &shared.files,
            &root.join("git"),
            &root.join("tree"),
            &revision.base,
            tree,
            &files.stats,
            |entry| self.bytes(&repo, entry, &mut reader),
        )?;
        let view_ms = milliseconds(started);
        lease.workspace = ws.id.clone();
        lease.revision = ws.head.clone();
        lease.state = "ready".into();
        lease.cleanup_error = None;
        self.lease_save(lease)?;
        Ok((
            view,
            json!({
                "timings_ms":{"shared_history":history_ms,"files":files_ms,"git_view":view_ms},
                "work":{
                    "shared_history":shared,
                    "files":{"adopted_pooled_tree":adopted,"written":files.written,"reused":files.reused,"verified":files.verified,"removed":files.removed,"bytes_written":files.bytes},
                    "git_view":linked,
                },
            }),
        ))
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
        let marker = root.join(physical::MARKER);
        if !marker.exists() {
            return Ok(None);
        }
        let ws = Workspace {
            id: lease.workspace.clone(),
            repo: lease.repo.clone(),
            head: lease.revision.clone(),
        };
        let (base, tree) = self.snapshot(&ws)?;
        let repo = self.repo(&ws.repo)?;
        // A tracked file whose listing still matches what was recorded when
        // vwork wrote it was not edited; only the others are opened. Copies
        // from before that record existed have none, so every file is read.
        let record = physical::Marker::read(&marker)?;
        let recorded = record.stats();
        let present = physical::scan(&root, &tree, &physical::directories(&tree), false)?.files;
        let mut paths: BTreeSet<String> = tree
            .iter()
            .filter(|(path, entry)| {
                let known = recorded.get(path.as_str());
                self.config.verify_materialized_content
                    || known.is_none()
                    || known != present.get(*path)
                    || record.entries.get(*path) != Some(entry)
            })
            .map(|(path, _)| path.clone())
            .collect();
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
                crate::git_store::write_overlay(
                    &tx,
                    &recovery.repo,
                    git2::Oid::from_str(&hash)?,
                    &bytes,
                )?;
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
            self.park_tree(lease, &root);
            self.discard_tree(&root)?;
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
        self.db.persist()?;
        Ok(result)
    }
    /// Releasing must not wait for one delete per file. A rename takes the
    /// whole tree out of the lease namespace at once; a detached worker, or the
    /// next reaper pass after a crash, removes it. Recovery already happened.
    fn discard_tree(&self, root: &Path) -> Result<()> {
        let bin = self.trash_bin()?;
        if std::fs::rename(root, bin.join("0")).is_ok() {
            Engine::empty_trash(bin);
            return Ok(());
        }
        let _ = std::fs::remove_dir(bin);
        // An open handle inside the tree blocks a Windows directory rename.
        // The record stays on failure, exactly as before, and is retried.
        std::fs::remove_dir_all(root)?;
        Ok(())
    }
    /// Trees a previous process renamed away but did not finish deleting.
    /// Recent bins may still be receiving renames from a running operation.
    fn sweep_trash(&self) {
        let trash = self.home.join("trash");
        if safe_ancestors(&trash).is_err() {
            return;
        }
        for bin in std::fs::read_dir(trash).into_iter().flatten().flatten() {
            let settled = bin
                .metadata()
                .and_then(|meta| meta.modified())
                .is_ok_and(|at| at.elapsed().is_ok_and(|age| age.as_secs() >= 60));
            if settled {
                Engine::empty_trash(bin.path());
            }
        }
    }

    /// Released trees waiting for the next lease of their repository:
    /// (repository, slot directory, time parked), oldest first.
    fn pooled(&self) -> Vec<(String, PathBuf, std::time::SystemTime)> {
        let mut all = Vec::new();
        let pool = self.home.join("pool");
        if safe_ancestors(&pool).is_err() {
            return all;
        }
        for repo in std::fs::read_dir(pool).into_iter().flatten().flatten() {
            let Some(name) = repo.file_name().to_str().map(String::from) else {
                continue;
            };
            for slot in std::fs::read_dir(repo.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                let parked = slot
                    .metadata()
                    .and_then(|meta| meta.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                all.push((name.clone(), slot.path(), parked));
            }
        }
        all.sort_by_key(|(_, _, parked)| *parked);
        all
    }
    /// Keep a released tree instead of deleting it file by file, only for the
    /// next lease to recreate it file by file. One rename now; the next copy
    /// of this repository rewrites just what differs. Under its waiting name
    /// the record cannot be mistaken for a live materialization.
    /// Caller owns the allocation lock; `lease` still has its record.
    fn park_tree(&self, lease: &Lease, root: &Path) -> bool {
        let tree = root.join("tree");
        let Ok(active) = self.leases() else {
            return false;
        };
        if self.config.pooled_tree_ttl_seconds == 0
            || !tree.join(physical::MARKER).is_file()
            || active.len().saturating_sub(1) + self.pooled().len()
                >= self.config.max_temporary_copies
        {
            return false;
        }
        let slot = self.home.join("pool").join(&lease.repo).join(id());
        let parked = safe_ancestors(&slot).is_ok()
            && std::fs::create_dir_all(&slot).is_ok()
            && std::fs::rename(tree.join(physical::MARKER), tree.join(physical::POOLED)).is_ok()
            && std::fs::rename(&tree, slot.join("tree")).is_ok();
        if !parked {
            // Whatever remains is removed with the rest of the lease.
            let _ = std::fs::remove_dir(&slot);
        }
        parked
    }
    /// Claim the most recently parked tree of `repo`. The rename is the claim:
    /// a reaper retiring the same slot simply loses or wins it whole.
    fn adopt_tree(&self, repo: &str, destination: &Path) -> bool {
        for (_, slot, _) in self
            .pooled()
            .into_iter()
            .rev()
            .filter(|(name, _, _)| name == repo)
        {
            if !slot.join("tree").join(physical::POOLED).is_file() {
                // Interrupted while parking: not a tree anyone can vouch for.
                let _ = self.discard_tree(&slot);
            } else if std::fs::rename(slot.join("tree"), destination).is_ok() {
                let _ = std::fs::remove_dir(slot);
                return true;
            }
        }
        false
    }
    /// Retire pooled trees until at most `keep` remain, oldest first and
    /// other repositories before `favored`.
    fn trim_pool(&self, keep: usize, favored: &str) {
        let mut pooled = self.pooled();
        pooled.sort_by_key(|(repo, _, parked)| (repo == favored, *parked));
        let excess = pooled.len().saturating_sub(keep);
        for (_, slot, _) in pooled.into_iter().take(excess) {
            let _ = self.discard_tree(&slot);
        }
    }
    fn expire_pool(&self) {
        let ttl = std::time::Duration::from_secs(self.config.pooled_tree_ttl_seconds);
        for (repo, slot, parked) in self.pooled() {
            let expired = parked.elapsed().map_or(true, |age| age >= ttl);
            if expired || self.repo(&repo).is_err() {
                let _ = self.discard_tree(&slot);
            }
        }
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
            // Remove old alternates before rebuilding the Git view from Fjall;
            // physical source edits remain untouched for recovery.
            let alternates = root.join("git/objects/info/alternates");
            safe_ancestors(&alternates)?;
            if alternates.exists() {
                std::fs::remove_file(alternates)?;
            }
            let (revision, tree) = self.snapshot(&ws)?;
            let (history, shared) = self.shared_history(&repo, &revision.base)?;
            let mut reader = None;
            // No recorded metadata: the adopted files were written long ago.
            git::prepare_build(
                &repo,
                &history,
                &shared.files,
                &root.join("git"),
                &root.join("tree"),
                &revision.base,
                &tree,
                &BTreeMap::new(),
                |entry| self.bytes(&repo, entry, &mut reader),
            )?;
            let mut ready = lease;
            ready.state = "ready".into();
            self.lease_save(&ready)?;
        }
        Ok(())
    }
    pub fn reap_temporary(&self) -> Result<()> {
        self.sweep_trash();
        self.cleanup_transfers()?;
        let _lifecycle = self
            .lifecycle
            .read()
            .map_err(|_| anyhow::anyhow!("lifecycle lock poisoned"))?;
        let _allocation = self
            .temporary_lock
            .lock()
            .map_err(|_| anyhow::anyhow!("temporary allocation lock poisoned"))?;
        self.expire_pool();
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
            let pooled: Vec<Value> = self
                .pooled()
                .into_iter()
                .map(|(repo, slot, parked)| {
                    json!({"repo":repo,"path":slot,"idle_seconds":parked.elapsed().map_or(0, |age| age.as_secs())})
                })
                .collect();
            return Ok(
                json!({"active":active,"recent":recent,"transfers":transfers,"pooled":pooled}),
            );
        }
        if op == "workspace_temporary" {
            let started = Instant::now();
            let ws = self.workspace(args["workspace"].as_str().context("missing workspace")?)?;
            ensure!(
                args["expected_revision"].as_str() == Some(&ws.head),
                "revision conflict"
            );
            let lock = self.build_lock(&ws.repo);
            let _job = lock
                .lock()
                .map_err(|_| anyhow::anyhow!("repository lock poisoned"))?;
            let repository_wait = milliseconds(started);
            let snapshot_started = Instant::now();
            let (_, tree) = self.snapshot(&ws)?;
            let snapshot = milliseconds(snapshot_started);
            let mut lease = self.create_lease(
                &ws,
                args["purpose"].as_str().context("missing purpose")?,
                false,
                self.ttl(args)?,
            )?;
            let (_, mut receipt) = self.prepare_lease(&mut lease, &ws, &tree)?;
            // Phase costs travel with the result: a slow host can be diagnosed
            // from numbers alone, without anyone seeing its files.
            receipt["timings_ms"]["repository_wait"] = json!(repository_wait);
            receipt["timings_ms"]["snapshot"] = json!(snapshot);
            receipt["timings_ms"]["total"] = json!(milliseconds(started));
            let mut value = self.lease_value(&lease);
            value["timings_ms"] = receipt["timings_ms"].take();
            value["work"] = receipt["work"].take();
            return Ok(value);
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
            lease.expires_at = lease.expires_at.max(now().saturating_add(self.ttl(args)?));
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
