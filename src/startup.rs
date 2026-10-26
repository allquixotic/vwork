//! Shared daemon startup for the CLI and MCP bridge. No shell or stdout output.
use crate::transport;
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    path::Path,
    time::{Duration, Instant},
};

pub const START_TIMEOUT: Duration = Duration::from_secs(15);
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const POLL_INTERVAL: Duration = Duration::from_millis(50);

fn lock_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?)
}

fn try_lock(file: &File) -> Result<bool> {
    match file.try_lock_exclusive() {
        Ok(()) => Ok(true),
        Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Short probes must not inherit the day-long deadline used for build tasks.
pub fn health(home: &Path, timeout: Duration) -> Result<Value> {
    let health = transport::connect_with_timeout(home, "health", &json!({}), timeout)?;
    ensure!(
        health["schema"] == 1 && health["version"].as_str().is_some_and(|s| !s.is_empty()),
        "unexpected daemon health response"
    );
    Ok(health)
}

pub fn ensure_started(home: &Path, executable: &Path) -> Result<Value> {
    ensure_started_with_timeout(home, executable, START_TIMEOUT)
}

/// Reuse a healthy owner, or serialize startup and wait for the new owner.
/// An unresponsive owner is never killed or replaced. The daemon outlives this caller.
pub fn ensure_started_with_timeout(
    home: &Path,
    executable: &Path,
    timeout: Duration,
) -> Result<Value> {
    ensure!(!timeout.is_zero(), "startup timeout must be positive");
    let deadline = Instant::now() + timeout;
    let probe = || {
        health(
            home,
            PROBE_TIMEOUT.min(deadline.saturating_duration_since(Instant::now())),
        )
    };
    if let Ok(health) = probe() {
        return Ok(health);
    }
    std::fs::create_dir_all(home)?;
    let startup_lock = lock_file(&home.join("startup.lock"))?;
    loop {
        ensure!(
            Instant::now() < deadline,
            "daemon startup timed out waiting for another starter; inspect {}",
            home.join("daemon.log").display()
        );
        if try_lock(&startup_lock).context("lock daemon startup")? {
            break;
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
    if Instant::now() < deadline
        && let Ok(health) = probe()
    {
        return Ok(health);
    }
    // Keep the startup lock until ready so simultaneous clients reuse this start.
    // The daemon lock also protects a manually started/compacting database owner.
    let owner_lock = lock_file(&home.join("daemon.lock"))?;
    let mut child: Option<crate::background::Child> = None;
    if try_lock(&owner_lock).context("check daemon owner")? {
        transport::initialize(home)?;
        transport::load_config(home)?;
        ensure!(
            Instant::now() < deadline,
            "daemon startup timed out before launch"
        );
        FileExt::unlock(&owner_lock)?;
        child = Some(
            crate::background::spawn(home, executable)
                .context("could not start vwork daemon; start it manually and retry")?,
        );
    }
    while Instant::now() < deadline {
        if let Ok(health) = probe() {
            return Ok(health);
        }
        if let Some(process) = child.as_mut()
            && let Some(status) = process.try_wait()?
        {
            // A manual `serve` may have won the daemon lock in the spawn gap.
            // Wait for that owner if present, otherwise expose the child failure.
            if try_lock(&owner_lock)? {
                anyhow::bail!(
                    "daemon exited {status}; inspect {}",
                    home.join("daemon.log").display()
                );
            }
            child = None;
        }
        std::thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
    anyhow::bail!(
        "daemon startup timed out after {:.1}s; an existing or starting owner was left running; inspect {}",
        timeout.as_secs_f64(),
        home.join("daemon.log").display()
    )
}
