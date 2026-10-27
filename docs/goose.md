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
| `config.fragment.yaml` | vwork binding plus enabled Developer; other extensions preserved |
| `recipe.yaml` | same shared instructions plus vwork and Developer extensions |
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

vwork **0.3.2+** enables Goose's Developer extension for host reads, file edits,
and shell/script execution. Keep other MCP extensions such as Exa and AWS enabled
as desired, with their existing credentials and permissions. The fragment contains
only `developer` and `vwork`; merge those entries without replacing the whole
extensions mapping. It does not disable Extension Manager or unrelated tools.
An unset or empty `available_tools` list allows all tools; remove any older
Developer allowlist that prevents the reads or scripts you need.

The working rule is simple: repository work defaults to vwork, ordinary host work
uses the appropriate tool. Reads are unrestricted by this policy, including reads
inside existing repositories. Skills and global instruction files can be edited
on disk. One-off Python/Bun scripts and AWS/API queries are ordinary host work;
use small temporary script directories and remove disposable files when finished.
Use `workspace_temporary` when a repo tool needs files, import wanted edits and
release promptly. Use `workspace_export` for an intentional durable copy: it has
no expiry. Avoid unmanaged clones, worktrees and bulk source copies.

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

Since 0.3.0, vwork's captured Git/build/package helpers do not allocate Windows
console windows. MCP calls travel over the existing bridge; they do not launch a
new vwork CLI process per tool call. If every call still flashes a console, check
that the installed policy hook has the 0.2.2+ matcher described below. An older
unfiltered hook makes Goose launch `sh` on every call, independently of vwork's
helper settings. Updating vwork alone does not replace that installed hook.

Agents can use `vwork_storage_info` for shared storage counters and deduplication
scopes without native CLI access. Reconnect the extension after replacing the
binary to refresh its tool list. Regenerated agent instructions direct storage
questions to that read-only tool instead of creating disposable probe workspaces.
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

### Global instructions and upgrades from the strict setup

vwork's virtual file tools access redb; Developer supplies host access. Skill
discovery from `~/.claude/skills` is separate from loading global Claude rules,
and does not implement Claude-specific hooks or subagents.

After upgrading from 0.3.1 or earlier:

1. Generate fresh setup files with the installed vwork executable.
2. Merge `config.fragment.yaml`: enable Developer, keep vwork and other desired
   MCPs, and preserve provider/model settings and credentials. A recipe alone
   is insufficient to update an existing session's configuration.
3. **Replace the old vwork section** in global Goose `AGENTS.md` with the new
   generated instructions; preserve unrelated user rules. Remove stale vwork
   statements that prohibit all shell/file tools. Do not append contradictory
   policies. MCP initialization, generated `AGENTS.md` and the recipe now use
   one source of instructions.
4. Replace the old installed `vwork-policy` hook with the generated plugin, or
   remove that optional plugin if you do not want the detector. Do not leave an
   older blanket-denial hook installed alongside it. Preserve unrelated plugins.
5. Start a new Goose/Fastrock conversation to load the new tools and hints.

On default Windows installs, global hints are in
`%APPDATA%\Block\goose\config\AGENTS.md`; with `GOOSE_PATH_ROOT`, use
`<GOOSE_PATH_ROOT>\config\AGENTS.md`. Keep `AGENTS.md` in `CONTEXT_FILE_NAMES`
if overriding it. With host tools enabled the agent can read/edit these files,
but existing conversations may need restarting to load their updated contents.

## Optional native policy hook

Copy the generated `plugin` directory to `~/.agents/plugins/vwork-policy/`, or
`.agents/plugins/vwork-policy/` under the directory from which you run Goose.
Preserve `plugin.json` and `hooks/hooks.json`.

The native `PreToolUse` hook checks known shell/execute/run-command tools for
recognizable direct `git clone`, `git init` and `git worktree add` operations,
including ordinary command chains and simple shell wrappers. It blocks those
calls with a redirect to vwork's clone/register, temporary-copy and durable-export
tools. Help commands and quoted command examples remain readable. It does not
inspect file contents or restrict which host paths may be read or edited.

Read/edit tools, vwork tools and other MCPs skip the hook process entirely.
Selected shell calls still launch it, including reads made through a shell.
`on_failure: allow` ensures an unavailable hook cannot prevent a pure read.
Goose 1.56.0 launches command hooks via `sh -c`, so Windows still requires an
available shell (commonly Git Bash); its launch can add overhead or a console
flash to those selected calls. Updating vwork alone cannot change Goose's hook
launcher. vwork's own captured helpers suppress console allocation.

This is a best-effort misuse detector, not a sandbox or filesystem monitor.
Arbitrary Python/Bun code, shell aliases, complex shell syntax, bulk copies and
other extensions can write repositories without detection. Unrecognized commands
are allowed. The shared instructions carry that responsibility; the hook catches
common direct mistakes without restricting general host work. It never deletes
unowned directories. Use tool results and `temporary_list` to verify cleanup.

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
See [package tool setup](packages.md).

The repository tests the actual stdio transport and hook decision function. It
does not provision an LLM account or assert a particular model's behavior in a
live Goose conversation. This final client smoke test belongs on your AVD.

References inspected for this integration:
[Goose config](https://github.com/aaif-goose/goose/blob/main/documentation/docs/guides/config-files.md),
[Goose hooks](https://github.com/aaif-goose/goose/blob/main/documentation/docs/guides/context-engineering/hooks.md),
[custom extensions](https://github.com/aaif-goose/goose/blob/main/documentation/docs/tutorials/custom-extensions.md).
