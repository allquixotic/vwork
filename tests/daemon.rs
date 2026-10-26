use anyhow::Result;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    time::Duration,
};
#[test]
fn daemon_authentication_and_stdio() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("state");
    let exe = env!("CARGO_BIN_EXE_vwork");
    let mut daemon = Command::new(exe)
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
    assert!(
        ureq::post(conn["url"].as_str().unwrap())
            .send_json(json!({"operation":"health"}))
            .is_err()
    );
    let mut bridge = Command::new(exe)
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
    let exe = env!("CARGO_BIN_EXE_vwork");
    let status = Command::new(exe)
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
    let mut hook = Command::new(exe)
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
