# Validation record

## 0.2.0

Local validation on 2026-10-26 uses the same Apple Silicon Mac and stable Rust
toolchain described below. The full suite passes **25 integration tests**,
including real offline npm, Cargo and pip fixtures. Formatting and strict Clippy
also pass.

`build_git_metadata` verifies annotated-tag discovery, base HEAD, dirty state for
added/modified/deleted virtual files, the private index/alternates arrangement,
accurate physical change reports and unchanged original index/HEAD/object count.
`package_workflows` compiles/tests a Rust project whose build.rs calls Git and
checks the managed target directory. `package_writeback` installs an npm fixture,
imports its generated lockfile, runs a Node script and checks invalidation.
`package_input_changes` creates a copied Python venv, installs empty offline
requirements, runs pip check, recreates the environment after input changes and
cleans it. `package_process_paths` checks that tool-facing Windows paths omit NT
verbatim prefixes. Protocol tests keep package configuration outside model tools.

The Mac test command selects an installed Python compatible with copied venvs:

```sh
VWORK_TEST_PYTHON=/absolute/path/to/python3 cargo +stable test --locked --all-targets
```

The first standalone Python on this Mac's PATH cannot load its libpython after
`venv --copies`; this also fails outside vwork. The installed Miniconda Python
passes. CI selects Python 3.12 and Node 22 explicitly on Windows/macOS. These are
test dependencies, not files bundled into the release.

Cross-platform CI and release artifact verification are recorded below after
publication. Corporate AVD execution policy and arbitrary third-party package
install scripts are not reproduced by these fixtures.

## 0.1.0 historical validation

Local validation on 2026-10-26 used an Apple Silicon Mac (`arm64`) and
`rustc 1.99.1 (58cf8f9c3 2026-10-13)`.

- `cargo fmt --all -- --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo test --locked --all-targets`: 20 integration tests passed.
- `cargo build --locked --release`: passed.
- The optimized executable registered this repository, created a virtual snapshot,
  materialized its managed build tree, and successfully ran the then-current test suite (18 tests)
  through `workspace_run_task`. This exercises the full local daemon/build path.

## Small synthetic scale check

A local fixture contained 10,000 small Rust files in 100 directories, totaling
426,000 logical bytes. It was committed to Git before registration. Operations
used separate native CLI processes talking to one already-running daemon.

| Operation | Observed elapsed time |
|---|---|
| Import initial path manifest / create workspace | 0.424 s |
| Create 100 virtual forks | 1.139 s total |
| Query recursive logical directory sizes | 0.009 s |

Afterwards the shared database reported two repositories (including the
self-host test), two revisions, 102 workspace references, zero overlay blobs and
a 4,460,544-byte database file. The forks created neither extra content blobs nor
extra revisions. This verifies the intended sharing mechanism.

These are single observations from a small synthetic fixture, not a benchmark
suite or a throughput guarantee. No claim is made for million-file trees,
Windows Defender behavior, AVD storage, complex Git packs or full-text search
speed. Measure those with your actual monorepo before relying on capacity limits.

## CI and client boundaries

The CI workflow validates Windows x64 and macOS. The release workflow independently
tests the tagged Windows revision and packages its x64 executable. Follow the
[workflow history](https://github.com/allquixotic/vwork/actions) for exact run logs.

Protocol tests exercise the actual stdio bridge and authenticated daemon; hook
tests verify routing decisions. A live Goose/LLM conversation and the corporate
AVD policy environment require the consumer smoke test in `goose.md`. They are
not simulated or claimed as verified here.

## Published artifact

- [Final source CI, Windows and macOS](https://github.com/allquixotic/vwork/actions/runs/40374269796): passed.
- [Windows release workflow](https://github.com/allquixotic/vwork/actions/runs/40374319348): passed, including native `start`, `doctor` and `stop`.
- [Release v0.1.0](https://github.com/allquixotic/vwork/releases/tag/v0.1.0).
- [Verified S3 copy](https://fastrock-dl-582961286906-us-east-1-an.s3.us-east-1.amazonaws.com/vwork-windows-x64.zip).

The ZIP is 3,131,804 bytes, with 37 entries and one executable:
`vwork/bin/vwork.exe` (7,220,224 bytes). Its PE machine type is x86_64.
CRC validation and the published SHA256SUMS file match. The executable imports
Windows system DLLs; it does not import a separate Visual C++ runtime or zstd DLL.
No macOS or Linux executable is included.

SHA-256:

```text
8d099896c65e9d06066c68b70cd50a229330f96b5e72a8d30ac44d755b5e20c7
```

The ship-to-aws upload used REDUCED_REDUNDANCY storage. Anonymous HEAD and full
HTTPS download passed size/hash verification; anonymous bucket listing was denied.

The final local cleanup probe dropped 100 synthetic workspace references in
0.884 seconds. As with the creation measurements, this is one small local test,
not a guarantee for another filesystem or workload.
