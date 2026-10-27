//! Incremental libgit2 fetch: existing objects remain in redb throughout transfer.
use crate::{
    engine::{Engine, RECORDS},
    git, git_store,
    model::{Repository, id, now, valid_id},
};
use anyhow::{Context, Result, ensure};
use redb::ReadableTable;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

impl Engine {
    pub(crate) fn fetch_repository(&self, args: &Value) -> Result<Value> {
        let repo_id = args["repo"].as_str().context("missing repo")?;
        valid_id(repo_id)?;
        let lock = self.build_lock(repo_id);
        let _repository = lock
            .lock()
            .map_err(|_| anyhow::anyhow!("repository lock poisoned"))?;
        let repo = self.repo(repo_id)?;
        let source = args["source"].as_str().map(String::from).or_else(|| repo.fetch_source.clone())
            .or_else(|| {
                if repo.git_dir.as_os_str().is_empty() { return None; }
                git2::Repository::open(&repo.git_dir).ok().and_then(|r| r.find_remote("origin").ok().and_then(|remote| remote.url().ok().map(String::from)))
            }).or_else(|| (!repo.path.as_os_str().is_empty() && repo.path.exists()).then(|| git::path_argument(&repo.path).to_string_lossy().into_owned()))
            .context("registration has no saved fetch source; supply source once (Git URL or absolute repository path)")?;
        let source = git::clone_source(&source)?;
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
        crate::engine::safe_ancestors(&path)?;
        self.put(
            &format!("transfer/{transfer}"),
            &json!({"repo":repo_id,"created":now(),"operation":"fetch"}),
        )?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::create_dir(&path)?;
        std::fs::write(path.join("owner"), &transfer)?;
        let result = (|| -> Result<Value> {
            let mut init = git2::RepositoryInitOptions::new();
            init.bare(true).external_template(false);
            let receiver = git2::Repository::init_opts(path.join("pack"), &init)?;
            git_store::attach_readonly(&receiver.odb()?, self.db.clone(), repo_id)?;
            for (name, oid) in &repo.refs {
                if name != "HEAD" {
                    receiver.reference(
                        name,
                        git2::Oid::from_str(oid)?,
                        true,
                        "vwork fetch negotiation",
                    )?;
                }
            }
            if let Some(head) = repo.refs.get("HEAD") {
                receiver.set_head_detached(git2::Oid::from_str(head)?)?;
            }
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
            // packs are imported. Publication waits for all objects to be durable.
            let mut imported = repo.clone();
            git::import(
                &self.db,
                &receiver,
                &mut imported,
                self.config.compression_level,
                self.config.task_timeout_seconds,
            )?;
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
            Ok(
                json!({"repo":repo_id,"head":updated.refs.get("HEAD"),"received_objects":received_objects,"received_bytes":received_bytes,"storage":"redb","workspaces_changed":false,"note":"Existing workspaces stay pinned; create a workspace from the fetched reference to use the updated source"}),
            )
        })();
        let cleanup = self.remove_transfer(&transfer);
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
