use serde_json::json;
use vwork::protocol;

#[test]
fn v27_reads_and_general_tools_remain_available() {
    for command in [
        "cat /etc/hosts",
        "rg 'git clone' ~/.claude/skills",
        "Get-Content C:\\Users\\Sean\\.claude\\CLAUDE.md",
        "git -C /existing/repo show HEAD:AGENTS.md",
        "git worktree list --porcelain",
        "git clone --help",
        "git init -h",
        "git --help clone",
        "git --version init",
        "git --exec-path init",
        "# git clone ignored\ncat /tmp/readme",
        "echo data # git clone ignored",
        "git worktree add --help",
        "echo 'git clone https://example.org/repo'",
        "printf '%s' 'x; git clone repo'",
        "cat <<'EOF'\ngit clone example\nEOF",
        "python -c 'print(\"git clone x\")'",
        "sh -c 'echo data' -c 'git init repo'",
        "sh -n -c 'git init repo'",
        "sh read-script.sh -c 'git init repo'",
        "powershell -File read-script.ps1 -Command 'git init repo'",
        "python query.py",
        "bun run query.ts",
        "aws s3api head-object --bucket example --key file",
        "aws ec2 describe-instances",
        "curl https://example.org",
        "cp ~/.claude/AGENTS.md /tmp/agent-notes.md",
        "git archive --format=zip HEAD -o result.zip",
        "echo 'unterminated",
    ] {
        assert_eq!(
            protocol::hook(&json!({"tool_name":"shell","tool_input":{"command":command}}))["decision"],
            "allow",
            "{command}"
        );
    }
    for tool in [
        "read_file",
        "filesystem__read_file",
        "tree",
        "search_files",
        "developer__read_image",
        "write",
        "edit",
        "developer__text_editor",
        "exa__web_search_exa",
        "aws__execute",
        "vwork_workspace_export",
    ] {
        assert_eq!(
            protocol::hook(
                &json!({"tool_name":tool,"tool_input":{"path":"/anywhere/AGENTS.md","content":"git clone example","command":"git clone example"}})
            )["decision"],
            "allow",
            "{tool}"
        );
    }
    assert!(
        protocol::tools(true)
            .iter()
            .any(|t| t["name"] == "vwork_workspace_export")
    );
}

#[test]
fn v27_direct_repository_creation_is_redirected() {
    for command in [
        "git clone https://example.org/repo copy",
        "git clone --bare origin objects",
        "git clone -v origin copy",
        "git init repo",
        "git -C /existing worktree add ../copy main",
        "git --git-dir=/existing/.git worktree add ../copy",
        "git -c credential.helper=manager clone origin copy",
        "cd /tmp && git clone origin copy",
        "pwd; git init repo",
        "pwd\ngit init repo",
        "& 'C:\\Program Files\\Git\\bin\\git.exe' clone origin copy",
        "sh -c 'git clone origin copy'",
        "powershell -NoProfile -Command 'git init repo'",
        "cmd /C git clone origin copy",
    ] {
        for tool in [
            "shell",
            "developer__shell",
            "terminal__execute",
            "run_command",
        ] {
            let reply = protocol::hook(&json!({"tool_name":tool,"tool_input":{"command":command}}));
            assert_eq!(reply["decision"], "block", "{tool}: {command}");
            assert!(
                reply["reason"]
                    .as_str()
                    .unwrap()
                    .contains("vwork_repo_clone")
            );
            assert!(
                reply["reason"]
                    .as_str()
                    .unwrap()
                    .contains("workspace_export")
            );
        }
    }
}

#[test]
fn v27_generated_configuration_and_single_prompt() -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    vwork::integration::goose(dir.path(), "one", dir.path())?;
    let config: serde_json::Value =
        serde_yaml::from_slice(&std::fs::read(dir.path().join("config.fragment.yaml"))?)?;
    assert_eq!(config["extensions"]["developer"]["enabled"], true);
    assert!(
        config["extensions"].get("extensionmanager").is_none(),
        "preserve unrelated extension choices"
    );
    let recipe: serde_json::Value =
        serde_yaml::from_slice(&std::fs::read(dir.path().join("recipe.yaml"))?)?;
    assert!(
        recipe["extensions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["name"] == "developer")
    );
    let instructions = std::fs::read_to_string(dir.path().join("AGENTS.md"))?;
    assert_eq!(instructions, protocol::INSTRUCTIONS);
    assert_eq!(recipe["instructions"], protocol::INSTRUCTIONS);
    Ok(())
}
