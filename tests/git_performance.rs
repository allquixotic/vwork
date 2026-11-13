//! Work bounds, not stopwatch thresholds: small log previews must not touch
//! distant commits, and materialization must not emit one file per Git object.
use anyhow::Result;
use git2::{Oid, Repository, Signature, Time};
use serde_json::json;
use vwork::{engine::Engine, model::Config};

#[test]
fn v41_concurrent_imports_preserve_shared_objects_and_repository_membership() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    let ids = history(&source, 260)?;
    let engine = std::sync::Arc::new(Engine::open(temp.path().join("store"), Config::default())?);
    let barrier = std::sync::Barrier::new(4);
    std::thread::scope(|scope| -> Result<()> {
        let mut calls = Vec::new();
        for n in 0..4 {
            let (engine, source, barrier) = (&engine, &source, &barrier);
            calls.push(scope.spawn(move || {
                barrier.wait();
                engine.call(
                    "repo_register",
                    &json!({"repo":format!("fixture-{n}"),"path":source}),
                )
            }));
        }
        for call in calls {
            call.join().unwrap()?;
        }
        Ok(())
    })?;
    let expected = vwork::git_store::members(&engine.db, "fixture-0")?;
    assert!(expected.len() > 768);
    for n in 0..4 {
        let name = format!("fixture-{n}");
        assert_eq!(
            engine.repo(&name)?.refs["HEAD"],
            ids.last().unwrap().to_string()
        );
        assert_eq!(vwork::git_store::members(&engine.db, &name)?, expected);
    }
    for oid in expected {
        let (kind, bytes) = vwork::git_store::read(&engine.db, oid)?;
        assert_eq!(Oid::hash_object(kind, &bytes)?, oid);
    }
    Ok(())
}

#[test]
fn v40_duplicate_import_links_membership_without_reading_source_payloads() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    let ids = history(&source, 1)?;
    let engine = Engine::open(temp.path().join("store"), Config::default())?;
    engine.call("repo_register", &json!({"repo":"fixture","path":source}))?;
    let repo = Repository::open(&source)?;
    let blob = repo
        .find_commit(ids[0])?
        .tree()?
        .get_name("file-0.txt")
        .unwrap()
        .id();
    let id = blob.to_string();
    // This OID already has verified immutable content in Fjall. Enumerating
    // another source must link membership using metadata, without inflating
    // its redundant copy. The deliberately unreadable duplicate proves that.
    let duplicate = repo.path().join("objects").join(&id[..2]).join(&id[2..]);
    #[cfg(windows)]
    // Only clears the Windows READONLY attribute on our generated fixture.
    // This block is absent on Unix, where this API would widen mode bits.
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut permissions = std::fs::metadata(&duplicate)?.permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&duplicate, permissions)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&duplicate)?.permissions();
        permissions.set_mode(permissions.mode() | 0o200);
        std::fs::set_permissions(&duplicate, permissions)?;
    }
    std::fs::write(duplicate, b"unreadable duplicate")?;
    let mut imported = engine.repo("fixture")?;
    imported.id = "shared".into();
    vwork::git::import(&engine.db, &repo, &mut imported, 3, 60)?;
    assert!(vwork::git_store::contains(&engine.db, "shared", blob)?);
    assert_eq!(
        vwork::git_store::read(&engine.db, blob)?.1,
        b"base file 0\n"
    );
    Ok(())
}

fn history(root: &std::path::Path, count: usize) -> Result<Vec<Oid>> {
    let repo = Repository::init(root)?;
    let mut ids = Vec::new();
    let mut tree = repo.treebuilder(None)?;
    for n in 0..96 {
        tree.insert(
            format!("file-{n}.txt"),
            repo.blob(format!("base file {n}\n").as_bytes())?,
            0o100644,
        )?;
    }
    for n in 0..count {
        tree.insert(
            "changing.txt",
            repo.blob(format!("revision {n}\n").as_bytes())?,
            0o100644,
        )?;
        let snapshot = repo.find_tree(tree.write()?)?;
        let author = Signature::new(
            "Performance Fixture",
            "fixture@example.invalid",
            &Time::new(1_700_000_000 + n as i64, 0),
        )?;
        let parent = ids.last().map(|id| repo.find_commit(*id)).transpose()?;
        let parents: Vec<_> = parent.iter().collect();
        ids.push(repo.commit(
            Some("HEAD"),
            &author,
            &author,
            &format!("revision {n}"),
            &snapshot,
            &parents,
        )?);
    }
    let tag = repo.find_object(ids[0], None)?;
    repo.tag(
        "historical",
        &tag,
        &Signature::now("Fixture", "fixture@example.invalid")?,
        "Keep annotated history",
        false,
    )?;
    Ok(ids)
}

