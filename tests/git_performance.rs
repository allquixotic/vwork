//! Work bounds, not stopwatch thresholds: small log previews must not touch
//! distant commits, and materialization must not emit one file per Git object.
use anyhow::Result;
use git2::{Oid, Repository, Signature, Time};
use serde_json::json;
use vwork::{engine::Engine, model::Config};

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
    let files: Vec<_> = walkdir::WalkDir::new(view.path().join("objects"))
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| entry.file_type().is_file())
        .collect();
    assert!(
        files
            .iter()
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "pack"))
    );
    assert!(
        files.len() < 20,
        "historical objects must be packed, got {} files",
        files.len()
    );
    let packs: Vec<_> = files
        .iter()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pack"))
        .map(|entry| Ok((entry.path().to_owned(), std::fs::read(entry.path())?)))
        .collect::<std::io::Result<_>>()?;
    let admin = view.path().to_owned();
    drop(odb);
    drop(view);
    let prepare = || -> Result<()> {
        let workspace = engine.workspace("one")?;
        let (revision, tree) = engine.snapshot(&workspace)?;
        vwork::git::prepare_build(
            &engine.db,
            &engine.repo("fixture")?,
            &admin,
            checkout,
            &revision.base,
            &tree,
            |entry| Ok(vwork::git_store::read(&engine.db, Oid::from_str(&entry.oid)?)?.1),
        )?;
        Ok(())
    };
    prepare()?;
    for (path, bytes) in &packs {
        assert_eq!(
            &std::fs::read(path)?,
            bytes,
            "reusing a lease must retain its existing pack"
        );
    }
    let pack_count = || {
        std::fs::read_dir(admin.join("objects/pack"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "pack"))
            .count()
    };
    assert_eq!(
        pack_count(),
        packs.len(),
        "unchanged preparation must not duplicate packs"
    );
    engine.call(
        "files_write",
        &json!({
            "workspace":"one", "expected_revision":revision,
            "path":"changing.txt", "content":"new virtual content"
        }),
    )?;
    prepare()?;
    assert_eq!(
        pack_count(),
        packs.len() + 1,
        "export only the missing overlay object"
    );
    for (path, bytes) in packs {
        assert_eq!(std::fs::read(path)?, bytes);
    }
    let fsck = std::process::Command::new("git")
        .arg("-C")
        .arg(checkout)
        .args(["fsck", "--full"])
        .output()?;
    assert!(
        fsck.status.success(),
        "{}",
        String::from_utf8_lossy(&fsck.stderr)
    );
    engine.call(
        "temporary_release",
        &json!({"lease":lease["id"],"discard_changes":true}),
    )?;
    assert!(!checkout.exists());
    Ok(())
}
