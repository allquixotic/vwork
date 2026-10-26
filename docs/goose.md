# Goose integration

No patch to aaif-goose/goose is needed. vwork is a stdio MCP extension and the
optional policy layer uses Goose's existing plugin hooks. Its executable supplies
the hook too; there are no Python or JavaScript bridge scripts.

## Generate configuration

After registering a repo, creating a workspace and starting the daemon:

```powershell
& $Vwork --home $State goose --workspace task-one --output "$env:USERPROFILE\vwork-goose"
```

On macOS, the equivalent is:

```sh
/path/to/vwork --home /path/to/state goose --workspace task-one --output /path/to/goose-setup
```

The output contains:

| File | Purpose |
|---|---|
| `config.fragment.yaml` | stdio extension bound to a workspace; disables developer/extensionmanager |
| `recipe.yaml` | repeatable instructions plus MCP extension |
| `AGENTS.md` | human-readable virtual workspace contract |
| `plugin/plugin.json` | optional Goose plugin manifest |
| `plugin/hooks/hooks.json` | native `PreToolUse` policy hook |

Absolute binary and state paths are captured at generation time. Regenerate if
those paths change. Generated files are machine-local, not portable templates.
Do not commit generated config containing private directory names to a public repo.

## Configure the extension and available tools

Back up your existing Goose configuration. Merge the generated `extensions`
entries into its `extensions` mapping, preserving your provider/model settings:

- Windows: `%APPDATA%\Block\goose\config\config.yaml`
- macOS/Linux: `~/.config/goose/config.yaml`

**Inspect all other enabled extensions.** Disable other shell, filesystem,
computer-control, code-execution and subagent extensions that can operate on
physical source. The fragment explicitly disables `developer` and
`extensionmanager`; it cannot anticipate every extension installed on your
machine. An empty Goose `available_tools` list means **all tools**, not none.

The configured program is the absolute `vwork.exe` path, with arguments:

```text
--home <absolute-state-path> mcp --workspace task-one
```

Since vwork **0.2.2**, the stdio bridge automatically reuses or starts the daemon
when Goose loads this enabled extension. Existing `mcp --workspace ...` config
works unchanged after upgrading vwork. Both Fastrock and Goose CLI sessions use
this path; no startup shortcut or SessionStart hook is needed. Disabled extensions
are not launched by Goose, so they do not trigger startup. This starts on extension
load, not Windows login or merely detecting the installation in Fastrock's UI.

For a manually managed daemon, append `--no-auto-start` to the MCP args and run
`vwork start` yourself first. Do not launch `vwork serve` as the MCP command:
serve is the shared database owner and uses an authenticated loopback HTTP endpoint.
Repository registration and workspace creation remain explicit setup operations.

Startup uses a per-home lock and authenticated health checks. Concurrent clients
reuse one owner; closing a conversation does not stop it. Health probes are bounded
to 500 ms and startup waits to 15 seconds. A locked, unresponsive owner is not killed
or replaced. Startup errors go to MCP stderr; daemon output goes to `daemon.log`,
leaving MCP stdout for JSON-RPC.
If workstation policy refuses the daemon launch, vwork reports the error; retain
your permitted manual startup route. It does not change policy or request elevation.

Start a new Goose session after configuration changes. A recipe alone can leave
other globally enabled extensions active, so use the configuration fragment too.
You can launch with:

```sh
goose run --recipe /absolute/path/to/goose-setup/recipe.yaml --text "Inspect the virtual workspace and summarize its structure."
```

For interactive sessions, use your usual Goose entrypoint after merging config.
Tool display names may include Goose's extension namespace (for example
`vwork__vwork_files_search`). They are the `vwork_files_search` tool described by
the server. The model receives instructions during MCP initialization, and the
recipe repeats them. You do not have to rely on automatic reading of `AGENTS.md`.

