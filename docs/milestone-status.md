# Windows-first milestone status

This document maps the original Torben App greenfield plan to repository evidence and the current
Windows-first delivery milestone. It distinguishes implemented behavior from the smaller set of
Windows x64 gates required for the next supported release.

## Approved plan changes

- The current delivery, CI, preview, and official release target is Windows x64. Windows ARM64,
  macOS, and Linux are deferred and do not block feature completion or release. Existing platform
  implementations, fixtures, and manual cross-platform workflows remain as future engineering
  assets, without a current support or parity commitment.
- New feature work completes the shared Core and Windows x64 path first. Deferred-platform work is
  resumed only by an explicit milestone change; shared contracts and platform abstractions remain
  portable in the meantime.
- The initial plan included Developer Certificate of Origin enforcement. That requirement was
  explicitly cancelled before the first commit. The DCO workflow, checker, tests, and sign-off
  documentation have been removed; Apache-2.0 remains the project license.

## Repository implementation

### Foundation

- The Cargo and pnpm workspace contains the desktop application, shared contracts, Core, plugin
  host, CLI, shim, private UI package, and six first-party native plugins.
- Product identity is fixed as `Torben App`, `torben`, and
  `io.github.torbenxiong.torbenapp` across package, Cargo, Tauri, release, and updater metadata.
- Core owns SQLite migrations, platform-standard paths, the managed application library,
  cross-process locking, durable journals, cancellation markers, diagnostic logs, settings, and
  startup recovery. Frontend and plugin processes do not access SQLite directly.
- Full Core startup transactionally synchronizes the ordered six-application directory, all six
  official sources, and the winget, Homebrew, apt, and DNF source descriptors into SQLite. App list,
  search, and detail queries read that persisted Core-owned snapshot.
- The desktop exposes Overview, Catalog, application detail, Installed, Tasks, Plugins,
  Diagnostics, and Settings routes. Theme, English/Simplified Chinese localization, keyboard
  navigation, reduced-motion behavior, and responsive minimum-window layouts are covered by
  frontend tests.
- Shell integration is explicit and user-level. Windows user `Path` and Unix login profiles use
  ownership-aware, receipt-backed transactions; system `PATH`, elevation, telemetry, accounts,
  cloud synchronization, background services, and project-level version pinning remain outside the
  product boundary.

### Node.js vertical milestone

- Official metadata discovery, exact/LTS/Current resolution, signed checksum verification,
  per-target archive selection, safe extraction, staging health checks, atomic commit, multi-version
  installation, global selection, external read-only discovery, cancellation, rollback, recovery,
  and permanent uninstall are implemented in the shared Core path.
- A single managed shim directory exposes `node`, `npm`, and `npx`. Selection changes are
  receipt-backed, and command resolution must remain inside the exact managed installation.
- Real CLI subprocess and desktop-command fixture tests cover discovery through uninstall, fresh
  terminal resolution, GUI/CLI concurrency, cross-process cancellation, and restart recovery.

### Application and source expansion

- Eclipse Temurin, Python, Git, Visual Studio Code, and Codex CLI have official-only metadata,
  per-platform distribution validation, supply-chain checks, staging, health checks, external
  read-only discovery, Schema UI, selection where applicable, managed updates, and uninstall.
- Python uses the PSF Install Manager target mode on Windows and verified CPython source builds on
  macOS/Linux. Git, VS Code, and Codex use their documented platform-specific official assets and
  signatures. Codex management never reads or changes authentication, Provider, configuration,
  history, plugin, skill, or credential-store data.
- winget, Homebrew, apt, and DNF adapters expose availability, installed-state inspection, reviewed
  plans, explicit system-change acceptance, ownership reconciliation, and source migration.
  External packages are not silently claimed or removed.

### Release and plugin ecosystem

- The required CI and preview workflows validate Windows x64. The official tag workflow signs and
  publishes only Windows x64 NSIS/MSI packages and the matching CLI, with deterministic metadata,
  updater signatures, install-and-launch acceptance, and immutable GitHub Release publication.
- A separate manual development workflow retains the original six-target packaging and native
  acceptance design. It is future-platform evidence and is not a current release gate.
- Torben App and managed-application updates default to notification. Managed automatic updates are
  opt-in per application and run only in a foreground desktop session.
- The official plugin registry has a two-level Ed25519 trust chain, publisher and package
  revocation, minimum-host enforcement, exact per-platform hashes, rollback-resistant sequences,
  bounded HTTPS refresh, verified cache, developer-mode sideloading, and schema-only plugin UI.
- The deterministic registry publisher and protected main-only artifact workflow keep private keys
  in temporary runner storage, independently re-verify both signature levels and every target hash,
  require the immediately previous signed sequence, and upload only a short-lived review artifact.
  They do not deploy a public registry endpoint.

The authoritative test-to-requirement mapping is maintained in
[test and acceptance evidence](testing.md). Packaging and signing invariants are maintained in
[release engineering](release.md), and registry key handling is maintained in
[plugin registry publishing](plugin-registry-publishing.md).

## Recorded external evidence

