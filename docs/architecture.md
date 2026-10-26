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

Base content stays in an existing Git object database. Each imported commit's
path/mode/size/object metadata is stored once per **repository ID and commit** in
redb. Creating another workspace at that commit adds a workspace record and an
empty revision, reusing the existing manifest. A fork only adds a workspace record
pointing at the existing revision. Separate repository registrations have separate
manifests even if they use the same Git directory or remote URL. Separately cloned
Git object stores remain separate on disk; vwork does not deduplicate them by URL.
Identical edited content still shares one compressed blob throughout the daemon.
`git cat-file --batch`
streams base bytes when a read/search/build needs them. Nothing checks out a
second tree for a virtual fork. A cached persistent `im::OrdMap` shares in-memory
path nodes across reconstructed snapshots.

Each edit stores a compressed whole file. Incompressible data may become slightly
larger; repeatedly editing a huge binary is a poor fit. Base Git objects already
use Git's own compression. Compression level 3 is a speed/size starting point,
not a measured optimum for every monorepo.

`vwork_storage_info` exposes read-only daemon counters, sharing scopes and byte
accounting to MCP. The CLI `stats` and `storage_info` operations return the same
information. Base-manifest/workspace/revision bytes count serialized keys and
values, not redb's physical page allocation. Counts include retained and
uncollected records. Database file size includes page overhead and reusable free
space; Git objects, builds, exports and caches live outside that file.

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
  build/<repo>/        # single reusable physical source tree, created on demand
  build-git/<repo>/    # private Git admin/index; alternates borrow source objects
  targets/<repo>/<project>/       # Cargo build outputs
  environments/<repo>/<project>/  # copied Python virtual environment
  artifacts/<repo>/<project>/     # built Python wheels
```

The version-1 schema is a string-keyed table `records_v1`:

| Prefix | Records |
|---|---|
| `r/` | Registered repositories and existing Git directories |
| `m/`, `t/` | Imported base markers and per-path Git entries |
| `w/` | Workspace heads |
| `v/` | Immutable revisions and path overlays |
| `b/` | BLAKE3-keyed zstd blobs |
| `task/`, `run/` | Human-configured tasks and bounded retained results |
| `project/`, `package-state/` | Package projects and successful dependency-input fingerprints |
| `export/` | Pins for snapshots exported as Git commits |

Git base pins live under `refs/vwork/bases/<repo>/<commit>` in the original Git
database, preventing normal Git GC from discarding required objects. Exported
commits live under `refs/vwork/commits/<workspace>/<revision>`. vwork does not change
the original branch, index or files, but these refs and exported Git objects are
intentional writes to its Git database.

Do not remove/move the original repo while workspaces depend on it. Restoring only
the redb file is insufficient; base Git objects are dependencies. Back up the
stopped state directory **and** every registered Git repository/object store.
Registered repo relocation and schema migration are not implemented.

The build view has a regular `.git` pointer to its own admin directory. Its index
holds the virtual snapshot; detached HEAD holds the base commit. An alternates
file borrows source objects, while changed overlay blobs are written only to the
private Git store. Ref names/OIDs are refreshed for native version discovery.
This costs one index and cached overlay Git objects per repository, not a full
clone of its history. The Git view and physical build tree are disposable and
reconstructed before each task. See [Git build semantics](builds.md#git-version-discovery-without-a-clone).

Package actions share that repo's lock and source tree. Their dependency-input
fingerprints are durable redb records; node_modules, target files and Python
environments are ordinary disk files outside redb. Package actions never publish
source edits implicitly. [The package guide](packages.md) describes reuse,
invalidation and explicit import. No automatic cache quota or eviction is provided.

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

Search is a scan over the selected virtual snapshot, with a reusable Git batch
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
- No Git fetch/push, credential handling, rebase, blame, interactive staging,
  filesystem mounting or general file-manager GUI is included.
- The MCP binding is a default workspace, not tenant isolation. The hook is a
  convenience policy. Build processes have your normal user permissions.

Sources:
[redb design](https://github.com/cberner/redb/blob/master/docs/design.md),
[redb API](https://docs.rs/redb/latest/redb/),
[zstd compression API](https://docs.rs/zstd/latest/zstd/stream/fn.encode_all.html).
