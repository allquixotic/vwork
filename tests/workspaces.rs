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
        git(&root, &["tag", "-a", "v9.8.7", "-m", "version"]);
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
    f.engine.call(
        "workspace_create",
        &json!({"repo":"repo","workspace":"exported","reference":out["commit"]}),
    )?;
    assert_eq!(f.read("exported", "src/a.rs"), "exported");
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
    std::fs::write(
        PathBuf::from(result["checkout"].as_str().unwrap()).join("src/a.rs"),
        "native tool mutation",
    )?;
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
    assert!(
        protocol::hook(&json!({"tool_name":"shell"}))["reason"]
            .as_str()
            .unwrap()
            .contains("vwork_package_list/run")
    );
    assert!(!protocol::tools(true).iter().any(|v| v["name"] == "shell"));
    assert!(
        protocol::tools(true)
            .iter()
            .any(|v| v["name"] == "vwork_package_run")
    );
    assert!(
        !protocol::tools(true)
            .iter()
            .any(|v| v["name"] == "vwork_package_configure")
    );
    let denied = protocol::mcp_response(&json!({"id":3,"method":"tools/call","params":{"name":"vwork_package_configure","arguments":{}}}),Some("one"),|_,_| unreachable!()).unwrap();
    assert_eq!(denied["error"]["code"], -32602);
    Ok(())
}

#[test]
fn v18_process_stdio() -> Result<()> {
    if std::env::var_os("VWORK_TEST_STDIO_PROBE").is_some() {
        #[cfg(windows)]
        // SAFETY: GetConsoleWindow has no arguments or ownership requirements.
        assert!(unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() }.is_null());
        let mut input = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)?;
        println!("captured-input:{input}");
        eprintln!("captured-error");
        std::process::exit(7);
    }
    use std::io::Write;
    use std::process::Stdio;
    let mut child = vwork::process::command(std::env::current_exe()?)
        .args(["--exact", "v18_process_stdio", "--nocapture"])
        .env("VWORK_TEST_STDIO_PROBE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(b"hello")?;
    let result = child.wait_with_output()?;
    assert_eq!(
        result.status.code(),
        Some(7),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8(result.stdout)?.contains("captured-input:hello"));
    assert!(String::from_utf8(result.stderr)?.contains("captured-error"));
    Ok(())
}

#[cfg(windows)]
#[test]
fn v18_windows_task_console() -> Result<()> {
    if std::env::var_os("VWORK_TEST_TASK_CONSOLE_PROBE").is_some() {
        // SAFETY: GetConsoleWindow has no arguments or ownership requirements.
        assert!(unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() }.is_null());
        println!("task-has-no-console");
        return Ok(());
    }
    let f = Fixture::new();
    f.engine.call(
        "task_set",
        &json!({"repo":"repo","name":"console-probe",
        "program":std::env::current_exe()?,
        "args":["--exact","v18_windows_task_console","--nocapture"],
        "env":{"VWORK_TEST_TASK_CONSOLE_PROBE":"1"},"timeout_seconds":10}),
    )?;
    let result = f.engine.call(
        "workspace_run_task",
        &json!({"workspace":"one",
        "expected_revision":f.head("one"),"task":"console-probe"}),
    )?;
    assert_eq!(result["success"], true, "{result}");
    assert!(
        result["stdout"]
            .as_str()
            .unwrap()
            .contains("task-has-no-console")
    );
    Ok(())
}

#[test]
fn v19_storage_sharing() -> Result<()> {
    let f = Fixture::new();
    let tool = protocol::tools(true)
        .into_iter()
        .find(|t| t["name"] == "vwork_storage_info")
        .expect("shared storage evidence must be available without native CLI");
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    let info = || f.engine.call("storage_info", &json!({}));
    let before = info()?;
    assert_eq!(before["base_manifests"], 1);
    assert_eq!(before["base_entries"], 3);
    assert_eq!(before["blobs"], 0);
    let reply = protocol::mcp_response(
        &json!({"id":1,"method":"tools/call",
        "params":{"name":"vwork_storage_info","arguments":{}}}),
        Some("one"),
        |op, a| f.engine.call(op, a),
    )
    .unwrap();
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(info()?, before, "inspection must not mutate records");
    f.engine.call(
        "workspace_create",
        &json!({"repo":"repo","workspace":"two"}),
    )?;
    let second = info()?;
    assert_ne!(f.head("one"), f.head("two"));
    for field in [
        "base_manifests",
        "base_entries",
        "base_manifest_bytes",
        "blobs",
        "compressed_blob_bytes",
    ] {
        assert_eq!(second[field], before[field], "shared {field}");
    }
    assert_eq!(second["workspaces"], 2);
    assert_eq!(second["revisions"], 2);
    assert!(second["workspace_record_bytes"].as_u64() > before["workspace_record_bytes"].as_u64());
    assert!(second["revision_record_bytes"].as_u64() > before["revision_record_bytes"].as_u64());
    f.engine
        .call("workspace_fork", &json!({"workspace":"one","name":"fork"}))?;
    assert_eq!(info()?["revisions"], 2);
    assert_eq!(f.head("one"), f.head("fork"));
    f.write("one", "README.md", "shared edited content");
    let edited = info()?;
    f.write("two", "README.md", "shared edited content");
    let twice = info()?;
    assert_eq!(edited["blobs"], 1);
    assert_eq!(twice["blobs"], 1);
    assert_eq!(
        twice["compressed_blob_bytes"],
        edited["compressed_blob_bytes"]
    );
    // Registration identity, not a URL or object-store path, scopes base manifests.
    f.engine
        .call("repo_register", &json!({"repo":"alias","path":f.root}))?;
    f.engine.call(
        "workspace_create",
        &json!({"repo":"alias","workspace":"alias-one"}),
    )?;
    assert_eq!(info()?["base_manifests"], 2);
    assert_eq!(info()?["base_entries"], 6);
    f.write("alias-one", "README.md", "shared edited content");
    assert_eq!(
        info()?["blobs"],
        1,
        "edited blobs also share across registrations"
    );
    let sharing = &info()?["sharing"];
    assert_eq!(sharing["base_manifest_key"], "repository_id+commit");
    assert_eq!(sharing["edited_blob_scope"], "daemon");
    assert_eq!(sharing["remote_url_deduplication"], false);
    Ok(())
}

