# §G
Provide compressed, isolated, persistent virtual workspaces for Goose without admin rights, symlinks, or per-agent full checkouts.

# §C
- Native Rust executables; git2/libgit2 handles Git objects, history, imports and clone transport. Git CLI only for explicitly configured tasks or credential helpers.
- macOS and Windows supported. Publish binaries only for Windows x64, inside ZIP.
- One authenticated loopback daemon owns one redb database. Multiple repositories, workspaces, and MCP clients.
- Zstd level 3 default; configurable. All persistent Git objects, refs, base contents and changed blobs inside redb; globally deduplicated objects.
- Serialized builds use bounded, expiring temporary copies. Explicit import/release; never mutate original checkout.
- No transparent virtual filesystem claim. vwork MCP stays structured; Goose host tools and other MCPs remain available.

# §I
- I.cli: `vwork init|serve|start|stop|call|mcp|hook|goose|package|doctor|compact`.
- I.rpc: authenticated loopback JSON API; stdio MCP bridge with workspace binding.
- I.storage: versioned redb tables for repositories, immutable revisions, workspaces, base manifests, blobs, tasks.
- I.goose: generated configuration fragment, recipe, agent instructions, optional native policy hook.
- I.release: tagged GitHub Windows x64 ZIP with executable, source, docs, templates; public S3 upload only on explicit `ship-to-aws` invocation.

# §V
- V1: Workspace forks share immutable revisions; writes never change siblings or Git base. Test `fork_isolation_and_reopen`.
- V2: All mutations validate expected head in the same transaction as publication. Test `optimistic_conflicts`.
- V3: Content hashes cover raw bytes; blobs deduplicate, compress, verify on read. Test `binary_and_compression`.
- V4: Paths cannot escape roots or overwrite control paths; materialization rejects links, gitlinks, Windows collisions and unsafe ancestors. Test `path_and_export_safety`.
- V5: Searches, diffs, status, history and merge observe overlay additions, edits and tombstones. Test `virtual_operations`.
- V6: Exported Git commits match virtual content and preserve user's index, worktree and branch. Test `git_commit_isolation`.
- V7: Builds serialize per repository, pin revision, respect timeout/output limits, and preserve active virtual edits. Tests `managed_builds`, `managed_build_serialization`.
- V8: Daemon authenticates clients; MCP speaks valid JSON-RPC and hides raw shell; hook redirects recognizable unmanaged Git creation; host reads/writes/scripts remain available. Tests `protocol_and_hooks`, `generated_goose_configuration`.
- V9: GC retains live/forked/exported revisions and blobs; database reopens. Tests `garbage_collection`, `squash_reclaims_old_blobs`, `export_retention`.
- V10: ZIP contains only Windows x64 binary plus allowlisted source/docs; no local state/secrets. Test `package_contents`.

- V11: Temporary build Git exports redb objects, owns HEAD/index/refs, reports the base commit and virtual dirtiness, and preserves source Git state. Test `build_git_metadata`.
- V12: Package actions serialize with builds; npm dependencies and pip environments require matching inputs; Cargo targets are per project. Tests `package_workflows`, `package_input_changes`.
- V13: Package-generated manifest/lock edits are reported for explicit import; virtual source never changes implicitly. Test `package_writeback`.
- V14: Build source-change reports compare content against the virtual index; a rebuilt stat cache alone never marks files changed. Tests `build_git_metadata`, `package_workflows`.
- V15: Package-tool path arguments and environment values use standard absolute paths; Windows NT verbatim prefixes stay internal. Tests `package_process_paths`, `package_input_changes`.
- V16: Enabled MCP startup ! reuse or start one daemon per home with bounded probes/waits; MCP stdout ! JSON-RPC only; bridge exit ! keep shared owner alive; explicit opt-out ! no start; startup failure ! no owner takeover. Tests `v16_mcp_cold_start_and_reuse`, `v16_concurrent_starters`, `v16_startup_bounds`, `v16_mcp_opt_out`.
- V17: Generated Goose hook matcher ! select known shell tools only; vwork/read/edit/other-MCP calls ! skip hook process; on_failure=allow ! preserve reads if hook fails. Tests `v17_generated_hook_matcher`, `protocol_and_hooks`.
- V18: Captured Git/task/package subprocesses on Windows ! no console allocation; preserve redirected IO and exit status. Tests `v18_windows_task_console`, `v18_process_stdio`.
- V19: Read-only MCP storage info ! report shared manifests/blobs and per-workspace/revision records without probe mutations; same registered repo+commit ! reuse base manifest; identical edits ! reuse daemon blobs. Test `v19_storage_sharing`.
- V20: MCP repository setup ! structured clone/register only; clones ! redb-owned objects/refs; bounded temporary transport staging; no persistent checkout; no overwrite; registration publication ! unique ID transaction; failures ! no registered partial clone. Tests `v20_repository_setup`, `v20_clone_failure_preserves_paths`, `v20_registration_is_atomic`.

