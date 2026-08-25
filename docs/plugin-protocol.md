# Plugin protocol v1

Each plugin is a native executable with stdin, stdout, and stderr pipes. stdout is reserved for UTF-8 JSON-RPC 2.0 messages separated by newlines; diagnostic output belongs on stderr. Calls are sequential in protocol v1 and have a host-defined timeout.

## Lifecycle

The host validates the manifest before starting a plugin:

- exact protocol and minimum host versions;
- current platform target and safe relative executable path;
- per-target SHA-256 digest;
- revoked state;
- official registry Ed25519 signature, unless developer mode explicitly allows sideloading.

The host then calls `initialize`. A crash, timeout, malformed response, mismatched request ID, or incompatible protocol terminates the client without affecting Core or another plugin.

For developer-mode sideloading, Core copies the verified package into an operation-specific staging
directory without following symbolic links, then repeats manifest, target, and executable hash
verification on the staged copy. Only that staged copy can be atomically committed. The plugin
record is written after the filesystem commit, and a durable plugin-specific operation journal
allows startup to finish or roll back an interrupted installation.

First-party plugins bundled in the signed Torben App package do not use the sideload registry path,
but they still run out of process and must pass the protocol, plugin ID, plugin version, target, and
application identity handshake before Core accepts a response.

## Methods

- `initialize`
- `app.describe`
- `versions.list`
- `version.resolve`
- `external.discover`
- `install.plan`
- `health.check`
- `uninstall.plan`
- `operation.event` notification

Install plans are declarative. Plugins do not receive a database connection or frontend runtime. Schema UI consists only of pages, sections, fields, and actions; arbitrary React code and direct Tauri API access are not supported.

The host authorizes each standard method from the manifest capabilities before sending a request:
version methods require `version_discovery`, external discovery requires `external_discovery`,
install and uninstall planning require their matching managed capabilities, and Schema UI requires
`schema_ui`. Health checks require at least one managed install, selection, or uninstall capability.
Missing capability declarations fail with `plugin_capability_denied` without invoking the plugin.

Permission arrays are bounded and duplicate-free. Network entries are lowercase host names without
schemes, ports, paths, or wildcards; filesystem entries are symbolic Torben scopes; external
commands are bare executable names without arguments or paths; package managers are supported
adapter names. These declarations support validation and user review. They do not sandbox a native
plugin or prove that it cannot access undeclared resources.

Official packages are assembled with the repository-owned publisher described in
[plugin registry publishing](plugin-registry-publishing.md). It calculates hashes from the copied
target bytes and signs the exact compact JSON representation expected by the Rust verifier. The
pretty-printed files and their ordering are therefore part of the publication contract; operators
must not reformat a manifest or registry after signing.

Registry and executable paths use one cross-platform portable subset even when a package is
published on only one operating system. The host rejects empty or traversal components, non-ASCII
names, Windows device names, NTFS alternate-data-stream syntax, trailing dots or spaces, oversized
components, case-folded package-directory collisions, and case-folded executable aliases. This
validation occurs before any registry-controlled filesystem access.

## Trust

Native plugins are trusted code. Separate processes isolate crashes and enforce a clear protocol boundary, but they do not form a security sandbox. Permission declarations are reviewed and displayed to users; they do not replace operating-system sandboxing.
