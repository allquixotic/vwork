//! An unchanged smart-HTTP fetch must not negotiate a pack or read blobs.
//! Loopback HTTP isolates Git protocol behavior from TLS/proxy/credential setup.
use anyhow::{Result, anyhow};
use git2::{Oid, Repository, Signature};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};
use vwork::{engine::Engine, model::Config};

fn packet(out: &mut String, line: &str) {
    out.push_str(&format!("{:04x}{line}", line.len() + 4));
}

struct Advertisement {
    url: String,
    stop: Arc<AtomicBool>,
    release: Arc<AtomicBool>,
    arrivals: Receiver<()>,
    worker: Option<JoinHandle<(usize, usize)>>,
}

struct ReleaseOnDrop(Arc<AtomicBool>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Advertisement {
    fn start(refs: &std::collections::BTreeMap<String, String>) -> Result<Self> {
        Self::with_gate(refs, false)
    }

    fn with_gate(refs: &std::collections::BTreeMap<String, String>, held: bool) -> Result<Self> {
        let server = tiny_http::Server::http("127.0.0.1:0").map_err(|error| anyhow!("{error}"))?;
        let url = format!("http://{}/fixture.git", server.server_addr());
        let mut body = String::new();
        packet(&mut body, "# service=git-upload-pack\n");
        body.push_str("0000");
        packet(
            &mut body,
            &format!(
                "{} HEAD\0multi_ack_detailed side-band-64k thin-pack ofs-delta symref=HEAD:refs/heads/main\n",
                refs["HEAD"]
            ),
        );
        for (name, oid) in refs {
            if name != "HEAD" {
                packet(&mut body, &format!("{oid} {name}\n"));
            }
        }
        body.push_str("0000");
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let release = Arc::new(AtomicBool::new(!held));
        let released = release.clone();
        let (sender, arrivals) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let (mut advertisements, mut unexpected) = (0, 0);
            let mut pending: Vec<tiny_http::Request> = Vec::new();
            loop {
                if released.load(Ordering::Acquire) {
                    for request in pending.drain(..) {
                        request
                            .respond(
                                tiny_http::Response::from_string(&body).with_header(
                                    tiny_http::Header::from_bytes(
                                        "Content-Type",
                                        "application/x-git-upload-pack-advertisement",
                                    )
                                    .unwrap(),
                                ),
                            )
                            .unwrap();
                    }
                }
                if stopped.load(Ordering::Acquire) {
                    break;
                }
                let Some(request) = server.recv_timeout(Duration::from_millis(50)).unwrap() else {
                    continue;
                };
                if request.method() == &tiny_http::Method::Get
                    && request.url() == "/fixture.git/info/refs?service=git-upload-pack"
                {
                    advertisements += 1;
                    sender.send(()).unwrap();
                    pending.push(request);
                } else {
                    // An unchanged fetch has nothing to negotiate or download.
                    unexpected += 1;
                    request.respond(tiny_http::Response::empty(500)).unwrap();
                }
            }
            (advertisements, unexpected)
        });
        Ok(Self {
            url,
            stop,
            release,
            arrivals,
            worker: Some(worker),
        })
    }

    fn finish(&mut self) -> (usize, usize) {
        self.release.store(true, Ordering::Release);
        self.stop.store(true, Ordering::Release);
        self.worker.take().unwrap().join().unwrap()
    }
}

#[test]
fn v41_independent_fetches_overlap_and_cleanup_preserves_active_receivers() -> Result<()> {
    gated_fetches(4, 4, false)
}

#[test]
fn v41_transfer_capacity_and_same_repository_serialization() -> Result<()> {
    gated_fetches(3, 2, false)?;
    gated_fetches(2, 4, true)?;
    gated_fetches(5, 8, false)
}

