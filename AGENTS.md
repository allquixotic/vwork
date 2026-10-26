# Developing vwork

This repository implements virtual workspaces. It is not itself a virtual checkout.
The consumer prompt is `examples/AGENTS.md`.

- Preserve the storage and isolation invariants in SPEC.md.
- All shipped programs are native Rust. Git is an existing runtime prerequisite.
- Default zstd level is 3. Never silently copy complete source into each workspace.
- Keep source changes, searches and diffs virtual. Physical exports must be explicit.
- Never point managed materialization at a registered source checkout.
- Keep build execution serialized per repository and revision-pinned.
- Keep administrative operations out of the model-visible MCP tool list.
- Use `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
  `cargo test --all-targets`. Verify Windows and macOS in CI.
- Only publish Windows x64 binaries, packaged as ZIP. Never publish local state,
  authentication files, logs, build directories, or macOS executables.
- Coordinate overlapping edits sequentially. No Agentlocks.
