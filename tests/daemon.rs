use anyhow::Result;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    time::Duration,
};

fn executable() -> std::ffi::OsString {
    std::env::var_os("VWORK_TEST_EXE").unwrap_or_else(|| env!("CARGO_BIN_EXE_vwork").into())
}

fn wait_output(child: std::process::Child) -> Result<std::process::Output> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    Ok(rx.recv_timeout(Duration::from_secs(25))??)
}

struct StopHome(std::path::PathBuf);
impl Drop for StopHome {
    fn drop(&mut self) {
        if vwork::startup::health(&self.0, Duration::from_millis(250)).is_ok() {
            let _ = vwork::transport::connect(&self.0, "stop", &json!({}));
            for _ in 0..100 {
                if !self.0.join("connection.json").exists() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

#[test]
fn v16_concurrent_starters() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("shared state");
    let _cleanup = StopHome(home.clone());
    std::fs::create_dir_all(&home)?;
    // Dead loopback metadata from a previous daemon must not prevent cold start.
    std::fs::write(
        home.join("connection.json"),
        br#"{"url":"http://127.0.0.1:9/","token":"fixture","pid":0}"#,
    )?;
    let mut clients = Vec::new();
    for _ in 0..6 {
        clients.push(
            Command::new(executable())
                .arg("--home")
                .arg(&home)
                .arg("start")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
    }
    let mut owner = None;
    for client in clients {
        let output = wait_output(client)?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let current = std::fs::read(home.join("connection.json"))?;
        if let Some(expected) = &owner {
            assert_eq!(&current, expected);
        }
        owner = Some(current);
    }
    assert!(mcp_exchange(&home, &[])?.status.success());
    assert_eq!(std::fs::read(home.join("connection.json"))?, owner.unwrap());
    Ok(())
}

#[test]
fn v16_mcp_opt_out() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("state");
    let output = mcp_exchange(&home, &["--no-auto-start"])?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("auto-start is disabled"));
    assert!(output.stdout.is_empty());
    assert!(!home.exists());
    let _cleanup = StopHome(home.clone());
    assert!(mcp_exchange(&home, &[])?.status.success());
    let connection = std::fs::read(home.join("connection.json"))?;
    assert!(mcp_exchange(&home, &["--no-auto-start"])?.status.success());
    assert_eq!(std::fs::read(home.join("connection.json"))?, connection);
    let bad = mcp_exchange(&home, &["--workspace", "missing"])?;
    assert!(!bad.status.success());
    assert!(
        bad.stdout.is_empty(),
        "invalid binding must fail before MCP initialization"
    );
    assert_eq!(std::fs::read(home.join("connection.json"))?, connection);
    Ok(())
}

#[test]
fn v16_startup_bounds() -> Result<()> {
    use fs2::FileExt;
    use std::time::Instant;
    let temp = tempfile::tempdir()?;
    let home = temp.path();
    let file = |name: &str| {
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.join(name))
    };
    let owner = file("daemon.lock")?;
    owner.lock_exclusive()?;
    // A connection that accepts HTTP but never responds must not use the build timeout.
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std::fs::write(
        home.join("connection.json"),
        serde_json::to_vec(
            &json!({"url":format!("http://{}/",listener.local_addr()?),"token":"fixture","pid":0}),
        )?,
    )?;
    let before = std::fs::read(home.join("connection.json"))?;
    let start = Instant::now();
    let error = vwork::startup::ensure_started_with_timeout(
        home,
        std::path::Path::new("must-not-spawn"),
        Duration::from_millis(300),
    )
    .unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(format!("{error:#}").contains("timed out"));
    assert_eq!(std::fs::read(home.join("connection.json"))?, before);
    assert!(!home.join("daemon.log").exists());
    drop(listener);
    std::fs::remove_file(home.join("connection.json"))?;
    let error = vwork::startup::ensure_started_with_timeout(
        home,
        std::path::Path::new("must-not-spawn"),
        Duration::from_millis(150),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("owner was left running"));
    assert!(!home.join("daemon.log").exists());
    FileExt::unlock(&owner)?;
    let starter = file("startup.lock")?;
    starter.lock_exclusive()?;
    let start = Instant::now();
    let error = vwork::startup::ensure_started_with_timeout(
        home,
        std::path::Path::new("must-not-spawn"),
        Duration::from_millis(150),
    )
    .unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(format!("{error:#}").contains("another starter"));
    FileExt::unlock(&starter)?;
    let error = vwork::startup::ensure_started_with_timeout(
        home,
        &home.join("missing-program"),
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("could not start vwork daemon"));
    Ok(())
}

fn mcp_exchange(home: &std::path::Path, extra: &[&str]) -> Result<std::process::Output> {
    let mut child = Command::new(executable())
        .arg("--home")
        .arg(home)
        .arg("mcp")
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n")?;
    wait_output(child)
}

#[test]
fn v16_mcp_cold_start_and_reuse() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("state with spaces");
    let _cleanup = StopHome(home.clone());
    let first = mcp_exchange(&home, &[])?;
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(first.stdout)?
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(lines.len(), 2, "MCP stdout must contain only its replies");
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(lines[1]["id"], 2);
    assert!(!lines[1]["result"]["tools"].as_array().unwrap().is_empty());
    assert!(
        vwork::transport::connect(&home, "health", &json!({})).is_ok(),
        "MCP must start the daemon and leave it alive on exit"
    );
    let connection = std::fs::read(home.join("connection.json"))?;
    assert!(mcp_exchange(&home, &[])?.status.success());
    assert_eq!(
        std::fs::read(home.join("connection.json"))?,
        connection,
        "reuse the same daemon and credentials"
    );
    Ok(())
}

