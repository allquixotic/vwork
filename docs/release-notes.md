vwork 0.2.3 fixes Windows console flashes from captured Git, configured-task and
package subprocesses. Their output still reaches the calling tool, and exit codes
remain intact. The external Goose/MCP architecture is unchanged.

A new read-only `vwork_storage_info` tool gives agents daemon-wide storage counters
and explicit sharing scopes. Base path manifests share by repository ID and commit;
identical edited content deduplicates across the daemon. Workspaces retain their
own small records and revisions. A common remote URL does not deduplicate separate
repository registrations or Git clones. Inspection needs no probe workspaces.

Upgrade: stop the old daemon, replace `vwork.exe` with this signed binary, and
reconnect Goose to refresh the MCP tool list. Regenerate/merge agent instructions
for the new storage guidance. If an old unfiltered policy hook is installed,
regenerate/merge `plugin/hooks/hooks.json` too: Goose's per-call `sh` launch is
separate from vwork's child processes. Preserve local configuration customizations.
Developer can remain disabled. No Fastrock or Goose rebuild is needed.

The Windows x64 ZIP is built and signed in CI, with publisher/timestamp verification,
native lifecycle smoke testing, checksums and GitHub provenance attestation.
The version-1 database is compatible; the new statistics do not migrate stored data.
Workstation application-control policy still applies to process creation.
