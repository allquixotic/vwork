# Releasing

1. Run formatting, strict Clippy and all tests locally. Inspect the change and
   update CHANGELOG.md/version/release notes.
2. Push main and verify the CI jobs on Windows x64 and macOS.
3. Push a version tag (for example `v0.2.0`). The Windows-only release workflow
   tests the tag, builds `x86_64-pc-windows-msvc`, signs through Azure Artifact
   Signing, verifies the publisher and timestamp, then runs the lifecycle smoke
   test against that signed executable. It creates the ZIP with native `vwork
   package`, calculates SHA-256, attests the ZIP, and publishes a stable release.
4. Download `vwork-windows-x64.zip`, verify `SHA256SUMS` and `gh attestation verify
   vwork-windows-x64.zip --repo allquixotic/vwork` before mirroring it.

`vwork package --root <source-root> --binary <path-to-vwork.exe> --output <zip>`
allows only a Windows x64 PE executable named `vwork.exe`. It includes a fixed
allowlist of root documents and the source/tests/docs/examples/.github/.cargo trees.
Local databases, generated machine configs, `.git`, `target` and caches are not
included. The repository's GitHub workflows do not upload macOS binaries.

The ZIP has one top-level directory, `vwork`, including `bin/vwork.exe`. SHA256SUMS
is a separate release asset. Starting with 0.2.1, the executable must have a valid
Authenticode signature from `CN=Sean McNamara, O=Sean McNamara, L=Pasadena, S=MD,
C=US` and a Microsoft RFC 3161 timestamp. Releases 0.1.0 and 0.2.0 were unsigned.
`windows-signing.json` records the executable hash, certificate and source commit;
`windows-attestation.sigstore.json` records GitHub's package provenance.

The tag-only `azure-artifact-signing` GitHub environment uses Azure OIDC with
the `fastrock-github-release` application and `allquixotic-signing/windows-public`
certificate profile. Its separate `vwork-github-release` federated credential
accepts only this repository's signing environment. No certificate private key
or long-lived Azure client secret is stored in GitHub. Signing or verification
failure prevents publication; there is no unsigned fallback. An organization's
application-control policy may still require its normal software approval.

The OIDC subject includes immutable owner/repository IDs:
`repo:allquixotic@322644/vwork@1434741335:environment:azure-artifact-signing`.
Match Azure trust to the actual token subject, audience and issuer; do not assume
the legacy name-only format for new repositories. CI registers PSGallery when
the hosted runner lacks it, before Azure's pinned signing action installs its
module. A failed tag stays unchanged; stable `vVERSION+build.N` tags permit a new
packaging attempt without changing the executable's application version.

The first release is also mirrored to the owner's public S3 release bucket using
the explicitly requested ship-to-aws procedure. That upload is performed outside
GitHub Actions with dedicated local credentials; no AWS keys are in this repo or
the release ZIP. Future S3 publishing is a separate explicit release step.