#[test]
fn v17_generated_hook_matcher() -> Result<()> {
    let temp = tempfile::tempdir()?;
    vwork::integration::goose(temp.path(), "one", temp.path())?;
    let hooks: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("plugin/hooks/hooks.json"))?)?;
    let rule = &hooks["hooks"]["PreToolUse"][0];
    let pattern = rule["matcher"].as_str().expect("filter before spawning sh");
    let matcher = regex::Regex::new(pattern)?;
    assert_eq!(rule["hooks"][0]["on_failure"], "block");
    for name in [
        "shell",
        "write",
        "edit",
        "tree",
        "text_editor",
        "read_file",
        "write_file",
        "edit_file",
        "list_directory",
        "search_files",
        "execute",
        "run_command",
        "developer__shell",
        "developer__future_tool",
        "filesystem__read",
        "terminal__execute",
        "DEVELOPER__SHELL",
        "READ_FILE",
    ] {
        assert!(matcher.is_match(name), "must check {name}");
        assert_eq!(
            vwork::protocol::hook(&json!({"tool_name":name}))["decision"],
            "block"
        );
    }
    for tool in vwork::protocol::tools(true) {
        let name = tool["name"].as_str().unwrap();
        assert!(!matcher.is_match(name));
        assert!(!matcher.is_match(&format!("vwork__{name}")));
    }
    for name in ["skills__loadskill", "apps__list", "tom__read", "not_shell"] {
        assert!(!matcher.is_match(name));
    }
    Ok(())
}
#[test]
fn daemon_authentication_and_stdio() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("state");
    let exe = executable();
    let mut daemon = Command::new(&exe)
        .arg("--home")
        .arg(&home)
        .arg("serve")
        .stdout(Stdio::null())
        .spawn()?;
    struct Cleanup(u32);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            #[cfg(unix)]
            unsafe {
                libc::kill(self.0 as i32, libc::SIGTERM);
            }
            #[cfg(windows)]
            {
                let _ = Command::new("taskkill")
                    .args(["/PID", &self.0.to_string(), "/F"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status();
            }
        }
    }
    let _cleanup = Cleanup(daemon.id());
    let mut ready = false;
    for _ in 0..100 {
        if vwork::transport::connect(&home, "health", &json!({})).is_ok() {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(ready, "daemon did not start");
    let conn: Value = serde_json::from_slice(&std::fs::read(home.join("connection.json"))?)?;
    // A corporate proxy configuration must never intercept daemon traffic or its bearer token.
    let direct = Command::new(&exe)
        .arg("--home")
        .arg(&home)
        .args(["call", "health"])
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("http_proxy", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("all_proxy", "http://127.0.0.1:9")
        .env_remove("NO_PROXY")
        .env_remove("no_proxy")
        .output()?;
    assert!(
        direct.status.success(),
        "{}",
        String::from_utf8_lossy(&direct.stderr)
    );
    let mut malformed = conn.clone();
    malformed["url"] = json!("http://127.0.0.1:9@invalid.invalid/");
    std::fs::write(
        home.join("connection.json"),
        serde_json::to_vec(&malformed)?,
    )?;
    assert!(vwork::transport::connect(&home, "health", &json!({})).is_err());
    std::fs::write(home.join("connection.json"), serde_json::to_vec(&conn)?)?;
    assert!(
        ureq::post(conn["url"].as_str().unwrap())
            .send_json(json!({"operation":"health"}))
            .is_err()
    );
    let mut bridge = Command::new(&exe)
        .arg("--home")
        .arg(&home)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut input = bridge.stdin.take().unwrap();
    let mut output = BufReader::new(bridge.stdout.take().unwrap());
    for (id, method) in [(1, "initialize"), (2, "tools/list")] {
        writeln!(
            input,
            "{}",
            json!({"jsonrpc":"2.0","id":id,"method":method,"params":{"protocolVersion":"2025-06-18"}})
        )?;
        input.flush()?;
        let mut line = String::new();
        output.read_line(&mut line)?;
        let result: Value = serde_json::from_str(&line)?;
        assert_eq!(result["id"], id);
        assert!(result.get("result").is_some());
    }
    drop(input);
    assert!(bridge.wait()?.success());
    vwork::transport::connect(&home, "stop", &json!({}))?;
    assert!(daemon.wait()?.success());
    Ok(())
}

#[test]
fn generated_goose_configuration() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let output = temp.path().join("goose settings");
    let exe = executable();
    let status = Command::new(&exe)
        .args([
            "--home",
            temp.path().to_str().unwrap(),
            "goose",
            "--workspace",
            "example",
            "--output",
            output.to_str().unwrap(),
        ])
        .output()?;
    assert!(
        status.status.success(),
        "{}",
        String::from_utf8_lossy(&status.stderr)
    );
    let config: Value = serde_yaml::from_str(&std::fs::read_to_string(
        output.join("config.fragment.yaml"),
    )?)?;
    assert_eq!(config["extensions"]["developer"]["enabled"], false);
    assert_eq!(config["extensions"]["vwork"]["args"][4], "example");
    let hooks: Value =
        serde_json::from_slice(&std::fs::read(output.join("plugin/hooks/hooks.json"))?)?;
    let command = hooks["hooks"]["PreToolUse"][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(!command.contains("//?/"));
    #[cfg(unix)]
    let mut hook = Command::new("sh")
        .args(["-c", command])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    #[cfg(windows)]
    let mut hook = Command::new(&exe)
        .arg("hook")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    hook.stdin
        .take()
        .unwrap()
        .write_all(br#"{"tool_name":"shell"}"#)?;
    let result = hook.wait_with_output()?;
    assert!(result.status.success());
    let decision: Value = serde_json::from_slice(&result.stdout)?;
    assert_eq!(decision["decision"], "block");
    Ok(())
}

#[test]
fn v22_periodic_cleanup() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let source_path = temp.path().join("source");
    let source = git2::Repository::init(&source_path)?;
    let blob = source.blob(b"base\n")?;
    let tree = {
        let mut tree = source.treebuilder(None)?;
        tree.insert("README.md", blob, 0o100644)?;
        tree.write()?
    };
    let signature = git2::Signature::now("Fixture", "fixture@example.invalid")?;
    source.commit(
        Some("HEAD"),
        &signature,
        &signature,
        "base",
        &source.find_tree(tree)?,
        &[],
    )?;
    drop(source);
    let home = temp.path().join("state");
    let _cleanup = StopHome(home.clone());
    assert!(mcp_exchange(&home, &[])?.status.success());
    vwork::transport::connect(
        &home,
        "repo_register",
        &json!({"repo":"repo","path":source_path}),
    )?;
    vwork::transport::connect(
        &home,
        "workspace_create",
        &json!({"workspace":"one","repo":"repo"}),
    )?;
    let info = vwork::transport::connect(&home, "workspace_info", &json!({"workspace":"one"}))?;
    let lease = vwork::transport::connect(
        &home,
        "workspace_temporary",
        &json!({"workspace":"one","expected_revision":info["revision"],"purpose":"periodic cleanup fixture","ttl_seconds":1}),
    )?;
    let path = std::path::PathBuf::from(lease["path"].as_str().unwrap());
    assert!(path.join("README.md").exists());
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while path.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !path.exists(),
        "daemon must reap expired copies without another model request"
    );
    assert_eq!(
        vwork::transport::connect(&home, "temporary_list", &json!({}))?["active"],
        json!([])
    );
    Ok(())
}
