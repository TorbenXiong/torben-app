# Architecture

## Boundaries

Torben App has two user-facing entry points: the Tauri desktop app and the `torben` CLI. Both call `torben-core`; neither owns installation or persistence behavior. Shared serialized models live in `torben-contracts`.

```text
React UI -> Tauri commands --+
                            +-> torben-core -> SQLite / filesystem / network
torben CLI -----------------+
                                  |
                                  +-> plugin host -> native plugin process
```

Each lifecycle Tauri command is a serialization wrapper over one desktop command handler that
accepts the shared Core. Production command registration and fixture-backed command tests therefore
exercise the same parsing and mutation path; the React API keeps the corresponding command names and
camel-case payloads in one typed module.

Plugins are native trusted processes that communicate through versioned JSON-RPC over stdio. A plugin describes applications, resolves aliases to exact versions, and produces application-specific plans. Core remains responsible for locking, durable operation state, download verification, staging, health checks, atomic commit, rollback, and SQLite state.

Every plugin request has a host-enforced timeout and a matching JSON-RPC request identifier. A
timeout, early process exit, malformed JSON, mismatched response, or plugin-reported structured
error terminates only the active call; the child process is killed when its client session is
dropped. Cross-platform local fixture processes exercise these failure modes without relying on a
live registry or network service.

Verified registry and sideloaded manifests have bounded, duplicate-free declarations. Domain
values are lowercase host names, filesystem access uses known symbolic Torben roots, external
commands are argument-free executable names, and package-manager names come from the supported
adapter set. The client maps each standard JSON-RPC method to its required manifest capability and
rejects undeclared calls before writing to plugin stdin. Bundled plugins use the capabilities from
their compiled manifest templates for the same method gate. This is host-side protocol
authorization and review metadata, not operating-system containment of native code.

Long-running `install.plan` and `uninstall.plan` calls may interleave `operation.event` JSON-RPC
notifications with their final response. The host accepts them only for the active `OperationId`,
validates bounded phase, message, and progress fields, limits each call to 1,024 events, and applies
one timeout to the complete response stream. Plain calls, unknown notification methods, malformed
events, and events for another operation fail closed. Core maps accepted events into the same
durable operation journal used by the GUI task center and CLI, while the plugin remains unable to
write journals or SQLite directly.

Schema UI uses two protocol methods: `schema.pages` returns bounded host-rendered pages and
`schema.action` invokes an action previously declared on one of those pages. Fields are restricted
to text, boolean, select, and read-only status values; actions are primary, secondary, or
destructive. Core enforces ASCII-safe unique identifiers, page/section/field/action count limits,
text and value limits, select options, read-only values, plugin identity, enabled state, and the
`schema_ui` capability. Destructive actions require explicit confirmation before Core forwards the
request. Action results must preserve the plugin and page identity and pass the same schema
validation again.

Installed native plugins are rechecked before every schema session: the package directory,
`plugin.json`, and executable must be regular non-link files; the executable hash and complete
manifest must match SQLite; and the initialization handshake must match the stored plugin ID,
version, target, and protocol. Schema actions hold the workspace lock and have the ordinary
30-second plugin call timeout. They are intentionally short actions, not a substitute for durable
installation operations or `OperationId` progress streams. React renders only the shared schema
contract and never loads plugin JavaScript, React components, HTML, or Tauri bindings.

The built-in Node.js provider follows the same process boundary as an installed plugin. Core locates
the app-shipped `torben-plugin-node` executable, performs an `initialize` identity handshake, and
rejects plans whose source, target, official URLs, steps, health check, or exposed commands do not
match the active operation. Selection health checks and uninstall plans use the same timeout-bound
JSON-RPC session. Health-check failures retain their structured Core error, while uninstall plans
must echo the active operation's application, exact version, immutable source owner, and managed
path. The plugin never receives the SQLite path or a database handle.

Desktop navigation remains host-owned. The global command palette derives its page entries from the
same navigation table and its application entries from Core's catalog snapshot, but only exposes
application IDs with an implemented detail route. A Radix dialog owns focus and Escape handling;
the combobox owns arrow, Home/End, and Enter selection. Shortcut labels and `aria-keyshortcuts`
adapt to macOS versus Windows/Linux, and the result pane scrolls independently so controls remain
reachable at the configured minimum window size. The app shell exposes a focus-managed skip control to
the main content landmark and applies a reduced-motion media query to nonessential transitions and
loading animations.

## Managed state

Platform-standard directories contain four independent areas:

- Data: SQLite, managed applications, staging, operation journals, and shims.
- Config: local preferences and trusted registry configuration.
- Cache: downloaded archives and read-through metadata.
- Logs: structured local diagnostics.

