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

## Current software-plugin support decision

- Git, Visual Studio Code, and Codex CLI remain temporarily unsupported.
  Production builds publish no capabilities or managed sources for them and reject their Core
  management actions with `capability_not_available`.
- Node.js, Eclipse Temurin JDK, Python, Rust, MySQL, Redis, and PostgreSQL are restored software plugins. Their provider payloads are
  installed under `userData/plugins`, and managed runtimes remain under the same `userData` root.
- Rust uses the official stable MSVC toolchain and shows the three newest stable toolchains by
  default while retaining exact-version installation. MySQL uses official Community Server ZIP
  archives and exposes 8.4.6, 8.0.46, and 5.7.44.
  Redis uses SHA-256-pinned Windows x64 community builds from `redis-windows`, because upstream Redis
  does not publish a native Windows Open Source binary. Each plugin supports independent version
  installation, terminal selection, and provider-owned data below `userData`.
- PostgreSQL uses fixed EDB Windows x64 installers whose SHA-256 values are pinned to exact
  Microsoft WinGet manifests. Core invokes only EDB's `extract-only` mode, installs no runtime,
  service, pgAdmin, or StackBuilder component. Runtime installation never initializes a cluster;
  explicit instance creation does so in a version-pinned managed directory. PostgreSQL client
  configuration lives below `userData/application-data/postgresql/client`; avoiding a shared `PGDATA`
  prevents incompatible major versions from silently sharing one cluster.
- MySQL, Redis, and PostgreSQL share a complete managed-instance lifecycle in Core and SQLite:
  create, start, stop, status, backup, restore, and confirmed delete. Desktop and CLI use identical
  contracts; instances listen only on loopback, are not Windows services, preserve their data across
  plugin changes and runtime upgrades, and block removal of a pinned runtime version.
- Database management pages separate runtime `Version management` from mutable-data `Instance
  management` tabs.
- The Python plugin package includes the pinned official Python Install Manager 26.3 MSI on
  Windows x64. Core verifies and extracts it inside operation staging, pins the official index,
  and invokes it with an exact tag, a Core-owned download directory, and staging `--target`.
  CPython and pip must pass health checks before atomic commit; `python`, `python3`, `pip`, and
  `pip3` use the shared Torben shim directory.
- Java discovery keeps only the newest release for each LTS feature line. The desktop reads its
  local catalog immediately, refreshes it through a process-local daily scheduled task only after
  startup and while the application is unfocused, and exposes an upgrade action when an installed
  LTS line is behind. Explicit plugin and JDK actions execute immediately and bypass the idle gate.
- The portable release contains only `TorbenApp.exe`. Its first launch scans D through Z and proposes
  `<drive>:\TorbenApp` on the first available drive, falling back to the launched executable's directory.
  Confirmation creates the base and `userData`, replaces an existing target `TorbenApp.exe` as an
  upgrade, relaunches the target copy, and removes the byte-identical original launch file. New
  builds keep the data path implicit as `<base>\userData` and create no pointer file.
- The catalog keeps the remaining three descriptors visible as unavailable so the product backlog
  remains explicit. Each application will be restored separately only after its own writable data
  is constrained to the user-selected Torben App installation root and the Windows x64 behavior is
  verified.
- Provider code, distribution validation, and local fixtures remain engineering assets. Their test
  coverage proves dormant implementation behavior and is not a current product-support claim.

## Repository implementation

### Foundation

- The Cargo and pnpm workspace contains the desktop application, shared contracts, Core, plugin
  host, CLI, shim, private UI package, and ten first-party native plugins.
- Product identity is fixed as `Torben App`, `torben`, and
  `io.github.torbenxiong.torbenapp` across package, Cargo, Tauri, release, and updater metadata.
- Core owns SQLite migrations, platform-standard paths, the managed application library,
  cross-process locking, durable journals, cancellation markers, diagnostic logs, settings, and
  startup recovery. Frontend and plugin processes do not access SQLite directly.
- Full Core startup transactionally synchronizes the ordered ten-application directory and its
  enabled managed sources, plus the winget, Homebrew, apt, and DNF source descriptors into SQLite. App list,
  search, and detail queries read that persisted Core-owned snapshot.
- The desktop opens on Plugins and exposes installed plugin applications such as Java directly in
  the sidebar, followed by Logs, Diagnostics, and Settings. The former Overview, Catalog, and
  Installed routes redirect to Plugins. Theme, English/Simplified Chinese localization, keyboard
  navigation, reduced-motion behavior, and responsive minimum-window layouts are covered by
  frontend tests.
- Shell integration is explicit and user-level. Windows user `Path` and Unix login profiles use
  ownership-aware, receipt-backed transactions; system `PATH`, elevation, telemetry, accounts,
  cloud synchronization, background services, and project-level version pinning remain outside the
  product boundary.

### Node.js vertical implementation

- Production builds support official metadata discovery, exact/LTS/Current resolution, signed checksum verification,
  per-target archive selection, safe extraction, staging health checks, atomic commit, multi-version
  installation, global selection, external read-only discovery, cancellation, rollback, recovery,
  and permanent uninstall in the shared Core path.
- The opt-in bundled Node.js plugin is embedded in the Windows x64 portable executable. Its shims
  prepare npm cache, global packages, configuration, temporary files, and REPL history below
  `userData/package-managers/node/npm` (with pnpm state under `userData/package-managers/node/pnpm`); explicit user path overrides and arbitrary scripts are not sandboxed.
- A single managed shim directory exposes `node`, `npm`, `npx`, and `pnpm`. Selection changes are
  receipt-backed, and command resolution must remain inside the exact managed installation.
- Real CLI subprocess and desktop-command fixture tests cover discovery through uninstall, fresh
  terminal resolution, GUI/CLI concurrency, cross-process cancellation, and restart recovery.

### Dormant application and source implementations

- Fixture builds retain Git, Visual Studio Code, and Codex CLI official-only metadata,
  per-platform distribution validation, supply-chain checks, staging, health checks, external
  read-only discovery, Schema UI, selection where applicable, managed updates, and uninstall.
- Deferred Python implementations use verified CPython source builds on macOS/Linux. Git, VS Code,
  and Codex use their documented platform-specific official assets and
  signatures. Codex management never reads or changes authentication, Provider, configuration,
  history, plugin, skill, or credential-store data.
- winget, Homebrew, apt, and DNF adapters expose availability, installed-state inspection, reviewed
  plans, explicit system-change acceptance, ownership reconciliation, and source migration.
  External packages are not silently claimed or removed.

### Release and plugin ecosystem

- The required CI and preview workflows validate Windows x64. The official tag workflow signs all
  embedded native components and publishes only the Windows x64 `TorbenApp.exe`, with an isolated
  clean-data launch, artifact-transfer verification, SHA-256 in the release notes, and immutable
  GitHub Release publication.
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

1. Configure the protected `official-release` environment with reviewed Windows Authenticode
   credentials. Only a successful exact-version tag run can prove embedded-component and portable
   executable signing, clean-data launch, transfer verification, and immutable GitHub Release
   publication.
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
