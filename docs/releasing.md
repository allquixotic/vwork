# Releasing

1. Run formatting, strict Clippy and all tests locally. Inspect the change and
   update CHANGELOG.md/version/release notes.
2. Push main and verify the CI jobs on Windows x64 and macOS.
3. Push a version tag (for example `v0.1.0`). The Windows-only release workflow
   tests the tag, builds `x86_64-pc-windows-msvc`, creates the ZIP with the native
   `vwork package` command, calculates SHA-256, and publishes a GitHub release.
4. Download `vwork-windows-x64.zip` and verify `SHA256SUMS` before mirroring it.

`vwork package --root <source-root> --binary <path-to-vwork.exe> --output <zip>`
allows only a Windows x64 PE executable named `vwork.exe`. It includes a fixed
allowlist of root documents and the source/tests/docs/examples/.github/.cargo trees.
Local databases, generated machine configs, `.git`, `target` and caches are not
included. The repository's GitHub workflows do not upload macOS binaries.

The ZIP has one top-level directory, `vwork`, including `bin/vwork.exe`. SHA256SUMS
is a separate release asset. No signing/notarization is claimed for the Windows
binary. A workstation application-control policy may still require the owner to
compile it locally or obtain their organization's normal software approval.

The first release is also mirrored to the owner's public S3 release bucket using
the explicitly requested ship-to-aws procedure. That upload is performed outside
GitHub Actions with dedicated local credentials; no AWS keys are in this repo or
the release ZIP. Future S3 publishing is a separate explicit release step.
