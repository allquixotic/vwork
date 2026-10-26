use crate::{
    engine::Engine,
    model::{Config, Connection, id},
    protocol,
};
use anyhow::{Context, Result, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

pub fn load_config(home: &Path) -> Result<Config> {
    let p = home.join("config.toml");
    let c: Config = if p.exists() {
        toml::from_str(&std::fs::read_to_string(p)?)?
    } else {
        Config::default()
    };
    c.validate()?;
    Ok(c)
}
pub fn initialize(home: &Path) -> Result<()> {
    std::fs::create_dir_all(home)?;
    let p = home.join("config.toml");
    if !p.exists() {
        std::fs::write(p, toml::to_string_pretty(&Config::default())?)?;
    }
    Ok(())
}
pub fn connect(home: &Path, op: &str, args: &Value) -> Result<Value> {
    connect_with_timeout(home, op, args, std::time::Duration::from_secs(86400))
}

pub(crate) fn connect_with_timeout(
    home: &Path,
    op: &str,
    args: &Value,
    timeout: std::time::Duration,
) -> Result<Value> {
    ensure!(!timeout.is_zero(), "daemon request timed out");
    let c: Connection = serde_json::from_slice(
        &std::fs::read(home.join("connection.json"))
            .context("daemon not running; run vwork start")?,
    )?;
    let address: std::net::SocketAddr = c
        .url
        .strip_prefix("http://")
        .and_then(|url| url.strip_suffix('/'))
        .context("invalid daemon address")?
        .parse()
        .context("invalid daemon socket address")?;
    ensure!(
        address.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        "refusing non-loopback daemon address"
    );
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .timeout_global(Some(timeout))
        .build()
        .into();
    let mut response = agent
        .post(&c.url)
        .header("Authorization", &format!("Bearer {}", c.token))
        .send_json(json!({"operation":op,"arguments":args}))
        .context("daemon unavailable; run vwork start or inspect daemon.log")?;
    let value: Value = response
        .body_mut()
        .with_config()
        .limit(128 * 1024 * 1024)
        .read_json()?;
    if let Some(error) = value.get("error") {
        anyhow::bail!("{}", error.as_str().unwrap_or("daemon error"));
    }
    Ok(value["result"].clone())
}
pub fn serve(home: &Path) -> Result<()> {
    initialize(home)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.join("daemon.lock"))?;
    lock.try_lock_exclusive()
        .context("another daemon owns this home")?;
    let config = load_config(home)?;
    let server = tiny_http::Server::http(format!("127.0.0.1:{}", config.port))
        .map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let address = server.server_addr().to_ip().context("not an IP server")?;
    let c = Connection {
        url: format!("http://127.0.0.1:{}/", address.port()),
        token: format!("{}{}", id(), id()),
        pid: std::process::id(),
    };
    let engine = Arc::new(Engine::open(home, config)?);
    let conn_path = home.join("connection.json");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut f = options.open(&conn_path)?;
    f.write_all(&serde_json::to_vec(&c)?)?;
    f.sync_all()?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopper = stop.clone();
    ctrlc::set_handler(move || stopper.store(true, Ordering::SeqCst))?;
    let reaper_engine = engine.clone();
    let reaper_stop = stop.clone();
    let reaper = std::thread::spawn(move || {
        let mut next = std::time::Instant::now();
        while !reaper_stop.load(Ordering::SeqCst) {
            if std::time::Instant::now() >= next {
                if let Err(error) = reaper_engine.reap_temporary() {
                    eprintln!("temporary cleanup: {error:#}");
                }
                next = std::time::Instant::now() + std::time::Duration::from_secs(5);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
    let active = Arc::new(AtomicUsize::new(0));
    let mut workers = Vec::new();
    while !stop.load(Ordering::SeqCst) {
        let Some(mut request) = server.recv_timeout(std::time::Duration::from_millis(200))? else {
            continue;
        };
        let auth = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Authorization"))
            .map(|h| h.value.as_str());
        if auth != Some(format!("Bearer {}", c.token).as_str()) {
            let _ = request
                .respond(tiny_http::Response::from_string("unauthorized").with_status_code(401));
            continue;
        }
        if request.method() != &tiny_http::Method::Post {
            let _ = request
                .respond(tiny_http::Response::from_string("POST required").with_status_code(405));
            continue;
        }
        if active.load(Ordering::SeqCst) >= 32 {
            let _ = request.respond(tiny_http::Response::from_string("busy").with_status_code(503));
            continue;
        }
        let max = engine
            .config
            .max_file_bytes
            .saturating_mul(2)
            .max(1024 * 1024);
        let mut body = Vec::new();
        request
            .as_reader()
            .take(max as u64 + 1)
            .read_to_end(&mut body)?;
        if body.len() > max {
            let _ = request
                .respond(tiny_http::Response::from_string("body too large").with_status_code(413));
            continue;
        }
        let engine = engine.clone();
        let stop = stop.clone();
        let active = active.clone();
        active.fetch_add(1, Ordering::SeqCst);
        workers.retain(|h: &std::thread::JoinHandle<()>| !h.is_finished());
        workers.push(std::thread::spawn(move || {
            let value = (|| -> Result<Value> {
                let input: Value = serde_json::from_slice(&body)?;
                let op = input["operation"].as_str().context("missing operation")?;
                if op == "stop" {
                    stop.store(true, Ordering::SeqCst);
                    return Ok(json!({"stopping":true}));
                }
                engine.call(op, &input["arguments"])
            })();
            let output = match value {
                Ok(v) => json!({"result":v}),
                Err(e) => json!({"error":format!("{e:#}")}),
            };
            let _ = request.respond(
                tiny_http::Response::from_string(output.to_string()).with_header(
                    tiny_http::Header::from_bytes("Content-Type", "application/json").unwrap(),
                ),
            );
            active.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for worker in workers {
        let _ = worker.join();
    }
    let _ = reaper.join();
    let _ = std::fs::remove_file(conn_path);
    drop(engine);
    FileExt::unlock(&lock)?;
    Ok(())
}
pub fn mcp(home: &Path, bound: Option<&str>) -> Result<()> {
    if let Some(ws) = bound {
        connect_with_timeout(
            home,
            "workspace_info",
            &json!({"workspace":ws}),
            std::time::Duration::from_secs(5),
        )?;
    }
    let input = std::io::stdin();
    let mut output = std::io::BufWriter::new(std::io::stdout().lock());
    let mut input = input.lock();
    loop {
        let mut line = String::new();
        let n = input
            .by_ref()
            .take(128 * 1024 * 1024 + 1)
            .read_line(&mut line)?;
        if n == 0 {
            break;
        }
        ensure!(n <= 128 * 1024 * 1024, "MCP message too large");
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => {
                protocol::mcp_response(&request, bound, |op, args| connect(home, op, args))
            }
            Err(_) => Some(
                json!({"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"invalid JSON"}}),
            ),
        };
        if let Some(response) = response {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
    Ok(())
}
