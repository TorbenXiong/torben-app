# Torben App

Torben App is a local-first, cross-platform application and runtime manager for Windows, macOS, and Linux.

The project is a greenfield rewrite. It does not share code, state, or compatibility guarantees with SoftPilot.

## Status

The repository is under active bootstrap development. Node.js, Eclipse Temurin, Python, Git,
Visual Studio Code, and Codex CLI now have local fixture-backed vertical management paths through
the desktop app and the `torben` CLI. Package source adapters can discover winget, Homebrew, apt,
and DNF, inspect installed package state, and produce exact operation plans. Mutations can run only
after explicit confirmation; DNF additionally resolves and locks one complete repository NEVRA.

Ordinary tests remain network-independent and use local fixtures. The weekly scheduled CI builds
the real `torben` CLI beside all six official provider plugins, queries Node.js, Temurin, Python,
Git, Visual Studio Code, and Codex through `torben version list <app> --json`, validates the stable
JSON envelope and non-empty recommended catalog, and publishes only the six validated read-only
snapshots as a short-lived CI artifact. The probe forwards an allowlisted local environment so CI
credentials are not exposed to provider processes. The distinction between local coverage,
configured native workflows, and authoritative remote acceptance evidence is documented in
[test and acceptance evidence](docs/testing.md). The complete original-plan mapping and the
remaining credential- or hosting-dependent gates are tracked in
[greenfield milestone status](docs/milestone-status.md).

## Repository layout

- `apps/desktop`: Tauri 2 desktop application and React UI.
- `crates/torben-contracts`: shared public models and plugin protocol.
- `crates/torben-core`: state, transactions, managed installations, and platform integration.
- `crates/torben-plugin-host`: isolated native plugin process host.
- `crates/torben-cli`: the `torben` command-line interface.
- `crates/torben-shim`: command forwarding shim used by managed applications.
- `plugins/node`: first-party Node.js plugin.
- `plugins/temurin`: first-party Eclipse Temurin plugin.
- `plugins/python`: first-party Python plugin.
- `plugins/git`: first-party Git plugin.
- `plugins/vscode`: first-party Visual Studio Code plugin.
- `plugins/codex`: first-party Codex CLI plugin.
- `packages/ui`: private design-system package.

## Development prerequisites