#[test]
fn v20_repository_setup() -> Result<()> {
    let f = Fixture::new();
    let tools = protocol::tools(true);
    for name in ["vwork_repo_clone", "vwork_repo_register"] {
        assert!(tools.iter().any(|t| t["name"] == name), "missing {name}");
    }
    for name in [
        "vwork_task_set",
        "vwork_package_configure",
        "vwork_gc",
        "shell",
    ] {
        assert!(!tools.iter().any(|t| t["name"] == name));
    }
    let config = std::fs::read(f.root.join(".git/config"))?;
    let index = std::fs::read(f.root.join(".git/index"))?;
    std::fs::write(f.root.join("README.md"), "uncommitted original content")?;
    let register = protocol::mcp_response(
        &json!({"id":1,"method":"tools/call",
        "params":{"name":"vwork_repo_register","arguments":{"repo":"registered","path":f.root}}}),
        Some("one"),
        |op, a| f.engine.call(op, a),
    )
    .unwrap();
    assert_eq!(register["result"]["isError"], false, "{register}");
    let clone = protocol::mcp_response(
        &json!({"id":2,"method":"tools/call",
        "params":{"name":"vwork_repo_clone","arguments":{"repo":"cloned","source":f.root}}}),
        Some("one"),
        |op, a| f.engine.call(op, a),
    )
    .unwrap();
    assert_eq!(clone["result"]["isError"], false, "{clone}");
    let repo = f.engine.repo("cloned")?;
    assert!(repo.stored);
    assert!(repo.path.as_os_str().is_empty());
    assert!(repo.git_dir.as_os_str().is_empty());
    assert!(!f.home.join("repos").exists());
    f.engine.call(
        "workspace_create",
        &json!({"repo":"cloned","workspace":"cloned-one"}),
    )?;
    assert_eq!(f.read("cloned-one", "README.md"), "base");
    f.write("cloned-one", "README.md", "virtual edit");
    f.engine.call(
        "task_set",
        &json!({"repo":"cloned","name":"git-head",
        "program":"git","args":["rev-parse","HEAD"],"timeout_seconds":10}),
    )?;
    let built = f.engine.call(
        "workspace_run_task",
        &json!({"workspace":"cloned-one",
        "expected_revision":f.head("cloned-one"),"task":"git-head"}),
    )?;
    assert_eq!(built["success"], true, "{built}");
    assert_eq!(
        built["stdout"].as_str().unwrap().trim(),
        git(&f.root, &["rev-parse", "HEAD"])
    );
    assert_eq!(
        std::fs::read_to_string(f.root.join("README.md"))?,
        "uncommitted original content"
    );
    assert_eq!(std::fs::read(f.root.join(".git/config"))?, config);
    assert_eq!(std::fs::read(f.root.join(".git/index"))?, index);
    assert!(
        f.engine
            .call(
                "workspace_export",
                &json!({"workspace":"cloned-one","destination":repo.path})
            )
            .is_err()
    );
    Ok(())
}

#[test]
fn v20_clone_failure_preserves_paths() -> Result<()> {
    let f = Fixture::new();
    for source in [
        "--upload-pack=bad",
        "ext::bad",
        "https://name:secret@example.invalid/repo",
        "https://example.invalid/repo?token=secret",
    ] {
        assert!(
            f.engine
                .call("repo_clone", &json!({"repo":"bad","source":source}))
                .is_err()
        );
        assert!(!f.home.join("repos/bad.git").exists());
    }
    let occupied = f.home.join("repos/occupied.git");
    std::fs::create_dir_all(&occupied)?;
    std::fs::write(occupied.join("keep.txt"), "do not overwrite")?;
    f.engine
        .call("repo_clone", &json!({"repo":"occupied","source":f.root}))?;
    assert!(
        f.engine
            .call("repo_clone", &json!({"repo":"occupied","source":f.root}))
            .is_err()
    );
    assert_eq!(
        std::fs::read_to_string(occupied.join("keep.txt"))?,
        "do not overwrite"
    );
    let result = f.engine.call(
        "repo_clone",
        &json!({"repo":"failed","source":f._temp.path().join("missing")}),
    );
    assert!(result.is_err());
    assert!(f.engine.repo("failed").is_err());
    assert!(
        !f.home.join("repos/failed.git").exists(),
        "only the owned partial clone is removed"
    );
    Ok(())
}

