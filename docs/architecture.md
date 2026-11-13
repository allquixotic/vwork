# Storage, concurrency and limits

## Three separate kinds of copy-on-write

1. **Application snapshots:** a workspace points to an immutable revision.
   Revisions contain changed paths and deletion tombstones and reference a parent.
   A fork writes a new workspace reference to the same revision; it copies no
   source content and no full path manifest.
2. **Content objects:** new edits and Git history share one typed Git object ID
   namespace. Bounded standard Git deltas reduce related versions; Fjall applies
   LZ4. Legacy BLAKE3 edit blobs remain readable and shared by hash.
3. **Database transactions:** Fjall 3.1.13 uses optimistic serializable
   transactions over an LSM tree. Revision guards remain per workspace; immutable
   objects and per-repository membership keys do not update hot reference counts.

All persistent Git objects, refs and source content live in Fjall. libgit2 uses
our in-process ODB backend. Git delta chains are bounded to four levels and
reconstruction verifies both object IDs and lengths. GC retains transitive delta
bases, even across repositories. SHA-1 repositories are supported. Unsupported
object formats fail explicitly.

Registration imports every available object and resolved ref, including history
and tags, without changing the original checkout/index/refs. It captures committed
state, not dirty or ignored checkout files. Clone uses libgit2 and stores its
result in Fjall. Remote transport uses a journaled temporary pack directory; success,
failure and restart cleanup remove it. Transfer progress has a deadline/8 GiB pack
limit and sockets have connect/read timeouts. Noninteractive HTTPS authentication
can use the installed Git credential helper; SSH uses the agent. No credentials
are persisted in repository records. Existing registrations migrate before use;
missing original stores produce a restoration instruction without deleting data.

New workspaces record a shared Git commit and an empty revision without walking
files or writing a duplicate path manifest; forks share an immutable revision.
Point reads resolve overlays and the requested Git path. Operations requiring a
full tree build a shared, bounded `im::OrdMap` cache on demand. Status, diff and
squash use that same base. Older per-repository manifests remain readable.
New edits share by Git object ID; migrated legacy blobs share by BLAKE3.
`storage_info` reports these scopes and counters without creating probe workspaces.
Serialized record bytes are logical sizes; Fjall directory size includes compressed segments, journals and indexes. Temporary copies and deliberate durable exports are outside Fjall.

## One owner, many clients

Fastrock embeds the sole owner. The retained CLI can alternatively use `vwork serve`;
a file lock rejects a second process.
`start` and the default MCP entrypoint share a per-home startup lock, bounded
authenticated health probes and daemon launch. A separate daemon lock protects
existing owners and exclusive maintenance. Startup never kills a nonresponsive owner,
and MCP disconnect does not stop the shared daemon. Windows uses an explicit
process handle allowlist (NUL stdin and daemon log only) so background children
cannot retain a bridge's MCP pipes. Unix starts a separate session with redirected
stdio. `mcp --no-auto-start` opts out of daemon creation.
Captured Git, configured task and package subprocesses use `CREATE_NO_WINDOW` on
Windows. Redirecting their output alone does not suppress console creation.
Pipes and exit codes are preserved. Normal user-invoked CLI behavior is unchanged.
CLI processes and stdio MCP bridges connect to its authenticated loopback HTTP
endpoint (explicitly bypassing HTTP proxy environment variables) using a generated token in `<home>/connection.json`. The daemon uses
multiple request workers; Fjall allows independent transactions to prepare concurrently
and briefly coordinates durable commits. A per-repository mutex
serializes builds and exports. Independent repositories can build concurrently.

Workspace writes compare `expected_revision` with the current workspace head in
the same transaction that inserts blobs/revision and advances the head. A failed
comparison makes no partial changes. Reads and builds pin the revision captured
at the start of their operation. An edit can advance the workspace while a build
runs; build results still name exactly the snapshot tested.

A lifecycle read lock protects active operations from GC. GC takes the write
lock and performs mark/sweep while no operation can lose the content it is
reading. A successful stop waits for in-flight work before closing Fjall.

Use one local state directory per daemon. Do not put a live database on SMB,
OneDrive or another syncing/network filesystem. Do not open it from another
program or share it between different machines. This is not a multi-user server.
The Windows state directory inherits its parent ACL; use your private profile.

## On-disk layout

```text
<home>/
  config.toml          # user settings
  workspaces.fjall/      # all repository/workspace records, blobs, task definitions/results
  daemon.lock          # exclusive owner lock; existence alone does not mean locked
  connection.json      # current loopback URL/token/PID; do not publish
  daemon.log           # background owner diagnostics
  temporary/<lease>/
    owner                        # ownership marker; journal also in Fjall
    tree/                        # temporary source copy, .git pointer
    git/                         # config, packed-refs, index; history packs are hard links
    targets/, environments/      # package outputs and environments
    artifacts/, uv-cache/        # results and uv cache, removed with lease
  git-cache/<repo>/objects/pack/ # derived history packs shared by every lease; state.json vouches for them
  pool/<repo>/<slot>/tree/       # released source trees waiting for the next lease
  trash/<id>/                    # renamed away; deleted in the background
  transfers/<id>/                # temporary libgit2 transfer staging only
```

