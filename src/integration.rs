use anyhow::{Result, ensure};
use serde_json::json;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

pub fn goose(home: &Path, workspace: &str, output: &Path) -> Result<()> {
    crate::model::valid_id(workspace)?;
    let exe = std::env::current_exe()?;
    std::fs::create_dir_all(output)?;
    let config = json!({"extensions":{
        "developer":{"type":"builtin","name":"developer","enabled":false},
        "extensionmanager":{"type":"platform","name":"extensionmanager","enabled":false},
        "vwork":{"type":"stdio","name":"vwork","enabled":true,"cmd":exe,"args":["--home",home,"mcp","--workspace",workspace],"timeout":900}
    }});
    std::fs::write(
        output.join("config.fragment.yaml"),
        serde_yaml::to_string(&config)?,
    )?;
    let recipe = json!({"version":"1.0.0","title":format!("vwork: {workspace}"),"description":"Coding with compressed virtual source","instructions":crate::protocol::INSTRUCTIONS,"extensions":[{"type":"stdio","name":"vwork","cmd":exe,"args":["--home",home,"mcp","--workspace",workspace],"timeout":900}]});
    std::fs::write(output.join("recipe.yaml"), serde_yaml::to_string(&recipe)?)?;
    std::fs::write(
        output.join("AGENTS.md"),
        include_str!("../examples/AGENTS.md"),
    )?;
    // Goose runs command hooks through sh; POSIX quoting also handles Git Bash on Windows.
    let executable = exe
        .to_string_lossy()
        .replace('\\', "/")
        .replace('\'', "'\"'\"'");
    let hook = json!({"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":format!("'{executable}' hook"),"on_failure":"block"}]}]}});
    let hooks = output.join("plugin/hooks");
    std::fs::create_dir_all(&hooks)?;
    std::fs::write(hooks.join("hooks.json"), serde_json::to_vec_pretty(&hook)?)?;
    std::fs::write(
        output.join("plugin/plugin.json"),
        serde_json::to_vec_pretty(
            &json!({"name":"vwork-policy","version":env!("CARGO_PKG_VERSION"),"description":"Route source operations to virtual workspace tools"}),
        )?,
    )?;
    Ok(())
}

pub fn package(root: &Path, binary: Option<&Path>, output: &Path) -> Result<()> {
    let root = root.canonicalize()?;
    if let Some(p) = binary {
        ensure!(
            p.file_name().and_then(|s| s.to_str()) == Some("vwork.exe"),
            "release binary must be named vwork.exe"
        );
        let mut f = std::fs::File::open(p)?;
        let mut magic = [0; 2];
        f.read_exact(&mut magic)?;
        ensure!(
            magic == *b"MZ",
            "only Windows PE release binaries are permitted"
        );
        f.seek(SeekFrom::Start(0x3c))?;
        let mut offset = [0; 4];
        f.read_exact(&mut offset)?;
        f.seek(SeekFrom::Start(u32::from_le_bytes(offset) as u64))?;
        let mut pe = [0; 6];
        f.read_exact(&mut pe)?;
        ensure!(
            &pe[..4] == b"PE\0\0" && u16::from_le_bytes([pe[4], pe[5]]) == 0x8664,
            "release executable must target Windows x64"
        );
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::File::create(output)?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o644);
    let names = [
        "Cargo.toml",
        "Cargo.lock",
        "rust-toolchain.toml",
        "README.md",
        "LICENSE",
        "SPEC.md",
        "FORMAT.md",
        "AGENTS.md",
        "CHANGELOG.md",
        ".gitignore",
    ];
    for name in names {
        let p = root.join(name);
        if p.is_file() {
            zip.start_file(format!("vwork/{name}"), options)?;
            zip.write_all(&std::fs::read(p)?)?;
        }
    }
    for dir in ["src", "tests", "docs", "examples", ".github", ".cargo"] {
        for entry in walkdir::WalkDir::new(root.join(dir)).follow_links(false) {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(&root)?
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("non-UTF8 package path"))?
                .replace('\\', "/");
            zip.start_file(format!("vwork/{relative}"), options)?;
            zip.write_all(&std::fs::read(entry.path())?)?;
        }
    }
    if let Some(p) = binary {
        zip.start_file("vwork/bin/vwork.exe", options)?;
        zip.write_all(&std::fs::read(p)?)?;
    }
    zip.finish()?;
    Ok(())
}
