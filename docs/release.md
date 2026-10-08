# Release engineering

The supported release is one Windows x64 portable executable, `TorbenApp.exe`. The exact tagged
revision must pass source checks, an isolated launch, and artifact-transfer verification before
publication. Installer and deferred-platform tooling is described in
[deferred package release engineering](deferred-package-release.md).

## Prepare a version

Update the Cargo workspace, root/desktop/UI packages, Tauri configuration, and bundled plugin
manifest templates together. Regenerate Cargo's workspace entries in `Cargo.lock` without changing
third-party dependency versions. The desktop reads its displayed and preview versions from
`apps/desktop/package.json`; it does not keep separate version literals.

Add `docs/releases/<version>.md` using the established Chinese headings: `新功能` when applicable,
`问题修复`, `文档`, and `杂项`. Keep the Windows x64 scope, single-file download, and unsigned-build
notice. The workflow adds `下载`, the verified `SHA-256`, and the tag-specific `变更日志` link.

Run the gates in [testing](testing.md), review the PR, and merge it before creating
`v<workspace-version>` at the merged revision. Do not reuse a tag or replace an existing Release.
Delete the merged feature branch after verifying that its changes are included in `main`.

## Local portable build

With the pinned toolchain and locked dependencies already available:

```powershell
pnpm run prepare:python-manager
pnpm run build
node eng/verify-windows-portable-release.mjs --directory artifacts/torben-app-portable-windows-x64
```

The preparation command verifies a cached Python Install Manager 26.3 MSI or downloads it from
python.org into `.tools/python-manager`, then checks its pinned SHA-256. The portable build itself
uses offline, locked Cargo commands and fails if the verified MSI is missing. It embeds the seven
supported Windows providers and shim into the executable. Output is:

```text
artifacts/torben-app-portable-windows-x64/TorbenApp.exe
```

## Official release workflow

`.github/workflows/official-release.yml` is triggered by version tags and is the only publishing
workflow. Its build job installs the pinned tooling and locked dependencies, verifies the tag and
release-note file, and runs the Rust and frontend gates before building.

The candidate must pass:

- `refs/tags/v<workspace-version>` matches the workspace and release-note version;
- ProductVersion, Windows x64 PE target, and the exact single-file inventory match;
- a ten-second isolated launch creates `userData/state.db` and no nested `tools/shims/userData`;
- the downloaded artifact is byte-identical to the build job's SHA-256.

The publish job requires review in the protected `official-release` environment. It rechecks the
downloaded executable, appends the download/hash/changelog sections to the version's notes, and
calls `gh release create --verify-tag` once. A failed gate or existing Release stops publication.
Public assets contain only `TorbenApp.exe`; the checksum is in the Release body.

The current executable is not Authenticode-signed. Windows may show an unknown-publisher or
SmartScreen warning, which must remain in the release notes. Publisher signing is a separate
release-engineering decision.

## Preview and deferred workflows

| Workflow | Trigger and purpose | Output |
| --- | --- | --- |
| `windows-preview.yml` | Manual, read-only Windows x64 installer acceptance | Separate unsigned NSIS, MSI, and CLI previews, each with a warning and checksum; retained 14 days |
| `release.yml` | Manual, read-only future-platform acceptance | Six native targets, fourteen package launch jobs, and a verified unsigned development release set; retained 14 days |
| `plugin-registry-release.yml` | Protected manual registry review | Signed review artifact; see [registry publishing](plugin-registry-publishing.md) |

These workflows preserve the existing package paths for future milestones. They do not publish
the current portable Release. All external GitHub Actions are pinned to immutable revisions.

## Updates

Portable upgrades replace `TorbenApp.exe` while preserving `userData`. The application-side updater
and signed metadata tooling remain for a future installer/update-channel milestone. Builds accept
the public minisign key through `TORBEN_UPDATER_PUBLIC_KEY`; current development and portable builds
omit it and therefore do not query the fixed GitHub Release `latest.json` endpoint.