#[test]
fn v20_registration_is_atomic() -> Result<()> {
    let f = Fixture::new();
    let engine = Arc::new(f.engine);
    let barrier = Arc::new(std::sync::Barrier::new(6));
    let mut handles = Vec::new();
    for _ in 0..6 {
        let engine = engine.clone();
        let barrier = barrier.clone();
        let root = f.root.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            engine
                .call("repo_register", &json!({"repo":"contended","path":root}))
                .is_ok()
        }));
    }
    assert_eq!(
        handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|ok| *ok)
            .count(),
        1
    );
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
    f.engine.call(
        "workspace_create",
        &json!({"repo":"repo","workspace":"retained","reference":commit["commit"]}),
    )?;
    assert_eq!(
        f.read("retained", "retained"),
        "export survives workspace drop"
    );
    Ok(())
}

#[test]
fn git_path_arguments() {
    #[cfg(windows)]
    {
        use std::path::Path;
        assert_eq!(
            vwork::git::path_argument(Path::new(r"\\?\C:\repo\.git")),
            "C:/repo/.git"
        );
        assert_eq!(
            vwork::git::path_argument(Path::new(r"\\?\UNC\server\share\repo")),
            "//server/share/repo"
        );
        assert_eq!(
            vwork::git::path_argument(Path::new(r"C:\repo with spaces")),
            "C:/repo with spaces"
        );
    }
    #[cfg(not(windows))]
    assert_eq!(
        vwork::git::path_argument(std::path::Path::new("/tmp/repo")),
        "/tmp/repo"
    );
}

#[test]
fn build_git_metadata() -> Result<()> {
    let f = Fixture::new();
    let original_head = git(&f.root, &["rev-parse", "HEAD"]);
    let original_index = std::fs::read(f.root.join(".git/index"))?;
    let object_files = || {
        walkdir::WalkDir::new(f.root.join(".git/objects"))
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .count()
    };
    let original_objects = object_files();
    f.engine.call("task_set",&json!({"repo":"repo","name":"describe","program":"git","args":["describe","--tags","--always","--dirty"]}))?;
    let clean = f.engine.call(
        "workspace_run_task",
        &json!({"workspace":"one","expected_revision":f.head("one"),"task":"describe"}),
    )?;
    assert_eq!(clean["success"], true);
    assert_eq!(clean["stdout"].as_str().unwrap().trim(), "v9.8.7");
    assert_eq!(clean["source_changes"], json!([]));
    f.write("one", "new-file", "virtual addition");
    let dirty = f.engine.call(
        "workspace_run_task",
        &json!({"workspace":"one","expected_revision":f.head("one"),"task":"describe"}),
    )?;
    assert_eq!(dirty["stdout"].as_str().unwrap().trim(), "v9.8.7-dirty");
    assert_eq!(dirty["source_changes"], json!([]));
    f.engine.call(
        "task_set",
        &json!({"repo":"repo","name":"head","program":"git","args":["rev-parse","HEAD"]}),
    )?;
    let head = f.engine.call(
        "workspace_run_task",
        &json!({"workspace":"one","expected_revision":f.head("one"),"task":"head"}),
    )?;
    assert_eq!(head["stdout"].as_str().unwrap().trim(), original_head);
    let build = PathBuf::from(head["checkout"].as_str().unwrap());
    assert!(build.join(".git").is_file());
    assert_eq!(git(&build, &["show", ":new-file"]), "virtual addition");
    assert!(
        !PathBuf::from(head["git_dir"].as_str().unwrap())
            .join("objects/info/alternates")
            .exists()
    );
    assert_eq!(git(&f.root, &["rev-parse", "HEAD"]), original_head);
    assert_eq!(std::fs::read(f.root.join(".git/index"))?, original_index);
    assert_eq!(object_files(), original_objects);
    assert!(!f.root.join("new-file").exists());
    f.write("one", "src/a.rs", "fn changed() {}\n");
    f.engine.call(
        "files_delete",
        &json!({"workspace":"one","expected_revision":f.head("one"),"path":"README.md"}),
    )?;
    let edits = f.engine.call(
        "workspace_run_task",
        &json!({"workspace":"one","expected_revision":f.head("one"),"task":"describe"}),
    )?;
    assert_eq!(edits["stdout"].as_str().unwrap().trim(), "v9.8.7-dirty");
    assert_eq!(edits["source_changes"], json!([]));
    assert!(git(&build, &["diff", "--cached", "--name-status"]).contains("D\tREADME.md"));
    assert_eq!(std::fs::read(f.root.join(".git/index"))?, original_index);
    assert_eq!(object_files(), original_objects);
    Ok(())
}

fn package_run(f: &Fixture, project: &str, action: &str) -> Result<Value> {
    f.engine.call("package_run",&json!({"workspace":"one","expected_revision":f.head("one"),"project":project,"action":action}))
}