#[test]
fn v39_short_log_does_not_read_distant_history() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    let ids = history(&source, 64)?;
    let engine = Engine::open(temp.path().join("store"), Config::default())?;
    engine.call("repo_register", &json!({"repo":"fixture","path":source}))?;
    std::fs::rename(&source, source.with_extension("offline"))?;
    let transaction = engine.db.begin_write()?;
    // Remove repository visibility, leaving possible delta bases intact.
    // Only a history walk reaching this commit should encounter the fault.
    transaction
        .open_table(vwork::git_store::MEMBERS)?
        .delete(format!("fixture/{}", ids[0]).as_str())?;
    transaction.commit()?;

    let result = engine.call("repo_log", &json!({"repo":"fixture","limit":3}))?;
    let lines: Vec<_> = result["log"].as_str().unwrap().lines().collect();
    assert_eq!(lines.len(), 3);
    for (line, id) in lines.iter().zip(ids.iter().rev()) {
        assert!(line.starts_with(&id.to_string()));
    }
    assert!(
        engine
            .call("repo_log", &json!({"repo":"fixture","limit":100}))
            .is_err()
    );
    Ok(())
}

#[test]
fn v39_incremental_log_keeps_both_merge_parents() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    let ids = history(&source, 1)?;
    let repo = Repository::open(&source)?;
    let root = repo.find_commit(ids[0])?;
    let tree = root.tree()?;
    let commit = |message: &str, seconds: i64, parents: &[&git2::Commit<'_>]| {
        let author = Signature::new("Fixture", "fixture@example.invalid", &Time::new(seconds, 0))?;
        repo.commit(None, &author, &author, message, &tree, parents)
    };
    let left = repo.find_commit(commit("left", 1_700_000_010, &[&root])?)?;
    let right = repo.find_commit(commit("right", 1_700_000_020, &[&root])?)?;
    let merge = commit("merge", 1_700_000_030, &[&left, &right])?;
    repo.set_head_detached(merge)?;
    let engine = Engine::open(temp.path().join("store"), Config::default())?;
    engine.call("repo_register", &json!({"repo":"fixture","path":source}))?;
    let result = engine.call("repo_log", &json!({"repo":"fixture","limit":4}))?;
    let actual: Vec<_> = result["log"]
        .as_str()
        .unwrap()
        .lines()
        .map(|line| line.split('\t').next().unwrap())
        .collect();
    let expected: Vec<_> = [merge, right.id(), left.id(), root.id()]
        .map(|id| id.to_string())
        .into_iter()
        .collect();
    assert_eq!(actual, expected);
    Ok(())
}

#[test]
fn v39_temporary_git_history_is_packed_and_complete() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    let ids = history(&source, 140)?;
    let engine = Engine::open(temp.path().join("store"), Config::default())?;
    engine.call("repo_register", &json!({"repo":"fixture","path":source}))?;
    engine.call(
        "workspace_create",
        &json!({"repo":"fixture","workspace":"one"}),
    )?;
    std::fs::rename(&source, source.with_extension("offline"))?;
    let revision = engine.workspace("one")?.head;
    let lease = engine.call(
        "workspace_temporary",
        &json!({
            "workspace":"one", "expected_revision":revision, "purpose":"packed Git regression"
        }),
    )?;
    let checkout = std::path::Path::new(lease["path"].as_str().unwrap());
    let view = Repository::open(checkout)?;
    let members = vwork::git_store::members(&engine.db, "fixture")?;
    assert!(members.len() > 400);
    let odb = view.odb()?;
    for oid in &members {
        let expected = vwork::git_store::read(&engine.db, *oid)?;
        let actual = odb.read(*oid)?;
        assert_eq!(actual.kind(), expected.0);
        assert_eq!(actual.data(), expected.1);
    }
    assert_eq!(view.head()?.target(), ids.last().copied());
    assert_eq!(view.revparse_single("historical^{}")?.id(), ids[0]);
    let files = object_files(view.path());
    assert!(
        files
            .iter()
            .any(|path| path.extension().is_some_and(|ext| ext == "pack"))
    );
    assert!(
        files.len() < 20,
        "historical objects must be packed, got {} files",
        files.len()
    );
    drop(odb);
    drop(view);
    fsck(checkout);
    engine.call(
        "temporary_release",
        &json!({"lease":lease["id"],"discard_changes":true}),
    )?;
    assert!(!checkout.exists());
    Ok(())
}

