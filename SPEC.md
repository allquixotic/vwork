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
- V8: Daemon authenticates clients; MCP speaks valid JSON-RPC and hides raw shell; hook blocks native bypass. Tests `protocol_and_hooks`, `generated_goose_configuration`.
- V9: GC retains live/forked/exported revisions and blobs; database reopens. Tests `garbage_collection`, `squash_reclaims_old_blobs`, `export_retention`.
- V10: ZIP contains only Windows x64 binary plus allowlisted source/docs; no local state/secrets. Test `package_contents`.

- V11: Build Git shares existing objects, owns HEAD/index/refs, reports the base commit and virtual dirtiness, and preserves source Git state. Test `build_git_metadata`.
- V12: Package actions serialize with builds; npm dependencies and pip environments require matching inputs; Cargo targets are per project. Tests `package_workflows`, `package_input_changes`.
- V13: Package-generated manifest/lock edits are reported for explicit import; virtual source never changes implicitly. Test `package_writeback`.
- V14: Build source-change reports compare content against the virtual index; a rebuilt stat cache alone never marks files changed. Tests `build_git_metadata`, `package_workflows`.
- V15: Package-tool path arguments and environment values use standard absolute paths; Windows NT verbatim prefixes stay internal. Tests `package_process_paths`, `package_input_changes`.
- V16: Enabled MCP startup ! reuse or start one daemon per home with bounded probes/waits; MCP stdout ! JSON-RPC only; bridge exit ! keep shared owner alive; explicit opt-out ! no start; startup failure ! no owner takeover. Tests `v16_mcp_cold_start_and_reuse`, `v16_concurrent_starters`, `v16_startup_bounds`, `v16_mcp_opt_out`.
- V17: Generated Goose hook matcher ! cover every blocked tool family; vwork calls ! skip hook process; selected hook ! retain on_failure=block. Tests `v17_generated_hook_matcher`, `protocol_and_hooks`.

# §T
id|status|task|cites
---|---|---|---
T1|x|Storage, Git object baseline, immutable workspaces, content operations|V1,V2,V3,V4,V5,I.storage
T2|x|Git export, merges, managed tasks and materialization|V4,V5,V6,V7,I.cli
T3|x|Daemon, CLI, MCP, policy hook, Goose generator|V8,I.rpc,I.goose
T4|x|Integration tests, documentation, ZIP and CI release|V1,V2,V3,V4,V5,V6,V7,V8,V9,V10,I.release

T5|x|Borrowed-object Git metadata for build checkout|V6,V7,V11,I.cli
T6|x|Native package plans for npm, Cargo and pip|V7,V12,V13,V14,I.rpc
T7|x|Docs, cross-platform validation and Windows release|V10,V11,V12,V13,V15,I.release
T8|x|Automatic MCP daemon lifecycle, bounded concurrent startup, opt-out and hook matching; signed Windows CI release|V8,V10,V16,V17,I.rpc,I.goose,I.release

# §B
id|date|cause|fix
---|---|---|---
B1|2026-10-26|Initial compile used String reference where mode list requires str reference|Mechanical correction; existing V4 covers materialization
B2|2026-10-26|Blanket symlink-ancestor rejection blocked macOS system /var temporary paths|Allow only exact macOS system aliases; continue rejecting user symlink ancestors (V4)
B3|2026-10-26|Strict Clippy rejected nested conditionals|Use let-chain conditionals; retain strict CI lint
B4|2026-10-26|Git for Windows rejects Rust canonical verbatim paths|Normalize only Git-bound path arguments, including UNC paths; retain native canonical paths for filesystem safety; regression git_path_arguments
B5|2026-10-26|HTTP client's default configuration inherits proxy environment and prefix checks do not validate URL authority|Disable proxies for loopback RPC and parse exact SocketAddr; daemon test covers bad proxy and userinfo URL
B6|2026-10-26|Mac's standalone Python copied executable cannot locate libpython even with direct venv --copies|Select a compatible installed interpreter through project program and VWORK_TEST_PYTHON; document prerequisite
B7|2026-10-26|Git diff-files reports empty stat-cache entries as changed after private index reconstruction|Use content-aware git diff, disable external diff/textconv, assert unchanged report (V14)
B8|2026-10-26|Windows Python ensurepip rejects the NT verbatim environment path passed by Rust|Normalize package-tool path arguments and environment values while retaining canonical paths for internal IO (V15)
B9|2026-10-26|Regression fixture's generic im map left string conversion ambiguous|Use explicit String constructor; V15 fixture retained
B10|2026-10-26|Azure trust used legacy name-only subject while new repository emits immutable owner/repository IDs|Match observed GitHub OIDC subject to verified repository IDs; retain environment/tag restrictions; one-time identity provisioning fix
B11|2026-10-26|Hosted Windows runner may omit PSGallery registration required by Azure signing action|Register default repository before signing; preserve failed tags and allow stable +build.N retries; publication still requires valid timestamped signature
B12|2026-10-26|Goose docs incorrectly tied hints to Developer; obscured host/virtual file and skill/instruction boundaries|Verify 1.56.0 prompt builder/lookup; document global context setup with Developer disabled; documentation correction, no runtime invariant
B13|2026-10-26|Windows background spawn inherited unrelated MCP/CLI pipe handles despite redirected stdio, preventing reader EOF after bridge exit|V16; explicit STARTUPINFOEX handle allowlist for NUL/log only; native Windows cold-start/exit and concurrent-start regressions
B14|2026-10-26|Release signing setup failed downloading the pinned TrustedSigning 0.5.8 package from PowerShell Gallery before signing began|Package URL subsequently returned HTTP 200; retry unchanged tag/source with mandatory signing and verification intact; external download failure, no runtime invariant change