Schema 2 retains `records_v1` for repository metadata, refs, immutable revisions,
workspaces, manifests, changed blobs, tasks, package fingerprints, export pins,
lease journals and recovery history. `git_objects_v1` holds compressed typed Git
objects; `git_members_v1` maps repositories to shared object IDs. Original source
paths are provenance and export-overlap protection, not storage dependencies.
The old binary rejects schema 2. Back up stopped state before upgrading. After
migration, the Fjall database contains the source/history needed for restoration;
also keep config and any deliberately retained physical artifacts.

Native tools cannot read Fjall directly. An explicit temporary request or configured
build exports a leased copy with its own HEAD, index and refs. No alternates point
outside the lease. Builds reuse a lease per repository and pin its revision.

### What a temporary copy costs

A copy is priced by what differs, not by the size of the repository:

| Part | Work per copy | How |
|---|---|---|
| Git history | two hard links per shared pack | History is transcoded from Fjall into `git-cache/<repo>` once, reusing stored deltas, and extended only when references or the requested base change. Packs are immutable, so every lease names the same files. Another volume or a filesystem without links falls back to copying. |
| Refs | one file | `packed-refs`, however many branches and tags exist. |
| Index | one file | Written directly with the size and timestamps of each materialized file, so Git and libgit2 compare metadata instead of rereading sources. |
| Virtual edits | one private pack | Blobs absent from shared history, for this lease only. |
| Source tree, first copy | one create per file | A few workers; one open per file. Files of a new tree share a modification time two seconds in the past so the index is never racily clean. |
| Source tree, later copies | renames plus what changed | Release renames the tree into `pool/`. The next lease of that repository adopts it, lists each tracked directory once, moves anything vwork did not write into `trash/`, and rewrites only files whose entry or recorded metadata differs. |
| Release | renames | Recovery opens only files whose metadata changed; deletion happens in the background. |

`.vwork-managed.json` in each tree records, for every file, the metadata observed
when vwork finished writing it. A file whose listing still matches is not opened.
This is the trust Git places in its index: an in-place edit that preserves both
length and modification time is invisible on filesystems without a change time
(Unix change times and Windows creation times are compared where available).
Set `verify_materialized_content = true` to read every file back instead.

Pooled trees are physical copies: they count toward `max_temporary_copies`, are
displaced by new leases, expire after `pooled_tree_ttl_seconds` (default four
hours; zero disables pooling) and are listed by `temporary_list`. A pooled tree
carries its record under another name, so nothing recovers edits from it.
`workspace_temporary` returns `timings_ms` and `work` (files written, reused and
removed; packs linked; history objects exported) so a slow host can be diagnosed
from numbers alone.

Shared packs are a cache. Deleting `git-cache/` costs the next copy one export.
A pack whose length differs from `state.json`, or any name it does not list, is
discarded and rebuilt from Fjall. Package targets/environments/artifacts are under that lease, so
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
are saved into a separate `recovery-*` workspace. Changed means metadata differing
from the tree's record, then content differing from the workspace. Original virtual workspaces are
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
  none). Results are stored once, in Fjall; output streams are always drained even
  after the capture limit so a verbose child does not deadlock.
- `max_cached_bases` limits cached path indexes, not bytes. One enormous path
  manifest can still consume substantial RAM. Eviction clears the cache when
  inserting beyond the count limit. There is no byte-accurate cache quota yet.
- Imported metadata and history have no automatic disk quota. Run GC/compaction
  and inspect stats. Database page reuse does not immediately shrink file length.
- At most 32 simultaneous request workers are accepted. Expensive searches may
  compete for CPU/disk; this release does not implement search cancellation.

Search is a scan over the selected virtual snapshot, with a reusable Fjall-backed object
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
- Incremental fetch refreshes refs inside the same registration; workspace snapshots remain pinned.
- No push, rebase, blame, interactive staging,
  filesystem mounting or general file-manager GUI is included.
- The MCP binding is a default workspace, not tenant isolation. The hook is a
  convenience policy. Build processes have your normal user permissions.

Sources:
[Fjall design](https://github.com/cberner/Fjall/blob/master/docs/design.md),
[Fjall API](https://docs.rs/Fjall/latest/Fjall/),
[zstd compression API](https://docs.rs/zstd/latest/zstd/stream/fn.encode_all.html).

## Idle maintenance and startup recovery

A daemon-wide read/write gate surrounds all requests and temporary-copy cleanup.
Fjall performs incremental background compaction without taking the application
lifecycle gate. Explicit full compaction takes that gate and runs GC first.
Automatic legacy hourly whole-store compaction is disabled. Startup opens the
engine directly; one-time redb import is described in [fjall.md](fjall.md).