Core appends newline-delimited JSON diagnostics to `torben.jsonl` under the platform log directory.
The CLI and desktop serialize writes through a separate cross-process file lock. The active file is
limited to 5 MiB and rotates to one backup, bounding retained diagnostics to approximately 10 MiB
plus at most one record. Lifecycle and operation-state records contain only host-defined structured
fields; free-form operation messages, subprocess output, environment variables, and credentials are
not copied into this log. The durable operation journal remains the source of truth for recovery.
Diagnostic logging is best-effort so a log I/O failure cannot invalidate a committed transaction;
Doctor separately probes the log target and reports an unhealthy check when it is unavailable.
Doctor distinguishes an optional capability that is not configured from an enabled capability that
is broken. With no selected terminal version, managed shims are not required; disabled Shell
integration and a missing optional package manager are also healthy states. Once terminal selection
or managed Shell ownership exists, missing, outdated, partial, or conflicting integration remains
an unhealthy actionable check.

SQLite is private to Core. The first migration creates installations, selections, sources, plugins,
operations, settings, and schema migration tables. The fourth migration adds the ordered
application catalog. On full Core startup, the six bundled application descriptors and their
official sources plus winget, Homebrew, apt, and DNF are synchronized in one SQLite transaction;
application list, search, and detail queries then read the persisted snapshot. Every journal update
is projected into the operations table with its kind, latest state, complete event JSON, and update
time; the desktop task center reads this Core-owned projection rather than opening the database or
journal files directly.
Every embedded migration records its exact version, including migrations whose structural change
was already present in a newly created database. Core checks the migration ledger before creating
or changing application tables and refuses a database containing a version newer than the running
binary supports, preventing an older Torben App from modifying newer state.
Theme and language preferences use a versioned shared contract and one Core-owned JSON row in the
settings table. The desktop receives preferences through its snapshot and writes them through a
Tauri command; React and plugins never open SQLite directly. Invalid persisted enum values fail with
a structured error instead of silently changing the user's preference.
External installations are discovered read-only and are never inserted as managed installations,
selected through managed shims, or uninstalled by Torben.
The desktop snapshot starts every built-in discovery concurrently, then merges results in catalog
order. A failed provider or unexpectedly stopped discovery task contributes a structured warning
with its application identity and preserves successful records from every other provider, so plugin
crashes, timeouts, and malformed responses cannot prevent or serially delay the rest of the GUI.

Migration 3 adds `package_installations` as package-manager ownership metadata linked to an
ordinary installation row. It records the source adapter, package coordinate and kind, raw package
version, architecture, executable path, health, and whether Torben owns the installation request.
`InstallScope::PackageManager` keeps these records distinct from Torben's archive-managed library
and from external discovery. Deleting the parent installation cascades only the SQLite ownership
metadata; package removal remains a separate, explicit source operation. Frontend code and plugins
do not access either table directly.

## Mutation transaction

Every mutating operation acquires one cross-process workspace lock and writes durable phase events.
The same Core implementation serves both desktop commands and the CLI. Separate-process regression
tests hold either the raw workspace file lock or an in-flight desktop Core install and verify that a
CLI mutation cannot enter until the holder exits. Task observation and cancellation remain available
through the task-only client while the desktop worker owns the mutation lock.

An install journal is created before plugin startup and alias resolution so the task is visible and
cancellable during resolution. Its version is initially absent, then durably locked to the resolved
exact version; changing it to a different exact version is rejected. Startup recovery for a journal
that never resolved a version removes only that operation's staging directory and never guesses an
application version directory to delete.

1. Resolve an alias into an exact, reproducible version.
2. Download into version-isolated cache using HTTPS.
3. Verify official integrity metadata. For Node.js, Core first verifies the detached OpenPGP
   signature over the original `SHASUMS256.txt` bytes, then reads the archive SHA-256 from that
   authenticated manifest. A missing, malformed, untrusted, or invalid signature fails closed.
4. Extract safely into a unique staging directory.
5. Run the provider health check and verify the actual version. Node.js checks `node --version`
   against the exact requested version, then verifies that the co-located `npm` and `npx` commands
   start successfully and return semantic versions. The health-check child process receives the
   managed installation's command directory as its first PATH entry, without changing user or
   system environment variables.
6. Atomically move the verified directory into the managed library.
7. Commit the installation and source owner to SQLite.

Uninstall first confirms that SQLite points to the exact standard managed path, validates the
plugin uninstall plan, moves the managed directory into operation-specific staging, then writes and
syncs a separate bounded receipt binding the operation, application, exact version, standard source,
and staging paths. Only after that proof exists does Core remove state and delete staging. A state
failure restores only the receipt-bound plain directory; if receipt persistence, validation, or
restoration fails, the journal remains non-terminal and preserves staging for inspection. Once
deletion begins, Torben App never pretends that a potentially partial directory was restored: a
cleanup failure keeps the committed state removal and a non-terminal journal so startup recovery can
safely finish deleting the tombstone. A path outside the managed library fails before the plugin or
filesystem mutation is attempted. The currently selected terminal version cannot be uninstalled.

