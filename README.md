# vwork

Compressed, copy-on-write **virtual source workspaces** for coding agents on
constrained Windows desktops. One native Rust executable supplies a shared redb
store, CLI, stdio MCP server, Goose configuration generator and policy hook.

A workspace is a reference to an immutable revision, not another directory full
of source files. Existing Git objects supply base content. Edits are deduplicated
by BLAKE3 and stored as zstd blobs (level **3**, configurable). Forks share history.
Builds use **one serialized physical source tree per registered repository**.

**This is an initial implementation, not a filesystem driver.** Native editors,
compilers and shell commands cannot see a virtual workspace until you export it.
Agent file operations use MCP. No administrator rights, symlinks, mount drivers,
Docker, Node or Python runtime are required. An existing Git installation is
required. All supplied executables are Rust; zstd's native library is linked into
the executable. Workstation execution policy must permit your binary to run.

## Get it

Download `vwork-windows-x64.zip` from
[GitHub Releases](https://github.com/allquixotic/vwork/releases/latest).
Extract it into a user-writable folder. The program is `vwork/bin/vwork.exe`; the
ZIP also contains source, documentation, examples and build workflows.

Windows x64 is the only published binary target. On macOS, build from source:

```sh
cargo build --locked --release
./target/release/vwork --help
```

Building requires a Rust toolchain and a native C toolchain for the statically
linked zstd library. Windows source builds normally use the MSVC toolchain and
its C/C++ build tools. The prebuilt ZIP avoids installing build tools on an AVD. Windows builds use a
statically linked MSVC C runtime to avoid requiring a separate redistributable.

## Windows quick start

Use PowerShell. Change these three paths for your machine. Put the state directory
on a **local, user-writable disk**, outside your source checkout and outside
OneDrive/network shares. Every vwork client must use the same `--home`.

```powershell
$Vwork = "$env:USERPROFILE\Tools\vwork\bin\vwork.exe"
$State = "$env:LOCALAPPDATA\vwork"
$Repo = 'C:\src\monorepo'
& $Vwork --home $State init
& $Vwork --home $State start

# stdin JSON avoids Windows command-line quoting pitfalls.
@{repo='mono'; path=$Repo} | ConvertTo-Json -Compress |
  & $Vwork --home $State call repo_register --json -
@{repo='mono'; workspace='task-one'; reference='HEAD'} | ConvertTo-Json -Compress |
  & $Vwork --home $State call workspace_create --json -
@{workspace='task-one'; name='task-two'} | ConvertTo-Json -Compress |
  & $Vwork --home $State call workspace_fork --json -

& $Vwork --home $State goose --workspace task-one --output "$env:USERPROFILE\vwork-goose"
& $Vwork --home $State doctor
```

Register more repositories with distinct `repo` IDs, and create/fork more
workspaces with globally unique workspace IDs. A workspace captures a **committed
Git tree**; uncommitted/ignored files from your original checkout are not imported.

Follow [Goose setup](docs/goose.md) to merge the generated configuration, disable
native source tools, install the optional hook and start a session. **No Goose
source patch is required.** Give each independent agent its own workspace binding.

## How the pieces connect

```text
Goose / other MCP client
    | stdio JSON-RPC
vwork mcp --workspace task-one       vwork CLI / other MCP bridges
    |                                      |
    +-------- authenticated localhost -----+
                       |
                   vwork serve
             single owner of one redb file
               /                    \
       Git base objects        compressed changed blobs
                       |
           explicit export or configured task
                       |
           one build tree per repository
```

`vwork start` launches the owner in the background. `vwork mcp` is a small bridge;
multiple clients share the daemon and database. CLI and MCP return JSON, including
revision IDs needed for compare-and-swap edits. A stale edit fails without
partially changing the workspace.

## What is included

- Virtual file listing, globbing, regex search with context, text/base64 reading,
  exact replacements, atomic batches, copy, rename and recursive deletion.
- Virtual status, unified diffs, history, restore, forks and three-way whole-file
  merge. Conflicting merges leave the destination unchanged.
- Git history and export of a real Git commit without touching your original
  branch, index or working files.
- Explicit full/filtered exports and import of explicitly selected physical edits.
- Human-defined build/test tasks with timeouts, bounded output and per-repo locks.
- Logical directory-size reports, global storage stats, GC, history squash and
  offline redb compaction.
- Generated Goose recipe/config fragment, consumer `AGENTS.md` and native
  `PreToolUse` hook. Server instructions work with other stdio MCP clients too.

See [all operations and examples](docs/operations.md),
[storage and concurrency](docs/architecture.md),
[builds and physical exports](docs/builds.md), and
[limits, recovery and disk maintenance](docs/operations.md#maintenance).

## Configuration

`vwork init` writes `<home>/config.toml` without overwriting existing configuration.
Restart the daemon after changing it:

```toml
compression_level = 3
port = 0                       # random loopback port
max_file_bytes = 67108864       # 64 MiB per file
max_results = 1000
max_output_bytes = 1048576      # per task stream; text/diff budget
max_cached_bases = 4            # in-memory path indexes
max_run_history = 20            # bounded results retained in redb; 0 disables retention
task_timeout_seconds = 600
```

Compression levels -7 through 22 are accepted. Changes affect new blob writes;
existing blobs remain readable. Equal content has one blob key across all repos.
See [configuration and resource limits](docs/architecture.md#resource-limits).

## Validation and releases

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
```

Tests cover persistence, fork isolation/deduplication, concurrent conflicts,
multiple repositories and linked Git checkouts, merge failures, export safety,
Git index/branch isolation, serialized builds, timeouts, authenticated daemon/MCP,
and package contents. CI runs on Windows x64 and macOS. A `v*` tag builds only a
Windows x64 executable and packages it through the Rust `vwork package` command.
See [release procedure](docs/releasing.md).

This package does not include a general Windows Explorer replacement or raw NTFS
size scanner. `workspace_size` covers virtual source; disk-wide scanning is a
separate tool. Full-text search currently scans the chosen virtual tree; it is
not a persistent inverted index and is not promised to outperform ripgrep.
