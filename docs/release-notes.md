Initial vwork release: native Rust CLI, shared redb daemon, stdio MCP bridge,
Goose integration and policy hook, zstd-compressed virtual edits, multi-repository
workspaces, Git snapshot export, explicit materialization/import, serialized
build tasks, storage stats and maintenance.

Download **vwork-windows-x64.zip** and read `vwork/README.md`. It contains the
Windows x64 executable and complete source/configuration/docs. Git must already
be available. No admin installation or symlinks are required by vwork.

This is an initial implementation. It is not a mounted filesystem. Builds use a
plain exported tree without `.git` metadata; Git-dependent version discovery needs
explicit configuration. Search scans the virtual tree rather than using a
persistent full-text index. See the documented limits before adopting it for a
large production monorepo.