Operation journals are replaced through a synced next file while retaining the previous valid file
through the replacement window. On startup, Core acquires the workspace lock before inspecting
non-terminal journals. The synced file is the recovery truth; Core repairs or rebuilds its SQLite
operation projection from valid files before recovery, while completed task history remains in
SQLite if terminal journal files are later archived. Installation recovery removes regular
`.partial` files directly inside the resolved app-version download cache and operation-specific
staging. After a provider atomically creates the standard final directory, Core writes and syncs a
separate bounded receipt binding the operation, app, exact version, and final path. An uncommitted
final directory is deleted only when that receipt is a plain file and matches the re-derived target;
a missing, linked, oversized, malformed, or mismatched receipt fails closed and preserves the
directory. If SQLite already contains the matching managed record, recovery instead verifies the
standard directory, removes residual staging and receipt data, and completes the journal. Completed
cache entries are retained. A download cache or `.partial` entry that is not a plain managed
directory or regular file also fails recovery closed with a structured path-conflict error and is
preserved for inspection. Uninstall recovery re-derives both the standard managed source and the
operation-specific staging path. While a matching managed SQLite record exists, it restores staging
only with the matching uninstall receipt; after state commits, it removes staging only with the same
proof. Missing, linked, oversized, malformed, or mismatched receipts and non-managed ownership
records fail closed without moving or deleting the ambiguous path. A pre-staging journal with the
ordinary managed directory still present safely rolls back without requiring a receipt.

Selection validates that the SQLite record is managed, points to the re-derived standard app/version
path, and names a plain directory before invoking the plugin. The plugin health check runs before
shim or state mutation. Core stages and hashes every command alias in an operation-specific
directory, then syncs a bounded receipt binding the operation, app, exact version, staging, backup,
source hash, and complete destination list before replacing any shim. A pre-commit interruption
restores receipt-bound backups and removes only new shims matching that receipt hash; a committed
selection verifies every destination hash and finishes staging cleanup. A receipt remaining after
staging cleanup is safely closed according to the SQLite commit result. Missing, linked, malformed,
oversized, or mismatched evidence and changed destinations fail closed. Selection recovery never
guesses or rewrites an earlier SQLite choice. Shim command resolution repeats the managed owner and
standard-directory checks, canonicalizes the provider-selected command, and rejects a target that
escapes the managed installation through a link or altered path. Plugin install
recovery uses a plugin-specific journal subject and exact version. Staging is derived from the
operation identity; after atomic final-directory rename, Core writes and syncs a bounded receipt
binding the operation, plugin, exact version, and standard target. Recovery deletes an uncommitted
final plugin directory only when that receipt is a plain matching file; missing or altered evidence
fails closed and preserves the directory. It completes the journal only when the matching SQLite
plugin record and plain managed directory both exist, then removes residual staging and receipt
data. Missing managed data, conflicting paths, invalid journals, and unsupported operation kinds
fail startup recovery with a structured error while preserving the
ambiguous state for inspection. Managed-library migration uses its own journal subject containing
the source, target, and target-volume staging paths. Before the SQLite switch, recovery removes only
the uncommitted copy and retains the source; after the atomic SQLite path update, recovery treats the
verified target as authoritative and finishes old-source cleanup. A failed post-commit source
deletion is persisted as `sourceCleanupPending` before the operation records success. Startup
recovery deliberately includes this terminal migration journal, retries both staging and old-source
cleanup, and clears the marker only after cleanup succeeds. A repeated cleanup error remains visible
as a startup recovery failure instead of being silently treated as complete.

Before copying any data, Core writes and syncs a separate bounded migration receipt beside the
operation journals. It binds the `OperationId`, source, target, target-volume staging path, and the
target's original existence state. Recovery first validates absolute non-overlapping paths, the
operation-specific staging name, and byte-equivalent journal/receipt fields; a missing, linked,
oversized, malformed, or mismatched receipt fails before any directory removal. The only missing-
receipt exception is a journal still in `prepare` whose source, staging, target, and SQLite state
prove that no mutation began.

Managed-library migration holds the same workspace lock as install, select, uninstall, and plugin
mutations. The target must be an empty absolute regular directory outside the source tree. Core
measures the source and target volume, rejects insufficient space, copies without following symbolic
links, and compares sorted relative paths, sizes, and SHA-256 hashes before renaming staging into the
target. SQLite updates every managed install path and the active library setting in one transaction.
If that transaction fails, the target copy is removed and the original library remains active.
After this transaction commits, later journal or cleanup failures never enter the pre-commit
rollback path or claim that the active library switch was reversed.
The journal also persists `targetCommitted` immediately after the verified staging directory is
renamed. A pre-SQLite failure may remove or recreate the target only when that marker proves Torben
owns the copy. Without it, an unexpected non-empty target is preserved and recovery fails closed.
Any incomplete staging cleanup or target restoration records `managed_library_rollback_pending`
as a recoverable non-terminal failure instead of emitting `rolled_back`.

