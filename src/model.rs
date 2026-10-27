use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u64 = 2;
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub compression_level: i32,
    pub port: u16,
    pub max_file_bytes: usize,
    pub max_results: usize,
    pub task_timeout_seconds: u64,
    pub max_output_bytes: usize,
    pub max_cached_bases: usize,
    pub max_run_history: usize,
    pub temporary_ttl_seconds: u64,
    pub temporary_max_ttl_seconds: u64,
    pub max_temporary_copies: usize,
    pub max_recovery_bytes: u64,
    pub auto_compact: bool,
    pub compact_interval_seconds: u64,
    pub compact_idle_seconds: u64,
    pub compact_min_reclaim_bytes: u64,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            compression_level: 3,
            port: 0,
            max_file_bytes: 64 * 1024 * 1024,
            max_results: 1000,
            task_timeout_seconds: 600,
            max_output_bytes: 1024 * 1024,
            max_cached_bases: 4,
            max_run_history: 20,
            temporary_ttl_seconds: 1800,
            temporary_max_ttl_seconds: 86400,
            max_temporary_copies: 4,
            max_recovery_bytes: 128 * 1024 * 1024,
            auto_compact: true,
            compact_interval_seconds: 3600,
            compact_idle_seconds: 300,
            compact_min_reclaim_bytes: 1_000_000_000,
        }
    }
}
impl Config {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            (-7..=22).contains(&self.compression_level),
            "compression_level must be -7..22"
        );
        anyhow::ensure!(
            self.max_file_bytes > 0
                && self.max_results > 0
                && self.max_output_bytes > 0
                && self.task_timeout_seconds > 0
                && self.max_cached_bases > 0
                && self.temporary_ttl_seconds > 0
                && self.temporary_ttl_seconds <= self.temporary_max_ttl_seconds
                && self.max_temporary_copies > 0
                && self.max_recovery_bytes > 0,
            "limits must be positive"
        );
        anyhow::ensure!(
            self.compact_interval_seconds > 0 && self.compact_interval_seconds <= 31_536_000,
            "compaction interval must be 1..31536000 seconds"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Repository {
    pub id: String,
    pub path: PathBuf,
    pub git_dir: PathBuf,
    #[serde(default)]
    pub stored: bool,
    #[serde(default)]
    pub refs: BTreeMap<String, String>,
    #[serde(default)]
    pub author_name: String,
    #[serde(default)]
    pub author_email: String,
    #[serde(default)]
    pub fetch_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Entry {
    pub source: String,
    pub oid: String,
    pub mode: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Revision {
    pub id: String,
    pub repo: String,
    pub parent: Option<String>,
    pub base: String,
    pub changes: BTreeMap<String, Option<Entry>>,
    pub message: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub repo: String,
    pub head: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub repo: String,
    pub name: String,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: String,
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Connection {
    pub url: String,
    pub token: String,
    pub pid: u32,
}

/// Persistent in-memory index: cloned snapshots share tree nodes as well as DB blobs.
pub type Tree = im::OrdMap<String, Entry>;

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
pub fn id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

pub fn valid_id(s: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        !s.is_empty()
            && s.len() <= 100
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
            && s != "."
            && s != "..",
        "invalid identifier"
    );
    Ok(())
}

pub fn path(s: &str) -> anyhow::Result<String> {
    anyhow::ensure!(
        !s.is_empty()
            && s.len() <= 30000
            && !s.starts_with('/')
            && !s.contains('\\')
            && !s.contains('\0'),
        "path must be a relative, forward-slash path"
    );
    for part in s.split('/') {
        anyhow::ensure!(
            !part.is_empty()
                && part != "."
                && part != ".."
                && !part.eq_ignore_ascii_case(".git")
                && !part.to_ascii_lowercase().starts_with(".vwork-"),
            "unsafe path component"
        );
        anyhow::ensure!(
            !part.chars().any(|c| c.is_control()),
            "control characters in path"
        );
    }
    Ok(s.into())
}

pub fn portable_path(s: &str) -> anyhow::Result<()> {
    path(s)?;
    for part in s.split('/') {
        anyhow::ensure!(
            !part.contains(['<', '>', ':', '"', '|', '?', '*']) && !part.ends_with(['.', ' ']),
            "path is not Windows-portable: {s}"
        );
        let stem = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        anyhow::ensure!(
            !["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
                && !(stem.len() == 4
                    && (stem.starts_with("COM") || stem.starts_with("LPT"))
                    && stem.as_bytes()[3].is_ascii_digit()),
            "reserved Windows path: {s}"
        );
    }
    Ok(())
}