fn object_files(admin: &std::path::Path) -> Vec<std::path::PathBuf> {
    walkdir::WalkDir::new(admin.join("objects"))
        .into_iter()
        .map(|entry| entry.unwrap())
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .collect()
}

fn fsck(checkout: &std::path::Path) {
    let fsck = std::process::Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["fsck", "--full"])
        .output()
        .unwrap();
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
}

fn porcelain(checkout: &std::path::Path) -> String {
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert!(status.status.success());
    String::from_utf8(status.stdout).unwrap()
}

/// History is exported once per repository, then every copy names the same
/// files. A copy writes only what is its own: no pack per copy, no file per
/// ref, and virtual edits as one private pack.
#[test]
fn v42_temporary_copies_share_history_and_write_only_their_own_objects() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    history(&source, 140)?;
    {
        let repo = Repository::open(&source)?;
        let head = repo.head()?.target().unwrap();
        for n in 0..300 {
            repo.reference(&format!("refs/heads/topic/{n:03}"), head, false, "fixture")?;
        }
    }
    let home = temp.path().join("store");
    let engine = Engine::open(&home, Config::default())?;
    engine.call("repo_register", &json!({"repo":"fixture","path":source}))?;
    for name in ["one", "two"] {
        engine.call(
            "workspace_create",
            &json!({"repo":"fixture","workspace":name}),
        )?;
    }
    std::fs::rename(&source, source.with_extension("offline"))?;
    let temporary = |workspace: &str| {
        engine.call(
            "workspace_temporary",
            &json!({
                "workspace":workspace,
                "expected_revision":engine.workspace(workspace).unwrap().head,
                "purpose":"shared history regression"
            }),
        )
    };
    let permanent = vwork::git_store::permanent_members(&engine.db, "fixture")?.len();
    let first = temporary("one")?;
    assert_eq!(first["work"]["shared_history"]["scanned"], true);
    assert_eq!(
        first["work"]["shared_history"]["exported_objects"],
        permanent
    );
    assert!(
        first["work"]["shared_history"]["reused_deltas"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(first["work"]["git_view"]["packs_linked"], 2);
    assert_eq!(first["work"]["git_view"]["packs_copied"], 0);
    assert_eq!(first["work"]["git_view"]["private_objects"], 0);
    let shared = home.join("git-cache/fixture/objects/pack");
    let packs = || -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut all: Vec<_> = std::fs::read_dir(&shared)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .map(|path| (path.clone(), std::fs::read(path).unwrap()))
            .collect();
        all.sort();
        all
    };
    let exported = packs();
    assert_eq!(exported.len(), 2, "one pack and its index");

    engine.call(
        "files_write",
        &json!({
            "workspace":"two", "expected_revision":engine.workspace("two")?.head,
            "path":"changing.txt", "content":"new virtual content"
        }),
    )?;
    let second = temporary("two")?;
    // Same references, base present: completeness needs no storage scan.
    assert_eq!(second["work"]["shared_history"]["scanned"], false);
    assert_eq!(second["work"]["shared_history"]["exported_objects"], 0);
    assert_eq!(second["work"]["git_view"]["packs_linked"], 2);
    assert_eq!(second["work"]["git_view"]["packs_copied"], 0);
    assert_eq!(second["work"]["git_view"]["private_objects"], 1);
    assert_eq!(packs(), exported, "shared packs are immutable");

    let edited = git2::Oid::hash_object(git2::ObjectType::Blob, b"new virtual content")?;
    for (lease, own_packs) in [(&first, 0), (&second, 1)] {
        let checkout = std::path::Path::new(lease["path"].as_str().unwrap());
        let view = Repository::open(checkout)?;
        let files = object_files(view.path());
        assert_eq!(files.len(), 2 + own_packs * 2, "{files:?}");
        for (path, bytes) in &exported {
            let linked = view
                .path()
                .join("objects/pack")
                .join(path.file_name().unwrap());
            assert_eq!(&std::fs::read(&linked)?, bytes);
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                // The same file under another name: nothing was copied.
                assert_eq!(
                    std::fs::metadata(&linked)?.ino(),
                    std::fs::metadata(path)?.ino()
                );
            }
        }
        // 300 topic branches, their remotes-free refs and the tag: one file.
        assert_eq!(
            walkdir::WalkDir::new(view.path().join("refs"))
                .into_iter()
                .filter_map(std::result::Result::ok)
                .filter(|entry| entry.file_type().is_file())
                .count(),
            0
        );
        assert_eq!(
            view.references()?.count(),
            engine.repo("fixture")?.refs.len() - 1
        );
        assert_eq!(view.odb()?.exists(edited), own_packs == 1);
        drop(view);
        fsck(checkout);
    }
    // Recorded metadata makes an untouched copy clean without reading it, and
    // a virtual edit appears staged against the base commit.
    assert_eq!(
        porcelain(std::path::Path::new(first["path"].as_str().unwrap())),
        ""
    );
    assert_eq!(
        porcelain(std::path::Path::new(second["path"].as_str().unwrap())),
        "M  changing.txt\n"
    );

    // A commit adds permanent objects without touching imported references;
    // only those objects are exported, and earlier copies keep their packs.
    let committed = engine.call(
        "workspace_commit",
        &json!({
            "workspace":"two", "expected_revision":engine.workspace("two")?.head,
            "message":"commit virtual edit"
        }),
    )?;
    engine.call(
        "workspace_create",
        &json!({"repo":"fixture","workspace":"three","reference":committed["commit"]}),
    )?;
    let third = temporary("three")?;
    assert_eq!(third["work"]["shared_history"]["scanned"], true);
    let added = third["work"]["shared_history"]["exported_objects"]
        .as_u64()
        .unwrap();
    assert!((1..10).contains(&added), "incremental export, got {added}");
    assert_eq!(third["work"]["git_view"]["packs_linked"], 4);
    assert_eq!(third["work"]["git_view"]["private_objects"], 0);
    let after = packs();
    assert_eq!(after.len(), 4);
    for pack in &exported {
        assert!(after.contains(pack));
    }
    let checkout = std::path::Path::new(third["path"].as_str().unwrap());
    assert_eq!(
        Repository::open(checkout)?
            .head()?
            .target()
            .unwrap()
            .to_string(),
        committed["commit"].as_str().unwrap()
    );
    fsck(checkout);
    assert_eq!(porcelain(checkout), "");

    // Releasing a copy removes names, never the shared files.
    for lease in [&first, &second, &third] {
        engine.call(
            "temporary_release",
            &json!({"lease":lease["id"],"discard_changes":true}),
        )?;
        assert!(!std::path::Path::new(lease["path"].as_str().unwrap()).exists());
    }
    assert_eq!(packs(), after);
    let again = temporary("one")?;
    assert_eq!(again["work"]["shared_history"]["scanned"], false);
    fsck(std::path::Path::new(again["path"].as_str().unwrap()));
    Ok(())
}