Developer-mode plugin installation verifies the source package, copies it without following
symbolic links, and verifies the staged manifest and executable hash again before the atomic move.
The filesystem commit precedes the SQLite plugin record. Both use the same durable `OperationId`;
cleanup failures remain non-terminal so startup recovery can finish the rollback safely.

Official plugin installation begins from a signed local registry artifact. The host build embeds
one Ed25519 registry root public key. That key verifies the complete registry payload, which
authorizes publisher public keys and records publisher and per-version revocation state. A selected
entry pins the plugin identifier, exact version, publisher, manifest path, and manifest SHA-256.
The authorized publisher key then verifies the manifest, and the manifest pins the executable
SHA-256 for the current platform target. Registry and manifest protocol or minimum-host
incompatibility fails before staging.

Release builds may also embed an HTTPS registry URL. Refresh responses are capped at 2 MiB,
redirects are disabled, and the final response must retain the configured network origin. Core
verifies the root signature, schema, nonzero monotonic sequence, minimum host version, and registry
uniqueness before writing any bytes to the cache. A snapshot older than the verified cached
sequence is a rollback error; different signed content reusing the same sequence is a conflict.
The verified cache is replaced through synced `.next` and retained `.previous` files so a failed
rename restores the last trusted snapshot. Cache mutation holds the same cross-process workspace
lock as installation operations.

Online installation selects an exact non-revoked entry from that verified snapshot before creating
its durable plugin operation. The publisher-signed manifest is limited to 1 MiB and verified before
the platform executable is requested. The executable is streamed with a 512 MiB ceiling, remains on
the registry origin, and is checked against the manifest SHA-256. Downloads go to an
operation-specific package staging directory, never directly to the active cache. Core verifies the
complete root-to-executable chain once more after the atomic package-cache move, then passes the
package into the ordinary plugin staging, filesystem commit, and SQLite transaction. Cancellation
is observed between response chunks and before both cache and managed-plugin commits.

```text
embedded registry root key
  -> signed registry and revocation state
      -> authorized publisher key
          -> signed plugin manifest
              -> current-target executable SHA-256
```

Staging re-reads the copied manifest, rechecks target compatibility and the copied executable hash,
and compares the complete serialized manifest with the already authorized source manifest. This
second pass intentionally does not infer trust from the manifest signature alone: official origin
comes only from the registry verification path. SQLite persists `built_in`, `official_registry`, or
`sideloaded` as explicit origin state; an older plugin table is migrated to `sideloaded` rather than
guessing from stored content. Direct manifest installation requires explicit developer mode and is
always persisted as sideloaded. Builds without a compile-time
`TORBEN_OFFICIAL_PLUGIN_REGISTRY_KEY` reject official installation. Builds without both that key and
`TORBEN_OFFICIAL_PLUGIN_REGISTRY_URL` report network refresh as unconfigured. Registry artifacts are
produced by the repository-owned deterministic publisher: it derives publisher public keys from
offline Ed25519 private-key files, hashes copied target executables, signs the exact Rust-compatible
manifest and registry payloads, and atomically renames a new output tree. Private keys never enter
that tree. A protected `workflow_dispatch` on `main` can create a short-lived GitHub Actions
artifact after independently checking every platform hash and publisher signature, verifying both
the new and immediately previous registries with the Rust host, matching the configured trust root,
and emitting a deterministic `SHA256SUMS`. The workflow has read-only repository permission and no
hosting deployment capability. Public hosting and automatic refresh policy remain outside the
current bootstrap; refresh is explicit.

`Failed` is deliberately not a terminal journal state. Core appends `RolledBack` only after it has
confirmed that filesystem and SQLite state were both restored. Cleanup or restore failures remain
at `Failed`, retain their staging evidence, and are reconsidered during the next locked startup.

Long installation cancellation does not wait for the workspace lock held by the active worker.
Another GUI or CLI process writes an atomic operation-specific cancellation marker; only the worker
that owns the journal appends events. Network waits poll the marker, archive streams and hashes check
between chunks, extraction and plugin copying check between entries, and both installation paths
check again before filesystem and SQLite commits. Acknowledged cancellation records
`cancelling`, then follows the ordinary failure cleanup and reaches `rolled_back` only after cleanup
is confirmed. Terminal and short selection/uninstall operations reject cancellation with a
structured `operation_not_cancellable` error.

The CLI routes `task list` and `task cancel` through a task-only Core client that opens the operation
projection and cancellation-marker directory without acquiring the mutation lock or running startup
recovery. It cannot install, select, uninstall, or mutate application state. Full Core startup still
acquires the workspace lock before recovery, so a task observer cannot mistake an active worker's
journal for an interrupted operation.

