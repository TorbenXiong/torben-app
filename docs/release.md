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
notice. Before publication, manually add `下载`, the verified `SHA-256`, and the tag-specific `变更日志` link.

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

## Manual publication

The repository has no CI, preview, or automated release workflows. Build and verify the exact
reviewed revision locally before manually publishing a version-tagged Release.

The candidate must pass:

- `refs/tags/v<workspace-version>` matches the workspace and release-note version;
- ProductVersion, Windows x64 PE target, and the exact single-file inventory match;
- a ten-second isolated launch creates `userData/state.db` and no nested `tools/shims/userData`;
- the transferred executable is byte-identical to the verified build's SHA-256.

Run `eng/verify-windows-portable-release.mjs` again after transferring the executable, using
`--expected-sha256 <verified-build-sha256>`. Publication is a separate manual action after review;
public assets contain only `TorbenApp.exe`, with the checksum in the Release body.

The current executable is not Authenticode-signed. Windows may show an unknown-publisher or
SmartScreen warning, which must remain in the release notes. Publisher signing is a separate
release-engineering decision.

Installer and deferred-platform scripts remain available for local validation. Plugin registry
artifacts are generated and verified locally; see [registry publishing](plugin-registry-publishing.md).

## Updates

Portable upgrades replace `TorbenApp.exe` while preserving `userData`. The application-side updater
and signed metadata tooling remain for a future installer/update-channel milestone. Builds accept
the public minisign key through `TORBEN_UPDATER_PUBLIC_KEY`; current development and portable builds
omit it and therefore do not query the fixed GitHub Release `latest.json` endpoint.