/// A damaged or half-written cache is never trusted: it is rebuilt from the
/// authoritative store, and stray names are removed.
#[test]
fn v42_shared_history_is_rebuilt_when_its_files_are_not_vouched_for() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    history(&source, 12)?;
    let home = temp.path().join("store");
    let engine = Engine::open(&home, Config::default())?;
    engine.call("repo_register", &json!({"repo":"fixture","path":source}))?;
    engine.call(
        "workspace_create",
        &json!({"repo":"fixture","workspace":"one"}),
    )?;
    let temporary = || {
        engine.call(
            "workspace_temporary",
            &json!({
                "workspace":"one",
                "expected_revision":engine.workspace("one").unwrap().head,
                "purpose":"cache recovery regression"
            }),
        )
    };
    let release = |lease: &serde_json::Value| {
        engine
            .call(
                "temporary_release",
                &json!({"lease":lease["id"],"discard_changes":true}),
            )
            .unwrap();
    };
    release(&temporary()?);
    let shared = home.join("git-cache/fixture/objects/pack");
    let pack = std::fs::read_dir(&shared)?
        .map(|entry| entry.unwrap().path())
        .find(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .unwrap();
    // Truncation, as after a power loss, and debris from an interrupted export.
    let length = std::fs::metadata(&pack)?.len();
    #[allow(clippy::permissions_set_readonly_false)]
    {
        let mut permissions = std::fs::metadata(&pack)?.permissions();
        permissions.set_readonly(false);
        std::fs::set_permissions(&pack, permissions)?;
    }
    std::fs::OpenOptions::new()
        .write(true)
        .open(&pack)?
        .set_len(length / 2)?;
    std::fs::write(shared.join("tmp-interrupted.pack"), b"partial")?;
    let lease = temporary()?;
    assert_eq!(lease["work"]["shared_history"]["scanned"], true);
    assert!(
        lease["work"]["shared_history"]["exported_objects"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert!(!shared.join("tmp-interrupted.pack").exists());
    assert_eq!(std::fs::read_dir(&shared)?.count(), 2);
    fsck(std::path::Path::new(lease["path"].as_str().unwrap()));
    release(&lease);
    Ok(())
}
