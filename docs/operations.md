# Operations reference

Every engine operation can be called through the native CLI:

```text
vwork --home <state> call <operation> --json <JSON>
vwork --home <state> call <operation> --json -
```

The second form reads JSON from stdin. It is the recommended PowerShell form:

```powershell
@{workspace='task-one'; glob='**/*.rs'; limit=50} | ConvertTo-Json -Compress |
  & $Vwork --home $State call files_glob --json -
```

MCP exposes the selected operations below with a `vwork_` prefix. A bridge started
with `--workspace task-one` supplies that workspace when the argument is omitted.
Use forward slashes for all **virtual** paths, on every OS.

## Model-visible operations

| Operation | Arguments beyond optional workspace | Behavior |
|---|---|---|
| `storage_info` | none | Read-only daemon storage counters, shared manifests/blobs and deduplication scopes |
| `repo_list` | none | Registered IDs/provenance/refs |
| `repo_register` | `repo`, `path` | Import existing repository objects/refs into redb |
| `repo_clone` | `repo`, `source` | Clone a selected URL or local repo into redb |
| `workspace_temporary` | `expected_revision`, `purpose`, optional `ttl_seconds` | Tracked temporary filesystem copy |
| `temporary_list` | none | Active copies, cleanup failures, transfer staging, recent recovery workspaces |
| `temporary_renew` | `lease`, optional `ttl_seconds` | Renew while a tool still needs a copy |
| `temporary_release` | `lease`, optional `discard_changes` | Save bounded source recovery, remove physical copy; intentional discard is explicit |
| `repo_log` | `repo`, optional `reference`, `limit` | Actual Git history |
| `workspace_list` | none | IDs, repos, current heads |
| `workspace_create` | `workspace`, `repo`, optional `reference` | From committed Git reference (default HEAD) |
| `workspace_fork` | `name` | New workspace sharing current head |
| `workspace_info` | none | Current revision, base SHA, file count |
| `workspace_size` | optional `offset`, `limit` | Directories ranked by recursive logical bytes |
| `files_list` | optional `path`, `glob`, `offset`, `limit` | Flat file metadata, optionally under directory |
| `files_glob` | `glob`, optional `offset`, `limit` | Path discovery |
| `files_stat` | `path`, optional `offset`, `limit` | Exact file or descendant metadata |
| `files_read` | `path`, optional `start_line`, `line_count`, `binary` | UTF-8 preview or exact base64 bytes |
| `files_search` | `pattern`, optional `glob`, `ignore_case`, `context`, `offset`, `limit` | Regex line matches |
| `files_write` | `expected_revision`, `path`, exactly one of `content`/`base64`, optional `mode` | Create/overwrite virtual file |
| `files_replace` | `expected_revision`, `path`, `before`, `after`, optional `occurrences` | Exact text substitution; default exactly one occurrence |
| `files_apply_patch` | `expected_revision`, `edits`, optional `message` | Atomic structured full-file writes/deletions |
| `files_copy`, `files_rename` | `expected_revision`, `path`, `destination` | One file; destination must not exist |
| `files_delete` | `expected_revision`, `path`, optional `recursive` | Tombstone file/subtree |
| `workspace_status`, `workspace_diff` | optional `against_revision`, `offset`, `limit` | Compare against original base or another revision |
| `workspace_history` | optional `offset`, `limit` | Virtual edit history |
| `workspace_restore` | `expected_revision`, `revision` | Restore content as a new edit |
| `workspace_merge` | `expected_revision`, `source` | Merge another workspace with shared virtual ancestry |
| `workspace_commit` | `expected_revision`, `message` | Export to Git object/ref without changing original branch |
| `workspace_export` | `destination`, optional `glob` | Explicit physical files |
| `workspace_import` | `expected_revision`, `source`, `paths` | Explicit physical edits/deletions |
| `task_list` | `repo` | Human-defined build/test tasks |
| `workspace_run_task` | `expected_revision`, `task`, optional `lease` | Serialized build of pinned snapshot |
| `package_list` | `repo` | Configured npm/Cargo/pip/uv/CDK projects and allowed actions |
| `package_run` | `expected_revision`, `project`, `action`, optional `lease` | Serialized package action; reports physical source/input changes |
| `workspace_drop` | `expected_revision` | Remove one workspace reference |

`temporary_list`, `temporary_renew`, `temporary_release`, `repo_register`, `repo_clone`, `storage_info`, `repo_list`, `repo_log`, `workspace_list`, `task_list` and `package_list` do not use a workspace.
A newly created workspace requires a new explicit ID, even when the bridge is
bound to another existing one.

Common replacements:

| Native command | Virtual operation |
|---|---|
| `find`, `Get-ChildItem`, `rg --files` | `files_list`, `files_glob` |
| `grep`, `rg`, `Select-String` | `files_search` |
| `cat`, file editor reads | `files_read` |
| editor writes, `sed` substitutions | `files_write`, `files_replace`, `files_apply_patch` |
| `cp`, `mv`, `rm -r` | `files_copy`, `files_rename`, `files_delete` |
| `git worktree add` | `workspace_create` or `workspace_fork` |
| `git status`, `git diff`, undo | `workspace_status`, `workspace_diff`, `workspace_restore` |
| `git log` | `repo_log` or virtual `workspace_history` |
| `git merge` | `workspace_merge` (whole-file conflict semantics) |
| `git commit` | `workspace_commit` (snapshot export semantics) |
| recursive disk-size scan of source | `workspace_size` (logical, not allocated size) |
| `npm ci`, `cargo test`, `pip install`, etc. | `package_run` or configured `workspace_run_task` |

## Edits and revisions

Read `workspace_info` or a file first. Pass its returned revision to every write:

