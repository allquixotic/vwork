//! Short allocation/cleanup coordination; network transfers own separate guards.
use crate::{
    engine::Engine,
    model::{id, now},
};
use anyhow::{Result, anyhow, ensure};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Condvar, Mutex},
    time::Instant,
};

/// Bound expensive transports even if a caller raises the temporary-copy limit.
const MAX_CONCURRENT_TRANSFERS: usize = 4;

#[derive(Default)]
pub(crate) struct Transfers {
    active: Mutex<HashSet<String>>,
    available: Condvar,
}

pub(crate) struct Transfer<'a> {
    engine: &'a Engine,
    id: String,
    pub path: PathBuf,
    pub wait_ms: f64,
    pub allocation_ms: f64,
}

impl Transfer<'_> {
    pub fn finish(self) -> Result<()> {
        // Keep the guard active until both filesystem and record cleanup finish.
        // A failed cleanup leaves its owned recovery record for a later reaper.
        self.engine.remove_transfer(&self.id)
    }
}

impl Drop for Transfer<'_> {
    fn drop(&mut self) {
        let mut active = self
            .engine
            .transfers
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        active.remove(&self.id);
        self.engine.transfers.available.notify_all();
    }
}

impl Engine {
    pub(crate) fn begin_transfer(&self, repo: &str, operation: &str) -> Result<Transfer<'_>> {
        let started = Instant::now();
        let mut active = self
            .transfers
            .active
            .lock()
            .map_err(|_| anyhow!("transfer allocation lock poisoned"))?;
        loop {
            let mut pending = self.scan("transfer/")?.len();
            if pending >= self.config.max_temporary_copies && pending > active.len() {
                // A restart can leave every slot occupied by abandoned packs.
                // Reap only inactive owned receivers on this worker before
                // deciding capacity is blocked; no explicit recovery step.
                self.cleanup_inactive_transfers(&active)?;
                pending = self.scan("transfer/")?.len();
            }
            if pending < self.config.max_temporary_copies && active.len() < MAX_CONCURRENT_TRANSFERS
            {
                break;
            }
            // Active owners can free a slot; abandoned records require cleanup.
            ensure!(
                !active.is_empty(),
                "pending transfer cleanup limit reached; inspect temporary_list"
            );
            active = self
                .transfers
                .available
                .wait(active)
                .map_err(|_| anyhow!("transfer allocation lock poisoned"))?;
        }
        let wait_ms = started.elapsed().as_secs_f64() * 1000.;
        let allocation_started = Instant::now();
        let transfer = id();
        let path = self.home.join("transfers").join(&transfer);
        crate::engine::safe_ancestors(&path)?;
        self.put(
            &format!("transfer/{transfer}"),
            &json!({"repo":repo,"created":now(),"operation":operation}),
        )?;
        // The cleanup intent must survive a crash before creating external files.
        self.db.persist()?;
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::create_dir(&path)?;
        std::fs::write(path.join("owner"), &transfer)?;
        active.insert(transfer.clone());
        Ok(Transfer {
            engine: self,
            id: transfer,
            path,
            wait_ms,
            allocation_ms: allocation_started.elapsed().as_secs_f64() * 1000.,
        })
    }

    pub(crate) fn cleanup_transfers(&self) -> Result<()> {
        let Ok(active) = self.transfers.active.try_lock() else {
            return Ok(());
        };
        self.cleanup_inactive_transfers(&active)?;
        self.transfers.available.notify_all();
        Ok(())
    }

    fn cleanup_inactive_transfers(&self, active: &HashSet<String>) -> Result<()> {
        for (key, _) in self.scan("transfer/")? {
            let transfer = &key[9..];
            if active.contains(transfer) {
                continue;
            }
            if let Err(error) = self.remove_transfer(transfer) {
                let mut record: Value = self.get(&key)?;
                record["cleanup_error"] = json!(format!("{error:#}"));
                self.put(&key, &record)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Config;

    #[test]
    fn v41_failed_cleanup_retains_capacity_and_abandoned_guards_are_reapable() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let engine = Engine::open(
            temp.path(),
            Config {
                max_temporary_copies: 1,
                ..Config::default()
            },
        )?;
        let first = engine.begin_transfer("fixture", "fetch")?;
        assert_eq!(engine.db.durability_status().pending_transactions, 0);
        let path = first.path.clone();
        let id = first.id.clone();
        std::fs::write(path.join("owner"), "foreign-owner")?;
        assert!(first.finish().is_err());
        assert!(engine.transfers.active.lock().unwrap().is_empty());
        assert!(engine.begin_transfer("other", "clone").is_err());
        engine.cleanup_transfers()?;
        assert!(path.exists());
        std::fs::write(path.join("owner"), id)?;
        engine.cleanup_transfers()?;
        assert!(!path.exists());
        let abandoned = engine.begin_transfer("fixture", "clone")?;
        let path = abandoned.path.clone();
        drop(abandoned);
        assert!(path.exists());
        let next = engine.begin_transfer("fixture", "fetch")?;
        assert!(!path.exists());
        next.finish()?;
        assert!(engine.scan("transfer/")?.is_empty());
        Ok(())
    }
}
