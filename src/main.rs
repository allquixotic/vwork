use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::path::PathBuf;
use vwork::{integration, protocol, startup, transport};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[arg(long, global = true)]
    home: Option<PathBuf>,
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    /// Create configuration without overwriting existing settings.
    Init,
    /// Run the database owner in the foreground.
    Serve,
    /// Start the database owner in the background.
    Start,
    /// Gracefully stop the database owner after in-flight work.
    Stop,
    /// Call an operation through the daemon. JSON may also be read from stdin with --json -.
    Call {
        operation: String,
        #[arg(long, default_value = "{}")]
        json: String,
    },
    /// Serve MCP over stdio, starting the shared daemon if needed. Logs never go to stdout.
    Mcp {
        #[arg(long)]
        workspace: Option<String>,
        /// Require an already-running daemon; never launch one from this bridge.
        #[arg(long)]
        no_auto_start: bool,
    },
    /// Native Goose PreToolUse policy hook; reads JSON on stdin.
    Hook,
    /// Generate Goose config fragment, recipe, prompt and optional hook plugin.
    Goose {
        #[arg(long)]
        workspace: String,
        #[arg(long)]
        output: PathBuf,
    },
    /// Verify Git, configuration and daemon connectivity.
    Doctor,
    /// Compact the database; daemon must be stopped.
    Compact,
    /// Create a source ZIP, optionally including a Windows PE executable.
    Package {
        #[arg(long, default_value = ".")]
        root: PathBuf,
        #[arg(long)]
        binary: Option<PathBuf>,
        #[arg(long)]
        output: PathBuf,
    },
}
fn main() {
    if let Err(e) = run() {
        eprintln!("vwork: {e:#}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let cli = Cli::parse();
    let home = cli.home.unwrap_or_else(|| {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("vwork")
    });
    match cli.command {
        Commands::Init => {
            transport::initialize(&home)?;
            println!("{}", home.display());
        }
        Commands::Serve => {
            // libgit2 requires these process-global options to be set before
            // any threads exist. The standalone serve entrypoint owns that boundary.
            unsafe {
                git2::opts::set_server_connect_timeout_in_milliseconds(15_000)?;
                git2::opts::set_server_timeout_in_milliseconds(30_000)?;
            }
            transport::serve(&home)?;
        }
        Commands::Start => {
            println!(
                "{}",
                startup::ensure_started(&home, &std::env::current_exe()?)?
            );
        }
        Commands::Stop => println!("{}", transport::connect(&home, "stop", &json!({}))?),
        Commands::Call {
            operation,
            json: arg,
        } => {
            let arg = if arg == "-" {
                let mut s = String::new();
                std::io::stdin().read_to_string(&mut s)?;
                s
            } else {
                arg
            };
            let v: Value = serde_json::from_str(&arg)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&transport::connect(&home, &operation, &v)?)?
            );
        }
        Commands::Mcp {
            workspace,
            no_auto_start,
        } => {
            if no_auto_start {
                startup::health(&home, std::time::Duration::from_millis(500))
                    .context("MCP auto-start is disabled; run vwork start first")?;
            } else {
                startup::ensure_started(&home, &std::env::current_exe()?)?;
            }
            transport::mcp(&home, workspace.as_deref())?;
        }
        Commands::Hook => {
            let mut s = String::new();
            std::io::stdin().take(1024 * 1024).read_to_string(&mut s)?;
            let payload: Value = serde_json::from_str(&s)?;
            writeln!(std::io::stdout(), "{}", protocol::hook(&payload))?;
        }
        Commands::Goose { workspace, output } => {
            integration::goose(&home, &workspace, &output)?;
            println!(
                "Generated {}. Merge config.fragment.yaml into Goose config; recipes alone may retain globally enabled extensions. See docs/goose.md.",
                output.display()
            );
        }
        Commands::Doctor => {
            let git = vwork::process::command("git")
                .arg("--version")
                .output()
                .context("Git unavailable")?;
            ensure!(git.status.success(), "Git unavailable");
            println!("{}", String::from_utf8_lossy(&git.stdout).trim());
            let c = transport::load_config(&home)?;
            println!(
                "home={} compression_level={}",
                home.display(),
                c.compression_level
            );
            println!(
                "daemon={}",
                transport::connect(&home, "health", &json!({}))?
            );
        }
        Commands::Compact => {
            use fs2::FileExt;
            std::fs::create_dir_all(&home)?;
            let f = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(home.join("daemon.lock"))?;
            if f.try_lock_exclusive().is_ok() {
                anyhow::ensure!(
                    home.join("workspaces.redb").exists(),
                    "no database to compact"
                );
                let service =
                    vwork::maintenance::Service::open(&home, transport::load_config(&home)?)?;
                println!("{}", service.compact()?);
            } else {
                println!(
                    "{}",
                    transport::connect(&home, "database_compact", &json!({}))?
                );
            }
        }
        Commands::Package {
            root,
            binary,
            output,
        } => {
            integration::package(&root, binary.as_deref(), &output)?;
            println!("{}", output.display());
        }
    }
    Ok(())
}
