# Changelog

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
