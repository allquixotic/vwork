//! Incremental libgit2 fetch: existing objects remain in Fjall throughout transfer.
use crate::{
    engine::{Engine, RECORDS},
    git, git_store,
    model::{Repository, valid_id},
};
use anyhow::{Context, Result, ensure};

use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

/// A receiver is private and disposable. Seed its sorted packed-refs directly
/// before libgit2 reads refs; creating thousands of loose refs here needlessly
/// multiplies file opens, locks, renames and antivirus work on every fetch.
fn seed_refs(receiver: &git2::Repository, refs: &BTreeMap<String, String>) -> Result<()> {
    git::write_packed_refs(receiver.path(), refs)?;
    if let Some(head) = refs.get("HEAD") {
        receiver.set_head_detached(git2::Oid::from_str(head)?)?;
    }
    Ok(())
}

fn milliseconds(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.
}

impl Engine {
    pub(crate) fn fetch_repository(&self, args: &Value) -> Result<Value> {
        let started = Instant::now();
        let mut timings = BTreeMap::new();
        let repo_id = args["repo"].as_str().context("missing repo")?;
        valid_id(repo_id)?;
        let lock = self.build_lock(repo_id);
        let _repository = lock
            .lock()
            .map_err(|_| anyhow::anyhow!("repository lock poisoned"))?;
        timings.insert("repository_wait", milliseconds(started));
        let source_started = Instant::now();
        let repo = self.repo(repo_id)?;
        let source = args["source"].as_str().map(String::from).or_else(|| repo.fetch_source.clone())
            .or_else(|| {
                if repo.git_dir.as_os_str().is_empty() { return None; }
                git2::Repository::open(&repo.git_dir).ok().and_then(|r| r.find_remote("origin").ok().and_then(|remote| remote.url().ok().map(String::from)))
            }).or_else(|| (!repo.path.as_os_str().is_empty() && repo.path.exists()).then(|| git::path_argument(&repo.path).to_string_lossy().into_owned()))
            .context("registration has no saved fetch source; supply source once (Git URL or absolute repository path)")?;
        let source = git::clone_source(&source)?;
        timings.insert("source", milliseconds(source_started));
        let transfer = self.begin_transfer(repo_id, "fetch")?;
        timings.insert("transfer_wait", transfer.wait_ms);
        let prepare_started = Instant::now();
        let path = &transfer.path;
        let result = (|| -> Result<Value> {
            let mut init = git2::RepositoryInitOptions::new();
            init.bare(true).external_template(false);
            let receiver = git2::Repository::init_opts(path.join("pack"), &init)?;
            git_store::attach_readonly(&receiver.odb()?, self.db.clone(), repo_id)?;
            seed_refs(&receiver, &repo.refs)?;
            timings.insert(
                "prepare",
                transfer.allocation_ms + milliseconds(prepare_started),
            );
            let transport_started = Instant::now();
            let mut remote = receiver.remote_anonymous(&source)?;
            let deadline = Instant::now() + Duration::from_secs(self.config.task_timeout_seconds);
            let mut callbacks = git2::RemoteCallbacks::new();
            callbacks.transfer_progress(move |p| {
                Instant::now() < deadline && p.received_bytes() <= 8 * 1024 * 1024 * 1024
            });
            let mut attempts = 0;
            callbacks.credentials(move |url, user, allowed| {
                attempts += 1;
                if attempts > 3 {
                    return Err(git2::Error::from_str(
                        "noninteractive authentication failed",
                    ));
                }
                if allowed.contains(git2::CredentialType::USERNAME) {
                    git2::Cred::username(user.unwrap_or("git"))
                } else if allowed.contains(git2::CredentialType::SSH_KEY) {
                    git2::Cred::ssh_key_from_agent(user.unwrap_or("git"))
                } else if allowed.contains(git2::CredentialType::USER_PASS_PLAINTEXT) {
                    crate::engine::git_credentials(url, user).map_err(|_| {
                        git2::Error::from_str("noninteractive Git credential helper failed")
                    })
                } else if allowed.contains(git2::CredentialType::DEFAULT) {
                    git2::Cred::default()
                } else {
                    Err(git2::Error::from_str("unsupported authentication"))
                }
            });
            let mut options = git2::FetchOptions::new();
            options
                .remote_callbacks(callbacks)
                .download_tags(git2::AutotagOption::All);
            remote.fetch(
                &["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"],
                Some(&mut options),
                None,
            )?;
            timings.insert("transport", milliseconds(transport_started));
            let import_started = Instant::now();
            let received_objects = remote.stats().received_objects();
            let received_bytes = remote.stats().received_bytes();
            let mut fetched = BTreeMap::new();
            for head in remote.list()? {
                let name = head.name();
                if (name == "HEAD"
                    || name.starts_with("refs/heads/")
                    || name.starts_with("refs/tags/"))
                    && !name.ends_with("^{}")
                    && !head.oid().is_zero()
                {
                    fetched.insert(name.to_owned(), head.oid().to_string());
                    if let Some(branch) = name.strip_prefix("refs/heads/") {
                        fetched.insert(
                            format!("refs/remotes/origin/{branch}"),
                            head.oid().to_string(),
                        );
                    }
                }
            }
            // Enumeration omits the read-through backend, so only newly received
            // packs are imported. References publish after successful import;
            // ordinary writes retain the engine's bounded buffered durability.
            let imported = git::import_objects(
                &self.db,
                &receiver,
                repo_id,
                self.config.compression_level,
                self.config.task_timeout_seconds,
            )?;
            timings.insert("import", milliseconds(import_started));
            let publish_started = Instant::now();
            for oid in fetched.values() {
                ensure!(
                    git_store::contains(&self.db, repo_id, git2::Oid::from_str(oid)?)?,
                    "fetched ref has no stored object"
                );
            }
            let transaction = self.db.begin_write()?;
            let updated;
            {
                let mut table = transaction.open_table(RECORDS)?;
                let key = format!("r/{repo_id}");
                let mut current: Repository = serde_json::from_slice(
                    table
                        .get(key.as_str())?
                        .context("repository missing")?
                        .value(),
                )?;
                if args["prune"].as_bool().unwrap_or(false) {
                    current.refs.retain(|name, _| {
                        !(name.starts_with("refs/heads/")
                            || name.starts_with("refs/tags/")
                            || name.starts_with("refs/remotes/origin/"))
                            || fetched.contains_key(name)
                    });
                }
                current.refs.extend(fetched);
                current.fetch_source = Some(source);
                table.insert(key.as_str(), serde_json::to_vec(&current)?.as_slice())?;
                updated = current;
            }
            transaction.commit()?;
            timings.insert("publish", milliseconds(publish_started));
            Ok(
                json!({"repo":repo_id,"head":updated.refs.get("HEAD"),"received_objects":received_objects,"received_bytes":received_bytes,"import":imported,"storage":"Fjall","workspaces_changed":false,"note":"Existing workspaces stay pinned; create a workspace from the fetched reference to use the updated source"}),
            )
        })();
        let cleanup_started = Instant::now();
        let cleanup = transfer.finish();
        timings.insert("cleanup", milliseconds(cleanup_started));
        timings.insert("total", milliseconds(started));
        let result = result.map(|mut value| {
            value["timings_ms"] = json!(timings);
            value
        });
        match (result, cleanup) {
            (Ok(mut value), Err(error)) => {
                value["cleanup_pending"] = json!(error.to_string());
                Ok(value)
            }
            (Err(error), _) => Err(error),
            (Ok(value), Ok(())) => Ok(value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v40_negotiation_refs_use_one_packed_file() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let receiver = git2::Repository::init_bare(temp.path())?;
        let oid = receiver.blob(b"reference fixture")?.to_string();
        let mut refs = BTreeMap::new();
        for n in 0..1000 {
            refs.insert(format!("refs/heads/topic/{n:04}"), oid.clone());
        }
        refs.insert("refs/tags/annotated".into(), oid.clone());
        refs.insert("refs/vwork/private".into(), oid.clone());
        seed_refs(&receiver, &refs)?;
        assert_eq!(receiver.references()?.count(), refs.len());
        for (name, id) in refs {
            assert_eq!(
                receiver.find_reference(&name)?.target(),
                Some(git2::Oid::from_str(&id)?)
            );
        }
        assert_eq!(
            walkdir::WalkDir::new(receiver.path().join("refs"))
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .count(),
            0
        );
        assert_eq!(
            walkdir::WalkDir::new(receiver.path())
                .into_iter()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name() == "packed-refs")
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn v40_negotiation_refs_reject_injected_records() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let receiver = git2::Repository::init_bare(temp.path())?;
        let refs = BTreeMap::from([("refs/heads/bad\nrefs/heads/injected".into(), "1".repeat(40))]);
        assert!(seed_refs(&receiver, &refs).is_err());
        Ok(())
    }
}
