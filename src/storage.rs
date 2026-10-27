//! Startup validation and bounded recovery evidence. Never create over an existing file.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use redb::{Database, DatabaseError};
use serde_json::{Value, json};
use std::path::Path;

fn snapshot(path: &Path) -> Result<std::path::PathBuf> {
    let backup = path.with_extension("redb.recovery.bak");
    let partial = path.with_extension("redb.recovery.partial");
    crate::engine::safe_ancestors(&backup)?;
    crate::engine::safe_ancestors(&partial)?;
    // Copy through the handle holding the lock. Reopening the same file for
    // std::fs::copy is rejected by Windows byte-range locking.
    let mut input = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    input
        .try_lock_exclusive()
        .context("database is owned by another process")?;
    let mut output = std::fs::File::create(&partial)?;
    std::io::copy(&mut input, &mut output).context("copy database before integrity checking")?;
    output.sync_all()?;
    drop(output);
    #[cfg(unix)]
    std::fs::File::open(path.parent().context("database has no parent")?)?.sync_all()?;
    Ok(partial)
}

pub fn open(path: &Path) -> Result<(Database, Value)> {
    crate::engine::safe_ancestors(path)?;
    let lock_path = path.with_extension("redb.recovery.lock");
    crate::engine::safe_ancestors(&lock_path)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    lock.try_lock_exclusive()
        .context("database startup/recovery already in progress")?;
    if !path.exists() {
        return Ok((
            Database::create(path)?,
            json!({"state":"new","integrity_checked":true}),
        ));
    }
    ensure!(
        path.is_file() && std::fs::metadata(path)?.len() > 0,
        "existing database is empty or not a regular file; preserved {}; restore a verified backup",
        path.display()
    );

    let backup = snapshot(path)?;
    // The startup guard serializes vwork openers across the snapshot/open handoff.
    // Probe while preventing redb from repairing anything before a snapshot exists.
    // Successful open holds redb's exclusive ownership throughout the check.
    let mut probe = Database::builder();
    probe.set_repair_callback(|session| session.abort());
    let (mut db, backup, dirty) = match probe.open(path) {
        Ok(db) => (db, backup, false),
        Err(DatabaseError::RepairAborted) => {
            eprintln!("Database was not cleanly closed; recovering from committed redb state");
            let mut builder = Database::builder();
            builder.set_repair_callback(|session| eprintln!("Database recovery: {:.0}%", session.progress() * 100.0));
            let db = builder.open(path).with_context(|| format!(
                "database repair failed; original retained at {}, recovery snapshot at {}; restore a verified backup",
                path.display(), backup.display()))?;
            (db, backup, true)
        }
        Err(error) => return Err(error).with_context(|| format!(
            "database could not be opened; preserved {}; no replacement database was created; restore a verified backup",
            path.display())),
    };
    eprintln!("Checking database integrity before serving requests...");
    let clean = db.check_integrity().with_context(|| format!(
        "database integrity check failed; service not started; original retained at {}, pre-check snapshot at {}; restore a verified backup",
        path.display(), backup.display()))?;
    let recovered = dirty || !clean;
    let previous = path.with_extension("redb.recovery.bak");
    let backup = if recovered {
        // Replace older evidence only after successful verification. Failed
        // startup retains the old backup plus this attempt's partial snapshot.
        std::fs::rename(&backup, &previous).context("publish verified recovery snapshot")?;
        #[cfg(unix)]
        std::fs::File::open(path.parent().context("database has no parent")?)?.sync_all()?;
        previous
    } else {
        std::fs::remove_file(&backup).context("remove verified startup snapshot")?;
        if previous.exists() {
            std::fs::remove_file(&previous)?;
        }
        backup
    };
    let report = json!({"state":if recovered {"recovered"} else {"clean"},
        "integrity_checked":true,"backup":if recovered {Some(backup)} else {None}});
    eprintln!("Database integrity verified ({})", report["state"]);
    Ok((db, report))
}