## Package-manager source adapters

Core has platform adapters for winget on Windows, Homebrew on macOS/Linux, and apt or DNF on Linux.
Discovery resolves commands already present on `PATH`; it does not install a manager or modify the
environment. Status probes and read-only inspection run with a fixed timeout and reject oversized
or malformed output. winget inspection consumes a bounded temporary JSON export, Homebrew consumes
`brew info --json=v2`, apt reads `dpkg-query`, and DNF installed-state inspection reads `rpm`
query output. Reviewed DNF install plans separately use bounded `dnf repoquery` output to resolve
the repository identity. Pre-existing packages remain external read-only state and never become
Torben-owned merely because inspection found them.

Package versions use `SourcePackageVersion`, an opaque validated string, because Debian epochs,
RPM release suffixes, Homebrew revisions, and winget versions are not one common SemVer domain.
Package coordinates and versions reject option-like or control input before each value is passed as
one process argument. Homebrew accepts versioned formula coordinates but does not claim it can
install an arbitrary raw historical version. apt installation plans require `package=version`.
DNF installation plans run a bounded, machine-formatted `repoquery` and require exactly one row
whose name and raw epoch/version/release match. The full NEVRA, including architecture, replaces
the prospective argument in both preview and execution commands.

The public slice exposes adapter status, installed-state inspection, Doctor checks, and
install/uninstall plans. winget, Homebrew, apt, and DNF plans can be passed to the execution API only
with `acceptSystemChanges=true`; the desktop requires plan review, a checkbox, and a second
confirmation. Plans avoid `sudo`, silent elevation, `--force`, signature/hash bypasses,
unauthenticated repositories, and automatic dependency cleanup. apt reports that external
authorization will normally be required but Torben does not acquire it by invoking `sudo` or by
silently elevating. Homebrew disables automatic updates and analytics for adapter queries and
plans. DNF rejects missing and cross-repository or cross-architecture ambiguity. A reviewed DNF
plan exposes its full NEVRA as `executionIdentity`; execution must return the same value as
`approvedExecutionIdentity`, otherwise `source_plan_approval_required` fails before mutation.

A package-manager mutation cannot use the archive transaction's atomic directory swap: a shared
manager may change dependencies and its own database before returning. The implemented state
machine is `request -> preview -> execute -> state re-query -> health check -> ownership commit`.
Command success alone is insufficient: installation requires the fresh manager state to contain
the requested exact version and the caller-declared absolute executable to be a regular,
non-symlink application command whose version output matches. Uninstall requires existing Torben
ownership, rejects external version drift, and commits removal only after a fresh query confirms
the package is absent.

Only a package with a successful Torben request and reconciled result becomes Torben-owned. A
pre-existing external package is never adopted, selected through the managed shim, or uninstalled.
The ordinary archive `select` and `uninstall` APIs reject `InstallScope::PackageManager`; callers
must use the source execution API so manager state and ownership stay coupled. Command or health
failure writes no ownership record, but Torben does not claim that the manager rolled back shared
system changes already made before failure.

Changing an immutable package source is a separate `SourceMigrate` operation, never an ordinary
install that overwrites ownership. Planning requires an existing Torben package owner, an absent
target coordinate, an exact Torben `app@version`, the target raw package version, and an absolute
health-check executable. The returned plan fixes current removal, target installation, target
cleanup, and current-source restoration. Core serializes those plans without their token and uses
SHA-256 as an approval token; execution re-resolves under the workspace lock and requires the exact
token before running any command.

The state machine is `recheck both sources -> remove current -> recheck -> install target -> recheck
-> health check -> atomic ownership replacement`. The application configuration is outside every
step. A target failure runs the reviewed cleanup and restoration plans, then verifies the original
raw package version and executable before recording rollback. Because a system package manager is
not transactional, failed compensation or process interruption before the SQLite commit drops the
unverified ownership receipt and requires explicit reconciliation; it never guesses a new owner.
Startup recovery recognizes a completed atomic target ownership replacement as success.

Official managed archives use a separate `ManagedToPackageMigrationPlan`. Planning requires an
existing standard managed directory, no active selection for that exact version, an absent target
package, a plugin-approved uninstall declaration, and reviewed target install and cleanup commands.
Its SHA-256 approval token binds the current managed record, uninstall declaration, target commands,
and target health path.

The state machine is `recheck -> stage managed version directory -> install target -> re-query exact
raw package version -> application health check -> atomic ownership replacement -> backup cleanup`.
Staging uses an operation-specific path and never touches application configuration outside the
exact version directory. Target failure runs the reviewed cleanup command, restores the directory,
and asks the owning plugin to verify the restored version. A verified restoration keeps managed
ownership; an unverifiable restoration deletes the ownership receipt and leaves residual files or
packages as external state. Startup recovery restores a pre-commit directory but never adopts
possible target package state. If SQLite already contains the reviewed target package owner,
recovery treats the migration as committed, removes the backup, and completes the journal. Before
any restore or backup removal, recovery re-derives the standard app/version path from the active
managed library and rejects a journal whose managed record or plugin uninstall path differs. The
operation-specific backup must also remain a plain directory rather than a symbolic link.

