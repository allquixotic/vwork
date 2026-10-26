vwork 0.2.1 is the first Azure-signed Windows release. GitHub Actions requires a
valid Sean McNamara Authenticode signature and RFC 3161 timestamp before packaging
and publication. The ZIP has SHA-256 checksums and GitHub provenance attestation;
public signing evidence is included as a separate release asset.

This release retains the native Git version discovery and npm/Cargo/pip workflows
introduced in 0.2.0:

- `git describe` and `git rev-parse` work through a private Git index/HEAD/refs
  borrowing the original object store. History is not cloned. HEAD is the base
  commit; virtual edits are staged and appear dirty.
- Configure named package projects with `package_configure`; agents use the new
  `vwork_package_list` / `vwork_package_run` tools. Cargo targets and copied Python
  environments are reused per project; npm uses the shared source tree.
- Dependency input checks, offline modes and explicit manifest/lockfile import
  protect the virtual snapshot from accidental physical writeback.

Download **vwork-windows-x64.zip** and read `vwork/README.md` and
`vwork/docs/packages.md`. It contains the native Windows x64 executable and all
source/configuration/docs. Only Windows binaries are published. Git is required;
optional package actions need their installed npm/Cargo/Python tools. No admin
installation or symlinks are required by vwork itself.

Upgrade: stop the old daemon, back up your state, replace the executable, start
again, and reconnect MCP clients. The existing version-1 database is compatible.
Regenerate Goose recipe/AGENTS files to include package guidance. Builds still
materialize one source tree per repo; package caches and outputs consume ordinary
disk space. This release is not a filesystem driver or hermetic build sandbox.