```json
{
  "workspace":"task-one",
  "expected_revision":"CURRENT_REVISION",
  "message":"Update two files atomically",
  "edits":[
    {"path":"src/main.rs","content":"fn main() {}\n"},
    {"path":"old.txt","delete":true}
  ]
}
```

Call `files_apply_patch` with this JSON. It does **not** accept unified diff text.
All edits either publish together or leave the workspace unchanged. On a revision
conflict, reread/reconcile; do not replace the token and blindly replay stale edits.

`files_read` text is a line-oriented preview: line numbers start at 1 and newline
separators are normalized in the preview. It reports `trailing_newline` and
`truncated`. For byte-exact transfers use `binary: true` and the returned `base64`.
Modes accepted for writes are `100644`, `100755` and `120000`; symlinks can remain
virtual or be exported to Git, but cannot be physically materialized by vwork.

`workspace_status` returns paginated paths. The `diff` string is bounded
independently and is not paginated by the path offset. Narrowing a huge change set
or reading the affected files is more useful than repeatedly requesting the same
truncated whole diff.

## Human/admin CLI operations

These are deliberately absent from the MCP tool list:

| Operation | Arguments | Purpose |
|---|---|---|
| `health` | none | Version/schema/compression |
| `task_set` | `repo`, `name`, `program`, optional `args`, `cwd`, `timeout_seconds`, `env` | Save/replace allowed task |
| `package_configure` | `repo`, `name`, `manager`, optional `path`, `program`, `args`, `env`, `offline`, `requirements`, `inputs`, `scripts` | Save/replace package project; see [package guide](packages.md) |
| `stats` | none | DB file size, blob count/compressed bytes, revision/workspace/repo counts |
| `workspace_squash` | `workspace`, `expected_revision` | Detach this workspace's old history while preserving current content |
| `export_forget` | `repo`, `revision` | Remove DB retention pin for a Git-exported snapshot; preserve actual Git commit/ref |
| `gc` | none | Reclaim unreachable revision/blob records for reuse |

CLI lifecycle commands are `init`, `start`, `serve`, `stop`, `doctor`, `compact`,
`goose`, `mcp`, `hook`, and `package`; run `--help` on each. `compact` is offline and
rejects an active daemon. The HTTP transport is local and authenticated; it is an
implementation transport, not a hosted/public API.

## Maintenance

Dropping a virtual workspace is a small database operation; no million-file tree
needs deleting. Its ancestor content remains while other forks, history or
exported-snapshot pins reference it.

1. Drop finished workspaces using `workspace_drop` with their current revision.
2. For a long-lived workspace, optionally use **human CLI** `workspace_squash`.
   This preserves content but discards its old ancestry, so later merges with old
   forks may have no common virtual ancestor. Keep forks when that matters.
3. For an already accepted Git-exported snapshot, optionally use `export_forget`
   with repo ID and revision. It removes only the database history pin; the Git
   commit/ref remains. Do this only when virtual-history restoration is unnecessary.
4. Run `vwork --home <state> call gc`. Freed pages become reusable.
5. To reduce physical DB file length, stop the daemon, back up state, run
   `vwork --home <state> compact`, then start it again. Compaction requires working
   free space and exclusive database access.

Base manifests and imported Git history remain in redb; there is no repository
unregister/history-pruning operation. GC also reclaims object records left by
failed unpublished imports. Registered Git objects remain shared by object ID.

Use `temporary_list` and `temporary_release` to reclaim physical copies. Builds
and packages return lease IDs; their environments, outputs and private Git stores
are removed together. Expiry/startup cleanup provides a fallback. Import wanted
source edits and save artifacts first. Cleanup errors retain their journal and
allocation slot for retry; inspect them instead of allocating unlimited copies.
Durable `workspace_export` destinations and global package caches remain explicit.

There is no online automatic eviction of source history or base manifests. For an
entirely retired dataset, retain a verified backup, stop its daemon, and remove
its state directory as a deliberate human action. A fresh state directory can
register the existing original repos again.

## Recovery and troubleshooting

- **Connection failed:** run `start` and inspect `<home>/daemon.log`. All clients
  must use the same absolute `--home`. `doctor` verifies Git and connectivity.
- **Already owned/compaction denied:** stop the daemon. Never delete a lock file
  to defeat a running owner. The OS releases the lock on process exit.
- **Revision conflict:** inspect current content and reconcile, then edit with the
  new head. A successful build of an earlier head does not validate newer changes.
- **Missing Git object:** restore a verified redb backup. Only a legacy registration still
  awaiting migration needs its original Git object store; schema 2 keeps objects
  and base contents in the database.
- **Export rejected:** check absolute path, no repo overlap, empty/managed target,
  filename portability and symlink/reparse points. AVD redirected profile folders
  may be reparse points; choose an approved plain local folder instead.
- **Commit export failed:** registration captures Git user.name/user.email, with vwork defaults when absent. Inspect the reported error; source Git is not required after migration.
- **Build needs ignored config:** create it explicitly in the build directory or
  arrange it in your approved task. It is not imported from the original checkout.
- **Package inputs not prepared:** run that project's install action, import any
  reported manifest/lockfile edits, then retry with the current revision.
- **Python venv creation fails:** configure `program` to an installed Python that
  supports copied virtual environments and ensurepip; test `python -m venv --copies`
  with that interpreter. See [package prerequisites](packages.md).
- **Disk full:** stop creating builds/exports. Free disposable artifacts, then use
  GC and offline compaction. Successful transactions are durable; a failed write
  is not reported as success. Back up before recovery operations.
- **No live Goose test here:** run the client smoke test in `goose.md`. Server
  protocol tests do not prove your Goose version or LLM follows the prompt.