The reverse `PackageToManagedMigrationPlan` binds the exact package owner and fresh manager state,
its reviewed uninstall and restore commands, the official plugin `InstallPlan`, and the standard
managed target path. Its state machine is `recheck package -> download and verify managed archive ->
stage -> managed health check -> remove package -> re-query absence -> atomic ownership replacement`.
The old package remains present throughout managed archive verification. A failed package removal
deletes the new managed payload and retains a still-verifiable package owner. Failure after package
absence attempts the reviewed exact restore and executable health check before deleting the managed
payload. If compensation cannot be verified, Torben revokes ownership rather than claiming either
source. After the managed provider atomically creates the final version directory, Core writes and
syncs a separate bounded ownership receipt beside the operation journal. It binds the operation,
app, exact version, standard managed target, and reviewed approval token. Recovery re-derives the
target path and validates this receipt before deleting a final payload; a missing, linked,
oversized, malformed, or mismatched receipt preserves the directory and fails closed. An
interruption before package removal cleans only transaction staging and retains the unchanged
package owner. Once package removal has begun, recovery removes only receipt-bound managed payloads,
revokes only the exact recorded package owner, and requires package-manager reconciliation. An
already committed managed SQLite owner is verified at the standard path and recovered as success.

`SourceInstall` and `SourceUninstall` use the workspace lock, durable operation events, and a
persisted source-operation subject containing the exact app, source, coordinate, package kind, and
raw version. SQLite commits the installation row and package ownership atomically. Startup recovery
therefore marks an operation successful when that atomic ownership change is already present. If
the journal was interrupted before ownership commit, startup records
`source_operation_reconciliation_required` and treats any manager-side result as external state;
it does not guess ownership or claim archive-style rollback.

## Torben App update channel

Update preferences are part of the versioned `UserSettings` contract. Missing fields in an older
settings row migrate through serde defaults: Torben App and managed applications notify by default,
while automatic installation and every per-application automatic-update list remain disabled. No
timer, resident process, scheduled task, login item, or system service is created.

The desktop updater endpoint is fixed to the repository's HTTPS GitHub Release `latest.json` asset.
The Base64-encoded minisign public key is compiled from `TORBEN_UPDATER_PUBLIC_KEY`; it is never loaded from user
settings, an update response, or a plugin. A build without that key is explicitly unconfigured and
does not perform its startup check. Invalid Base64, invalid minisign, oversized, or control-character input
fails desktop startup instead of silently disabling verification.

When configured, the desktop checks once after startup only if the local notify preference is
enabled, or when the user presses **Check now**. Finding an update does not download it. Download and
installation require a separate explicit action. Tauri's updater downloads the selected platform
artifact and verifies the response signature with the compiled public key before invoking the
installer; only after a successful install does the process plugin relaunch the application. The
development release workflow neither embeds a key nor generates updater artifacts, so it cannot be
used as an update channel.

Managed application discovery is a separate Core path over the built-in provider catalogs. It
examines only `InstallScope::Managed` records and groups versions by safe release line: Node/Temurin
by major, Python by `major.minor`, and Git/VS Code/Codex by major. Package-manager and external
records never become managed update candidates. Catalog failures retain their structured code,
details, and remediation as per-application warnings while other catalogs continue.

Applying a candidate re-runs discovery and requires the exact installed/available pair to remain
current before calling the ordinary install transaction. The old version is retained. If a version
from that line was selected before installation, Core takes the workspace lock again and moves the
selection only when it is still unchanged; a concurrent CLI or GUI choice wins and is not
overwritten. Per-application automatic updates use the same method sequentially during one
foreground startup check. They are stored as validated, unique `AppId` values and never create a
background task or bypass an installation integrity check.

## Terminal selection

One shim directory is the only Torben App PATH entry. `node`, `npm`, `npx`, `java`, `javac`,
`python`, `python3`, `pip`, `pip3`, `git`, `code`, and `codex` aliases forward to the exact application
installation selected in SQLite. The workspace and desktop package build
`torben-shim` as a native tool shipped beside the host. Switching verifies the installation,
transactionally deploys twelve byte-identical aliases from that bundled binary, and only then
updates selection state. Existing aliases are staged for rollback during replacement, and a
non-file or symbolic-link destination fails closed. Selection and clearing a selection use the
workspace lock and emit durable operation events shared by the CLI and desktop task center.

