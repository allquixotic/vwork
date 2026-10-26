vwork 0.2.2 automatically starts its daemon when Goose loads the enabled MCP
extension. Existing `vwork --home ... mcp --workspace ...` configuration works
without a separate startup shortcut, in Fastrock and the Goose CLI.

- Running daemons are reused. Concurrent starts share one owner per state folder.
- Startup and health checks have bounded waits; existing owners are never killed
  or replaced. The daemon stays running after an MCP client exits.
- `mcp --no-auto-start` preserves explicitly managed daemon startup.
- Windows launches inherit only the daemon log and NUL input handles, keeping
  MCP pipes independent of the shared daemon's lifetime.
- Generated Goose hooks use a matcher before command execution, so ordinary
  vwork calls avoid launching `sh`. Existing native-tool blocks and
  `on_failure: block` remain in place for matching tools.

Upgrade: stop the old daemon, replace `vwork.exe` with this Azure-signed binary,
and reconnect the enabled extension. To reduce hook overhead, regenerate the
Goose setup with `vwork goose`, then merge the generated `plugin/hooks/hooks.json`
into the installed plugin while preserving your customizations. The binary does
not rewrite your Goose configuration. Developer remains disabled for vwork-only
workflows. The database format is unchanged.

Workstation policy must allow the daemon process to start. If automatic launch
is denied, keep the permitted manual startup route; no security-policy changes,
elevation, or alternate launcher retries are attempted. Starting the daemon does
not register repositories or create missing workspaces.

The Windows x64 ZIP contains the native executable, source, docs and configuration
templates. Publication requires Azure Authenticode signing, publisher/timestamp
verification, a native MCP cold-start/reuse smoke test, SHA-256 checksums and
GitHub provenance attestation. Only Windows binaries are published.