- V21: Register/clone import all Git objects/refs into redb via git2; reads/history/commits work after source removal; legacy registration migrates before use without modifying source; shared payloads retain per-repository membership. Tests `v21_redb_owns_git`, `v21_legacy_migration`, `v21_clone_transport`, `v21_import_batches`, `v21_shared_objects_keep_repository_membership`.
- V22: Temporary copies ! journaled ownership, bounded count/TTL, explicit release/renew, startup and periodic expiry cleanup; active jobs protected; failed cleanup retained for retry; preserve bounded nonignored source edits as recovery snapshot before automatic removal. Tests `v22_temporary_lifecycle`, `v22_temporary_recovery`, `v22_cleanup_ownership`, `v22_temporary_bounds_and_active_job`, `v22_legacy_build_cleanup`, `v22_recovery_limit_preserves_copy`, `v22_periodic_cleanup`.
- V23: uv/CDK ! structured configured package actions over temporary copies; report completion, revision, lease and changes; no implicit virtual writeback or CDK deployment. Tests `v23_uv_cdk_plans`, `v23_uv_cdk_execution`.
- V24: Compaction ! daemon-exclusive, retain connection identity, reject/defer active jobs; hourly automatic check ! OS input idle >300s, unknown idle ! skip; GC then compact only if estimated reclaimable bytes >1,000,000,000 by default; return measured before/after and skip/error reasons. Tests `v24_online_compaction`, `v24_compact_reclaims_holes_and_preserves_live_data`, `v24_schedule_idle_and_busy`.
- V25: Existing empty/damaged DB ! never replaced by a fresh database; redb crash repair ! preserve pre-repair copy, integrity-check before serving, report recovered state; unrecoverable corruption ! fail closed with original retained. Tests `v25_existing_empty_database`, `v25_crash_recovery_and_corruption_preservation`.
- V26: Fetch ! git2 transport with redb-backed existing objects, temporary new packs only, atomic ref publication, same repo ID; preserve pinned workspaces/private commits; failure ! refs unchanged; explicit prune only. Tests `v26_fetch_updates_refs`, `v26_fetch_failure_prune_and_private_commits`, `v26_legacy_clone_requires_explicit_source_once`.

- V27: Flexible Goose defaults ! Developer enabled, unrelated extensions preserved, all pure reads and general scripts available; hook ! detect direct clone/init/worktree-add, preserve quoted examples/help/read commands; one shared prompt ! retain indefinite workspace_export and recommend leased transient copies. Tests `v27_reads_and_general_tools_remain_available`, `v27_direct_repository_creation_is_redirected`, `v27_generated_configuration_and_single_prompt`.
- V28: Shared MCP ! existing authenticated loopback owner, matching Host, no browser Origin, same protocol/tool allowlist, request-local workspace binding, concurrent clients isolated; no bridge process required, no maintenance tools added. Test `v28_shared_mcp_preserves_bindings_and_owner`.

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
T9|x|Suppress Windows helper consoles; expose storage-sharing evidence to MCP; signed Windows release|V8,V10,V18,V19,I.rpc,I.goose,I.release
T10|x|Model-visible clone/register with shared Git objects in redb; preserve other administration boundaries|V4,V8,V18,V20,I.rpc,I.goose

T11|x|redb Git object backend, migration, library Git operations|V1,V3,V6,V11,V19,V20,V21,I.storage
T12|x|Recoverable leased temporary copies and uv/CDK package actions|V4,V7,V9,V12,V13,V22,V23,I.rpc,I.goose
T13|x|Online/manual and hourly idle-aware compaction; crash/corruption recovery|V7,V9,V16,V24,V25,I.cli,I.rpc,I.storage
T14|x|Incremental fetch into existing redb registrations; distinct MCP tool descriptions|V20,V21,V26,I.rpc,I.goose
T15|x|Verify Windows/macOS, sign/release 0.3.1 through CI, explicit ship-to-aws upload|V10,V24,V25,V26,I.release

