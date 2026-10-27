# Validation record

## 0.2.3 — Windows helpers and storage visibility

Verified 2026-10-26:

- [Source CI 40413998004](https://github.com/allquixotic/vwork/actions/runs/40413998004)
  passed formatting, strict Clippy and all integration tests: **33 on Windows**,
  **32 on macOS**, for `23ad9626b8306205c26bd37bb034d0098b4d9cd8`.
- On the `games` Windows VM, the new helper/task tests first failed because
  `GetConsoleWindow` returned a console handle. After the fix both passed,
  including redirected stdin/stdout/stderr and a deliberate nonzero exit code.
  All production Git/task/package command constructors use the tested helper.
- The new storage test first failed because MCP lacked `vwork_storage_info`.
  It now verifies read-only inspection, shared base manifests for another
  workspace at the same registered commit, forked revision sharing, and identical
  edited blobs across workspaces and repository registrations. A separate
  registration of the same Git path deliberately has a separate base manifest.
- Local formatting and strict Clippy for macOS and Windows targets passed. No
  tests or GUI applications ran on Sean's Mac. Corporate AVD policy was not
  reproduced. An old unfiltered Goose hook still requires its configuration
  upgrade; vwork does not change the external host's process-launch behavior.

## 0.2.2 — automatic MCP startup

Verified 2026-10-26:

- [Source CI 40408738471](https://github.com/allquixotic/vwork/actions/runs/40408738471)
  passed formatting, strict Clippy and all **30 integration tests** on Windows
  and macOS for `b2c64b2b2ea858146c7e52921f8bebf0c22922d6`.
- Seven daemon/configuration integration tests also passed on the `games`
  Windows VM. These exercise cold MCP startup, reuse after bridge exit, six
  simultaneous starters, stale connection metadata, bounded unresponsive-owner
  handling, explicit opt-out, invalid workspace binding and generated hook
  matching. Cold-start and missing-matcher regressions failed before the fix.
- The first Windows implementation exposed inherited MCP pipe handles keeping
  readers open after bridge exit. The explicit process handle allowlist fixes
  this (B13); the cold-start test waits for actual process/pipe completion.
- Local macOS and Windows-target strict Clippy, formatting and release-workflow
  actionlint passed. Tests were run on Windows and hosted CI, not Sean's Mac.
- Corporate AVD launch policy and the installed Goose configuration are not
  changed or reproduced by these fixtures. Existing installations must upgrade
  vwork and regenerate/merge their hook configuration to use the matcher.
- [Stable v0.2.2](https://github.com/allquixotic/vwork/releases/tag/v0.2.2) was
  published by [release CI 40409173923, attempt 2](https://github.com/allquixotic/vwork/actions/runs/40409173923/attempts/2).
  Attempt 1 failed downloading the pinned PowerShell signing module before
  signing began; the unchanged tag/source succeeded on retry. Both attempts ran
  all 30 tests successfully. The successful attempt verified Sean McNamara's
  Authenticode publisher and Microsoft RFC 3161 timestamp, then exercised cold
  MCP startup, owner reuse, doctor and stop with the signed binary.
- Downloaded ZIP: **3,250,848 bytes**, 46 entries and one Windows x64 executable.
  ZIP CRC, SHA256SUMS, bundled version and the signing evidence's executable hash
  match. `gh attestation verify` passed with the exact repository, release
  workflow, `refs/tags/v0.2.2` and source commit constraints. ZIP SHA-256:
  `cef11c66ea04177c675dcc47dfcaa92444ba2aa497ce942f67ea5067bd5675b0`.
  Signed executable SHA-256:
  `16054f166b9c146635a951ce2411082e9a1b2035159af9b94cd88d1f0afcd2c1`.
- A subsequent explicit `ship-to-aws` invocation uploaded this exact ZIP to the
  stable S3 key with REDUCED_REDUNDANCY. Anonymous HEAD and full-download hash
  verification passed; anonymous bucket listing returned 403.

## 0.2.1 — signed stable release

Verified 2026-10-26:

- [Stable v0.2.1+build.1](https://github.com/allquixotic/vwork/releases/tag/v0.2.1%2Bbuild.1)
  is public, latest and not a prerelease. Windows x64 is the only published binary.
- [Release CI 40382242131](https://github.com/allquixotic/vwork/actions/runs/40382242131)
  passed formatting, strict Clippy, all 25 integration tests, native build,
  Azure OIDC login/signing, signature verification, signed lifecycle smoke test,
  packaging and GitHub attestation verification. Source:
  `a6899f7ec44bfa85f8975caf6e04de57557f44cc`. Main CI also passed both platforms.
- Azure Artifact Signing used `allquixotic-signing/windows-public`. Authenticode
  publisher: `CN=Sean McNamara, O=Sean McNamara, L=Pasadena, S=MD, C=US`.
  Microsoft RFC 3161 timestamp verified. Certificate SHA-1:
  `0063631B695A60361BF66A861649BD0CB7F91CB2`. Signed executable SHA-256:
  `9d2b6e8fdd705d559060f782828a6c85ae1e46c04e43f6f9c5f17c811587a8dc`.
- ZIP: 3,230,881 bytes; SHA-256:
  `06898f230a0c69a682389813d1166915286405068c56478c16d35c9cc06ad4ce`.
  Download matches SHA256SUMS and the signing evidence's executable hash.
  GitHub provenance verification passed for the exact source/tag/workflow.
- [S3 ZIP](https://fastrock-dl-582961286906-us-east-1-an.s3.us-east-1.amazonaws.com/vwork-windows-x64.zip)
  matches the GitHub ZIP. The explicitly invoked ship-to-aws helper verified
  anonymous HEAD and full-download SHA-256, REDUCED_REDUNDANCY storage and denied
  anonymous bucket listing. Failed tag v0.2.1 remains unchanged; B10/B11 record
  the corrected immutable-ID OIDC subject and explicit PSGallery registration.

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

- [Source CI on Windows and macOS](https://github.com/allquixotic/vwork/actions/runs/40376971645): passed on commit `cb6ede3`, including all 25 tests.
- [Tagged Windows release workflow](https://github.com/allquixotic/vwork/actions/runs/40377488745): passed validation, optimized build and native daemon lifecycle smoke test.
- [Release v0.2.0](https://github.com/allquixotic/vwork/releases/tag/v0.2.0).
- [Verified S3 ZIP](https://fastrock-dl-582961286906-us-east-1-an.s3.us-east-1.amazonaws.com/vwork-windows-x64.zip).

The ZIP contains 42 entries, is 3,216,260 bytes, and includes one executable:
`vwork/bin/vwork.exe` (7,413,760 bytes; Windows x64 PE). CRC and SHA256SUMS
verification passed. Its imports are Windows system DLLs; there is no separate
MSVC runtime or zstd DLL dependency. Source, package examples and updated agent
guidance are included. No macOS or Linux binaries are published.

SHA-256:

```text
ba8e2d66399ba09bd868b0d3edc262f19c9012d901473b5a70a21fee23cfa138
```

The S3 copy uses REDUCED_REDUNDANCY. Anonymous HEAD and a complete anonymous HTTPS
download passed size/hash checks; anonymous bucket listing returned 403. The
optimized Mac build also passes. Corporate AVD execution policy and arbitrary
third-party package install scripts are not reproduced by these fixtures.

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

This subsection records 0.1.0. The S3 URL is mutable and may lag GitHub releases;
use the GitHub v0.1.0 release for the historical artifact described here.

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

## 0.3.0 — redb Git storage and temporary copies

- [Tagged-source CI 40419171696](https://github.com/allquixotic/vwork/actions/runs/40419171696)
  passed formatting, strict Clippy and all **50 Windows / 49 macOS integration
  tests** for `0b24894eac221782d70420c7f9cada962df35188`, including the Git
  membership regression and real offline uv sync/check and CDK synth/list fixtures.
- Baseline Windows `v21_redb_owns_git` failed after source removal; the redb object
  backend now passes reads, history, commits and reopen without the original Git
  store. Tests also cover legacy migration, libgit2 clone transport, import batches
  and deduplicated objects retaining repository membership. The membership test
  failed before the fix and now passes with a native Git export of a reused blob.
- Baseline temporary-copy tests failed with unknown operation. Windows regressions
  now pass create/renew/release, ownership protection, expiry recovery, copy limits,
  active-job protection, old-build reclamation and deliberate recovery-limit
  failure without data loss. Eight real-daemon tests passed on games, including
  periodic expiry deletion without any subsequent model request.
- Baseline uv/CDK plan test rejected uv as an unknown manager. Structured plans
  now pass, including refusal to expose CDK deployment actions. The actual-tool
  CI fixture verifies lease-local outputs, explicit uv.lock import, deletion on
  release and invalidation of installed state when a new lease is allocated.
- Strict local formatting/Clippy and Windows cross-compilation pass. No tests or
  GUI ran on Sean's Mac. The final copied Windows workspace suite passed 38 tests;
  four source/native-package tests were filtered and the real uv/CDK fixture is
  enabled in CI, not the copied EXE. Hosted CI runs the complete suite.
- Corporate AVD policy, a live Goose conversation, external credential helpers
  and arbitrary third-party build scripts are outside these fixtures. Global
  package caches are not lease-owned; temporary copies have count/TTL limits, not
  an operating-system disk quota. Cleanup errors remain visible for retry.
- [Stable v0.3.0](https://github.com/allquixotic/vwork/releases/tag/v0.3.0) was
  published by [release CI 40419177762](https://github.com/allquixotic/vwork/actions/runs/40419177762).
  The workflow independently passed all 50 Windows integration tests, built and
  signed the executable, verified Sean McNamara's publisher and Microsoft RFC 3161
  timestamp, and passed cold MCP startup, owner reuse, doctor and stop with the
  signed executable before publication.
- Downloaded ZIP: **4,220,070 bytes**, 55 entries and one Windows x64 executable.
  CRC, SHA256SUMS, bundled version, Git/transport license files and the executable
  hash in the signing evidence all match. GitHub provenance verification passed
  with the exact repository, release workflow, `refs/tags/v0.3.0` and source
  commit. Independent Windows `Get-AuthenticodeSignature` returned **Valid** for
  the downloaded executable with the expected publisher and timestamp; its
  version command returned `vwork 0.3.0`.
  ZIP SHA-256:
  `d5713d536867372cf5afe147293f3cff1d028681ff7e0f6c6950fc99be501cf0`.
  Executable SHA-256:
  `d063ab84f177b8fb80d76d48dc76d2cbea926d271586b8b5a816a74c18338839`.
- S3's mutable key remains at the last explicitly requested upload (0.2.2).
  Publishing this GitHub release does not invoke the manual-only upload skill.

## 0.3.1 pre-release verification

Native Windows fixtures on games passed: two maintenance tests and 44 workspace
integration tests (four package/distribution tests reserved for hosted CI).
The new coverage checks actual free-page compaction with live content preserved,
hourly scheduling and idle/busy boundaries, abrupt process termination after a
committed transaction, corrupted-header and earlier-backup preservation, online
CLI/RPC identity reuse, incremental fetch, pruning, private commits, pinned
workspaces, MCP dispatch, and legacy imports requiring a source only once.
Mac compilation and strict all-target Clippy passed; no tests/GUI ran on Sean's Mac.
Full Windows/macOS package fixtures and signed release evidence follow in CI.

## 0.3.1 published release — 2026-10-26

- [CI 40425570540](https://github.com/allquixotic/vwork/actions/runs/40425570540)
  passed formatting, strict Clippy and all fixtures: 59 Windows / 58 macOS tests,
  including actual uv/CDK, npm/Cargo/pip and distribution packaging. No local Mac tests ran.
- [Release run 40425591796](https://github.com/allquixotic/vwork/actions/runs/40425591796)
  passed validation, release build, Azure Authenticode/RFC 3161 signing, publisher
  checks, signed native lifecycle, packaging and provenance. Source:
  `60a2602667357347282a2047c64bb4b680ffce61`.
- [v0.3.1](https://github.com/allquixotic/vwork/releases/tag/v0.3.1) is public/latest.
  Downloaded ZIP: 4,284,361 bytes; SHA-256
  `c9ab0a0e1718142f401c31a95ae54f86951c7614cd4e5d8a5c0bd82964acb99b`.
  Signed executable SHA-256:
  `c25358e723f096a88482bb509f784b8939563c7a05eb8d6880254e4124ac776a`.
  Downloaded bytes match GitHub's digest, checksum file and signing receipt;
  provenance verified against the exact source/tag/release workflow.
- Publisher: Sean McNamara; certificate SHA-1
  `F65E5B72C3747B4450FC367E1AD28B15F1418729`; Microsoft timestamp present.
- The explicitly invoked ship-to-aws helper overwrote
  [vwork-windows-x64.zip](https://fastrock-dl-582961286906-us-east-1-an.s3.us-east-1.amazonaws.com/vwork-windows-x64.zip)
  with identical bytes using REDUCED_REDUNDANCY. Anonymous HEAD size/class and
  full-download SHA-256 passed; anonymous listing remained 403.


## 0.3.2 published release — 2026-10-26

- Shared Goose policy enables host reads/edits/scripts and other MCPs, defaults
  repository work to vwork, and retains indefinite workspace_export. A narrow
  optional hook detects direct unmanaged Git creation; no filesystem sandbox claim.
- Three new V27 policy tests failed against the strict baseline on native Windows,
  then passed after implementation. A shell-argument read false positive was also
  reproduced and fixed before publication (B28). Native checks passed 3 policy,
  9 daemon and 44 workspace fixtures; four package/distribution cases ran in CI.
- [CI 40428428375](https://github.com/allquixotic/vwork/actions/runs/40428428375)
  passed strict lint and all tests: **62 Windows / 61 macOS**, including package
  tools and ZIP fixtures. No tests/GUI ran on Sean's Mac; local lint/check passed.
- [Release 40428430764](https://github.com/allquixotic/vwork/actions/runs/40428430764)
  built, signed, timestamped, verified and published
  [v0.3.2+build.1](https://github.com/allquixotic/vwork/releases/tag/v0.3.2%2Bbuild.1)
  from `8676d41cf15733340943936fbf771c9643910199`. The original v0.3.2 attempt was
  cancelled before publication; its tag was preserved.
- Downloaded ZIP: 4,284,699 bytes; SHA-256
  `98cf400c07643d94a4cc07716bf9fd67d7ad3f878b0ff2256b15131b9bbb2a84`.
  Executable SHA-256:
  `54da723088fc1b0736c663611c483df1604c7be28397b9f2d16cd0e031f0763d`.
  GitHub digest, checksum file and signing receipt match; provenance verified for
  the exact repository/workflow/tag/source commit. Packaged policy/prompt sources
  match after normalizing Windows checkout line endings.
- Independent Windows verification of the downloaded executable: signature Valid,
  Sean McNamara publisher, certificate SHA-1
  `F65E5B72C3747B4450FC367E1AD28B15F1418729`, Microsoft timestamp,
  version `vwork 0.3.2`; all four native hook checks passed, including the shell
  argument read regression. The verification extraction was removed afterward.
- Existing Goose configuration is not rewritten automatically. Regenerate setup,
  enable Developer, replace the old vwork hints/hook and start a new conversation.
  No Fastrock binary update is needed. S3 remains at the last explicitly requested
  0.3.1 upload; ship-to-aws was not invoked for this release.
