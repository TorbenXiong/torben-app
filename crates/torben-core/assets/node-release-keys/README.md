# Node.js release keys

These ASCII-armored public keys are vendored from the Node.js `release-keys` repository:

`https://github.com/nodejs/release-keys/tree/main/keys`

The allowlist was synchronized with the active releaser section on 2026-08-23. Core does not trust
filenames alone: every certificate is parsed, its full primary-key fingerprint is compared with the
compiled allowlist, and its key bindings are verified before it can authenticate a checksum
manifest.

Updating this directory requires reviewing the upstream active-key list, adding or removing the
corresponding full fingerprints in `src/node_signature.rs`, and running the trust-root and detached
signature tests. Retired keys are not accepted implicitly.
