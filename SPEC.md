# §G
Provide compressed, isolated, persistent virtual workspaces for Goose without admin rights, symlinks, or per-agent full checkouts.

# §C
- Native Rust executables; existing Git required for Git object plumbing and explicit remote operations.
- macOS and Windows supported. Publish binaries only for Windows x64, inside ZIP.
- One authenticated loopback daemon owns one redb database. Multiple repositories, workspaces, and MCP clients.
- Zstd level 3 default; configurable. Git-backed immutable base; compressed changed blobs.
- One serialized managed build checkout per repository. Explicit export/import; never mutate original checkout.
- No transparent virtual filesystem claim. No arbitrary shell in MCP.

# §I
- I.cli: `vwork init|serve|start|stop|call|mcp|hook|goose|package|doctor|compact`.
- I.rpc: authenticated loopback JSON API; stdio MCP bridge with workspace binding.
- I.storage: versioned redb tables for repositories, immutable revisions, workspaces, base manifests, blobs, tasks.
- I.goose: generated configuration fragment, recipe, agent instructions, optional native policy hook.
- I.release: tagged GitHub Windows x64 ZIP with executable, source, docs, templates; matching public S3 copy.

# §V
- V1: Workspace forks share immutable revisions; writes never change siblings or Git base. Test `fork_isolation_and_reopen`.
- V2: All mutations validate expected head in the same transaction as publication. Test `optimistic_conflicts`.
- V3: Content hashes cover raw bytes; blobs deduplicate, compress, verify on read. Test `binary_and_compression`.
- V4: Paths cannot escape roots or overwrite control paths; materialization rejects links, gitlinks, Windows collisions and unsafe ancestors. Test `path_and_export_safety`.
- V5: Searches, diffs, status, history and merge observe overlay additions, edits and tombstones. Test `virtual_operations`.
- V6: Exported Git commits match virtual content and preserve user's index, worktree and branch. Test `git_commit_isolation`.
- V7: Builds serialize per repository, pin revision, respect timeout/output limits, and preserve active virtual edits. Tests `managed_builds`, `managed_build_serialization`.
- V8: Daemon authenticates clients; MCP speaks valid JSON-RPC and hides raw shell; hook blocks native bypass. Test `protocol_and_hooks`.
- V9: GC retains live/forked/exported revisions and blobs; database reopens. Tests `garbage_collection`, `squash_reclaims_old_blobs`, `export_retention`.
- V10: ZIP contains only Windows x64 binary plus allowlisted source/docs; no local state/secrets. Test `package_contents`.

# §T
id|status|task|cites
---|---|---|---
T1|x|Storage, Git object baseline, immutable workspaces, content operations|V1,V2,V3,V4,V5,I.storage
T2|x|Git export, merges, managed tasks and materialization|V4,V5,V6,V7,I.cli
T3|x|Daemon, CLI, MCP, policy hook, Goose generator|V8,I.rpc,I.goose
T4|~|Integration tests, documentation, ZIP and CI release|V1,V2,V3,V4,V5,V6,V7,V8,V9,V10,I.release

# §B
id|date|cause|fix
---|---|---|---
B1|2026-10-26|Initial compile used String reference where mode list requires str reference|Mechanical correction; existing V4 covers materialization
B2|2026-10-26|Blanket symlink-ancestor rejection blocked macOS system /var temporary paths|Allow only exact macOS system aliases; continue rejecting user symlink ancestors (V4)
B3|2026-10-26|Strict Clippy rejected nested conditionals|Use let-chain conditionals; retain strict CI lint