- Rust stable `1.98.0` with the MSVC toolchain on Windows.
- Node.js `24.19.0` LTS.
- pnpm `11.19.0`.
- Platform prerequisites required by [Tauri 2](https://v2.tauri.app/start/prerequisites/).

## Common commands

```powershell
. .\eng\dev-shell.ps1
pnpm install --frozen-lockfile
cargo build --workspace --locked
cargo test --workspace --locked
pnpm run check
pnpm run test
pnpm dev
```

Commands that install or resolve dependencies require explicit approval under the project rules.
The workspace build places `torben-plugin-node`, `torben-plugin-temurin`,
`torben-plugin-python`, `torben-plugin-git`, `torben-plugin-vscode`, `torben-plugin-codex`, and
`torben-shim` beside the CLI and desktop development binaries. Desktop packaging includes all seven
native tools as Tauri sidecars.
Selecting a managed application verifies and deploys the bundled shim as the
`node`/`npm`/`npx`, `java`/`javac`, `python`/`python3`/`pip`/`pip3`, `git`, and `code` aliases
plus `codex` before committing selection state.

Selection accepts only a plain standard managed app/version directory; package-manager records and
altered paths fail before plugin execution. Shim replacement stages and hashes every alias, then
syncs an operation receipt before replacing any existing command. Startup either rolls back a
receipt-bound partial replacement or verifies and finishes cleanup after the SQLite selection
commit. Missing or altered evidence is preserved for inspection. At launch, Core revalidates the
selected owner and rejects any command whose canonical target escapes its managed installation.

Active managed-application and plugin installations can be cancelled from the desktop task center
or another terminal. The CLI exposes the same durable state and structured errors:

```powershell
torben task list --json
torben task cancel <operation-id> --json
```

When `--json` is present, command-line validation failures such as a missing argument or unknown
subcommand also use the stable response envelope on stdout and retain clap's exit code `2`.
Explicit `--help` and `--version` requests remain human-readable discovery output.

Cancellation is cooperative: the worker observes a cross-process request while downloading,
hashing, extracting, copying, or before committing state, then records
`cancelling -> failed -> rolled_back`. Selection and uninstall operations are intentionally too
short and transaction-sensitive to accept cancellation in this milestone.

Core also writes bounded, structured local diagnostics to `torben.jsonl` in the platform-standard
log directory. A cross-process log lock serializes CLI and desktop writers, and one rotated backup
limits retained data to approximately 10 MiB. Operation diagnostics contain identifiers, state,
phase, and progress but deliberately exclude free-form operation messages, command output,
environment variables, and credentials. Log failures never turn an otherwise durable transaction
into a failure; `torben doctor --json` reports the active path and whether it is writable. Doctor
treats optional capabilities that the user has not enabled—terminal selection, Shell integration,
or an absent package manager—as healthy configuration. A selected version with missing/outdated
shims, stale managed Shell ownership, or another broken enabled capability still requires attention.

Desktop theme and language preferences are Core-owned settings persisted in SQLite. Theme supports
system, light, and dark modes; language supports the system locale, English, and Simplified Chinese.
Changing either preference takes effect immediately and remains local to the current OS user.

Desktop startup runs read-only external-installation discovery concurrently and isolates it per
application. A crashed, timed-out, malformed, or unexpectedly stopped provider task is reported as
a structured non-fatal warning while the catalog, managed installations, tasks, diagnostics,
settings, and results from healthy providers remain available.

Available package-manager version probes also start concurrently and retain the stable adapter
catalog order. A failed or timed-out probe marks only that adapter as unavailable, so one slow
manager cannot serially delay every other read-only startup check.

The desktop header provides a searchable command palette for every primary page and all six bundled
applications. It opens with `Ctrl+K` on Windows/Linux or `⌘K` on macOS, keeps focus inside a Radix
dialog, supports arrow-key selection and Enter navigation, and remains fully visible at the minimum
desktop window size. A keyboard-only skip control bypasses repeated sidebar navigation, and the UI
disables nonessential transitions, loading shimmer, and spinner animation when the operating system
requests reduced motion.

Update preferences are stored in the same Core-owned settings row. Torben App and managed
applications default to notify-only, automatic installation remains disabled, and no background
service is introduced. Desktop builds check Torben App updates only when compiled with the reviewed,
Base64-encoded `TORBEN_UPDATER_PUBLIC_KEY`; development builds without that key display an unconfigured state and
do not contact the fixed GitHub Release endpoint. Installation requires an explicit button click,
and Tauri verifies the downloaded updater artifact's minisign signature before invoking it.

Managed application update discovery preserves release lines instead of silently crossing runtime
channels: Node.js and Temurin stay on the installed major line, Python stays on its `major.minor`
line, and Git, VS Code, and Codex stay on their installed major line. Existing versions remain
installed. If the selected version belongs to the updated line, selection moves only when it still
matches the value observed before installation, so a concurrent user choice is never overwritten.

```powershell
torben update list --json
torben update list node --json
torben update apply node --json
torben update auto node on --json
torben update auto node off --json
```

Automatic updates are opt-in per application and run only during a foreground Torben App session;
there is no scheduler or resident background service. One application's catalog failure is returned
as a structured warning without hiding update candidates for other installed applications.

Shell integration is an explicit user action and adds only the managed shim directory. It never
changes the system PATH:

```powershell
torben shell status --json
torben shell enable --json
torben shell disable --json
```

On Windows, Torben App preserves the user `Path` registry value type and broadcasts the environment
change. On macOS and Linux, it maintains marked, reversible blocks for POSIX, Zsh, and Fish login
configuration. A matching path configured outside Torben App is reported as `external` and is never
removed by `disable`. Before changing either the registry value or any profile, Core syncs a bounded
transaction receipt containing the original and intended states. Startup under the workspace lock
finishes cleanup after a complete commit or restores a provable partial commit; an altered receipt,
target, or backup is preserved and fails closed instead of overwriting user configuration. Open a
new terminal after any managed change.

Managed installation recovery deletes an uncommitted final app/version directory only when its
standard path matches a separately synced operation ownership receipt. A crash after the atomic
directory rename but before receipt creation, or any missing, linked, malformed, oversized, or
mismatched receipt, preserves the directory and fails closed for inspection. A committed matching
SQLite record remains authoritative and startup only verifies its plain managed directory.

Managed uninstall uses the inverse proof. After moving the exact standard app/version directory to
operation-specific staging, Core immediately syncs a bounded receipt that binds the operation,
application, exact version, source, and staged paths. SQLite deletion failure restores only that
receipt-bound plain directory; committed or startup cleanup deletes it under the same rule. A
missing or altered receipt, a symbolic link, a non-directory path, or a package-manager-scoped
record is preserved and fails closed instead of being treated as a Torben-owned tombstone.

The managed application library can be moved explicitly to an empty absolute directory. Core holds
the workspace lock, checks available space, copies without following links, verifies every file,
atomically switches SQLite installation paths, and only then removes the old copy. If that final
cleanup cannot complete, the migration remains committed, `sourceCleanupPending` is returned, and
the next Torben App startup retries the old-library removal from the durable journal. Recovery
performs directory cleanup only when the journal paths match a separately synced transaction
receipt; an unowned or altered path fails closed and is preserved for inspection:

```powershell
torben library status --json
torben library migrate "D:\Torben Apps" --json
```

Eclipse Temurin uses the official Adoptium v3 LTS catalog and exact Eclipse HotSpot JDK archives.
The Core validates target metadata, archive size, SHA-256, the detached OpenPGP signature, and the
pinned Adoptium release-key fingerprint before staging. Both `java` and `javac` must report the
expected version before commit. Managed versions use the same commands as Node.js:

```powershell
torben version list temurin --json
torben install temurin@lts --json
torben use temurin@21.0.2+13.0.LTS --json
torben uninstall temurin@21.0.2+13.0.LTS --json
```

Global Java selection is routed through the single managed shim directory. This milestone does not
write a user or system `JAVA_HOME`; tools that require it must be pointed explicitly at the managed
installation until reversible environment-variable ownership is added to shell integration.

Python uses the stable python.org release catalog and resolves `current`, a `major.minor` line, or
an exact version before installation. On Windows, Torben App requires the official Python Install
Manager to already be available as `py` and asks it to extract the exact runtime into transaction
staging with `py install --target`; Torben App does not install the manager or register the runtime.
On macOS and Linux, Torben App builds the official XZ CPython source archive in staging, so a
working compiler, platform development headers, and `make` must already be installed.

The Unix source path requires both the python.org SHA-256 and the adjacent Sigstore bundle. Core
verifies the Fulcio certificate chain, signed certificate timestamp, Rekor transparency-log proof,
and the pinned Python release-manager identity and OIDC issuer before building. Missing or invalid
integrity metadata fails closed. The build runs `configure` with a managed prefix and installs with
`make install` into staging; it does not invoke a package manager or elevate privileges.

Managed Python exposes `python`, `python3`, `pip`, and `pip3` through the same Torben shim directory:

```powershell
torben version list python --json
torben install python@3.14 --json
torben use python@3.14.7 --json
torben uninstall python@3.14.7 --json
```

Torben App deliberately does not set `PYTHONHOME`. Python subprocess health checks also remove
ambient Python and virtual-environment variables so the staged runtime is validated on its own.

Git uses an official-only platform split. Windows x64 and ARM64 use the matching MinGit ZIP from
the latest stable `git-for-windows/git` Release. Core validates the release tag, asset name, target,
size, URL, and GitHub-provided SHA-256 before safe extraction. MinGit supplies the managed `git`
CLI without executing an installer or adding Git Bash as a Torben-managed command.

macOS and Linux use stable XZ source archives from kernel.org. Core first verifies the clear-signed
`sha256sums.asc` against the pinned kernel.org checksum key, then verifies the corresponding
uncompressed tar stream against Git's pinned upstream release key before building. The source path
requires `make`, a compiler, and Git's ordinary development headers such as curl, expat, OpenSSL,
and zlib; it does not invoke a package manager or elevate privileges.

```powershell
torben version list git --json
torben install git@current --json
torben use git@2.55.0+windows.5 --json
torben uninstall git@2.55.0+windows.5 --json
```

Torben App manages only the selected `git` executable. External Git installations are discovered
read-only, while user repositories, credentials, credential helpers, and global `.gitconfig`
remain outside Torben App ownership.

Visual Studio Code versions must be both published stable `microsoft/vscode` GitHub Releases and
available through the exact Microsoft Update API for the active platform. Torben accepts only the
official Windows ZIP, macOS application ZIP, or Linux tar.gz for x64/ARM64, pins the exact product
version and 40-character build commit, and verifies the Microsoft-provided SHA-256 before staging.

```powershell
torben version list vscode --json
torben install vscode@current --json
torben use vscode@1.134.0 --json
torben uninstall vscode@1.134.0 --json
```

The managed `code` shim always adds `--disable-updates`, keeping the selected installation at its
locked version without modifying user settings. Torben deliberately does not enable VS Code
Portable Mode or own `%APPDATA%\Code`, `~/.config/Code`, macOS Application Support, extensions,
accounts, credentials, projects, or `.vscode` directories. Those standard user locations can be
shared by multiple managed versions and always survive a managed-version uninstall.

Codex CLI uses the stable native archives attached to official `openai/codex` Releases. Torben
accepts only exact `rust-v<version>` tags whose release name, target asset name, GitHub path, size,
and SHA-256 digest all match. Windows x64/ARM64 use the official `.exe.zip`; macOS Intel/Apple
Silicon and Linux x86_64/ARM64 use the official tar.gz assets. Linux additionally verifies the
published legacy Cosign bundle by converting it into the equivalent Sigstore structure and
checking Fulcio, SCT, Rekor, the exact `rust-release.yml` tag identity, and GitHub Actions issuer
against the extracted native binary.

```powershell
torben version list codex --json
torben install codex@current --json
torben use codex@0.149.1 --json
torben uninstall codex@0.149.1 --json
```

Torben runs install and selection health checks with a temporary isolated `CODEX_HOME`, then removes
it. Normal launches through the managed shim retain the user's ordinary Codex environment so the
official client can reuse an existing login. Torben never invokes `codex login` or `codex logout`
and never reads, copies, displays, migrates, switches, or deletes `auth.json`, `config.toml`, account
state, Provider configuration, history, plugins, skills, or OS credential-store entries. These
boundaries follow the [official Codex CLI](https://learn.chatgpt.com/docs/codex/cli) and
[authentication](https://learn.chatgpt.com/docs/auth) documentation.

Package-manager source adapters discover only a manager executable already available on `PATH`,
bound command time and output, and preserve package versions as raw manager-owned strings instead
of interpreting them as SemVer. Read-only discovery and planning are available through:

```powershell
torben source list --json
torben source inspect winget Microsoft.VisualStudioCode --json
torben source inspect homebrew visual-studio-code --package-kind cask --json
torben source plan install winget Microsoft.VisualStudioCode --package-version 1.134.0 --json
torben source plan install apt nodejs --package-version 20.11.1+dfsg-2~deb12u1 --json
torben source owned --json
```

winget, Homebrew, apt, and DNF plans have an explicit execution path. The caller must provide the
application version and expected installed executable, review the exact command, and set
`--accept-system-changes`; the desktop Diagnostics page exposes the same review, checkbox, and
second-confirmation flow. For example:

```powershell
torben source execute install vscode@1.134.0 winget Microsoft.VisualStudioCode `
  --package-version 1.134.0 `
  --executable-path "C:\Program Files\Microsoft VS Code\Code.exe" `
  --accept-system-changes `
  --json

torben source execute uninstall vscode@1.134.0 winget Microsoft.VisualStudioCode `
  --package-version 1.134.0 `
  --accept-system-changes `
  --json
```

Torben-owned package installations can change their immutable source only through a separately
reviewed migration. Migration preserves the exact Torben application version, but deliberately
does not read, copy, or transform the application's own configuration:

```bash
torben source migrate plan vscode@1.134.0 dnf code \
  --target-package-version 1.134.0-1.fc42 \
  --target-executable-path /usr/bin/code \
  --json

torben source migrate execute vscode@1.134.0 dnf code \
  --target-package-version 1.134.0-1.fc42 \
  --target-executable-path /usr/bin/code \
  --approved-plan-token <token-from-plan> \
  --accept-system-changes \
  --json
```

The reviewed migration contains four complete commands: remove the current package, install the
target, clean a failed target, and restore the previous source. A SHA-256 approval token binds all
four plans, including a DNF NEVRA when applicable. Core re-resolves the plan while holding the
workspace lock and rejects any change before mutation. Only a reconciled exact target version and
successful application health check atomically replace SQLite ownership. If target installation
fails, Torben attempts the reviewed cleanup and restore commands and verifies the previous
executable. Failed compensation or an interrupted pre-commit migration removes unverified Torben
ownership; any remaining package is external until the user inspects and explicitly reconciles it.

An official managed archive can migrate to a reviewed package-manager source through the explicit
`to-package` path. The managed version must not be the currently selected terminal version; select
another version or clear the selection first:

```bash
torben source migrate to-package plan vscode@1.134.0 dnf code \
  --target-package-version 1.134.0-1.fc42 \
  --target-executable-path /usr/bin/code \
  --json

torben source migrate to-package execute vscode@1.134.0 dnf code \
  --target-package-version 1.134.0-1.fc42 \
  --target-executable-path /usr/bin/code \
  --approved-plan-token <token-from-plan> \
  --accept-system-changes \
  --json
```

The reviewed plan fixes the plugin uninstall declaration, the target install command, the failed
target cleanup command, and the current managed directory. Execution moves only that version
directory into an operation-specific backup before running the package manager. Torben commits the
new immutable owner only after a fresh exact-version query and application health check, then
deletes the backup. It never reads or copies application configuration outside the managed version
directory. If target installation fails, Torben cleans the target, restores the directory, and
re-runs the official plugin health check. An interrupted pre-commit operation restores managed
ownership but reports the target package as requiring inspection; an interruption after the atomic
SQLite commit resumes backup cleanup. If restoration cannot be verified, Torben removes the
untrusted ownership receipt and treats all remaining files or packages as external state. Startup
recovery re-derives the standard managed path before restoring or deleting the operation-specific
backup, and rejects altered paths or symbolic-link backups.

The reverse migration installs the official managed archive before removing the currently owned
package:

```bash
torben source migrate to-managed plan vscode@1.134.0 --json

torben source migrate to-managed execute vscode@1.134.0 \
  --approved-plan-token <token-from-plan> \
  --accept-system-changes \
  --json
```

The approval token binds the exact current package owner and state, its reviewed uninstall and
restore commands, the official plugin `InstallPlan`, and the managed target directory. Execution
downloads, verifies, stages, and health-checks the official archive while the package is still
present. It then removes and re-queries the package before atomically replacing the package receipt
with managed ownership. If removal fails while the package remains verifiable, the new managed
payload is deleted and package ownership is retained. If the package disappears but SQLite commit
fails, Torben attempts the reviewed restore command and deletes the managed payload. Failed
compensation after package removal begins removes unverified ownership and requires explicit
package-manager reconciliation; an interruption before package removal retains the unchanged
package owner. Final managed directories are deleted during rollback only when the standard target
and a separately synced operation ownership receipt agree. Missing or altered receipts fail closed
and preserve the directory for inspection; an already committed managed owner is recovered as
success.

```bash
torben source plan install dnf code --package-version 1.134.0-1.fc42 --json
torben source execute install vscode@1.134.0 dnf code \
  --package-version 1.134.0-1.fc42 \
  --executable-path /usr/bin/code \
  --approved-execution-identity code-1.134.0-1.fc42.x86_64 \
  --accept-system-changes \
  --json
```

No adapter invokes `sudo`, requests silent elevation, changes Torben's managed application library,
or accepts integrity-bypass flags. When apt needs authorization, the surrounding terminal or OS
interaction remains visible and external to Torben. Homebrew does not promise arbitrary historical
versions. DNF plans require an exact raw repository version and use bounded `repoquery` output to
resolve one full name/epoch/version/release/architecture identity. Missing or multiple matches fail
closed. The JSON plan returns that `executionIdentity`, and DNF execution requires the exact same
value through `--approved-execution-identity`; a changed resolution requires a new review. Package
managers expose only one active version, while Torben's official archive providers remain the
multi-version path.

An already installed package is external read-only state: Torben neither takes it over nor uninstalls
it. A package becomes Torben-owned only after an explicit Torben install request returns
successfully, a fresh manager query observes the expected exact version, and the declared executable
passes the application health check. Ordinary `torben use` and `torben uninstall` reject
package-manager-scoped records; removal must go through `source execute uninstall` so the source
ownership and current manager state are reconciled.

Package-manager mutations are not filesystem-atomic and may have changed shared dependencies before
a command fails. Torben therefore never describes command failure as an archive-style rollback.
Durable `SourceInstall` and `SourceUninstall` journals record request, preview, execution, state
re-query, health check, and ownership commit. On restart, an atomically committed SQLite ownership
change is recovered as success; an interrupted operation without that commit is marked
`source_operation_reconciliation_required` and remains external until explicitly reconciled.

## Security model

Plugins are native trusted programs. Process isolation contains crashes and protocol failures; it
is not a security sandbox.

Plugin installation uses verified staging followed by an atomic move into the standard
plugin/version directory. Before SQLite ownership is committed, Core syncs a separate receipt for
that final directory. Startup and live rollback delete it only when the operation, plugin, exact
version, and target all match; missing, linked, malformed, oversized, or mismatched evidence
preserves the directory for inspection.

The host validates bounded, duplicate-free capability and permission declarations before starting
registry or sideloaded plugins. Network permissions are host names rather than URLs, filesystem
permissions use known Torben scope tokens, external commands must be bare executable names, and
package-manager permissions use the supported adapter names. Every standard JSON-RPC method is
gated by the corresponding declared capability; a missing capability fails locally before a
request reaches the plugin. These checks make manifests reviewable and prevent accidental host
calls, but cannot confine a trusted native executable at the operating-system level.

Registry-controlled paths use a single portable ASCII subset on every platform. Both the publisher
and host reject traversal, Windows device names, NTFS alternate-data-stream syntax, trailing dots
or spaces, oversized components, case-folded package-directory collisions, and case-folded target
executable aliases before accessing a package path.

Long-running install and uninstall planning may emit bounded `operation.event` notifications over
the existing JSON-RPC session. Torben accepts only validated events for the active `OperationId`
and persists them through Core's operation journal; unexpected, malformed, cross-operation, or
excessive notifications terminate the active call without giving the plugin database access.

Official plugin artifacts use a two-level Ed25519 trust chain. A registry root key embedded at
build time signs the registry, the registry authorizes publisher keys, and each publisher signs its
plugin manifest. The manifest fixes the executable SHA-256 for every supported target. The host
also enforces registry and manifest minimum-host versions and rejects revoked publishers or plugin
entries before installation.

The bootstrap accepts a local signed registry artifact and can refresh a release-configured HTTPS
registry into a verified local cache. Every signed snapshot carries a monotonically increasing
sequence; Torben App rejects older snapshots and rejects different content that reuses a cached
sequence. Redirects, oversized responses, invalid signatures, and non-HTTPS production URLs fail
closed. The repository includes a deterministic, fixture-verified publisher that hashes every
target executable, signs publisher manifests and the root registry, and commits a new output
directory through an atomic rename. A protected, main-branch-only manual workflow can materialize
Environment-scoped signing keys in runner temporary storage, produce the tree, re-verify its full
two-level trust chain with both release tooling and the Rust host, enforce a signed contiguous
sequence, and upload a 14-day artifact with a deterministic `SHA256SUMS`. It does not deploy the
tree or grant Pages/Release write access. The public HTTPS hosting endpoint is not live yet; see
[plugin registry publishing](docs/plugin-registry-publishing.md) for the release input and key
handling contract.

Development builds without both `TORBEN_OFFICIAL_PLUGIN_REGISTRY_KEY` and
`TORBEN_OFFICIAL_PLUGIN_REGISTRY_URL` compiled in report the network registry as unconfigured.
Registry status and an explicit refresh use the same stable JSON envelope as other CLI commands:

```powershell
torben plugin registry status --json
torben plugin registry refresh --json
torben plugin install-from-registry app.example.plugin --version 1.2.3 --json
```

A build with the trust root can still install from a verified local artifact with:

```powershell
torben plugin install-official .\registry.json app.example.plugin --version 1.2.3 --json
```

Developer-mode sideloading is a separate, explicit trust path. A sideloaded manifest remains
labelled `sideloaded` even when it contains a publisher-controlled signature field:

```powershell
torben plugin install .\plugin.json --developer-mode --json
```

Plugins declaring `schema_ui` can expose bounded pages through `schema.pages` and short actions
through `schema.action`. Torben App renders only host-owned text, boolean, select, and status fields;
plugins cannot inject React components or call Tauri APIs. The desktop plugin page provides the
visual renderer, while the CLI exposes the same Core validation:

```powershell
torben plugin pages app.torben.plugin.node --json
torben plugin action app.example.plugin settings general apply --value channel=lts --json
torben plugin action app.example.plugin settings danger reset --confirm --json
```

Cross-platform packaging, signing gates, and deterministic SHA-256 metadata are documented in
[release engineering](docs/release.md). Unsigned packages are development artifacts and cannot be
promoted to an official release. Windows NSIS/MSI and macOS DMG artifacts must install or copy and
sustain a GUI launch on native x64/ARM64 runners. Linux AppImage, deb, and rpm artifacts must do the
same in disposable Ubuntu, Debian, Fedora, and Rocky Linux containers on both architectures. The
development workflow is manual, read-only, and produces 14-day artifacts only; it never creates a
GitHub Release.

Formal tag publishing is isolated in a protected `official-release` GitHub environment. It requires
Windows Authenticode, macOS Developer ID/notarization, and matching minisign updater credentials;
the release job re-verifies all signatures after artifact transfer before creating `latest.json` or
the immutable GitHub Release.

## License

Licensed under the Apache License 2.0.
