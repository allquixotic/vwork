vwork 0.3.4 fixes database startup lock handoff and intermittent CI races.

- Explicitly unlock the recovery snapshot before redb opens the database. A
  descriptor temporarily inherited by another process can no longer prolong
  that snapshot lock. Startup/recovery locks also release on error paths.
- Keep exclusive database ownership and recovery snapshots intact; no retry
  that bypasses locking and no change to the stored schema.
- Cleanup acceptance now waits for both directory removal and the durable lease
  update. CI repeats the reopen and periodic-cleanup regressions on Windows and
  macOS, with a deterministic inherited-descriptor lock regression.

The shared MCP transport introduced in 0.3.3 remains localhost-only. Fastrock
0.3.3 can reuse it without per-conversation vwork client processes. Host/Origin
checks, bearer authentication, workspace bindings and tool boundaries remain.
LTO remains disabled.

Upgrade: stop vwork, back up its state, replace the executable and restart.
Schema 2 and existing Goose configuration remain compatible. Existing Fastrock
0.3.3 installations need no application update for this fix.

The Windows x64 ZIP is built, signed, timestamped and verified by GitHub Actions.