T16|x|Flexible Goose host tools, narrow misuse hook, shared instructions and indefinite exports|V8,V17,V27,I.goose,I.rpc
T17|~|Authenticated daemon MCP reuse for Fastrock, Windows/Mac CI and signed Windows release; LTO off|V8,V16,V28,I.rpc,I.release

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
B15|2026-10-26|Captured Git and task subprocesses used default Windows creation flags, allocating consoles from the detached daemon|V18; shared no-window command constructor with native Windows console/IO regression
B16|2026-10-26|MCP lacked shared-storage counters and manifest-sharing contract, prompting speculative answers and disposable probe workspaces|V19; read-only storage_info plus explicit sharing scopes and counter-based regression
B17|2026-10-26|Repository registration checked ID existence outside publication, so six simultaneous requests all succeeded and could replace each other's registration|V20; per-ID setup serialization plus uniqueness check inside publication transaction; native concurrent-registration regression

B18|2026-10-26|Legacy build directories could survive a redb migration and retain heavy outputs; interrupted moves could leave stale Git pointers|V22; journal/adopt owned directories, resume from moved marker, reconstruct private Git view before source recovery and cleanup
B19|2026-10-26|libgit2 C error enum is unsigned on Unix but signed on Windows, making its required cross-platform ABI cast trigger Windows-only strict lint|Narrow documented unnecessary_cast allowance at the FFI boundary; retain strict checks on both targets
B20|2026-10-26|Daemon health moved to schema 2 while startup probes still accepted only schema 1, making healthy owners appear unavailable|V16,V21; share SCHEMA_VERSION between health and startup, rerun real cold-start/concurrent-start/periodic-cleanup tests
B21|2026-10-26|A global ODB existence hit could skip adding repository membership on reused writes, and allow unrelated commit IDs to resolve|V21; scope backend read/header/existence to repository membership while deduplicating payload globally; shared-object/native-export regression
B22|2026-10-26|Database::create silently initialized an existing zero-byte state file, concealing truncation/data loss|V25; existing files use open-only validation, pre-repair snapshots and integrity checks; empty/damaged files fail closed
B23|2026-10-26|New fetch code assumed older git2 URL return type, callable nonoptional FFI callback and an in-scope redb trait|Mechanical API corrections; retain V21/V26 native fetch and callback coverage

B24|2026-10-26|system-idle-time build.rs selects framework linking by host OS, breaking Mac-to-Windows compilation|V24; use GetLastInputInfo directly on Windows and restrict wrapper dependency to macOS

B25|2026-10-26|Windows mandatory byte-range locks prevent fs::copy after redb opens the file|V25; snapshot through an exclusively locked source handle before redb open, serialize startup/recovery handoff; native reopen/crash regression

B26|2026-10-26|Legacy clones have empty provenance paths; attempting Git discovery there could select the process working directory|V26; skip empty legacy paths, require an explicit source once and persist it; v26_legacy_clone_requires_explicit_source_once

B27|2026-10-26|Strict generated config and blanket hook prevented host reads/scripts and contradicted broad MCP access; three V27 baseline tests fail on Windows|V27; enable Developer, preserve other MCPs, target direct Git creation and share one prompt

B28|2026-10-26|Wrapper detector scanned argument data after the executed shell command and blocked a harmless echo; V27 regression reproduced on Windows|V27; inspect only the first executed command string, allow script/unknown/syntax-check forms; cancel initial release before publication

B29|2026-10-26|Download verification compared Windows CRLF packaged prompt bytes directly with local LF source|Normalize line endings only for source-text comparisons; retain exact ZIP/executable cryptographic checks; no runtime change
B30|2026-10-29|Goose spawns stdio bridge per conversation despite shared database owner; expensive Windows process creation repeats|V28; expose existing MCP handler through authenticated daemon API for reusable client transports
B31|2026-10-29|New binding regression inspected workspace-info ID at wrong JSON depth; Windows/Mac CI caught Null vs expected ID|V28; assert workspace.id, preserve real concurrent-client test
