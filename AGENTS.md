# Developing vwork

This repository implements virtual workspaces. It is not itself a virtual checkout.
The consumer prompt is `examples/AGENTS.md`.

- Preserve the storage and isolation invariants in SPEC.md.
- All shipped programs are native Rust. Use git2/libgit2 for Git; store all persistent Git objects, refs and source in redb.
- Default zstd level is 3. Never silently copy complete source into each workspace.
- Keep source changes, searches and diffs virtual. Physical exports must be explicit; tool copies must be tracked, bounded and reclaimable.
- Never point managed materialization at a registered source checkout.
- Keep build execution serialized per repository and revision-pinned.
- MCP may clone/register repositories through structured tools, as explicitly
  authorized by Sean. Keep task/package configuration and destructive global
  maintenance outside MCP; never expose arbitrary shell or Git arguments.
- Use `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test --all-targets`. Verify Windows and macOS in CI.
- Only publish Windows x64 binaries, packaged as ZIP. Never publish local state,
  authentication files, logs, build directories, or macOS executables.
- Release binaries must be signed in GitHub Actions using Azure Artifact Signing,
  verified for Sean McNamara's publisher identity and RFC 3161 timestamp, and
  packaged only after verification. No unsigned release fallback.
- Coordinate overlapping edits sequentially. No Agentlocks.
