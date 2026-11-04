//! Buffered transactions with a non-debounced durability deadline.
//! One dedicated worker per database: never queued behind workspace tools or
//! Tokio's blocking pool. Engine memtables/journal buffers remain bounded.
use anyhow::{Result, ensure};
use fjall::{OptimisticTxDatabase, PersistMode};
use std::{
    sync::{Arc, Condvar, Mutex, OnceLock, Weak},
    thread::JoinHandle,
    time::{Duration, Instant},
};

const BUFFER_INTERVAL: Duration = Duration::from_secs(1);
const SYNC_INTERVAL: Duration = Duration::from_secs(30);
pub const DURABILITY_LIMIT: Duration = Duration::from_secs(60);
static JOURNALS: OnceLock<Mutex<Vec<Weak<Shared>>>> = OnceLock::new();

/// RAM-only snapshots. Holding a status handle cannot retain/drop a database,
/// so UI diagnostics can never inherit database shutdown IO.
pub fn statuses() -> Vec<Status> {
    let mut registry = JOURNALS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    registry.retain(|entry| entry.strong_count() > 0);
    registry
        .iter()
        .filter_map(Weak::upgrade)
        .map(|shared| status(&shared))
        .collect()
}

#[derive(Default)]
struct State {
    committed: u64,
    buffered: u64,
    durable: u64,
    oldest: Option<Instant>,
    last_buffer: Option<Instant>,
    failed: bool,
    stop: bool,
}
impl State {
    fn committed(&mut self, now: Instant) {
        self.committed += 1;
        self.oldest.get_or_insert(now);
    }
    fn completed(&mut self, generation: u64, started: Instant, mode: PersistMode) {
        self.buffered = self.buffered.max(generation);
        self.last_buffer = Some(Instant::now());
        if mode == PersistMode::SyncAll {
            self.durable = self.durable.max(generation);
            // Writes arriving during IO belong to the next epoch. Do not lose
            // their dirty state or give them a later-than-arrival deadline.
            self.oldest = (self.committed > self.durable).then_some(started);
        }
    }
    fn due(&self, now: Instant, buffer: Duration, sync: Duration) -> Option<PersistMode> {
        let oldest = self.oldest?;
        if now.duration_since(oldest) >= sync {
            Some(PersistMode::SyncAll)
        } else if self.buffered < self.committed
            && now.duration_since(self.last_buffer.unwrap_or(oldest).max(oldest)) >= buffer
        {
            Some(PersistMode::Buffer)
        } else {
            None
        }
    }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    // Serialize flush bookkeeping, not transaction preparation or commits.
    sync: Mutex<()>,
}

#[derive(Debug, serde::Serialize)]
pub struct Status {
    pub pending_transactions: u64,
    pub oldest_pending_ms: u64,
    pub failed: bool,
    pub overdue: bool,
}

pub(super) struct Journal {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
}
impl Journal {
    pub fn start(db: OptimisticTxDatabase) -> Result<Self> {
        Self::start_with_intervals(db, BUFFER_INTERVAL, SYNC_INTERVAL)
    }
    fn start_with_intervals(
        db: OptimisticTxDatabase,
        buffer: Duration,
        sync: Duration,
    ) -> Result<Self> {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Condvar::new(),
            sync: Mutex::new(()),
        });
        let background = shared.clone();
        {
            let mut registry = JOURNALS
                .get_or_init(Default::default)
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            registry.retain(|entry| entry.strong_count() > 0);
            registry.push(Arc::downgrade(&shared));
        }
        let worker = std::thread::Builder::new()
            .name("vwork-journal".into())
            .spawn(move || {
                loop {
                    let mut state = background.state.lock().unwrap_or_else(|e| e.into_inner());
                    if state.stop {
                        break;
                    }
                    if state.failed {
                        drop(
                            background
                                .wake
                                .wait(state)
                                .unwrap_or_else(|e| e.into_inner()),
                        );
                        continue;
                    }
                    let mode = state.due(Instant::now(), buffer, sync);
                    if let Some(mode) = mode {
                        drop(state);
                        let _ = persist(&background, mode, || db.persist(mode));
                    } else {
                        // Idle databases sleep until a write; busy ones check
                        // at the buffer cadence, never resetting on new writes.
                        if state.oldest.is_none() {
                            state = background
                                .wake
                                .wait(state)
                                .unwrap_or_else(|e| e.into_inner());
                        } else {
                            state = background
                                .wake
                                .wait_timeout(state, buffer.min(sync))
                                .unwrap_or_else(|e| e.into_inner())
                                .0;
                        }
                        drop(state);
                    }
                }
            })?;
        Ok(Self {
            shared,
            worker: Some(worker),
        })
    }
    pub fn status(&self) -> Status {
        status(&self.shared)
    }
    pub fn check_write(&self) -> Result<()> {
        let status = self.status();
        ensure!(
            !status.failed,
            "Workspace journal sync failed; pending changes are not confirmed saved. Preserve the running application and resolve the storage error"
        );
        ensure!(
            !status.overdue,
            "Workspace journal has not reached permanent storage within 60 seconds; further writes are paused until syncing completes"
        );
        Ok(())
    }
    pub fn committed(&self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .committed(Instant::now());
        self.shared.wake.notify_one();
    }
    pub fn persist(&self, db: &OptimisticTxDatabase) -> Result<()> {
        persist(&self.shared, PersistMode::SyncAll, || {
            db.persist(PersistMode::SyncAll)
        })
    }
}

