//! Storage acceptance: concurrent publication, standard Git deltas and retained
//! legacy sources. All files are isolated fixtures, never installed user data.
use anyhow::Result;
use git2::{ObjectType, Oid};
use std::{
    path::Path,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
};
use vwork::{
    engine::Engine,
    git_store,
    kv::{Database, TableDefinition},
    migration,
    model::Config,
};
const RECORDS: TableDefinition<&str, &[u8]> = TableDefinition::new("records_v1");

#[test]
fn v32_disjoint_writers_prepare_together_same_head_conflicts() -> Result<()> {
    let root = tempfile::tempdir()?;
    let db = Arc::new(Database::open(&root.path().join("data"))?);
    db.transaction(|tx| {
        let mut t = tx.open_table(RECORDS)?;
        t.insert("w/a", b"old")?;
        t.insert("w/b", b"old")
    })?;
    let barrier = Arc::new(Barrier::new(2));
    std::thread::scope(|scope| -> Result<()> {
        let workers: Vec<_> = ["w/a", "w/b"]
            .into_iter()
            .map(|key| {
                let db = db.clone();
                let barrier = barrier.clone();
                scope.spawn(move || -> Result<()> {
                    let tx = db.begin_write()?;
                    {
                        let mut t = tx.open_table(RECORDS)?;
                        assert_eq!(t.get(key)?.unwrap().value(), b"old");
                        barrier.wait();
                        t.insert(key, b"new")?;
                    }
                    tx.commit()
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap()?;
        }
        Ok(())
    })?;
    const NUMBERS: TableDefinition<u64, u64> = TableDefinition::new("numeric_order");
    db.transaction(|tx| {
        let mut table = tx.open_table(NUMBERS)?;
        for i in [1, 255, 256, 300] {
            table.insert(i, i)?;
        }
        Ok(())
    })?;
    let read = db.begin_read()?;
    let keys = read
        .open_table(NUMBERS)?
        .range(1..300)?
        .map(|row| row.map(|(key, _)| key.value()))
        .collect::<Result<Vec<_>>>()?;
    assert_eq!(keys, vec![1, 255, 256]);
    let first = db.begin_write()?;
    let second = db.begin_write()?;
    for tx in [&first, &second] {
        let mut t = tx.open_table(RECORDS)?;
        assert_eq!(t.get("w/a")?.unwrap().value(), b"new");
        t.insert("w/a", b"next")?;
    }
    first.commit()?;
    assert!(second.commit().unwrap_err().is::<vwork::kv::Conflict>());
    Ok(())
}

#[test]
fn v33_bulk_deltas_dedup_bounds_and_reopen() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("data");
    let db = Database::open(&path)?;
    git_store::initialize(&db)?;
    let mut objects = Vec::new();
    for index in 0..20 {
        let mut bytes = "fn unchanged_function() { return 42; }\n"
            .repeat(2000)
            .into_bytes();
        bytes.splice(200..204, index.to_string().bytes());
        let oid = Oid::hash_object(ObjectType::Blob, &bytes)?;
        git_store::write(&db, "repo-a", oid, ObjectType::Blob, &bytes, 0)?;
        git_store::write(&db, "repo-b", oid, ObjectType::Blob, &bytes, 0)?;
        objects.push((oid, bytes));
    }
    let tx = db.begin_read()?;
    let table = tx.open_table(git_store::OBJECTS)?;
    assert_eq!(table.len()?, 20);
    let mut deltas = 0;
    for (oid, bytes) in &objects {
        let record = table.get(oid.to_string().as_str())?.unwrap();
        assert!(record.value()[10] <= vwork::git_delta::MAX_DEPTH);
        if git_store::delta_base(record.value())?.is_some() {
            deltas += 1;
            assert!(record.value().len() < bytes.len() / 4);
        }
        assert_eq!(git_store::read(&db, *oid)?.1, *bytes);
    }
    assert!(
        deltas >= 15,
        "related objects should use bounded Git deltas: {deltas}"
    );
    drop(table);
    drop(tx);
    drop(db);
    let db = Database::open(&path)?;
    for (oid, bytes) in objects {
        assert_eq!(git_store::read(&db, oid)?.1, bytes);
    }
    Ok(())
}

fn legacy(path: &Path, objects: usize) -> Result<()> {
    let db = redb::Database::create(path)?;
    let tx = db.begin_write()?;
    {
        let mut records = tx.open_table(redb::TableDefinition::<&str, &[u8]>::new("records_v1"))?;
        records.insert("schema", b"2".as_slice())?;
        let blob = b"legacy edited content";
        records.insert(
            format!("b/{}", blake3::hash(blob).to_hex()).as_str(),
            zstd::encode_all(blob.as_slice(), 3)?.as_slice(),
        )?;
        for index in 0..300 {
            records.insert(
                format!("fixture/{index:04}").as_str(),
                b"preserved metadata".as_slice(),
            )?;
        }
    }
    {
        let mut git = tx.open_table(redb::TableDefinition::<&str, &[u8]>::new("git_objects_v1"))?;
        let mut members =
            tx.open_table(redb::TableDefinition::<&str, &[u8]>::new("git_members_v1"))?;
        for index in 0..objects {
            let bytes = format!("{index:06}{}", "similar source text\n".repeat(100));
            let oid = Oid::hash_object(ObjectType::Blob, bytes.as_bytes())?;
            let mut record = vec![3];
            record.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            record.extend(zstd::encode_all(bytes.as_bytes(), 3)?);
            git.insert(oid.to_string().as_str(), record.as_slice())?;
            members.insert(format!("fixture/{oid}").as_str(), b"".as_slice())?;
        }
    }
    for name in [
        "fastrock_search_current_v1",
        "fastrock_memory_state_v1",
        "fastrock_checkpoints_v1",
        "fastrock_restore_journals_v1",
    ] {
        tx.open_table(redb::TableDefinition::<&str, &[u8]>::new(name))?
            .insert(
                "fixture",
                zstd::encode_all(b"{\"retained\":true}".as_slice(), 3)?.as_slice(),
            )?;
    }
    tx.open_table(redb::TableDefinition::<&str, u64>::new(
        "fastrock_search_requested_v1",
    ))?
    .insert("thread", 77)?;
    tx.open_table(redb::TableDefinition::<&str, bool>::new(
        "fastrock_memory_forgotten_v1",
    ))?
    .insert("forgotten", true)?;
    for name in [
        "fastrock_workspace_references_v1",
        "fastrock_restore_pending_v1",
    ] {
        tx.open_table(redb::TableDefinition::<&str, &str>::new(name))?
            .insert("thread", "workspace")?;
    }
    tx.open_table(redb::TableDefinition::<&str, &[u8]>::new(
        "fastrock_project_settings_v1",
    ))?
    .insert("repo", b"{\"name\":\"saved\"}".as_slice())?;
    tx.commit()?;
    Ok(())
}
fn verify_migration(home: &Path, before: &[u8]) -> Result<()> {
    assert_eq!(
        std::fs::read(home.join("workspaces.redb"))?,
        before,
        "original must be byte-for-byte unchanged"
    );
    let engine = Engine::open_embedded(home, Config::default())?;
    let tx = engine.db.begin_read()?;
    assert_eq!(
        tx.open_table(TableDefinition::<&str, u64>::new(
            "fastrock_search_requested_v1"
        ))?
        .get("thread")?
        .unwrap()
        .value(),
        77
    );
    assert!(
        tx.open_table(TableDefinition::<&str, bool>::new(
            "fastrock_memory_forgotten_v1"
        ))?
        .get("forgotten")?
        .unwrap()
        .value()
    );
    for name in [
        "fastrock_search_current_v1",
        "fastrock_memory_state_v1",
        "fastrock_checkpoints_v1",
        "fastrock_restore_journals_v1",
    ] {
        assert_eq!(
            tx.open_table(TableDefinition::<&str, &[u8]>::new(name))?
                .get("fixture")?
                .unwrap()
                .value(),
            b"{\"retained\":true}"
        );
    }
    let table = tx.open_table(git_store::OBJECTS)?;
    assert!(table.iter()?.any(|r| r.unwrap().1.value()[9] == 1));
    for row in table.iter()? {
        let (key, _) = row?;
        git_store::read(&engine.db, Oid::from_str(key.value())?)?;
    }
    Ok(())
}
#[test]
fn v34_automatic_migration_preserves_source_and_reopens_without_source() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("workspaces.redb");
    legacy(&source, 20)?;
    let before = std::fs::read(&source)?;
    let engine = Engine::open_embedded(root.path(), Config::default())?;
    assert_eq!(engine.recovery["state"], "migrated");
    drop(engine);
    verify_migration(root.path(), &before)?;
    std::fs::rename(&source, root.path().join("preserved.redb"))?;
    let engine = Engine::open_embedded(root.path(), Config::default())?;
    assert_eq!(engine.recovery["state"], "clean");
    Ok(())
}
#[test]
fn v34_copy_and_delta_repack_resume_after_cancellation() -> Result<()> {
    for phase in ["Copying records_v1", "Optimizing Git delta storage"] {
        let root = tempfile::tempdir()?;
        let source = root.path().join("workspaces.redb");
        legacy(&source, 300)?;
        let before = std::fs::read(&source)?;
        let dest = root.path().canonicalize()?.join(migration::DIRECTORY);
        let target = dest.clone();
        let once = Arc::new(AtomicBool::new(false));
        let observed = once.clone();
        migration::observe(move |p| {
            if p.destination == target
                && p.phase == phase
                && p.completed >= 128
                && !observed.swap(true, Ordering::SeqCst)
            {
                migration::cancel(&target);
            }
            !observed.load(Ordering::SeqCst)
        });
        assert!(migration::migrate(&source, root.path()).is_err());
        assert!(once.load(Ordering::SeqCst));
        assert!(!dest.exists());
        assert_eq!(std::fs::read(&source)?, before);
        migration::migrate(&source, root.path())?;
        verify_migration(root.path(), &before)?;
    }
    Ok(())
}
#[test]
fn v34_bad_locked_changed_and_existing_destinations_fail_closed() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("workspaces.redb");
    legacy(&source, 4)?;
    let owner = redb::Database::open(&source)?;
    assert!(migration::migrate(&source, root.path()).is_err());
    drop(owner);
    let dest = root.path().canonicalize()?.join(migration::DIRECTORY);
    let target = dest.clone();
    let once = Arc::new(AtomicBool::new(false));
    let observed = once.clone();
    migration::observe(move |p| {
        if p.destination == target && p.completed >= 128 && !observed.swap(true, Ordering::SeqCst) {
            migration::cancel(&target);
        }
        !observed.load(Ordering::SeqCst)
    });
    assert!(migration::migrate(&source, root.path()).is_err());
    let db = redb::Database::open(&source)?;
    let tx = db.begin_write()?;
    tx.open_table(redb::TableDefinition::<&str, &[u8]>::new("records_v1"))?
        .insert("new", b"changed".as_slice())?;
    tx.commit()?;
    drop(db);
    assert!(
        migration::migrate(&source, root.path())
            .unwrap_err()
            .to_string()
            .contains("changed")
    );
    assert!(!dest.exists());
    let fresh = root.path().join("manual");
    migration::migrate(&source, &fresh)?;
    assert!(
        migration::migrate(&source, &fresh)
            .unwrap_err()
            .to_string()
            .contains("live store")
    );
    let bad = root.path().join("bad.redb");
    std::fs::write(&bad, b"not a database")?;
    assert!(migration::migrate(&bad, &root.path().join("bad-home")).is_err());
    assert_eq!(std::fs::read(&bad)?, b"not a database");
    Ok(())
}
#[test]
fn v34_legacy_dirty_child() -> Result<()> {
    let Some(path) = std::env::var_os("VWORK_LEGACY_CRASH") else {
        return Ok(());
    };
    legacy(Path::new(&path), 4)?;
    let db = redb::Database::open(&path)?;
    let tx = db.begin_write()?;
    tx.open_table(redb::TableDefinition::<&str, &[u8]>::new("records_v1"))?
        .insert("crash", b"committed".as_slice())?;
    tx.commit()?;
    std::process::exit(0);
}
#[test]
fn v34_dirty_legacy_repairs_only_private_copy() -> Result<()> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("workspaces.redb");
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "v34_legacy_dirty_child"])
        .env("VWORK_LEGACY_CRASH", &source)
        .status()?;
    assert!(status.success());
    let before = std::fs::read(&source)?;
    migration::migrate(&source, root.path())?;
    assert_eq!(std::fs::read(&source)?, before);
    let db = Database::open(&root.path().join(migration::DIRECTORY))?;
    assert_eq!(
        db.begin_read()?
            .open_table(RECORDS)?
            .get("crash")?
            .unwrap()
            .value(),
        b"committed"
    );
    Ok(())
}
