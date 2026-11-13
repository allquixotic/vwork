//! Work receipt for temporary copies of a real repository. Counts files and
//! bytes written rather than asserting stopwatch thresholds, so numbers from a
//! fast disk still predict cost under an on-access scanner.
//!
//! cargo run --release --example temporary_bench -- <source-repository> <state-directory> [copies]
//!
//! The state directory is reused: the first run pays the one-time import.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::{path::Path, time::Instant};
use vwork::{engine::Engine, model::Config};

fn census(root: &Path) -> Value {
    let (mut files, mut directories, mut bytes, mut links) = (0u64, 0u64, 0u64, 0u64);
    for entry in walkdir::WalkDir::new(root).into_iter().flatten() {
        if entry.file_type().is_dir() {
            directories += 1;
        } else if let Ok(meta) = entry.metadata() {
            #[cfg(unix)]
            let shared = std::os::unix::fs::MetadataExt::nlink(&meta) > 1;
            #[cfg(not(unix))]
            let shared = false;
            if shared {
                // Another name for bytes that already exist; nothing was written.
                links += 1;
            } else {
                files += 1;
                bytes += meta.len();
            }
        }
    }
    json!({"files_written":files,"bytes_written":bytes,"directories":directories,"shared_links":links})
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let source = args.next().context("source repository path")?;
    let home = args.next().context("state directory")?;
    let copies: usize = args.next().map_or(Ok(3), |n| n.parse())?;
    let engine = Engine::open(&home, Config::default())?;
    let mut report = json!({});
    if engine.repo("bench").is_err() {
        let started = Instant::now();
        engine.call("repo_register", &json!({"repo":"bench","path":source}))?;
        report["import_ms"] = json!(started.elapsed().as_millis());
    }
    report["git_objects"] = json!(vwork::git_store::members(&engine.db, "bench")?.len());
    report["refs"] = json!(engine.repo("bench")?.refs.len());
    let mut runs = Vec::new();
    for n in 0..copies {
        let name = format!("bench-{}-{n}", std::process::id());
        let started = Instant::now();
        engine.call(
            "workspace_create",
            &json!({"repo":"bench","workspace":name}),
        )?;
        let create_ms = started.elapsed().as_secs_f64() * 1000.;
        let revision = engine.workspace(&name)?.head;
        let started = Instant::now();
        let lease = engine.call(
            "workspace_temporary",
            &json!({"workspace":name,"expected_revision":revision,"purpose":"benchmark"}),
        )?;
        let temporary_ms = started.elapsed().as_secs_f64() * 1000.;
        let tree = Path::new(lease["path"].as_str().context("lease path")?);
        let root = tree.parent().context("lease root")?;
        let started = Instant::now();
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(tree)
            .args(["status", "--porcelain"])
            .output()?;
        let git_status_ms = started.elapsed().as_secs_f64() * 1000.;
        let mut run = json!({
            "workspace_create_ms":create_ms,
            "workspace_temporary_ms":temporary_ms,
            "git_status_ms":git_status_ms,
            "git_status_clean":status.status.success() && status.stdout.is_empty(),
            "git":census(&root.join("git")),
            "tree":census(tree),
            "reported":lease.get("timings_ms").cloned().unwrap_or(Value::Null),
            "work":lease.get("work").cloned().unwrap_or(Value::Null),
        });
        let started = Instant::now();
        engine.call("temporary_release", &json!({"lease":lease["id"]}))?;
        run["temporary_release_ms"] = json!(started.elapsed().as_secs_f64() * 1000.);
        let head = engine.workspace(&name)?.head;
        engine.call(
            "workspace_drop",
            &json!({"workspace":name,"expected_revision":head}),
        )?;
        runs.push(run);
    }
    report["runs"] = json!(runs);
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}
