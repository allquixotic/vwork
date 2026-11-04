//! Fjall owner startup. Legacy redb is read only during one-time migration.
use crate::kv::Database;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::Path;

pub fn open(home: &Path) -> Result<(Database, Value)> {
    let path = home.join(crate::migration::DIRECTORY);
    crate::engine::safe_ancestors(&path)?;
    let legacy = home.join("workspaces.redb");
    let mut migrated = false;
    if !path.exists() && legacy.exists() {
        ensure!(
            !crate::migration::paused(&path),
            "Storage migration is paused; use Resume in the migration panel"
        );
        crate::migration::migrate(&legacy, home)?;
        migrated = true;
    }
    ensure!(
        path.exists() || !home.join("workspaces.fjall.migrating").exists(),
        "Incomplete migration has no source; preserve staging and restore the original redb before retrying"
    );
    let existed = path.exists();
    if existed {
        ensure!(
            path.is_dir() && path.join("version").is_file(),
            "Existing Fjall store is incomplete or damaged; preserved without replacement: {}",
            path.display()
        );
    }
    let db=Database::open(&path).with_context(||format!("Could not open workspace storage at {}; another running instance may own it. Existing files were preserved",path.display()))?;
    Ok((
        db,
        json!({"state":if migrated {"migrated"} else if existed {"clean"} else {"new"},"engine":"fjall","compression":"lz4","integrity_checked":migrated}),
    ))
}