#[test]
fn package_process_paths() -> Result<()> {
    let project: vwork::packages::Project = serde_json::from_value(json!({
        "repo":"repo", "name":"py", "manager":"pip"
    }))?;
    #[cfg(windows)]
    let home = PathBuf::from(r"\\?\C:\state with spaces");
    #[cfg(not(windows))]
    let home = PathBuf::from("/state with spaces");
    let plan = vwork::packages::plan(
        &project,
        "install",
        &home,
        &home.join("checkout"),
        &[(
            String::from("requirements.txt"),
            vwork::model::Entry {
                source: "git".into(),
                oid: "unused".into(),
                mode: "100644".into(),
                size: 0,
            },
        )]
        .into_iter()
        .collect(),
        false,
    )?;
    for step in &plan.steps {
        if let vwork::packages::Step::Command { program, args } = step {
            assert!(!program.contains(r"\\?\"));
            assert!(!args.iter().any(|a| a.contains(r"\\?\")));
        }
    }
    assert!(!plan.env["VIRTUAL_ENV"].contains(r"\\?\"));
    assert!(!plan.env["PATH"].starts_with(r"\\?\"));
    #[cfg(windows)]
    assert_eq!(
        plan.env["VIRTUAL_ENV"],
        "C:/state with spaces/environments/repo/py"
    );
    Ok(())
}

#[test]
fn package_workflows() -> Result<()> {
    let f = Fixture::new();
    f.write(
        "one",
        "rust/Cargo.toml",
        "[package]\nname='vwork-fixture'\nversion='0.1.0'\nedition='2021'\n",
    );
    f.write(
        "one",
        "rust/Cargo.lock",
        "version = 4\n[[package]]\nname = \"vwork-fixture\"\nversion = \"0.1.0\"\n",
    );
    f.write(
        "one",
        "rust/src/lib.rs",
        "#[test] fn passes() { assert_eq!(2+2,4); }\n",
    );
    f.write("one","rust/build.rs","fn main() { let r=std::process::Command::new(\"git\").args([\"rev-parse\",\"HEAD\"]).output().unwrap(); assert!(r.status.success()); assert_eq!(String::from_utf8(r.stdout).unwrap().trim(), std::env::var(\"VWORK_BASE_COMMIT\").unwrap()); }\n");
    f.engine.call("package_configure",&json!({"repo":"repo","name":"rust","manager":"cargo","path":"rust","args":["+stable"],"offline":true}))?;
    let result = package_run(&f, "rust", "test")?;
    assert_eq!(result["success"], true, "{result}");
    assert_eq!(result["source_changes"], json!([]));
    let target = PathBuf::from(result["checkout"].as_str().unwrap())
        .parent()
        .unwrap()
        .join("targets/repo/rust");
    assert!(target.is_dir());
    assert!(!f.root.join("rust").exists());
    assert!(
        f.engine.call("package_list", &json!({"repo":"repo"}))?["projects"]
            .as_array()
            .unwrap()
            .len()
            == 1
    );
    let result = package_run(&f, "rust", "clean")?;
    assert_eq!(result["success"], true);
    assert!(!target.exists());
    Ok(())
}

#[test]
fn package_writeback() -> Result<()> {
    let f = Fixture::new();
    f.write("one","web/package.json",r#"{"name":"vwork-fixture","version":"1.0.0","private":true,"scripts":{"test":"node check.cjs"}}"#);
    f.write(
        "one",
        "web/check.cjs",
        "if (!process.env.VWORK_BASE_COMMIT) process.exit(2); console.log('npm fixture passed');\n",
    );
    f.engine.call(
        "package_configure",
        &json!({"repo":"repo","name":"web","manager":"npm","path":"web","offline":true}),
    )?;
    assert!(package_run(&f, "web", "test").is_err());
    let head = f.head("one");
    let install = package_run(&f, "web", "install")?;
    assert_eq!(install["success"], true, "{install}");
    assert_eq!(f.head("one"), head);
    assert!(
        install["package"]["input_changes"]
            .as_array()
            .unwrap()
            .contains(&json!("web/package-lock.json"))
    );
    assert!(
        package_run(&f, "web", "test").is_err(),
        "new lock must be imported first"
    );
    f.engine.call("workspace_import",&json!({"workspace":"one","expected_revision":head,"source":install["checkout"],"paths":["web/package-lock.json"]}))?;
    let test = package_run(&f, "web", "test")?;
    assert_eq!(test["success"], true, "{test}");
    assert!(
        test["stdout"]
            .as_str()
            .unwrap()
            .contains("npm fixture passed")
    );
    f.write("one","web/package.json",r#"{"name":"vwork-fixture","version":"1.0.1","private":true,"scripts":{"test":"node check.cjs"}}"#);
    assert!(
        package_run(&f, "web", "test").is_err(),
        "changed package inputs must invalidate installed state"
    );
    Ok(())
}

#[test]
fn package_input_changes() -> Result<()> {
    let f = Fixture::new();
    f.write("one", "py/requirements.txt", "");
    f.engine.call(
        "package_configure",
        &json!({"repo":"repo","name":"py","manager":"pip","path":"py","offline":true,"program":std::env::var("VWORK_TEST_PYTHON").ok()}),
    )?;
    assert!(package_run(&f, "py", "check").is_err());
    let install = package_run(&f, "py", "install")?;
    assert_eq!(install["success"], true, "{install}");
    let check = package_run(&f, "py", "check")?;
    assert_eq!(check["success"], true, "{check}");
    let venv = PathBuf::from(install["package"]["environment"].as_str().unwrap());
    let python = vwork::packages::python_executable(&venv);
    assert!(python.is_file());
    assert!(!std::fs::symlink_metadata(&python)?.file_type().is_symlink());
    std::fs::write(venv.join("old-dependency-marker"), "old")?;
    f.write(
        "one",
        "py/requirements.txt",
        "# changed dependency inputs\n",
    );
    assert!(package_run(&f, "py", "check").is_err());
    let reinstall = package_run(&f, "py", "install")?;
    assert_eq!(reinstall["success"], true, "{reinstall}");
    assert!(!venv.join("old-dependency-marker").exists());
    let clean = package_run(&f, "py", "clean")?;
    assert_eq!(clean["success"], true);
    assert!(!venv.exists());
    Ok(())
}

#[test]
fn v21_redb_owns_git() -> Result<()> {
    let f = Fixture::new();
    let original = git(&f.root, &["rev-parse", "HEAD"]);
    let hidden = f._temp.path().join("removed-source");
    std::fs::rename(&f.root, &hidden)?;
    assert_eq!(f.read("one", "README.md"), "base");
    f.engine.call(
        "workspace_create",
        &json!({"repo":"repo","workspace":"independent","reference":"main"}),
    )?;
    assert!(
        f.engine.call("repo_log", &json!({"repo":"repo"}))?["log"]
            .as_str()
            .unwrap()
            .contains(&original)
    );
    f.write("one", "README.md", "owned by redb");
    let commit = f.engine.call(
        "workspace_commit",
        &json!({"workspace":"one","expected_revision":f.head("one"),"message":"database commit"}),
    )?;
    f.engine.call(
        "workspace_create",
        &json!({"repo":"repo","workspace":"committed","reference":commit["commit"]}),
    )?;
    assert_eq!(f.read("committed", "README.md"), "owned by redb");
    assert!(!f.home.join("repos").exists());
    let Fixture {
        engine,
        home,
        _temp,
        ..
    } = f;
    drop(engine);
    let reopened = Engine::open(home, Config::default())?;
    assert_eq!(
        reopened.call(
            "files_read",
            &json!({"workspace":"committed","path":"README.md"})
        )?["content"],
        "owned by redb"
    );
    Ok(())
}

#[test]
fn v22_temporary_lifecycle() -> Result<()> {
    let f = Fixture::new();
    let lease = f.engine.call("workspace_temporary", &json!({"workspace":"one","expected_revision":f.head("one"),"purpose":"filesystem-only test","ttl_seconds":60}))?;
    let root = PathBuf::from(lease["path"].as_str().unwrap());
    assert!(root.join("README.md").is_file());
    assert_eq!(
        git(&root, &["rev-parse", "HEAD"]),
        git(&f.root, &["rev-parse", "HEAD"])
    );
    f.engine.call(
        "task_set",
        &json!({"repo":"repo","name":"in-copy","program":"git","args":["rev-parse","HEAD"]}),
    )?;
    let run = f.engine.call("workspace_run_task", &json!({"workspace":"one","expected_revision":f.head("one"),"task":"in-copy","lease":lease["id"]}))?;
    assert_eq!(run["success"], true, "{run}");
    assert_eq!(run["temporary"]["id"], lease["id"]);
    assert_eq!(
        f.engine.call("temporary_list", &json!({}))?["active"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let renewed = f.engine.call(
        "temporary_renew",
        &json!({"lease":lease["id"],"ttl_seconds":120}),
    )?;
    assert!(renewed["expires_at"].as_u64() >= run["temporary"]["expires_at"].as_u64());
    let released = f
        .engine
        .call("temporary_release", &json!({"lease":lease["id"]}))?;
    assert_eq!(released["released"], true);
    assert!(!root.exists());
    assert_eq!(
        f.engine.call("temporary_list", &json!({}))?["active"],
        json!([])
    );
    Ok(())
}

#[test]
fn v22_temporary_recovery() -> Result<()> {
    let f = Fixture::new();
    let lease = f.engine.call("workspace_temporary", &json!({"workspace":"one","expected_revision":f.head("one"),"purpose":"edit with external tool","ttl_seconds":1}))?;
    let root = PathBuf::from(lease["path"].as_str().unwrap());
    std::fs::write(root.join("README.md"), "recover this edit")?;
    std::fs::write(root.join("new.txt"), "recover new file")?;
    let Fixture {
        engine,
        home,
        _temp,
        ..
    } = f;
    drop(engine);
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let reopened = Engine::open(home, Config::default())?;
    assert!(!root.exists(), "expired copies are reclaimed after restart");
    let history = reopened.call("temporary_list", &json!({}))?;
    let recovery = &history["recent"][0]["recovery_workspace"];
    assert!(recovery.is_string(), "{history}");
    assert_eq!(
        reopened.call(
            "files_read",
            &json!({"workspace":recovery,"path":"README.md"})
        )?["content"],
        "recover this edit"
    );
    assert_eq!(
        reopened.call(
            "files_read",
            &json!({"workspace":recovery,"path":"new.txt"})
        )?["content"],
        "recover new file"
    );
    assert_eq!(
        reopened.call("files_read", &json!({"workspace":"one","path":"README.md"}))?["content"],
        "base"
    );
    Ok(())
}

#[test]
fn v22_cleanup_ownership() -> Result<()> {
    let f = Fixture::new();
    let lease = f.engine.call(
        "workspace_temporary",
        &json!({"workspace":"one","expected_revision":f.head("one"),"purpose":"ownership test"}),
    )?;
    let tree = PathBuf::from(lease["path"].as_str().unwrap());
    let marker = tree.parent().unwrap().join("owner");
    std::fs::write(&marker, "not owned")?;
    assert!(
        f.engine
            .call("temporary_release", &json!({"lease":lease["id"]}))
            .is_err()
    );
    assert!(tree.join("README.md").exists());
    assert!(
        f.engine
            .call("temporary_release", &json!({"lease":"../escape"}))
            .is_err()
    );
    Ok(())
}

#[test]
fn v23_uv_cdk_plans() -> Result<()> {
    use vwork::packages::{Project, Step, plan};
    let temp = tempfile::tempdir()?;
    let checkout = temp.path().join("tree");
    let tree = vwork::model::Tree::new();
    let uv: Project =
        serde_json::from_value(json!({"repo":"r","name":"python","manager":"uv","offline":true}))?;
    let sync = plan(&uv, "install", temp.path(), &checkout, &tree, false)?;
    assert!(sync.environment.as_ref().unwrap().starts_with(temp.path()));
    assert!(sync.env["UV_CACHE_DIR"].contains(temp.path().file_name().unwrap().to_str().unwrap()));
    assert_eq!(sync.env["UV_OFFLINE"], "true");
    assert!(matches!(&sync.steps[0], Step::Command { args, .. } if args[0] == "sync"));
    assert!(plan(&uv, "test", temp.path(), &checkout, &tree, false).is_err());
    let cdk: Project = serde_json::from_value(json!({"repo":"r","name":"infra","manager":"cdk"}))?;
    let synth = plan(&cdk, "synth", temp.path(), &checkout, &tree, false)?;
    assert!(
        matches!(&synth.steps[0], Step::Command { args, .. } if args.contains(&"--no-lookups".to_string()) && args.contains(&"--output".to_string()))
    );
    for action in ["deploy", "destroy", "bootstrap", "watch", "publish-assets"] {
        assert!(plan(&cdk, action, temp.path(), &checkout, &tree, false).is_err());
    }
    Ok(())
}

#[test]
fn v21_legacy_migration() -> Result<()> {
    use redb::{Database, ReadableTable, TableDefinition};
    let f = Fixture::new();
    f.write("one", "README.md", "legacy edit");
    let original_index = std::fs::read(f.root.join(".git/index"))?;
    let Fixture {
        engine,
        home,
        root,
        _temp,
    } = f;
    drop(engine);
    let db = Database::create(home.join("workspaces.redb"))?;
    let tx = db.begin_write()?;
    {
        let mut table = tx.open_table(TableDefinition::<&str, &[u8]>::new("records_v1"))?;
        let mut repo: Value = serde_json::from_slice(table.get("r/repo")?.unwrap().value())?;
        for key in ["stored", "refs", "author_name", "author_email"] {
            repo.as_object_mut().unwrap().remove(key);
        }
        table.insert("r/repo", serde_json::to_vec(&repo)?.as_slice())?;
        table.insert("schema", b"1".as_slice())?;
    }
    tx.delete_table(vwork::git_store::OBJECTS)?;
    tx.delete_table(vwork::git_store::MEMBERS)?;
    tx.commit()?;
    drop(db);
    let reopened = Engine::open(&home, Config::default())?;
    assert!(reopened.repo("repo")?.stored);
    std::fs::rename(&root, _temp.path().join("source-gone"))?;
    assert_eq!(
        reopened.call("files_read", &json!({"workspace":"one","path":"README.md"}))?["content"],
        "legacy edit"
    );
    assert_eq!(
        reopened.call("files_read", &json!({"workspace":"one","path":"src/a.rs"}))?["content"],
        "fn alpha() {}"
    );
    assert_eq!(
        std::fs::read(_temp.path().join("source-gone/.git/index"))?,
        original_index
    );
    Ok(())
}

#[test]
fn v22_temporary_bounds_and_active_job() -> Result<()> {
    let f = Fixture::new();
    let mut leases = Vec::new();
    for _ in 0..f.engine.config.max_temporary_copies {
        leases.push(f.engine.call("workspace_temporary", &json!({"workspace":"one","expected_revision":f.head("one"),"purpose":"bounded fixture"}))?);
    }
    assert!(
        f.engine
            .call(
                "workspace_temporary",
                &json!({"workspace":"one","expected_revision":f.head("one"),"purpose":"too many"})
            )
            .is_err()
    );
    assert!(
        f.engine
            .call(
                "temporary_renew",
                &json!({"lease":leases[0]["id"],"ttl_seconds":86401})
            )
            .is_err()
    );
    for lease in leases {
        f.engine
            .call("temporary_release", &json!({"lease":lease["id"]}))?;
    }
    #[cfg(windows)]
    let (program, args) = ("cmd", vec!["/C", "ping -n 4 127.0.0.1 >NUL"]);
    #[cfg(unix)]
    let (program, args) = ("/bin/sh", vec!["-c", "sleep 3"]);
    f.engine.call(
        "task_set",
        &json!({"repo":"repo","name":"busy","program":program,"args":args}),
    )?;
    let head = f.head("one");
    let engine = Arc::new(f.engine);
    let worker = engine.clone();
    let job = std::thread::spawn(move || {
        worker.call(
            "workspace_run_task",
            &json!({"workspace":"one","expected_revision":head,"task":"busy"}),
        )
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let lease = loop {
        let active = engine.call("temporary_list", &json!({}))?;
        if active["active"][0]["state"] == "ready" {
            break active["active"][0].clone();
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(10));
    };
    assert!(
        engine
            .call("temporary_release", &json!({"lease":lease["id"]}))
            .is_err()
    );
    engine.reap_temporary()?;
    assert!(PathBuf::from(lease["path"].as_str().unwrap()).exists());
    assert_eq!(job.join().unwrap()?["success"], true);
    engine.call("temporary_release", &json!({"lease":lease["id"]}))?;
    Ok(())
}

#[test]
fn v22_legacy_build_cleanup() -> Result<()> {
    let f = Fixture::new();
    let workspace = f.engine.workspace("one")?;
    let (_, tree) = f.engine.snapshot(&workspace)?;
    let legacy = f.home.join("build/repo");
    f.engine.materialize(&workspace, &tree, &legacy)?;
    std::fs::write(legacy.join("README.md"), "old build edit")?;
    std::fs::create_dir_all(f.home.join("targets/repo/rust"))?;
    std::fs::write(f.home.join("targets/repo/rust/disposable"), "cache")?;
    let Fixture {
        engine,
        home,
        _temp,
        ..
    } = f;
    drop(engine);
    let reopened = Engine::open(&home, Config::default())?;
    assert!(!legacy.exists());
    assert!(!home.join("targets/repo").exists());
    let recent = reopened.call("temporary_list", &json!({}))?;
    let recovered = &recent["recent"][0]["recovery_workspace"];
    assert!(recovered.is_string(), "{recent}");
    assert_eq!(
        reopened.call(
            "files_read",
            &json!({"workspace":recovered,"path":"README.md"})
        )?["content"],
        "old build edit"
    );
    Ok(())
}

#[test]
fn v23_uv_cdk_execution() -> Result<()> {
    // Full CLI execution is enabled in CI, where uv and CDK are provisioned.
    if std::env::var_os("VWORK_TEST_PACKAGE_TOOLS").is_none() {
        return Ok(());
    }
    let f = Fixture::new();
    f.write("one", "python/pyproject.toml", "[project]\nname='vwork-uv-fixture'\nversion='0.1.0'\nrequires-python='>=3.10'\ndependencies=[]\n[tool.uv]\npackage=false\n");
    f.engine.call("package_configure", &json!({"repo":"repo","name":"uv","manager":"uv","path":"python","offline":true,"env":{"UV_PYTHON_DOWNLOADS":"never"}}))?;
    let install = package_run(&f, "uv", "install")?;
    assert_eq!(install["success"], true, "{install}");
    assert!(PathBuf::from(install["package"]["environment"].as_str().unwrap()).is_dir());
    f.engine.call("workspace_import", &json!({"workspace":"one","expected_revision":f.head("one"),"source":install["checkout"],"paths":["python/uv.lock"]}))?;
    let check = package_run(&f, "uv", "check")?;
    assert_eq!(check["success"], true, "{check}");
    f.write("one", "infra/cdk.json", r#"{"app":"node app.cjs"}"#);
    f.write("one", "infra/app.cjs", r#"const fs=require('fs'),p=require('path'),d=process.env.CDK_OUTDIR;fs.mkdirSync(d,{recursive:true});fs.writeFileSync(p.join(d,'Fixture.template.json'),JSON.stringify({Resources:{}}));fs.writeFileSync(p.join(d,'manifest.json'),JSON.stringify({version:'36.0.0',artifacts:{Fixture:{type:'aws:cloudformation:stack',environment:'aws://unknown-account/unknown-region',properties:{templateFile:'Fixture.template.json'}}}}));"#);
    f.engine.call(
        "package_configure",
        &json!({"repo":"repo","name":"infra","manager":"cdk","path":"infra"}),
    )?;
    let synth = package_run(&f, "infra", "synth")?;
    assert_eq!(synth["success"], true, "{synth}");
    let artifacts = PathBuf::from(synth["package"]["artifacts"].as_str().unwrap());
    assert!(artifacts.join("Fixture.template.json").is_file());
    let list = package_run(&f, "infra", "list")?;
    assert_eq!(list["success"], true, "{list}");
    assert!(list["stdout"].as_str().unwrap().contains("Fixture"));
    let root = PathBuf::from(synth["checkout"].as_str().unwrap());
    f.engine.call(
        "temporary_release",
        &json!({"lease":synth["temporary"]["id"]}),
    )?;
    assert!(!root.exists());
    assert!(!artifacts.exists());
    assert!(
        package_run(&f, "uv", "check").is_err(),
        "released environment cannot remain installed"
    );
    Ok(())
}

#[test]
fn v21_clone_transport() -> Result<()> {
    let f = Fixture::new();
    let path = vwork::git::path_argument(&f.root)
        .to_string_lossy()
        .replace(' ', "%20");
    let source = format!("file://{}{}", if cfg!(windows) { "/" } else { "" }, path);
    let before = f.engine.call("storage_info", &json!({}))?;
    let result = f
        .engine
        .call("repo_clone", &json!({"repo":"transport","source":source}))?;
    assert_eq!(result["storage"], "redb");
    assert_eq!(
        f.engine.call("storage_info", &json!({}))?["git_objects"],
        before["git_objects"]
    );
    assert_eq!(
        f.engine.call("temporary_list", &json!({}))?["transfers"],
        json!([])
    );
    assert!(
        std::fs::read_dir(f.home.join("transfers"))?
            .next()
            .is_none()
    );
    std::fs::rename(&f.root, f._temp.path().join("source-removed"))?;
    f.engine.call(
        "workspace_create",
        &json!({"repo":"transport","workspace":"transported"}),
    )?;
    assert_eq!(f.read("transported", "README.md"), "base");
    Ok(())
}

#[test]
fn v22_recovery_limit_preserves_copy() -> Result<()> {
    let mut f = Fixture::new();
    f.engine.config.max_recovery_bytes = 1;
    let lease = f.engine.call("workspace_temporary", &json!({"workspace":"one","expected_revision":f.head("one"),"purpose":"recovery limit fixture"}))?;
    let root = PathBuf::from(lease["path"].as_str().unwrap());
    std::fs::write(root.join("README.md"), "keep me")?;
    assert!(
        f.engine
            .call("temporary_release", &json!({"lease":lease["id"]}))
            .is_err()
    );
    assert!(root.exists());
    assert!(
        f.engine.call("temporary_list", &json!({}))?["active"][0]["cleanup_error"]
            .as_str()
            .unwrap()
            .contains("recovery limit")
    );
    f.engine.call(
        "temporary_release",
        &json!({"lease":lease["id"],"discard_changes":true}),
    )?;
    assert!(!root.exists());
    assert_eq!(f.read("one", "README.md"), "base");
    Ok(())
}

#[test]
fn v21_import_batches() -> Result<()> {
    let f = Fixture::new();
    let source = git2::Repository::open(&f.root)?;
    let mut tree = source.treebuilder(None)?;
    for n in 0..300 {
        tree.insert(
            format!("source-{n:03}.txt"),
            source.blob(format!("contents {n}").as_bytes())?,
            0o100644,
        )?;
    }
    let tree = tree.write()?;
    let parent = source.head()?.peel_to_commit()?;
    let sig = source.signature()?;
    source.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "batched objects",
        &source.find_tree(tree)?,
        &[&parent],
    )?;
    f.engine
        .call("repo_register", &json!({"repo":"batch","path":f.root}))?;
    f.engine.call(
        "workspace_create",
        &json!({"repo":"batch","workspace":"batch"}),
    )?;
    assert_eq!(f.read("batch", "source-299.txt"), "contents 299");
    assert_eq!(f.read("batch", "source-000.txt"), "contents 0");
    assert_eq!(f.read("one", "README.md"), "base");
    assert!(
        f.engine.call("storage_info", &json!({}))?["git_objects"]
            .as_u64()
            .unwrap()
            > 300
    );
    Ok(())
}

#[test]
fn v21_shared_objects_keep_repository_membership() -> Result<()> {
    let f = Fixture::new();
    let other_path = f._temp.path().join("other");
    let other = git2::Repository::init(&other_path)?;
    let bytes = b"content shared between independent repositories\n";
    let blob = other.blob(bytes)?;
    let tree = {
        let mut t = other.treebuilder(None)?;
        t.insert("other.txt", blob, 0o100644)?;
        t.write()?
    };
    let sig = git2::Signature::now("Other", "other@example.invalid")?;
    let other_commit = other.commit(
        Some("HEAD"),
        &sig,
        &sig,
        "unrelated",
        &other.find_tree(tree)?,
        &[],
    )?;
    f.engine
        .call("repo_register", &json!({"repo":"other","path":other_path}))?;
    assert!(f.engine.call("workspace_create", &json!({"repo":"repo","workspace":"wrong-repo","reference":other_commit.to_string()})).is_err(), "a globally stored object does not make an unrelated commit part of this repo");
    f.write("one", "README.md", std::str::from_utf8(bytes)?);
    let commit = f.engine.call("workspace_commit", &json!({"workspace":"one","expected_revision":f.head("one"),"message":"reuse existing shared blob"}))?;
    let lease = f.engine.call("workspace_temporary", &json!({"workspace":"one","expected_revision":f.head("one"),"purpose":"verify shared object membership"}))?;
    assert_eq!(
        git(
            std::path::Path::new(lease["path"].as_str().unwrap()),
            &[
                "show",
                &format!("{}:README.md", commit["commit"].as_str().unwrap())
            ]
        ),
        std::str::from_utf8(bytes)?.trim()
    );
    Ok(())
}