fn gated_fetches(total: usize, limit: usize, same_repo: bool) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    let repo = Repository::init_bare(&source)?;
    let tree = repo.find_tree(repo.treebuilder(None)?.write()?)?;
    let author = Signature::now("Fixture", "fixture@example.invalid")?;
    repo.commit(
        Some("refs/heads/main"),
        &author,
        &author,
        "fixture",
        &tree,
        &[],
    )?;
    repo.set_head("refs/heads/main")?;
    let engine = Arc::new(Engine::open(
        temp.path().join("store"),
        Config {
            max_temporary_copies: limit,
            ..Config::default()
        },
    )?);
    for n in 0..total {
        engine.call(
            "repo_register",
            &json!({"repo":format!("fixture-{n}"),"path":source}),
        )?;
    }
    let mut server = Advertisement::with_gate(&engine.repo("fixture-0")?.refs, true)?;
    let (arrived, before, after) = std::thread::scope(|scope| -> Result<_> {
        let _unblock = ReleaseOnDrop(server.release.clone());
        let mut calls = Vec::new();
        for n in 0..total {
            let engine = &engine;
            let url = &server.url;
            let n = if same_repo { 0 } else { n };
            calls.push(scope.spawn(move || {
                engine.call(
                    "repo_fetch",
                    &json!({"repo":format!("fixture-{n}"),"source":url}),
                )
            }));
        }
        let arrived = (0..total)
            .take_while(|_| server.arrivals.recv_timeout(Duration::from_secs(3)).is_ok())
            .count();
        let pending = || -> Result<usize> {
            Ok(engine.call("temporary_list", &json!({}))?["transfers"]
                .as_array()
                .unwrap()
                .len())
        };
        let before = pending()?;
        let cleanup = engine.reap_temporary();
        let after = pending()?;
        // Release even on baseline failure so scoped threads cannot hang.
        server.release.store(true, Ordering::Release);
        cleanup?;
        for call in calls {
            let result = call.join().unwrap()?;
            assert_eq!(result["received_objects"], 0);
        }
        Ok((arrived, before, after))
    })?;
    assert_eq!(server.finish(), (total, 0));
    println!(
        "{}",
        json!({"requests_before_release":arrived,"active_before_cleanup":before,"active_after_cleanup":after})
    );
    let expected = if same_repo {
        1
    } else {
        total.min(limit).min(4)
    };
    assert_eq!((arrived, before, after), (expected, expected, expected));
    assert!(
        engine.call("temporary_list", &json!({}))?["transfers"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    Ok(())
}

impl Drop for Advertisement {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.finish();
        }
    }
}

#[test]
fn v40_unchanged_http_fetch_neither_requests_a_pack_nor_reads_large_blobs() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source = temp.path().join("source");
    let repo = Repository::init_bare(&source)?;
    let blob = repo.blob(&vec![0xa5; 16 * 1024 * 1024])?;
    let mut builder = repo.treebuilder(None)?;
    builder.insert("large.bin", blob, 0o100644)?;
    let tree = repo.find_tree(builder.write()?)?;
    let author = Signature::now("Fixture", "fixture@example.invalid")?;
    let head = repo.commit(
        Some("refs/heads/main"),
        &author,
        &author,
        "large fixture",
        &tree,
        &[],
    )?;
    repo.set_head("refs/heads/main")?;
    for n in 0..24 {
        repo.reference(&format!("refs/heads/topic-{n}"), head, false, "fixture")?;
    }
    for n in 0..12 {
        repo.tag(
            &format!("tag-{n}"),
            &repo.find_object(head, None)?,
            &author,
            "fixture",
            false,
        )?;
    }
    let engine = Engine::open(temp.path().join("store"), Config::default())?;
    engine.call("repo_register", &json!({"repo":"fixture","path":source}))?;
    let before = engine.repo("fixture")?;
    engine.call(
        "workspace_create",
        &json!({"repo":"fixture","workspace":"old"}),
    )?;
    let revision = engine.workspace("old")?.head;
    let mut server = Advertisement::start(&before.refs)?;

    // Preserve metadata but make the stored binary unreadable. Any attempt to
    // decode/hash it during fetch must fail, regardless of caches or machine speed.
    let tx = engine.db.begin_write()?;
    tx.open_table(vwork::git_store::OBJECTS)?.insert(
        blob.to_string().as_str(),
        b"fault: blob read during unchanged fetch",
    )?;
    tx.commit()?;
    assert!(vwork::git_store::read(&engine.db, blob).is_err());
    drop(tree);
    drop(builder);
    drop(repo);
    std::fs::rename(&source, source.with_extension("offline"))?;

    for n in 0..2 {
        let start = Instant::now();
        let result = engine.call("repo_fetch", &json!({"repo":"fixture","source":server.url}))?;
        println!(
            "{}",
            json!({"iteration":n,"elapsed_ms":start.elapsed().as_secs_f64()*1000.,"result":result})
        );
        assert_eq!(result["received_objects"], 0);
        assert_eq!(result["received_bytes"], 0);
        assert_eq!(result["head"], head.to_string());
        assert_eq!(engine.workspace("old")?.head, revision);
        for (name, oid) in &before.refs {
            assert_eq!(engine.repo("fixture")?.refs.get(name), Some(oid));
        }
    }
    assert_eq!(server.finish(), (2, 0));
    assert_eq!(Oid::from_str(&engine.repo("fixture")?.refs["HEAD"])?, head);
    Ok(())
}
