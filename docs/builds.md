# Builds, exports and imports

## Register a task once

Task definitions are administrative CLI operations. The model can list/run them
but cannot create a new arbitrary shell task through MCP.

PowerShell example for a Rust monorepo:

```powershell
@{
  repo='mono'; name='test'; program='cargo'
  args=@('test','--locked'); cwd=''; timeout_seconds=600
  env=@{ CARGO_TARGET_DIR='C:\build-cache\mono-target' }
} | ConvertTo-Json -Depth 8 -Compress |
  & $Vwork --home $State call task_set --json -
```

macOS equivalent:

```sh
vwork --home /path/to/state call task_set --json '{"repo":"mono","name":"test","program":"cargo","args":["test","--locked"],"cwd":"","timeout_seconds":600,"env":{"CARGO_TARGET_DIR":"/path/to/shared-target"}}'
```

Choose tools already available on your AVD. Environment values are stored in the
database; avoid embedding credentials. Tasks cannot prompt for input. Executable
lookup uses the daemon's inherited PATH, so restart it if your environment changes.

The model calls `vwork_workspace_run_task` with its workspace, current
`expected_revision`, and `task: "test"`. vwork exports the pinned snapshot into
`<home>/build/mono`, runs the configured executable with argument-array semantics
and returns success, exit code, timeout, captured output and revision.

For `.cmd`/`.bat` scripts, explicitly configure `cmd.exe` and arguments such as
`["/D", "/C", "your-script.cmd"]`; do not assume a Unix shell. Task programs can
be arbitrary native tools; only the orchestration supplied by vwork is Rust.

## What serialization means

Every task for a registered repo waits on one mutex. Source cannot switch under a
running task because another workspace requested a build. Different repos have
different locks. Export requests for the same repo share the lock. Virtual edits
can continue concurrently, so always report the **returned tested revision**.

Before each run, vwork repairs modified tracked files and removes tracked files
that disappeared from the next snapshot. Generated/untracked files remain for
incremental caches. Empty obsolete directories are removed. A newly tracked path
that would overwrite an existing untracked file fails visibly. An explicit clean
of the stopped build tree is appropriate after branch-sensitive cache failures.

The physical source tree is an export, **not a registered Git worktree** and has no
`.git` directory. Builds that run Git for version metadata must accept an explicit
version/revision input or need a future separate-index checkout integration. The
environment includes `VWORK_WORKSPACE` and `VWORK_REVISION`; the latter is a virtual
revision UUID, not a Git SHA. `workspace_info` reports the base Git SHA. Do not
point `GIT_DIR` at your original checkout to make tests run: that can mix the
original index with the exported source.

On timeout the process tree is terminated using Unix process groups or a Windows
Job Object. Descendants are also terminated after the parent exits so inherited
pipes do not hang the request. A task that starts a detached service is not a
supported persistent-service workflow. OS policies can reject job assignment;
vwork then reports the error rather than claiming successful supervision.

The daemon is **not a sandbox**. A configured build script can access the same
files/network as your account. Review task definitions and source as you normally
would before compiling it. No service installation or admin operation is used.

## Export only the files you need

Ask the model to use `vwork_workspace_export` with:

```json
{"workspace":"task-one","destination":"C:/Users/me/exports/task-one","glob":"src/**"}
```

Omit `glob` for the complete snapshot. The destination must be absolute and either
empty or a vwork-managed export belonging to the same repository/state directory.
It cannot overlap **any registered checkout**, the state directory (for explicit
exports), a symlink/reparse point or an unsafe portable path. The marker
`.vwork-managed.json` describes the exact exported revision and selected files.

Reusing a managed destination switches its tracked files to the requested export;
previously exported files omitted by a narrower glob are removed. Keep a separate
export directory if you want to preserve a previous export. Generated files are
not deleted automatically. An interrupted first export may leave an unmarked
partial directory: use a new empty destination or remove the disposable partial
export before retrying.

## Import deliberately

After editing physical files, call:

```json
{"workspace":"task-one","expected_revision":"REVISION_FROM_EXPORT","source":"C:/Users/me/exports/task-one","paths":["src/main.rs","src/new.rs"]}
```

Only named paths are considered. Missing named tracked files become deletions;
new named files become additions. No build outputs or unknown files are swept in.
The marker must identify the same home/repo/workspace/revision, and the workspace
must still have that head. If it advanced, import fails; preserve the export and
reconcile the edits explicitly with revision-checked virtual tools.

## Export a real Git commit

After reviewing `workspace_diff` and testing, call `workspace_commit` with a
message and expected revision. Git must have author/committer identity configured.
The result supplies a commit SHA and a private ref. It never moves your original
branch, stages files, writes the original index, or pushes.

To apply it later in a clean normal checkout, the human can inspect and cherry-pick:

```sh
git show <returned-commit>
git cherry-pick <returned-commit>
```

Each export is a full snapshot commit against the virtual workspace's **original
Git base**, not a continuation of the previous exported commit. Do not blindly
cherry-pick repeated exports one after another; either use a fresh target branch,
apply the intended final snapshot, or create a new virtual workspace from the
accepted Git commit. This release does not export a linear series of virtual edits
as separate Git commits.
