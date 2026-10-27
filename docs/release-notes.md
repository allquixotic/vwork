vwork 0.3.1 adds incremental Git fetch, idle-time compaction and startup recovery checks.

- `repo_fetch` refreshes the same registration from its saved origin using libgit2.
  Existing objects stay in redb; only new transfer packs temporarily use disk.
  Refs publish atomically. Existing workspaces and private commits stay pinned;
  create a new workspace from a fetched ref to use updated source. Older imports
  may require `source` once. Optional pruning removes vanished upstream refs.
- Automatic compaction evaluates hourly, only after more than five minutes of
  Windows/macOS input inactivity and when no vwork operation is active. GC and
  allocated-page statistics preflight the work; compaction requires more than
  1 GB estimated free space. `vwork compact` and Fastrock 0.3.1 provide manual
  access with the same threshold, without stopping the daemon.
- Startup refuses empty/damaged replacements, snapshots before redb recovery,
  verifies integrity before serving, and retains evidence after repairs. Full
  validation temporarily needs one database-sized copy and may slow startup.
  Unrecoverable corruption requires restoring an independent verified backup.
- Copy/rename and status/diff MCP descriptions now distinguish their behavior.

Upgrade: stop vwork, back up its state, replace the executable, and restart.
Existing schema-2 registrations/workspaces remain compatible. Reconnect Goose and
regenerate/merge agent instructions for the new fetch tool. Upgrades from 0.2.x
still need original registered Git stores for the one-time schema-2 import.

The Windows x64 ZIP is built, signed, timestamped and verified by GitHub Actions.
