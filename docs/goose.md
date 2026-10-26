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

The stdio bridge requires the daemon to be running. Start it with `vwork start`
before opening Goose. Do not launch `vwork serve` as the MCP command: serve is the
shared database owner and uses an authenticated loopback HTTP endpoint.

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

Goose's `.goosehints` loading is tied to its developer extension in the inspected
implementation. With that extension disabled, server/recipe instructions are the
reliable route. Copy the generated `AGENTS.md` into other clients' context only
when those clients actually load it.

## Optional native policy hook

Copy the generated `plugin` directory to:

```text
~/.agents/plugins/vwork-policy/
```

or to `.agents/plugins/vwork-policy/` beneath the directory from which you run
Goose. Preserve `plugin.json` and `hooks/hooks.json`.

The hook blocks known shell and physical-file tools and explains their virtual
replacements. `on_failure: block` prevents a selected hook's runtime failure from
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

The repository tests the actual stdio transport and hook decision function. It
does not provision an LLM account or assert a particular model's behavior in a
live Goose conversation. This final client smoke test belongs on your AVD.

References inspected for this integration:
[Goose config](https://github.com/aaif-goose/goose/blob/main/documentation/docs/guides/config-files.md),
[Goose hooks](https://github.com/aaif-goose/goose/blob/main/documentation/docs/guides/context-engineering/hooks.md),
[custom extensions](https://github.com/aaif-goose/goose/blob/main/documentation/docs/tutorials/custom-extensions.md).