In Goose **1.56.0**, system prompt construction loads hints independently of the
Developer extension. The server/recipe instructions above also remain available.
This is verified in [prompt construction](https://github.com/aaif-goose/goose/blob/v1.56.0/crates/goose/src/agents/prompt_manager.rs)
and [hint lookup](https://github.com/aaif-goose/goose/blob/v1.56.0/crates/goose/src/hints/load_hints.rs);
older guidance that Developer is required does not describe this version.

### Global instructions with Developer disabled

Keep Developer and Extension Manager disabled for the vwork-only workflow.
vwork's file tools access virtual workspaces, not arbitrary host files such as
`~/.claude/CLAUDE.md`. Skill discovery from `~/.claude/skills` is separate: it
neither imports global Claude instructions nor provides Claude-specific hooks,
subagents, or a general host filesystem tool.

To load global rules, manually merge the desired text into `AGENTS.md` beside
the active Goose `config.yaml`, preserving existing rules. On default Windows
installations this is `%APPDATA%\Block\goose\config\AGENTS.md`; with
`GOOSE_PATH_ROOT`, use `<GOOSE_PATH_ROOT>\config\AGENTS.md`. The default context
filenames include `AGENTS.md` and `.goosehints`. If overriding
`CONTEXT_FILE_NAMES`, keep `AGENTS.md` in that list. Start a new session after
setup. Adapt shell/Git instructions to vwork and omit requirements for
unavailable hooks or subagents. This does not enable native execution tools.

If an agent claims it cannot access files, distinguish virtual files from host
files: ask it to call `vwork_workspace_info` and `vwork_files_list` for the bound
workspace using the names actually exposed by Goose. Missing or failing vwork
tools require checking the extension, daemon, and workspace binding. Enabling
Developer is not the repair for a vwork-only session. Importing host-side global
instructions is a setup operation outside the agent's virtual-file tools.

## Optional native policy hook

Copy the generated `plugin` directory to:

```text
~/.agents/plugins/vwork-policy/
```

or to `.agents/plugins/vwork-policy/` beneath the directory from which you run
Goose. Preserve `plugin.json` and `hooks/hooks.json`.

The hook blocks known shell and physical-file tools and explains their virtual
replacements. Since 0.2.2, generated hooks include a Goose `matcher` for those
same tool names/families. Goose checks it before spawning a command: vwork calls,
skills and other already-allowed tools avoid the per-call `sh`/hook process cost.
Matching native tools still run the hook. Regenerate your Goose setup and replace
the installed `plugin/hooks/hooks.json` to apply this change; upgrading the binary
alone does not rewrite installed configuration. Preserve local hook customizations.
`on_failure: block` prevents a selected hook's runtime failure from
silently allowing a call. Goose currently runs command hooks via `sh -c`; Windows
therefore needs the shell used by your Goose installation (commonly Git Bash)
available. The generated command uses forward slashes and shell quoting. The
vwork hook executable itself is native Rust. If your Goose build lacks plugin
hooks or `sh`, leave this optional layer uninstalled and disable native tools in
configuration. Do not assume an unloaded/misconfigured hook provides protection.

This is a tool-routing policy, not an OS sandbox. Unknown extensions may expose
other commands; configured build tasks execute with your user account's normal
permissions. Keep only the tools you intend the model to use enabled.

## Multiple agents

1. Create `task-one` once from a committed Git reference.
2. Fork to `task-two`, `task-three`, etc. with `workspace_fork`.
3. Generate a binding/configuration for each agent session. Use distinct Goose
   profiles/configurations or another MCP client that supports per-session args.
4. Assign one agent to each workspace. All clients use the same state directory.
5. Inspect/merge virtual changes, run a configured task, then export a Git commit.

`--workspace` supplies a default argument; it is **not** a permission boundary.
A client can explicitly name another workspace. Revisions prevent accidental lost
updates; assignment discipline still matters. Repo and workspace IDs are local
names, not accounts or tenants.

## Smoke test

Ask Goose:

> Call workspace_info, find all Rust files using files_glob, then search for a
> symbol using files_search. Do not export anything or invoke a shell.

Verify that it uses only vwork tools. Then fork a disposable workspace, make a
small edit, request its diff, and drop the disposable workspace. Finally register
a build task and verify the returned revision/exit status.

For package workflows, configure a project with the human CLI, then have Goose
call `vwork_package_list` and `vwork_package_run`. Import any needed lockfile changes
before the next action. Regenerate the recipe/AGENTS files after upgrading from
0.1.0 so the new package guidance is included; the MCP tools refresh on reconnect.
See [npm, Cargo and pip setup](packages.md).

The repository tests the actual stdio transport and hook decision function. It
does not provision an LLM account or assert a particular model's behavior in a
live Goose conversation. This final client smoke test belongs on your AVD.

References inspected for this integration:
[Goose config](https://github.com/aaif-goose/goose/blob/main/documentation/docs/guides/config-files.md),
[Goose hooks](https://github.com/aaif-goose/goose/blob/main/documentation/docs/guides/context-engineering/hooks.md),
[custom extensions](https://github.com/aaif-goose/goose/blob/main/documentation/docs/tutorials/custom-extensions.md).
