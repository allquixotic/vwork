//! One maintenance gate covers RPCs, the lease reaper and compaction.
use crate::{
    engine::Engine,
    model::{Config, now},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub struct Service {
    engine: RwLock<Engine>,
    status: Mutex<Value>,
    config: Config,
    failed: AtomicBool,
}
impl Service {
    pub fn open(home: &Path, config: Config) -> Result<Self> {
        Ok(Self {
            engine: RwLock::new(Engine::open(home, config.clone())?),
            status: Mutex::new(json!({"state":"ready"})),
            config,
            failed: AtomicBool::new(false),
        })
    }
    pub fn call(&self, op: &str, args: &Value) -> Result<Value> {
        if op == "maintenance_status" {
            return Ok(self
                .status
                .lock()
                .map_err(|_| anyhow::anyhow!("maintenance status lock poisoned"))?
                .clone());
        }
        if self.failed.load(Ordering::SeqCst) {
            bail!(
                "database maintenance failed; restart vwork to run recovery before further operations"
            );
        }
        if op == "database_compact" {
            return self.compact();
        }
        let engine = self.engine.try_read().map_err(|_| {
            anyhow::anyhow!("database maintenance is in progress; retry after it finishes")
        })?;
        if self.failed.load(Ordering::SeqCst) {
            bail!("database maintenance failed; restart vwork for recovery");
        }
        engine.call(op, args)
    }
    pub fn reap_temporary(&self) -> Result<()> {
        if self.failed.load(Ordering::SeqCst) {
            return Ok(());
        }
        if let Ok(engine) = self.engine.try_read()
            && !self.failed.load(Ordering::SeqCst)
        {
            engine.reap_temporary()?;
        }
        Ok(())
    }
    pub fn compact(&self) -> Result<Value> {
        if self.failed.load(Ordering::SeqCst) {
            bail!("database maintenance failed; restart vwork for recovery");
        }
        let Ok(mut engine) = self.engine.try_write() else {
            return Ok(
                json!({"compacted":false,"reason":"busy","message":"A vwork operation is active; retry after it finishes"}),
            );
        };
        if self.failed.load(Ordering::SeqCst) {
            bail!("database maintenance failed; restart vwork for recovery");
        }
        *self
            .status
            .lock()
            .map_err(|_| anyhow::anyhow!("maintenance status lock poisoned"))? =
            json!({"state":"compacting","started":now()});
        let result = engine.compact_database();
        if result.is_err() {
            self.failed.store(true, Ordering::SeqCst);
        }
        *self
            .status
            .lock()
            .map_err(|_| anyhow::anyhow!("maintenance status lock poisoned"))? = match &result {
            Ok(value) => json!({"state":"ready","finished":now(),"result":value}),
            Err(error) => json!({"state":"error","finished":now(),"error":format!("{error:#}")}),
        };
        result
    }
    pub fn automatic_check(&self, idle: Option<Duration>) -> Result<Value> {
        if !self.config.auto_compact {
            return Ok(json!({"compacted":false,"reason":"disabled"}));
        }
        let Some(idle) = idle else {
            return Ok(json!({"compacted":false,"reason":"idle_unavailable"}));
        };
        if idle <= Duration::from_secs(self.config.compact_idle_seconds) {
            return Ok(json!({"compacted":false,"reason":"user_active"}));
        }
        self.compact()
    }
}

pub struct Schedule {
    next: Instant,
    interval: Duration,
}
impl Schedule {
    pub fn new(now: Instant, seconds: u64) -> Self {
        let interval = Duration::from_secs(seconds);
        Self {
            next: now + interval,
            interval,
        }
    }
    pub fn due(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + self.interval;
        true
    }
}

#[cfg(target_os = "macos")]
pub fn idle_time() -> Option<Duration> {
    system_idle_time::get_idle_time().ok()
}
#[cfg(windows)]
pub fn idle_time() -> Option<Duration> {
    use windows_sys::Win32::{
        System::SystemInformation::GetTickCount,
        UI::Input::KeyboardAndMouse::{GetLastInputInfo, LASTINPUTINFO},
    };
    let mut input = LASTINPUTINFO {
        cbSize: std::mem::size_of::<LASTINPUTINFO>() as u32,
        dwTime: 0,
    };
    // Both API timestamps are wrapping 32-bit milliseconds since boot.
    unsafe {
        if GetLastInputInfo(&mut input) == 0 {
            return None;
        }
        Some(Duration::from_millis(
            GetTickCount().wrapping_sub(input.dwTime) as u64,
        ))
    }
}
#[cfg(not(any(windows, target_os = "macos")))]
pub fn idle_time() -> Option<Duration> {
    None
}

impl Engine {
    pub fn compact_database(&mut self) -> Result<Value> {
        let path = self.home.join("workspaces.redb");
        let before = std::fs::metadata(&path)?.len();
        let threshold = self.config.compact_min_reclaim_bytes;
        // No need even to traverse table statistics when the entire file is smaller.
        if before <= threshold {
            return Ok(
                json!({"compacted":false,"reason":"below_threshold","before_bytes":before,"after_bytes":before,"reclaimed_bytes":0,"threshold_bytes":threshold}),
            );
        }
        self.call("gc", &json!({}))?;
        let current_size = std::fs::metadata(&path)?.len();
        let transaction = self.db.begin_write()?;
        let stats = transaction.stats()?;
        // Allocated pages include live data, metadata and pages pinned by commits.
        // Subtract a conservative header/region allowance; do not mistake internal
        // fragmentation or compressed-vs-logical bytes for removable free pages.
        let allocated = stats
            .allocated_pages()
            .saturating_mul(stats.page_size() as u64);
        let overhead = (current_size / (1024 * 1024 * 1024) + 1).saturating_mul(1024 * 1024);
        let estimated = current_size
            .saturating_sub(allocated)
            .saturating_sub(overhead);
        transaction.abort()?;
        if estimated <= threshold {
            return Ok(
                json!({"compacted":false,"reason":"below_threshold","before_bytes":before,"after_bytes":current_size,"estimated_reclaimable_bytes":estimated,"threshold_bytes":threshold,"reclaimed_bytes":before.saturating_sub(current_size)}),
            );
        }
        let db =
            Arc::get_mut(&mut self.db).context("database readers remain; compaction deferred")?;
        let changed = db.compact().context(
            "compaction failed; committed data retained; restart to run recovery before retrying",
        )?;
        if !db
            .check_integrity()
            .context("post-compaction integrity check failed; stop and inspect database recovery")?
        {
            bail!("post-compaction check required repair; restart to verify before continuing");
        }
        let after = std::fs::metadata(path)?.len();
        Ok(
            json!({"compacted":changed,"reason":if changed {"compacted"} else {"already_compact"},"before_bytes":before,"after_bytes":after,"estimated_reclaimable_bytes":estimated,"reclaimed_bytes":before.saturating_sub(after),"threshold_bytes":threshold,"integrity_checked":true}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redb::{ReadableDatabase, ReadableTableMetadata, TableDefinition};
    const DATA: TableDefinition<u64, &[u8]> = TableDefinition::new("maintenance_fixture");

    #[test]
    fn v24_schedule_idle_and_busy() -> Result<()> {
        let now = Instant::now();
        let mut schedule = Schedule::new(now, 3600);
        assert!(!schedule.due(now + Duration::from_secs(3599)));
        assert!(schedule.due(now + Duration::from_secs(3600)));
        assert!(!schedule.due(now + Duration::from_secs(3600)));
        assert!(schedule.due(now + Duration::from_secs(7200)));
        let home = tempfile::tempdir()?;
        let mut service = Service::open(home.path(), Config::default())?;
        assert_eq!(service.automatic_check(None)?["reason"], "idle_unavailable");
        assert_eq!(
            service.automatic_check(Some(Duration::from_secs(300)))?["reason"],
            "user_active"
        );
        let job = service.engine.read().unwrap();
        assert_eq!(
            service.automatic_check(Some(Duration::from_secs(301)))?["reason"],
            "busy"
        );
        drop(job);
        assert_eq!(
            service.automatic_check(Some(Duration::from_secs(301)))?["reason"],
            "below_threshold"
        );
        service.config.auto_compact = false;
        assert_eq!(
            service.automatic_check(Some(Duration::from_secs(301)))?["reason"],
            "disabled"
        );
        service.failed.store(true, Ordering::SeqCst);
        assert!(service.call("health", &json!({})).is_err());
        assert!(service.compact().is_err());
        Ok(())
    }

    #[test]
    fn v24_compact_reclaims_holes_and_preserves_live_data() -> Result<()> {
        let home = tempfile::tempdir()?;
        let service = Service::open(
            home.path(),
            Config {
                compact_min_reclaim_bytes: 1_000_000,
                ..Config::default()
            },
        )?;
        {
            let engine = service.engine.read().unwrap();
            let tx = engine.db.begin_write()?;
            {
                let mut table = tx.open_table(DATA)?;
                let value = vec![42; 64 * 1024];
                for i in 0..512 {
                    table.insert(i, value.as_slice())?;
                }
            }
            tx.commit()?;
            let tx = engine.db.begin_write()?;
            {
                let mut table = tx.open_table(DATA)?;
                for i in (0..512).step_by(2) {
                    table.remove(i)?;
                }
            }
            tx.commit()?;
        }
        let result = service.compact()?;
        assert_eq!(result["compacted"], true, "{result}");
        assert!(
            result["reclaimed_bytes"].as_u64().unwrap() > 1_000_000,
            "{result}"
        );
        assert_eq!(result["integrity_checked"], true);
        drop(service);
        let engine = Engine::open(home.path(), Config::default())?;
        let tx = engine.db.begin_read()?;
        let table = tx.open_table(DATA)?;
        assert_eq!(table.len()?, 256);
        for i in 0..512 {
            let value = table.get(i)?;
            if i % 2 == 0 {
                assert!(value.is_none());
            } else {
                assert_eq!(value.unwrap().value(), vec![42; 64 * 1024]);
            }
        }
        Ok(())
    }
}
