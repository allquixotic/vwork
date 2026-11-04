//! One-time, resumable redb reader. Zstd decoding lives only at this boundary.
//! The original file is never repaired, rewritten, renamed or deleted.
use crate::{git_store, kv::Database};
use anyhow::{Context, Result, ensure};
use fjall::Readable;
use fs2::FileExt;
use redb::{ReadableDatabase, ReadableTableMetadata, TableHandle};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

pub const DIRECTORY: &str = "workspaces.fjall";
const STATE: &str = "migration_state_v1";
const TABLES: &[&str] = &[
    "records_v1",
    "git_objects_v1",
    "git_members_v1",
    "fastrock_search_current_v1",
    "fastrock_search_requested_v1",
    "fastrock_memory_state_v1",
    "fastrock_memory_forgotten_v1",
    "fastrock_project_settings_v1",
    "fastrock_workspace_references_v1",
    "fastrock_checkpoints_v1",
    "fastrock_restore_journals_v1",
    "fastrock_restore_pending_v1",
];

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub source: PathBuf,
    pub destination: PathBuf,
    pub phase: String,
    pub completed: u64,
    pub total: u64,
    pub active: bool,
    pub error: Option<String>,
}
static PROGRESS: OnceLock<Mutex<BTreeMap<PathBuf, Progress>>> = OnceLock::new();
static CANCEL: OnceLock<Mutex<std::collections::BTreeSet<PathBuf>>> = OnceLock::new();
type Observer = Box<dyn Fn(Progress) -> bool + Send + Sync>;
static OBSERVERS: OnceLock<Mutex<Vec<Observer>>> = OnceLock::new();
/// Return false from the callback when its window has closed.
pub fn observe(observer: impl Fn(Progress) -> bool + Send + Sync + 'static) {
    OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(Box::new(observer));
}
pub fn progress() -> Vec<Progress> {
    PROGRESS
        .get()
        .and_then(|p| p.try_lock().ok())
        .map(|p| p.values().cloned().collect())
        .unwrap_or_default()
}
pub fn cancel(destination: &Path) {
    CANCEL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(destination.into());
}
fn publish(p: &Progress) {
    PROGRESS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(p.destination.clone(), p.clone());
    OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|callback| callback(p.clone()));
}
pub fn paused(destination: &Path) -> bool {
    CANCEL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .contains(destination)
}
fn check_cancel(path: &Path) -> Result<()> {
    ensure!(
        !paused(path),
        "Migration paused; original retained. Reopen or use Migrate legacy storage to resume."
    );
    Ok(())
}
struct Lock(std::fs::File);
impl Drop for Lock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
fn lock(home: &Path) -> Result<Lock> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(home.join("storage-migration.lock"))?;
    file.try_lock_exclusive()
        .context("Storage migration is already running")?;
    Ok(Lock(file))
}
enum Legacy {
    ReadOnly(redb::ReadOnlyDatabase),
    Recovered {
        db: redb::Database,
        _source_lock: Lock,
    },
}
impl Legacy {
    fn read(&self) -> Result<redb::ReadTransaction> {
        Ok(match self {
            Self::ReadOnly(db) => db.begin_read()?,
            Self::Recovered { db, .. } => db.begin_read()?,
        })
    }
}
fn open_source(source: &Path, staging: &Path) -> Result<Legacy> {
    match redb::Database::builder()
        .set_cache_size(32 * 1024 * 1024)
        .open_read_only(source)
    {
        Ok(db) => Ok(Legacy::ReadOnly(db)),
        Err(redb::DatabaseError::DatabaseAlreadyOpen) => anyhow::bail!(
            "Workspace database is already in use by another running instance. Quit its tray owner before migrating; the original database is unchanged."
        ),
        Err(redb::DatabaseError::RepairAborted) => {
            // Repair only a private copy. Lock the source during the copy;
            // never copy bytes while another process may be changing them.
            let input = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(source)?;
            input
                .try_lock_exclusive()
                .context("Legacy database is in use")?;
            let guard = Lock(input);
            let copy = staging.join("legacy-recovery.redb");
            let mut output = std::fs::File::create(&copy)?;
            std::io::copy(&mut &guard.0, &mut output)?;
            output.sync_all()?;
            drop(output);
            let mut db = redb::Database::builder()
                .set_cache_size(32 * 1024 * 1024)
                .open(&copy)
                .context("Legacy database recovery failed; original preserved")?;
            ensure!(
                db.check_integrity()?,
                "Legacy recovery did not pass integrity validation"
            );
            Ok(Legacy::Recovered {
                db,
                _source_lock: guard,
            })
        }
        Err(e) => Err(e).context("Cannot read legacy database; original preserved"),
    }
}
fn decompress(bytes: &[u8], limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    zstd::stream::Decoder::new(bytes)?
        .take(limit as u64 + 1)
        .read_to_end(&mut out)?;
    ensure!(out.len() <= limit, "Legacy record exceeds migration limit");
    Ok(out)
}
fn convert(table: &str, key: &str, value: &[u8]) -> Result<Vec<u8>> {
    if table == "git_objects_v1" {
        ensure!(value.len() >= 9, "Truncated legacy Git object");
        let kind = git2::ObjectType::from_raw(value[0] as libgit2_sys::git_object_t)
            .context("Invalid legacy Git object type")?;
        let len = usize::try_from(u64::from_le_bytes(value[1..9].try_into()?))?;
        ensure!(
            len <= crate::git_delta::MAX_OBJECT_BYTES,
            "Legacy Git object exceeds migration limit"
        );
        let bytes = decompress(&value[9..], len)?;
        ensure!(
            bytes.len() == len && git2::Oid::hash_object(kind, &bytes)?.to_string() == key,
            "Legacy Git object integrity failure"
        );
        return Ok(git_store::raw_record(kind, &bytes));
    }
    if table == "records_v1" && key.starts_with("b/") {
        let bytes = decompress(value, crate::git_delta::MAX_OBJECT_BYTES)?;
        ensure!(
            blake3::hash(&bytes).to_hex().as_str() == &key[2..],
            "Legacy blob integrity failure"
        );
        return Ok(bytes);
    }
    let limit = match table {
        "fastrock_search_current_v1" => Some(8 * 1024 * 1024),
        "fastrock_memory_state_v1" => Some(8 * 1024 * 1024),
        "fastrock_checkpoints_v1" | "fastrock_restore_journals_v1" => Some(256 * 1024 * 1024),
        _ => None,
    };
    if let Some(limit) = limit {
        let bytes = decompress(value, limit)?;
        serde_json::from_slice::<serde_json::Value>(&bytes)
            .context("Invalid legacy JSON record")?;
        return Ok(bytes);
    }
    Ok(value.to_vec())
}
fn visit(
    tx: &redb::ReadTransaction,
    table: &str,
    after: Option<&str>,
    mut f: impl FnMut(&str, &[u8]) -> Result<()>,
) -> Result<()> {
    macro_rules! rows {
        ($ty:ty,$convert:expr) => {{
            let t = tx.open_table(redb::TableDefinition::<&str, $ty>::new(table))?;
            let start = after.unwrap_or("");
            for row in t.range(start..)? {
                let (k, v) = row?;
                if after == Some(k.value()) {
                    continue;
                }
                let bytes = ($convert)(v.value());
                f(k.value(), &bytes)?;
            }
        }};
    }
    match table {
        "fastrock_search_requested_v1" => rows!(u64, |v: u64| v.to_be_bytes().to_vec()),
        "fastrock_memory_forgotten_v1" => rows!(bool, |v: bool| vec![u8::from(v)]),
        "fastrock_workspace_references_v1" | "fastrock_restore_pending_v1" => {
            rows!(&str, |v: &str| v.as_bytes().to_vec())
        }
        _ => rows!(&[u8], |v: &[u8]| v.to_vec()),
    }
    Ok(())
}

