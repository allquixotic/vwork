# Storage, concurrency and limits

## Three separate kinds of copy-on-write

1. **Application snapshots:** a workspace points to an immutable revision.
   Revisions contain changed paths and deletion tombstones and reference a parent.
   A fork writes a new workspace reference to the same revision; it copies no
   source content and no full path manifest.
2. **Content blobs:** changed file bytes are hashed with BLAKE3 and compressed with
   zstd. Multiple paths/workspaces/repos can reference one blob. This is whole-file
   deduplication, not rolling-chunk deduplication or binary delta compression.
3. **Database transactions:** redb uses copy-on-write B+trees. That provides atomic
   publication and durable state, but it does not magically supply application
   branches. The revision/overlay layer supplies those.

All persistent Git objects, refs and source content live in redb. `git2`/libgit2
reads and writes through a custom redb object backend; normal reads, history,
trees and commits launch no Git CLI processes. Typed raw Git objects are hashed
by libgit2, zstd-compressed and shared daemon-wide by Git object ID. Reads verify
both size and hash. Repository membership records identify objects needed for a
temporary native Git view. SHA-1 repositories are supported; unsupported object
formats fail explicitly rather than silently converting IDs.

Registration imports every available object and resolved ref, including history
and tags, without changing the original checkout/index/refs. It captures committed
state, not dirty or ignored checkout files. Clone uses libgit2 and stores its
result in redb. Remote transport uses a journaled temporary pack directory; success,
failure and restart cleanup remove it. Transfer progress has a deadline/8 GiB pack
limit and sockets have connect/read timeouts. Noninteractive HTTPS authentication
can use the installed Git credential helper; SSH uses the agent. No credentials
are persisted in repository records. Existing registrations migrate before use;
missing original stores produce a restoration instruction without deleting data.

Base manifests are stored once per repository ID and commit. New workspaces reuse
them; forks share an immutable revision. Different registrations may have separate
manifests while sharing identical Git objects. Edited file blobs share by BLAKE3.
A cached persistent `im::OrdMap` shares in-memory path nodes across snapshots.
`storage_info` reports these scopes and counters without creating probe workspaces.
Serialized record bytes are logical sizes; redb file size includes overhead and
reusable pages. Temporary copies and deliberate durable exports are outside redb.

## One owner, many clients

Only `vwork serve` opens the redb database. A file lock rejects a second owner.
`start` and the default MCP entrypoint share a per-home startup lock, bounded
authenticated health probes and daemon launch. A separate daemon lock protects
existing owners and offline compaction. Startup never kills a nonresponsive owner,
and MCP disconnect does not stop the shared daemon. Windows uses an explicit
process handle allowlist (NUL stdin and daemon log only) so background children
cannot retain a bridge's MCP pipes. Unix starts a separate session with redirected
stdio. `mcp --no-auto-start` opts out of daemon creation.
Captured Git, configured task and package subprocesses use `CREATE_NO_WINDOW` on
Windows. Redirecting their output alone does not suppress console creation.
Pipes and exit codes are preserved. Normal user-invoked CLI behavior is unchanged.
CLI processes and stdio MCP bridges connect to its authenticated loopback HTTP
endpoint (explicitly bypassing HTTP proxy environment variables) using a generated token in `<home>/connection.json`. The daemon uses
multiple request workers; redb serializes writes. A per-repository mutex
serializes builds and exports. Independent repositories can build concurrently.

Workspace writes compare `expected_revision` with the current workspace head in
the same transaction that inserts blobs/revision and advances the head. A failed
comparison makes no partial changes. Reads and builds pin the revision captured
at the start of their operation. An edit can advance the workspace while a build
runs; build results still name exactly the snapshot tested.

A lifecycle read lock protects active operations from GC. GC takes the write
lock and performs mark/sweep while no operation can lose the content it is
reading. A successful stop waits for in-flight work before closing redb.

Use one local state directory per daemon. Do not put a live database on SMB,
OneDrive or another syncing/network filesystem. Do not open it from another
program or share it between different machines. This is not a multi-user server.
The Windows state directory inherits its parent ACL; use your private profile.

## On-disk layout

```text
<home>/
  config.toml          # user settings
  workspaces.redb      # all repository/workspace records, blobs, task definitions/results
  daemon.lock          # exclusive owner lock; existence alone does not mean locked
  connection.json      # current loopback URL/token/PID; do not publish
  daemon.log           # background owner diagnostics
  temporary/<lease>/
    owner                        # ownership marker; journal also in redb
    tree/                        # temporary source copy, .git pointer
    git/                         # disposable exported Git objects/index/refs
    targets/, environments/      # package outputs and environments
    artifacts/, uv-cache/        # results and uv cache, removed with lease
  transfers/<id>/                # temporary libgit2 transfer staging only
```

