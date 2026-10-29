//! Startup validation and bounded recovery evidence. Never create over an existing file.
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use redb::{Database, DatabaseError};
use serde_json::{Value, json};
use std::path::Path;

/// Closing a file does not release flock while a fork/duplicate still holds
/// its open description. Release explicitly at handoff and on error paths.
struct ExclusiveLock<'a>(Option<&'a std::fs::File>);

impl<'a> ExclusiveLock<'a> {
    fn acquire(file: &'a std::fs::File) -> std::io::Result<Self> {
        file.try_lock_exclusive()?;
        Ok(Self(Some(file)))
    }

    fn release(mut self) -> std::io::Result<()> {
        FileExt::unlock(self.0.take().unwrap())
    }
}

impl Drop for ExclusiveLock<'_> {
    fn drop(&mut self) {
        if let Some(file) = self.0 {
            let _ = FileExt::unlock(file);
        }
    }
}

fn snapshot(path: &Path) -> Result<std::path::PathBuf> {
    let backup = path.with_extension("redb.recovery.bak");
    let partial = path.with_extension("redb.recovery.partial");
    crate::engine::safe_ancestors(&backup)?;
    crate::engine::safe_ancestors(&partial)?;
    // Copy through the handle holding the lock. Reopening the same file for
    // std::fs::copy is rejected by Windows byte-range locking.
    let input = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)?;
    let lock = ExclusiveLock::acquire(&input).context("database is owned by another process")?;
    let mut output = std::fs::File::create(&partial)?;
    std::io::copy(&mut &input, &mut output).context("copy database before integrity checking")?;
    output.sync_all()?;
    drop(output);
    #[cfg(unix)]
    std::fs::File::open(path.parent().context("database has no parent")?)?.sync_all()?;
    lock.release().context("release database snapshot lock")?;
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
    let _startup =
        ExclusiveLock::acquire(&lock).context("database startup/recovery already in progress")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v29_handoff_releases_lock_while_duplicate_survives() -> Result<()> {
        let temp = tempfile::tempdir()?;
        for explicit in [false, true] {
            let path = temp.path().join(format!("guard-{explicit}"));
            let file = std::fs::File::create(&path)?;
            let guard = ExclusiveLock::acquire(&file)?;
            // Deterministically model a descriptor inherited between fork and
            // exec, including the CLOEXEC case before exec closes it.
            let inherited = file.try_clone()?;
            let observer = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)?;
            assert!(observer.try_lock_exclusive().is_err());
            if explicit {
                guard.release()?;
            } else {
                drop(guard); // Early return/error cleanup uses the same path.
            }
            drop(file);
            observer
                .try_lock_exclusive()
                .context("handoff must not wait for child descriptor close")?;
            FileExt::unlock(&observer)?;
            drop(inherited);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn v29_close_alone_retains_inherited_lock() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let path = temp.path().join("inherited");
        let file = std::fs::File::create(&path)?;
        file.try_lock_exclusive()?;
        let inherited = file.try_clone()?;
        drop(file);
        let observer = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)?;
        assert!(
            observer.try_lock_exclusive().is_err(),
            "control must reproduce the old close-only behavior"
        );
        FileExt::unlock(&inherited)?;
        observer.try_lock_exclusive()?;
        Ok(())
    }
}