Torben App never changes the system-level PATH. User-level shell integration is an explicit,
workspace-locked and reversible action exposed through the same Core API to the desktop and CLI.
Status distinguishes `managed`, `external`, `disabled`, and a partial or stale `outdated` state.
Torben App removes only entries it owns: Windows records the exact inserted shim path in a
Core-owned receipt while preserving the registry value type; macOS and Linux use exact marked blocks
in `.profile`, `.zprofile`, and Fish `conf.d`. Existing matching PATH configuration without Torben
ownership is reported as external and disable fails closed rather than removing it. Unix profile
updates are prepared before mutation and roll back already-written files if a later target fails.

Shell mutations also have a separate bounded transaction receipt synced before the first registry
or profile change. On Windows it binds the raw user `Path` value, registry kind and ownership receipt
before and after the action. On Unix it binds the exact three profile paths, action, shim path,
original and updated SHA-256 values, and operation-specific original-file backups. Core performs
recovery while holding the workspace lock during startup. A fully applied transaction only resumes
cleanup; a partial transaction restores the original state only while every target still matches a
recorded state and all required backups validate. A missing, malformed, linked, oversized,
mismatched, or externally changed target or receipt fails closed and remains available for manual
inspection. Profile replacement writes and syncs a pending file before atomic rename. PATH changes
affect newly opened terminals and never mutate the desktop process environment.

## Node.js release trust roots

Core embeds the active Node.js releaser public keys published by the Node.js `release-keys`
repository and pins each certificate by its full primary-key fingerprint. Signing subkeys are
accepted only after their binding to an embedded primary key is validated. Key parsing, fingerprint
matching, signature verification, and archive hashing run inside Core; the Node plugin supplies the
plan but cannot replace trust roots or disable verification.

The embedded active-key set is intended for current and future releases. Historical releases signed
only by a retired key fail with `checksum_signer_untrusted` until that retired trust root is reviewed
and explicitly added.

## Eclipse Temurin release trust

The Temurin provider uses the official Adoptium v3 `available_releases` and
`assets/feature_releases/{feature}/ga` endpoints to discover exact Eclipse Temurin HotSpot JDK
archives for Windows, Linux, and macOS on x64 and ARM64. Core accepts only `vendor=eclipse`, GA JDK
assets whose architecture, operating system, heap, JVM, project, filename, GitHub release path,
size, SHA-256, and detached signature link all match the active request. Metadata, keys, signatures,
and archives have explicit response-size bounds and origin allowlists.

Installation verifies the API-provided SHA-256 and the archive's detached OpenPGP signature. The
official Adoptium public key is fetched from `packages.adoptium.net`, but trust is pinned to primary
fingerprint `3B04D753C9050D9A5D343F39843C48A565F8F04B`; a rotated key therefore fails closed until the
release trust root is reviewed. Archives are extracted through the same traversal-safe staging
path as Node.js. macOS `Contents/Home` is normalized to the managed version root, and both `java`
and `javac` must report the expected Java version before filesystem commit.

The independent `torben-plugin-temurin` process is bundled as a desktop sidecar and uses the same
initialize, discovery, exact-resolution, install-plan, external-discovery, health-check,
uninstall-plan, and Schema UI contracts as Node.js. A cross-platform local fixture covers the full
Core operation sequence from alias resolution through install, selection, `java` command routing,
selection clearing, uninstall, journals, and SQLite state without contacting the live API.

## Python installation strategy

Python.org does not publish one portable binary format across Torben App's three desktop
platforms. The provider therefore has two official-only execution paths. On Windows it delegates
the exact runtime extraction to the PSF Python Install Manager using `py install --target`; this
does not register the extracted runtime or create global aliases. On macOS and Linux it selects the
official XZ CPython source archive, verifies its Python release-manager Sigstore bundle, and builds
with a managed user-level prefix inside staging. The source build requires the platform compiler
and build tools but does not invoke a package manager or elevate privileges.

The implemented metadata layer consumes the python.org v2 release and release-file APIs, excludes
pre-releases, resolves current, minor-line, and exact versions, validates release resource IDs and
FTP paths, and requires the API-declared bundle URL to be exactly the archive URL with the
`.sigstore` suffix. Core pins the reviewed release-manager identity and OIDC issuer per active
Python line. The production verifier embeds the Sigstore production trust root and validates the
Fulcio certificate chain, signed certificate timestamp, Rekor transparency-log evidence, artifact
digest, identity, and issuer. It never skips the certificate chain or transparency log; a webpage
SHA-256 alone is not treated as a sufficient trust root.

Both installation executors run inside the durable Core transaction and Python is available in the
catalog, CLI, desktop detail page, plugin Schema UI, task center, and diagnostics. Windows requires
the official Python Install Manager to be preinstalled and invokes it only with an exact tag and a
staging `--target`. Unix runs `configure`, parallel `make`, and `make install` with `DESTDIR` under
staging, without a package manager or privilege elevation. Core validates the exact CPython version
and a working pip before commit. Selection deploys `python`, `python3`, `pip`, and `pip3` shims; it
does not set `PYTHONHOME`. External Python discovery remains read-only.