Schema 2 retains `records_v1` for repository metadata, refs, immutable revisions,
workspaces, manifests, changed blobs, tasks, package fingerprints, export pins,
lease journals and recovery history. `git_objects_v1` holds compressed typed Git
objects; `git_members_v1` maps repositories to shared object IDs. Original source
paths are provenance and export-overlap protection, not storage dependencies.
The old binary rejects schema 2. Back up stopped state before upgrading. After
migration, the redb database contains the source/history needed for restoration;
also keep config and any deliberately retained physical artifacts.

Native tools cannot read redb directly. An explicit temporary request or configured
build exports a leased copy with private Git objects, HEAD/index and refs. No
alternates point outside the lease. Builds reuse a lease per repository and pin
its revision. Package targets/environments/artifacts are under that lease, so
release removes the heavy state together. Global Cargo/npm/pip caches remain
owned by those tools; custom human-configured output locations remain explicit.

Leases default to 30 minutes, at most four copies, with renewals capped at 24 hours.
The model should explicitly release as soon as a tool finishes. A five-second
background sweep and startup recovery remove expired copies; a per-repository
lock protects active vwork jobs. Records precede filesystem creation and survive
cleanup failures for retry. Ownership markers and safe-ancestor checks prevent
following links or deleting arbitrary paths. Locked files/ownership failures stay
visible in `temporary_list` and still count against allocation limits. When the
daemon is stopped, cleanup resumes at its next start; there is no external service.

Before cleanup or reuse, changed tracked files and nonignored source additions
are saved into a separate `recovery-*` workspace. Original virtual workspaces are
unchanged. Generated dependency/cache directories are excluded. Recovery is capped
by per-file, result-count and 128 MiB total limits; exceeding a cap preserves the
copy and reports an error for explicit import or intentional discard. This is a
count/TTL bound, not a strict byte quota on running third-party tools. Artifacts
must be saved before release. Legacy build trees with valid vwork ownership
markers are adopted and reclaimed, preserving source edits the same way.

## Resource limits

- `max_file_bytes` caps reading/writing one file (default 64 MiB).
- `max_results` caps list/search result count. Pagination uses `offset`/`limit`.
- `max_output_bytes` bounds text previews/diffs and each task stream. Binary
  reads deliberately return exact base64 bytes up to the file limit.
- Search caps each displayed line at 8 KiB; context lines are also bounded.
  It reports truncation and a continuation offset. One result can exceed a tiny
  configured output budget; the line/context limits still bound it.
- `max_run_history` bounds stored task results globally (20 by default, 0 retains
  none). Results are stored once, in redb; output streams are always drained even
  after the capture limit so a verbose child does not deadlock.
- `max_cached_bases` limits cached path indexes, not bytes. One enormous path
  manifest can still consume substantial RAM. Eviction clears the cache when
  inserting beyond the count limit. There is no byte-accurate cache quota yet.
- Imported metadata and history have no automatic disk quota. Run GC/compaction
  and inspect stats. Database page reuse does not immediately shrink file length.
- At most 32 simultaneous request workers are accepted. Expensive searches may
  compete for CPU/disk; this release does not implement search cancellation.

Search is a scan over the selected virtual snapshot, with a reusable redb-backed object
reader within the request. It skips binary/non-UTF-8/oversized files and respects
edits and tombstones. There is no persistent full-text index yet. Narrow with
globs and paginate. `workspace_size` sums logical file sizes from metadata and
needs no content reads; it is not an NTFS allocation scanner.

## Semantics and deliberate boundaries

- Empty directories are not tracked, matching Git's tree model.
- Paths must be UTF-8 and use `/`. Control paths, traversal and unsafe names are
  rejected. Physical exports additionally reject symlinks, gitlinks, Windows
  reserved names and case collisions (including directory components).
- Git submodules are entries, not recursively imported repos. Register their
  checked-out repos separately. Git LFS files are pointer blobs; automatic LFS
  hydration, clean/smudge filters, submodule initialization and sparse projections
  are not implemented.
- Content bytes are Git blob bytes. Physical exports preserve bytes and executable
  modes on Unix; they do not apply checkout CRLF conversion or Git filters.
- Merge uses the shared virtual ancestor and whole-file comparison. It does not
  do text conflict-marker merges, rename detection or full Git merge-DAG semantics.
- Virtual revisions are not Git commits. Commit export makes one commit whose
  parent is the workspace's original base. Repeated exports remain independent
  snapshots against that base. See the apply workflow before cherry-picking.
- No refresh/fetch/push, rebase, blame, interactive staging,
  filesystem mounting or general file-manager GUI is included.
- The MCP binding is a default workspace, not tenant isolation. The hook is a
  convenience policy. Build processes have your normal user permissions.

Sources:
[redb design](https://github.com/cberner/redb/blob/master/docs/design.md),
[redb API](https://docs.rs/redb/latest/redb/),
[zstd compression API](https://docs.rs/zstd/latest/zstd/stream/fn.encode_all.html).
