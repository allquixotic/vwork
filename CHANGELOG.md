# Changelog

## 0.3.0

- Store Git objects, refs and all committed source in redb using git2/libgit2; share identical objects daemon-wide and migrate old registrations without modifying original repositories.
- Expose structured clone/register tools; no permanent cloned checkout or external Git object dependency.
- Add temporary-copy create/list/renew/release, bounded leases, startup/periodic cleanup, active-job protection, source recovery and legacy build cleanup.
- Put task/package copies, private Git views, environments and outputs under leases; return cleanup handles with results.
- Add uv and local CDK package actions with explicit source writeback and scoped artifacts.
- Suppress Windows helper consoles and expose read-only storage counters and sharing guidance.

Tag v0.2.3 was retained without a published release; its pending CI release was cancelled when clone/register was added to this change.

## 0.2.2

- MCP automatically starts or reuses the shared daemon when an enabled extension
  loads; `--no-auto-start` retains manual lifecycle management.
- Bounded health/startup waits and per-home locks handle simultaneous clients,
  stale connection metadata and existing database owners without takeover.
- Windows daemon handle inheritance excludes MCP pipes; bridges exit cleanly
  while their shared daemon stays running.
- Generated Goose hook matchers skip process startup for vwork and other
  already-allowed tools while retaining native-tool blocks and failure policy.

## 0.2.1

- Windows releases use Azure Artifact Signing through GitHub OIDC, with mandatory
  publisher and timestamp verification before packaging.
- Public signing evidence, SHA-256 checksums and GitHub provenance attestations
  accompany the Windows ZIP. Virtual workspace and database formats are unchanged.

## 0.2.0

- Shared build checkouts now support native Git version discovery through a
  private index/HEAD/refs and borrowed source objects, without cloning history.
- Base commit, virtual revision and Git paths are exposed to build scripts.
  Virtual edits appear staged and make `git describe --dirty` dirty.
- Native npm/Cargo/pip project plans, MCP list/run tools and human configuration.
- Reusable per-project Cargo targets and copied Python environments; dependency
  input fingerprints prevent silently using stale npm/pip installations.
- Package manifest/lockfile and tracked-source changes reported for explicit import.
- Offline package integration fixtures, Windows/macOS CI, updated agent guidance.

## 0.1.0

- Shared redb daemon and native Rust CLI/MCP/hook in one executable.
- Git-backed bases, immutable virtual revisions, BLAKE3 deduplication and
  configurable zstd compression (default level 3).
- Multi-repo create/fork/read/search/edit/diff/merge/restore workflows.
- Explicit physical export/import and Git commit export without index mutation.
- Per-repository serialized build tasks with timeout and bounded output.
- Workspace size, storage stats, squash, GC and offline compaction.
- Goose config/recipe/AGENTS/hook generation.
- Windows x64 ZIP releases; Windows/macOS integration tests.
