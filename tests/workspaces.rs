use anyhow::Result;
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command, sync::Arc};
use tempfile::TempDir;
use vwork::{engine::Engine, model::Config, protocol};

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    home: PathBuf,
    engine: Engine,
}
fn git(root: &std::path::Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let home = temp.path().join("state");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("README.md"), "base\n").unwrap();
        std::fs::write(root.join("src/a.rs"), "fn alpha() {}\n").unwrap();
        std::fs::write(root.join("binary.dat"), [0, 1, 255, 2]).unwrap();
        git(&root, &["init", "-b", "main"]);
        git(&root, &["config", "user.name", "Vwork Test"]);
        git(&root, &["config", "user.email", "test@example.invalid"]);
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "base"]);
        let engine = Engine::open(&home, Config::default()).unwrap();
        engine
            .call("repo_register", &json!({"repo":"repo","path":root}))
            .unwrap();
        engine
            .call(
                "workspace_create",
                &json!({"repo":"repo","workspace":"one"}),
            )
            .unwrap();
        Self {
            _temp: temp,
            root,
            home,
            engine,
        }
    }
    fn head(&self, ws: &str) -> String {
        self.engine.workspace(ws).unwrap().head
    }
    fn write(&self, ws: &str, path: &str, text: &str) -> Value {
        self.engine.call("files_write",&json!({"workspace":ws,"expected_revision":self.head(ws),"path":path,"content":text})).unwrap()
    }
    fn read(&self, ws: &str, path: &str) -> String {
        self.engine
            .call("files_read", &json!({"workspace":ws,"path":path}))
            .unwrap()["content"]
            .as_str()
            .unwrap()
            .into()
    }
}