fn status(shared: &Shared) -> Status {
    let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let age = state.oldest.map(|t| t.elapsed()).unwrap_or_default();
    Status {
        pending_transactions: state.committed - state.durable,
        oldest_pending_ms: age.as_millis().min(u64::MAX as u128) as u64,
        failed: state.failed,
        overdue: age >= DURABILITY_LIMIT,
    }
}
impl Drop for Journal {
    fn drop(&mut self) {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stop = true;
        self.shared.wake.notify_one();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn persist<E: std::error::Error + Send + Sync + 'static>(
    shared: &Shared,
    mode: PersistMode,
    write: impl FnOnce() -> std::result::Result<(), E>,
) -> Result<()> {
    let _sync = shared.sync.lock().unwrap_or_else(|e| e.into_inner());
    let (generation, started) = {
        let state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.committed, Instant::now())
    };
    let result = write();
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let result = match result {
        Ok(()) => {
            state.completed(generation, started, mode);
            state.failed = false;
            Ok(())
        }
        Err(error) => {
            state.failed = true;
            Err(anyhow::anyhow!(error)
                .context("Workspace journal sync failed; changes are not confirmed saved"))
        }
    };
    drop(state);
    shared.wake.notify_one();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn v37_continuous_writes_do_not_debounce_sync_and_racing_writes_stay_dirty() {
        let start = Instant::now();
        let mut state = State::default();
        state.committed(start);
        for second in 1..30 {
            state.committed(start + Duration::from_secs(second));
        }
        assert_eq!(
            state.due(start + SYNC_INTERVAL, BUFFER_INTERVAL, SYNC_INTERVAL),
            Some(PersistMode::SyncAll)
        );
        state.committed(start + SYNC_INTERVAL);
        state.completed(30, start + SYNC_INTERVAL, PersistMode::SyncAll);
        assert_eq!(state.committed - state.durable, 1);
        assert_eq!(state.oldest, Some(start + SYNC_INTERVAL));
        state.completed(31, start + SYNC_INTERVAL, PersistMode::SyncAll);
        assert!(state.oldest.is_none());
    }
    #[test]
    fn v37_sync_failure_keeps_pending_data_and_rejects_new_writes() {
        let temp = tempfile::tempdir().unwrap();
        let db = OptimisticTxDatabase::builder(temp.path()).open().unwrap();
        let journal = Journal::start(db.clone()).unwrap();
        journal.committed();
        let result = persist(&journal.shared, PersistMode::SyncAll, || {
            Err(std::io::Error::other("fixture"))
        });
        assert!(result.is_err());
        assert_eq!(journal.status().pending_transactions, 1);
        assert!(journal.check_write().is_err());
        journal.persist(&db).unwrap();
        assert_eq!(journal.status().pending_transactions, 0);
        assert!(journal.check_write().is_ok());
        journal.committed();
        journal.shared.state.lock().unwrap().oldest = Some(Instant::now() - DURABILITY_LIMIT);
        assert!(journal.status().overdue);
        assert!(journal.check_write().is_err());
        journal.persist(&db).unwrap();
        assert!(journal.check_write().is_ok());
    }
    #[test]
    fn v37_idle_worker_syncs_one_write_without_another_operation() {
        let temp = tempfile::tempdir().unwrap();
        let db = OptimisticTxDatabase::builder(temp.path())
            .manual_journal_persist(true)
            .open()
            .unwrap();
        let journal = Journal::start_with_intervals(
            db.clone(),
            Duration::from_millis(10),
            Duration::from_millis(50),
        )
        .unwrap();
        let table = db.keyspace("test", Default::default).unwrap();
        let mut tx = db.write_tx().unwrap().durability(None);
        tx.insert(&table, "key", "value");
        tx.commit().unwrap().unwrap();
        journal.committed();
        let start = Instant::now();
        while journal.status().pending_transactions > 0 {
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!journal.status().failed);
    }
}
