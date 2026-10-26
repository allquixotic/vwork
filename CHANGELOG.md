# Changelog

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