#[test]
fn fork_isolation_and_reopen() -> Result<()> {
    let f = Fixture::new();
    let base = f.head("one");
    f.engine
        .call("workspace_fork", &json!({"workspace":"one","name":"two"}))?;
    assert_eq!(f.head("two"), base);
    f.write("one", "src/a.rs", "changed\n");
    assert_eq!(f.read("two", "src/a.rs"), "fn alpha() {}");
    assert_eq!(
        std::fs::read_to_string(f.root.join("src/a.rs"))?,
        "fn alpha() {}\n"
    );
    let Fixture {
        engine,
        home,
        _temp,
        ..
    } = f;
    drop(engine);
    let reopened = Engine::open(home, Config::default())?;
    assert_ne!(
        reopened.workspace("one")?.head,
        reopened.workspace("two")?.head
    );
    drop(_temp);
    Ok(())
}
#[test]
fn optimistic_conflicts() -> Result<()> {
    let f = Fixture::new();
    let old = f.head("one");
    f.write("one", "a.txt", "one");
    assert!(
        f.engine
            .call(
                "files_write",
                &json!({"workspace":"one","expected_revision":old,"path":"b.txt","content":"lost"})
            )
            .is_err()
    );
    let current = f.head("one");
    assert!(f.engine.call("files_apply_patch",&json!({"workspace":"one","expected_revision":current,"edits":[{"path":"x","content":"ok"},{"path":"../bad","content":"bad"}]})).is_err());
    assert_eq!(f.head("one"), current);
    Ok(())
}
#[test]
fn binary_and_compression() -> Result<()> {
    let f = Fixture::new();
    let original = f.engine.call(
        "files_read",
        &json!({"workspace":"one","path":"binary.dat","binary":true}),
    )?;
    assert_eq!(original["base64"], "AAH/Ag==");
    let content = "repeated source content\n".repeat(5000);
    f.write("one", "large.txt", &content);
    f.write("one", "duplicate.txt", &content);
    let stats = f.engine.call("stats", &json!({}))?;
    assert_eq!(stats["blobs"], 1);
    assert!(stats["compressed_blob_bytes"].as_u64().unwrap() < 1000);
    let mut c = Config {
        compression_level: 23,
        ..Config::default()
    };
    assert!(c.validate().is_err());
    c.compression_level = 8;
    assert!(c.validate().is_ok());
    Ok(())
}
#[test]
fn path_and_export_safety() -> Result<()> {
    let f = Fixture::new();
    for p in [
        "../bad",
        "/bad",
        "a/../b",
        ".git/config",
        "a\\b",
        ".vwork-managed.json",
    ] {
        assert!(f.engine.call("files_write",&json!({"workspace":"one","expected_revision":f.head("one"),"path":p,"content":"bad"})).is_err());
    }
    assert!(
        f.engine
            .call(
                "workspace_export",
                &json!({"workspace":"one","destination":f.root})
            )
            .is_err()
    );
    let dest = f._temp.path().join("export");
    f.engine.call(
        "workspace_export",
        &json!({"workspace":"one","destination":dest,"glob":"src/**"}),
    )?;
    assert!(dest.join("src/a.rs").is_file());
    assert!(!dest.join("README.md").exists());
    std::fs::write(dest.join("src/a.rs"), "physical edit\n")?;
    f.engine.call("workspace_import",&json!({"workspace":"one","expected_revision":f.head("one"),"source":dest,"paths":["src/a.rs"]}))?;
    assert_eq!(f.read("one", "src/a.rs"), "physical edit");
    f.write("one", "CON.txt", "bad Windows name");
    assert!(
        f.engine
            .call(
                "workspace_export",
                &json!({"workspace":"one","destination":f._temp.path().join("reject")})
            )
            .is_err()
    );
    #[cfg(unix)]
    {
        let outside = f._temp.path().join("outside");
        std::fs::create_dir(&outside)?;
        std::os::unix::fs::symlink(&outside, f._temp.path().join("link"))?;
        assert!(
            f.engine
                .call(
                    "workspace_export",
                    &json!({"workspace":"one","destination":f._temp.path().join("link/sub")})
                )
                .is_err()
        );
    }
    Ok(())
}
#[test]
fn virtual_operations() -> Result<()> {
    let f = Fixture::new();
    f.engine
        .call("workspace_fork", &json!({"workspace":"one","name":"two"}))?;
    f.write("two", "new.txt", "needle\n");
    let result = f.engine.call(
        "workspace_merge",
        &json!({"workspace":"one","source":"two","expected_revision":f.head("one")}),
    )?;
    assert_eq!(result["merged"], true);
    assert_eq!(
        f.engine.call(
            "files_search",
            &json!({"workspace":"one","pattern":"needle"})
        )?["matches"][0]["path"],
        "new.txt"
    );
    f.engine.call(
        "files_delete",
        &json!({"workspace":"one","expected_revision":f.head("one"),"path":"src","recursive":true}),
    )?;
    assert!(
        f.engine
            .call("files_read", &json!({"workspace":"one","path":"src/a.rs"}))
            .is_err()
    );
    let status = f
        .engine
        .call("workspace_status", &json!({"workspace":"one"}))?;
    assert_eq!(status["changes"].as_array().unwrap().len(), 2);
    f.write("one", "new.txt", "ours");
    f.write("two", "new.txt", "theirs");
    let prior = f.head("one");
    let result = f.engine.call(
        "workspace_merge",
        &json!({"workspace":"one","source":"two","expected_revision":prior}),
    )?;
    assert_eq!(result["merged"], false);
    assert_eq!(f.head("one"), prior);
    Ok(())
}
#[test]
fn git_commit_isolation() -> Result<()> {
    let f = Fixture::new();
    let head = git(&f.root, &["rev-parse", "HEAD"]);
    let index = std::fs::read(f.root.join(".git/index"))?;
    f.write("one", "src/a.rs", "exported\n");
    let out = f.engine.call(
        "workspace_commit",
        &json!({"workspace":"one","expected_revision":f.head("one"),"message":"virtual change"}),
    )?;
    assert_eq!(
        git(
            &f.root,
            &[
                "show",
                &format!("{}:src/a.rs", out["commit"].as_str().unwrap())
            ]
        ),
        "exported"
    );
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), head);
    assert_eq!(std::fs::read(f.root.join(".git/index"))?, index);
    assert!(git(&f.root, &["status", "--porcelain"]).is_empty());
    Ok(())
}
#[test]
fn managed_builds() -> Result<()> {
    let f = Fixture::new();
    #[cfg(unix)]
    let (program, args) = ("/bin/cat", vec!["src/a.rs"]);
    #[cfg(windows)]
    let (program, args) = ("cmd", vec!["/C", "type src\\a.rs"]);
    f.engine.call(
        "task_set",
        &json!({"repo":"repo","name":"check","program":program,"args":args,"timeout_seconds":5}),
    )?;
    f.write("one", "src/a.rs", "build this\n");
    let head = f.head("one");
    let result = f.engine.call(
        "workspace_run_task",
        &json!({"workspace":"one","expected_revision":head,"task":"check"}),
    )?;
    assert_eq!(result["success"], true);
    assert_eq!(result["revision"], head);
    assert!(result["stdout"].as_str().unwrap().contains("build this"));
    std::fs::write(f.home.join("build/repo/src/a.rs"), "native tool mutation")?;
    let result = f.engine.call(
        "workspace_run_task",
        &json!({"workspace":"one","expected_revision":head,"task":"check"}),
    )?;
    assert!(result["stdout"].as_str().unwrap().contains("build this"));
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-c", "sleep 10"]);
    #[cfg(windows)]
    let (program, args) = ("cmd", vec!["/C", "ping -n 11 127.0.0.1 >NUL"]);
    f.engine.call(
        "task_set",
        &json!({"repo":"repo","name":"timeout","program":program,"args":args,"timeout_seconds":1}),
    )?;
    let result = f.engine.call(
        "workspace_run_task",
        &json!({"workspace":"one","expected_revision":head,"task":"timeout"}),
    )?;
    assert_eq!(result["timed_out"], true);
    assert_eq!(result["success"], false);
    Ok(())
}
#[test]
fn protocol_and_hooks() -> Result<()> {
    let f = Fixture::new();
    let init=protocol::mcp_response(&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}),Some("one"),|_,_|unreachable!()).unwrap();
    assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
    let response=protocol::mcp_response(&json!({"id":2,"method":"tools/call","params":{"name":"vwork_files_read","arguments":{"path":"README.md"}}}),Some("one"),|op,a|f.engine.call(op,a)).unwrap();
    assert_eq!(response["result"]["isError"], false);
    for name in [
        "shell",
        "developer__text_editor",
        "write",
        "filesystem__read_file",
    ] {
        assert_eq!(
            protocol::hook(&json!({"tool_name":name}))["decision"],
            "block"
        );
    }
    assert_eq!(
        protocol::hook(&json!({"tool_name":"vwork_files_read"}))["decision"],
        "allow"
    );
    assert!(!protocol::tools(true).iter().any(|v| v["name"] == "shell"));
    Ok(())
}
#[test]
fn garbage_collection() -> Result<()> {
    let f = Fixture::new();
    f.write("one", "held", "held content");
    f.engine
        .call("workspace_fork", &json!({"workspace":"one","name":"two"}))?;
    f.engine.call(
        "workspace_drop",
        &json!({"workspace":"one","expected_revision":f.head("one")}),
    )?;
    f.engine.call("gc", &json!({}))?;
    assert_eq!(f.read("two", "held"), "held content");
    f.engine.call(
        "workspace_drop",
        &json!({"workspace":"two","expected_revision":f.head("two")}),
    )?;
    f.engine.call("gc", &json!({}))?;
    assert_eq!(f.engine.call("stats", &json!({}))?["blobs"], 0);
    Ok(())
}
#[test]
fn concurrent_edits() -> Result<()> {
    let f = Fixture::new();
    let head = f.head("one");
    let engine = Arc::new(f.engine);
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let mut handles = vec![];
    for n in 0..8 {
        let e = engine.clone();
        let b = barrier.clone();
        let head = head.clone();
        handles.push(std::thread::spawn(move||{b.wait();e.call("files_write",&json!({"workspace":"one","expected_revision":head,"path":format!("file{n}"),"content":"race"})).is_ok()}));
    }
    assert_eq!(
        handles
            .into_iter()
            .filter_map(|h| h.join().ok())
            .filter(|ok| *ok)
            .count(),
        1
    );
    Ok(())
}
#[test]
fn package_contents() -> Result<()> {
    let tmp = tempfile::tempdir()?;
    let zip = tmp.path().join("source.zip");
    vwork::integration::package(std::path::Path::new(env!("CARGO_MANIFEST_DIR")), None, &zip)?;
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?)?;
    assert!(archive.by_name("vwork/Cargo.toml").is_ok());
    assert!(archive.by_name("vwork/src/engine.rs").is_ok());
    for i in 0..archive.len() {
        let f = archive.by_index(i)?;
        assert!(
            !f.name().contains("/target/")
                && !f.name().contains("/.git/")
                && !f.name().ends_with(".redb")
        );
    }
    let binary = tmp.path().join("vwork");
    std::fs::write(&binary, b"native Mac")?;
    assert!(
        vwork::integration::package(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
            Some(&binary),
            &tmp.path().join("bad.zip")
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn export_directory_transitions_and_collision() -> Result<()> {
    let f = Fixture::new();
    let dest = f._temp.path().join("export");
    f.engine.call(
        "workspace_export",
        &json!({"workspace":"one","destination":dest}),
    )?;
    f.engine.call("files_apply_patch",&json!({"workspace":"one","expected_revision":f.head("one"),"edits":[{"path":"src/a.rs","delete":true},{"path":"src","content":"now a file"}]}))?;
    f.engine.call(
        "workspace_export",
        &json!({"workspace":"one","destination":dest}),
    )?;
    assert_eq!(std::fs::read_to_string(dest.join("src"))?, "now a file");
    f.engine.call("files_apply_patch",&json!({"workspace":"one","expected_revision":f.head("one"),"edits":[{"path":"src","delete":true},{"path":"src/b.rs","content":"directory again"}]}))?;
    f.engine.call(
        "workspace_export",
        &json!({"workspace":"one","destination":dest}),
    )?;
    assert!(dest.join("src/b.rs").is_file());
    f.write("one", "SRC/c.rs", "case collision");
    assert!(
        f.engine
            .call(
                "workspace_export",
                &json!({"workspace":"one","destination":dest})
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn squash_reclaims_old_blobs() -> Result<()> {
    let f = Fixture::new();
    f.write("one", "README.md", "old transient version");
    f.write("one", "README.md", "current version");
    assert_eq!(f.engine.call("stats", &json!({}))?["blobs"], 2);
    f.engine.call(
        "workspace_squash",
        &json!({"workspace":"one","expected_revision":f.head("one")}),
    )?;
    f.engine.call("gc", &json!({}))?;
    assert_eq!(f.engine.call("stats", &json!({}))?["blobs"], 1);
    assert_eq!(f.read("one", "README.md"), "current version");
    Ok(())
}

#[test]
fn multiple_repositories_and_linked_checkout() -> Result<()> {
    let f = Fixture::new();
    let linked = f._temp.path().join("linked");
    git(
        &f.root,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().unwrap(),
            "HEAD",
        ],
    );
    f.engine
        .call("repo_register", &json!({"repo":"linked","path":linked}))?;
    f.engine.call(
        "workspace_create",
        &json!({"repo":"linked","workspace":"other"}),
    )?;
    f.write("other", "README.md", "other repo alias");
    assert_eq!(f.read("one", "README.md"), "base");
    assert_eq!(f.read("other", "README.md"), "other repo alias");
    let unrelated = f._temp.path().join("unrelated");
    std::fs::create_dir(&unrelated)?;
    git(&unrelated, &["init", "-b", "main"]);
    git(&unrelated, &["config", "user.name", "Test"]);
    git(
        &unrelated,
        &["config", "user.email", "test@example.invalid"],
    );
    std::fs::write(unrelated.join("unique"), "different root")?;
    git(&unrelated, &["add", "."]);
    git(&unrelated, &["commit", "-m", "root"]);
    f.engine.call(
        "repo_register",
        &json!({"repo":"unrelated","path":unrelated}),
    )?;
    f.engine.call(
        "workspace_create",
        &json!({"repo":"unrelated","workspace":"third"}),
    )?;
    assert_eq!(f.read("third", "unique"), "different root");
    assert!(
        f.engine
            .call(
                "workspace_merge",
                &json!({"workspace":"one","source":"third","expected_revision":f.head("one")})
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn many_forks_share_storage() -> Result<()> {
    let f = Fixture::new();
    f.write(
        "one",
        "large.txt",
        &"large compressible content\n".repeat(100000),
    );
    let before = f.engine.call("stats", &json!({}))?;
    for i in 0..100 {
        f.engine.call(
            "workspace_fork",
            &json!({"workspace":"one","name":format!("fork-{i}")}),
        )?;
    }
    let after = f.engine.call("stats", &json!({}))?;
    assert_eq!(after["blobs"], before["blobs"]);
    assert_eq!(after["revisions"], before["revisions"]);
    assert_eq!(
        after["compressed_blob_bytes"],
        before["compressed_blob_bytes"]
    );
    assert_eq!(after["workspaces"], 101);
    Ok(())
}

#[test]
fn managed_build_serialization() -> Result<()> {
    let f = Fixture::new();
    f.engine
        .call("workspace_fork", &json!({"workspace":"one","name":"two"}))?;
    f.write("one", "README.md", "ONE");
    f.write("two", "README.md", "TWO");
    #[cfg(unix)]
    let (program, args) = (
        "/bin/sh",
        vec!["-c", "cat README.md; sleep 1; cat README.md"],
    );
    #[cfg(windows)]
    let (program, args) = (
        "cmd",
        vec![
            "/C",
            "type README.md & ping -n 2 127.0.0.1 >NUL & type README.md",
        ],
    );
    f.engine.call("task_set",&json!({"repo":"repo","name":"serialize","program":program,"args":args,"timeout_seconds":10}))?;
    let one = f.head("one");
    let two = f.head("two");
    let engine = Arc::new(f.engine);
    let other = engine.clone();
    let t = std::thread::spawn(move || {
        other
            .call(
                "workspace_run_task",
                &json!({"workspace":"one","expected_revision":one,"task":"serialize"}),
            )
            .unwrap()
    });
    let result = engine.call(
        "workspace_run_task",
        &json!({"workspace":"two","expected_revision":two,"task":"serialize"}),
    )?;
    assert_eq!(result["stdout"], "TWOTWO");
    assert_eq!(t.join().unwrap()["stdout"], "ONEONE");
    Ok(())
}

#[test]
fn export_retention() -> Result<()> {
    let f = Fixture::new();
    f.write("one", "retained", "export survives workspace drop");
    let revision = f.head("one");
    let commit = f.engine.call(
        "workspace_commit",
        &json!({"workspace":"one","expected_revision":revision,"message":"keep"}),
    )?;
    f.engine.call(
        "workspace_drop",
        &json!({"workspace":"one","expected_revision":revision}),
    )?;
    f.engine.call("gc", &json!({}))?;
    assert_eq!(f.engine.call("stats", &json!({}))?["blobs"], 1);
    f.engine
        .call("export_forget", &json!({"repo":"repo","revision":revision}))?;
    f.engine.call("gc", &json!({}))?;
    assert_eq!(f.engine.call("stats", &json!({}))?["blobs"], 0);
    assert_eq!(
        git(
            &f.root,
            &[
                "show",
                &format!("{}:retained", commit["commit"].as_str().unwrap())
            ]
        ),
        "export survives workspace drop"
    );
    Ok(())
}