## Git installation strategy

Git has two official-only execution paths. Windows x64 and ARM64 consume the target-specific
MinGit ZIP from the latest stable `git-for-windows/git` Release. Core accepts only a non-draft,
non-prerelease `v<git>.windows.<patch>` tag whose release identity, GitHub URL, exact MinGit asset
name, architecture suffix, uploaded state, bounded size, and `sha256:` asset digest all match. The
Windows exact version preserves the packaging revision as SemVer build metadata, for example
`2.55.0+windows.5`. Safe ZIP extraction produces a CLI-focused managed installation without
executing an installer, writing the registry, or claiming Git Bash as a managed command.

macOS and Linux discover stable upstream `git-<version>.tar.xz` archives from the kernel.org index.
Before download trust is committed, Core parses and verifies the clear-signed `sha256sums.asc`
against pinned primary fingerprint `B8868C80BA62A1FFFAF5FDA9632D3A06589DA6B1`. After the compressed
archive matches that signed SHA-256, Core downloads `git-<version>.tar.sign`, decompresses the XZ
stream under a fixed bound, and verifies the uncompressed tar bytes against Git release primary
fingerprint `96E07AF25771955980DAD10020D04E5A713660A7`; signing subkeys are accepted only after their
binding to that primary key validates.

The Unix executor runs `configure` with the final managed prefix, then parallel `make` and
`make install` with `DESTDIR` under transaction staging. It disables gettext and Tcl/Tk additions
but relies on a preinstalled compiler, `make`, and the ordinary Git development libraries. It never
invokes a package manager or elevates privileges. Both paths require `git --version` to resolve to
the exact locked version before filesystem commit. Selection deploys only the `git` shim. External
Git discovery is read-only; repositories, credentials, credential helpers, Git Bash, and global
`.gitconfig` remain outside Torben App ownership.

## Visual Studio Code installation strategy

The VS Code provider treats a published, non-draft, non-prerelease `microsoft/vscode` GitHub
Release as the release fact and requires matching Microsoft Update API metadata for the exact
version and current platform. This avoids trusting the broad stable-version list alone. Core
validates the release identity and URL, product version, target identifier, 40-character build
commit, archive filename, Microsoft download path, response origins, timestamp, and SHA-256.

Windows x64/ARM64 use the official ZIP archives, macOS Intel/Apple Silicon use the official
application ZIPs, and Linux x86_64/ARM64 uses the official tar.gz archives. Downloads have a fixed
512 MiB ceiling, remain on Microsoft-owned origins, and are extracted with traversal protection;
Unix ZIP extraction additionally restores bounded relative symlinks and executable permission
bits required by the macOS application bundle. The staged `code --version --disable-updates`
output must begin with the exact locked version before filesystem commit.

Selection adds only the `code` shim. The shim injects `--disable-updates` on every managed launch so
the immutable source-owned installation cannot update itself behind SQLite state. Torben does not
enable Portable Mode and never migrates, reads, or deletes VS Code settings, sessions, extensions,
accounts, credentials, projects, workspace files, or standard user-data directories. External
VS Code commands are discovered read-only and are never taken over or uninstalled.

## Codex CLI installation strategy

The Codex provider consumes only published, non-draft, non-prerelease `openai/codex` GitHub
Releases whose tag is exactly `rust-v<semver>` and whose release name is the same semantic version.
Windows x64/ARM64 select the exact target `.exe.zip`; macOS Intel/Apple Silicon and Linux
x86_64/ARM64 select the exact target tar.gz. Core validates the release identity, target name,
GitHub release path, uploaded state, bounded size, and GitHub-provided SHA-256 before extraction.
The target-named binary is then normalized to the single managed `codex` command.

Linux releases additionally publish a Cosign-era `.sigstore` file for the uncompressed native
binary. Core verifies the bundle asset's own GitHub digest, parses its certificate, signature, Rekor
canonical body, signed entry timestamp, log ID, index, and integrated time, and converts it to the
equivalent Sigstore v0.1 verification structure. After safe extraction, Core hashes the actual
binary and verifies Fulcio chain, SCT, Rekor inclusion promise, GitHub Actions issuer, and exact
workflow identity
`https://github.com/openai/codex/.github/workflows/rust-release.yml@refs/tags/rust-v<version>`.
It does not skip certificate or transparency-log checks.

Health checks set `CODEX_HOME` to an empty operation-specific temporary directory and execute only
`codex --version`, preventing install or selection checks from consulting user authentication or
configuration. The selected shim launches the official client without changing the user's normal
Codex environment. Torben never invokes login/logout and never opens, migrates, reports, switches,
or deletes `auth.json`, `config.toml`, account or Provider state, history, plugins, skills, logs, or
OS credential-store entries. External Codex discovery is version-only and remains read-only.
