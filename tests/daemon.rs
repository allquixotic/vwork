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