- Pull request [#1](https://github.com/TorbenXiong/torben-app/pull/1) merged the reviewed bootstrap
  into `main` as commit `785dfa4423710f29dad10d041bf54d62d854902b` on 2026-08-26.
- The final pull-request CI
  [run 32868153680](https://github.com/TorbenXiong/torben-app/actions/runs/32868153680)
  passed on Windows, macOS, and Ubuntu for the exact feature revision before merge.
- The post-merge `main` CI
  [run 32909764190](https://github.com/TorbenXiong/torben-app/actions/runs/32909764190)
  passed the same Windows, macOS, and Ubuntu matrix for the exact merge commit. The scheduled
  official-catalog job was intentionally skipped because this run was triggered by a push.
- Pull request [#9](https://github.com/TorbenXiong/torben-app/pull/9) merged the cross-platform
  native-package acceptance fixes into `main` as commit
  `6a00b212e2404df13b2155b996a752e07b20e3e6` on 2026-08-27.
- The final pull-request CI
  [run 33070388415](https://github.com/TorbenXiong/torben-app/actions/runs/33070388415)
  passed on Windows, macOS, and Ubuntu for exact feature revision
  `69b792efa61935e1eec3bbd3d1d684d44e454c02` before merge.
- The feature-revision development release
  [run 33070415451](https://github.com/TorbenXiong/torben-app/actions/runs/33070415451)
  successfully built all six native targets, passed all fourteen native package installation and
  sustained-launch jobs, and verified the complete six-target release set. This included Rocky
  Linux 10.2 RPM installation on both x86_64 and ARM64. Its outputs are explicitly unsigned
  development artifacts.
- The post-merge `main` CI
  [run 33072700894](https://github.com/TorbenXiong/torben-app/actions/runs/33072700894)
  passed the Windows, macOS, and Ubuntu matrix for exact merge commit
  `6a00b212e2404df13b2155b996a752e07b20e3e6`.
- The post-merge `main` development release
  [run 33072736312](https://github.com/TorbenXiong/torben-app/actions/runs/33072736312)
  independently rebuilt all six native targets from that merge commit, passed all fourteen native
  package installation and sustained-launch jobs, including Rocky Linux 10.2 x86_64 and ARM64,
  and passed the complete six-target release-set verifier. Its outputs are also unsigned
  development artifacts, not an official release.
- Pull request [#14](https://github.com/TorbenXiong/torben-app/pull/14) merged the Temurin legacy
  signature-metadata compatibility fix into `main` as commit
  `8081f3f7f0aa205151d7d011e01f5ff0caef94e9` on 2026-08-28. The fix excludes historical
  Adoptium packages that cannot supply the detached signature required by Torben, rather than
  weakening signature verification or failing the complete catalog.
- The manually dispatched post-merge `main` CI
  [run 33140148036](https://github.com/TorbenXiong/torben-app/actions/runs/33140148036)
  passed Windows, macOS, Ubuntu, and the read-only official-catalog job for that exact merge
  commit. The uploaded `live-official-catalogs` artifact contained the expected
  `catalog-summary.json`, `node.json`, `temurin.json`, `python.json`, `git.json`, `vscode.json`,
  and `codex.json` files. Independent artifact inspection found 863 Node.js, 79 Temurin, 5 Python,
  5 Git, 5 Visual Studio Code, and 5 Codex versions; every non-empty catalog contained at least one
  recommended version.
- That successful `workflow_dispatch` run is a manual operational preflight, not evidence that the
  scheduled trigger itself has executed successfully. With the current `17 3 * * 1` schedule, the
  first eligible scheduled window after this evidence is 2026-08-31 03:17 UTC
  (2026-08-31 11:17 China Standard Time).
- Pull request [#16](https://github.com/TorbenXiong/torben-app/pull/16) merged the persistent
  application and source catalog into `main` as squash commit
  `5c5408e75243f4c2dea307313fe8afe1047fe373` on 2026-08-28.
- The pull-request CI and post-merge `main` CI
  [run 33158835795](https://github.com/TorbenXiong/torben-app/actions/runs/33158835795) and
  [run 33160929903](https://github.com/TorbenXiong/torben-app/actions/runs/33160929903) passed the
  Windows, macOS, and Ubuntu matrix. These runs included the default-off CLI and desktop Node.js
  lifecycle fixtures, which completed successfully in the locked CI dependency environment.

## Evidence still requiring external state

The following items are not proven by local source or simulated fixtures and must not be described
as complete until their authoritative remote evidence exists:

1. Configure the protected `official-release` environment with reviewed Windows Authenticode and
   Tauri updater credentials. Only a successful exact-version tag run can prove Windows package,
   CLI and sidecar signing, updater signatures, and immutable GitHub Release publication.
2. Configure the protected `official-plugin-registry` environment with the offline root,
   publisher keys, and reviewed public trust root. Generate and review an artifact from committed
   production registry inputs.
3. Provision an immutable HTTPS origin for the reviewed registry tree, then configure release builds
   with its exact `registry.json` URL and trust root. Refresh and install every published plugin on
   Windows x64. Public hosting is not currently live; deferred-platform acceptance will be added
   when those milestones resume.
4. Record the first successful `schedule`-triggered read-only check against every official provider
   catalog. Manual run 33140148036 proves current upstream availability and the same job path, but
   it does not prove that GitHub invoked the weekly schedule. Local fixtures remain the default test
   authority for deterministic behavior.
5. Complete project-name, domain, trademark, and package-registry registration checks before a
   public release. The repository's initial name collision search is not legal clearance.

Until these steps are complete, locally built packages are development artifacts. Missing signing
credentials must never be replaced with bypass switches, and unsigned artifacts must never be
presented as an official release.