/// Import into a new home, or resume its incomplete migration. Never replace
/// an existing live destination. The GUI calls this on a blocking worker.
pub fn migrate(source: &Path, home: &Path) -> Result<Progress> {
    crate::engine::safe_ancestors(source)?;
    crate::engine::safe_ancestors(home)?;
    ensure!(source.is_file(), "Select an existing redb file");
    std::fs::create_dir_all(home)?;
    let source = source.canonicalize()?;
    let home = home.canonicalize()?;
    let destination = home.join(DIRECTORY);
    ensure!(
        !destination.exists(),
        "Destination already contains a live store; choose an unused folder. Existing work was preserved."
    );
    let _lock = lock(&home)?;
    let mut p = Progress {
        source: source.clone(),
        destination: destination.clone(),
        phase: "Preparing migration".into(),
        active: true,
        ..Default::default()
    };
    CANCEL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&destination);
    publish(&p);
    let result = migrate_inner(&source, &home, &mut p);
    p.active = false;
    match result {
        Ok(()) => {
            p.phase = "Migration complete; original redb retained".into();
            p.completed = p.total;
        }
        Err(ref e) => {
            p.phase = "Migration stopped; original retained".into();
            p.error = Some(format!("{e:#}"));
        }
    }
    publish(&p);
    result?;
    Ok(p)
}
fn migrate_inner(source: &Path, home: &Path, p: &mut Progress) -> Result<()> {
    let staging = home.join("workspaces.fjall.migrating");
    std::fs::create_dir_all(&staging)?;
    let legacy = open_source(source, &staging)?;
    let read = legacy.read()?;
    ensure!(
        read.list_multimap_tables()?.next().is_none(),
        "Unknown legacy multimap table; migration stopped without activating destination"
    );
    let mut tables = Vec::new();
    let mut counts = BTreeMap::new();
    let mut total = 0u64;
    let mut objects = 0u64;
    for handle in read.list_tables()? {
        let name = handle.name().to_owned();
        ensure!(
            TABLES.contains(&name.as_str()),
            "Unknown legacy table {name}; migration stopped without activating destination"
        );
        let count = read.open_untyped_table(handle)?.len()?;
        counts.insert(name.clone(), count);
        total += count;
        if name == "git_objects_v1" {
            objects = count;
        }
        tables.push(name);
    }
    ensure!(
        tables.iter().any(|t| t == "records_v1"),
        "Not a vwork workspace database"
    );
    // Schema/record validation precedes opening the new engine.
    let records = read.open_table(redb::TableDefinition::<&str, &[u8]>::new("records_v1"))?;
    let schema = records
        .get("schema")?
        .context("Legacy workspace schema missing")?;
    ensure!(
        [b"1".as_slice(), b"2".as_slice()].contains(&schema.value()),
        "Unsupported legacy workspace schema"
    );
    drop(schema);
    drop(records);
    let metadata = std::fs::metadata(source)?;
    let identity = serde_json::to_vec(&(
        source,
        metadata.len(),
        metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos(),
        &tables,
        total,
    ))?;
    let db = Database::open(&staging.join("data"))?;
    git_store::initialize(&db)?;
    let state = db.keyspace(STATE)?;
    if let Some(old) = state.get("source")? {
        ensure!(
            old.as_ref() == identity.as_slice(),
            "Legacy source changed since interrupted migration; choose a fresh destination folder"
        );
    } else {
        state.insert("source", identity)?;
        db.persist()?;
    }
    p.total = total.saturating_mul(2).saturating_add(objects);
    p.completed = 0;
    for table in &tables {
        check_cancel(&p.destination)?;
        p.phase = format!("Copying {table}");
        publish(p);
        let destination = db.keyspace(table)?;
        let cursor = state
            .get(format!("cursor/{table}"))?
            .map(|v| String::from_utf8(v.to_vec()))
            .transpose()?;
        let count = state
            .get(format!("count/{table}"))?
            .map(|v| v.as_ref().try_into().map(u64::from_le_bytes))
            .transpose()?
            .unwrap_or(0);
        p.completed += count;
        let mut count = count;
        let mut batch = db
            .inner
            .write_tx()?
            .durability(Some(fjall::PersistMode::SyncAll));
        let mut bytes = 0usize;
        let mut pending = 0usize;
        let mut last = String::new();
        visit(&read, table, cursor.as_deref(), |key, value| {
            check_cancel(&p.destination)?;
            let converted = convert(table, key, value)?;
            bytes += converted.len();
            pending += 1;
            count += 1;
            last = key.into();
            batch.insert(&destination, key.as_bytes(), converted);
            if pending >= 128 || bytes >= 16 * 1024 * 1024 {
                batch.insert(&state, format!("cursor/{table}"), last.as_bytes());
                batch.insert(
                    &state,
                    format!("count/{table}"),
                    count.to_le_bytes().as_slice(),
                );
                let next = db
                    .inner
                    .write_tx()?
                    .durability(Some(fjall::PersistMode::SyncAll));
                std::mem::replace(&mut batch, next)
                    .commit()?
                    .map_err(|_| crate::kv::Conflict)?;
                p.completed += pending as u64;
                publish(p);
                pending = 0;
                bytes = 0;
            }
            Ok(())
        })?;
        if pending > 0 {
            batch.insert(&state, format!("cursor/{table}"), last.as_bytes());
            batch.insert(
                &state,
                format!("count/{table}"),
                count.to_le_bytes().as_slice(),
            );
            batch.commit()?.map_err(|_| crate::kv::Conflict)?;
            p.completed += pending as u64;
            publish(p);
        }
    }
    p.phase = "Optimizing Git delta storage".into();
    publish(p);
    git_store::repack(&db, || {
        check_cancel(&p.destination)?;
        p.completed += 1;
        if p.completed.is_multiple_of(128) {
            publish(p);
        }
        Ok(())
    })?;
    // Compare every logical record, including data copied before a crash.
    // This bounds memory and catches changed sources or incomplete destinations.
    for table in &tables {
        p.phase = format!("Verifying {table}");
        publish(p);
        let destination = db.keyspace(table)?;
        let actual_count =
            db.inner
                .read_tx()
                .iter(&destination)
                .try_fold(0u64, |n, row| -> Result<u64> {
                    row.key()?;
                    Ok(n + 1)
                })?;
        ensure!(
            actual_count == counts[table],
            "Migrated record count mismatch in {table}"
        );
        visit(&read, table, None, |key, value| {
            check_cancel(&p.destination)?;
            let expected = convert(table, key, value)?;
            if table == "git_objects_v1" {
                let (kind, bytes) = git_store::read(&db, git2::Oid::from_str(key)?)?;
                ensure!(
                    git_store::raw_record(kind, &bytes) == expected,
                    "Migrated Git object mismatch"
                );
            } else {
                let actual = destination.get(key)?.context("Migrated record missing")?;
                ensure!(
                    actual.as_ref() == expected.as_slice(),
                    "Migrated record mismatch in {table}"
                );
            }
            p.completed += 1;
            if p.completed.is_multiple_of(128) {
                publish(p);
            }
            Ok(())
        })?;
    }
    let final_metadata = std::fs::metadata(source)?;
    ensure!(
        metadata.len() == final_metadata.len()
            && metadata.modified()? == final_metadata.modified()?,
        "Legacy source changed during migration; original and staging retained"
    );
    p.phase = "Finalizing storage".into();
    publish(p);
    state.insert("complete", b"1")?;
    db.persist()?;
    drop(state);
    drop(db);
    drop(read);
    drop(legacy);
    check_cancel(&p.destination)?;
    ensure!(
        !p.destination.exists(),
        "Destination appeared during migration; preserved both stores"
    );
    std::fs::rename(staging.join("data"), &p.destination)
        .context("Activate verified Fjall store")?;
    #[cfg(unix)]
    std::fs::File::open(home)?.sync_all()?;
    // Staging may contain the verified recovery copy. Keep it as evidence.
    Ok(())
}
