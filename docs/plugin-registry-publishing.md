# Plugin registry publishing

Torben App's official plugin registry is a static, signed directory. The repository supports both
local offline generation and a protected GitHub Actions artifact workflow. Neither path deploys to
a public endpoint, rotates keys, or changes a build's trust root. The workflow uploads only a
short-lived review artifact; public HTTPS hosting is not live.

## Trust and inputs

The trust chain has two Ed25519 levels. An offline registry root signs `registry.json`; each
publisher key signs that publisher's package manifests. The registry pins the publisher public key
and the SHA-256 of each signed manifest. Each manifest, in turn, pins the SHA-256 of every target
executable.

Keep root and publisher private keys outside the repository, source tree, and output directory.
Pass them as PEM file paths only. The tool verifies that each key is Ed25519 and emits only raw
Base64 public keys. It rejects reuse of the root key as a publisher key and reuse of one publisher
key for multiple publisher identities. It never copies private-key material into the result.

The config is strict JSON:

```json
{
  "schemaVersion": 1,
  "sequence": 1,
  "generatedAt": "2026-08-23T00:00:00Z",
  "minimumHostVersion": "0.1.0",
  "publishers": [
    {
      "id": "example.publisher",
      "displayName": "Example Publisher",
      "revoked": false
    }
  ],
  "packages": [
    {
      "manifestPath": "packages/example/1.2.3/plugin.json",
      "publisherId": "example.publisher",
      "revoked": false
    }
  ]
}
```

`sequence` and `generatedAt` are explicit release inputs. The publisher never reads the current
clock or guesses a sequence. Increase the sequence for every changed registry snapshot; hosts reject
rollback and reject different content that reuses a cached sequence. `generatedAt` uses a valid UTC
timestamp with whole-second precision.

Manifest paths and target executable paths use `/`-separated relative paths. They cannot contain
empty, `.`, `..`, absolute, backslash, symbolic-link-file, or outside-root references. Components
use a portable ASCII subset and reject leading/trailing dots or spaces, Windows device names, NTFS
alternate-data-stream syntax, oversized paths, and case-folded package aliases. Every manifest must
be named `plugin.json` under a directory whose final component is its exact version. The source
manifest includes all public protocol fields, but its field order is irrelevant because the tool
reconstructs the canonical order. Existing signature and target `sha256` values are replaced during
publication. Publisher display names must match between the config and manifest.

## Create an artifact

Use the repository's approved Node.js version. The command uses only Node built-ins and does not
install or resolve packages:

```powershell
node .\eng\publish-plugin-registry.mjs `
  --config D:\secure\registry-release.json `
  --source D:\secure\registry-source `
  --output D:\secure\registry-output-1 `
  --root-key D:\secure\keys\registry-root.pem `
  --publisher-key example.publisher=D:\secure\keys\example-publisher.pem `
  --emit-root-public-key
```

Repeat `--publisher-key` once for every configured publisher. The output directory must not already
exist and must be outside the source tree. The tool builds a sibling staging directory and renames
it only after every package and signature succeeds. On failure, it removes its uniquely named
staging directory and leaves no partially published output.

The result contains:

- `registry.json`;
- each `packages/.../plugin.json` at its configured path;
- exactly the executable files referenced by those manifests;
- `registry-root-public-key.txt` when `--emit-root-public-key` is present.

The root public key printed by the command is the Base64 value reviewed for
`TORBEN_OFFICIAL_PLUGIN_REGISTRY_KEY`. Do not change that build input merely because a registry
snapshot changes. Root rotation requires a separate product and release decision because existing
hosts trust only the key compiled into their build.

## Create a protected review artifact

`.github/workflows/plugin-registry-release.yml` is a manual `workflow_dispatch` that runs only for
`main` and enters the protected `official-plugin-registry` GitHub Environment. Configure that
Environment with:

- secret `TORBEN_PLUGIN_REGISTRY_ROOT_PRIVATE_KEY`: the registry-root Ed25519 private key in PEM
  form;
- secret `TORBEN_PLUGIN_REGISTRY_PUBLISHER_PRIVATE_KEYS_JSON`: a JSON object mapping every reviewed
  publisher ID to its Ed25519 private key PEM;
- variable `TORBEN_PLUGIN_REGISTRY_ROOT_PUBLIC_KEY`: the exact raw Base64 Ed25519 trust root already
  reviewed for release builds.

For example, the publisher secret has this shape; the values shown are placeholders, not usable
keys:

```json
{
  "example.publisher": "-----BEGIN PRIVATE KEY-----\n<PRIVATE_KEY>\n-----END PRIVATE KEY-----\n"
}
```

Require Environment reviewers and prevent unreviewed branches from deploying to that Environment.
The workflow itself additionally rejects any ref other than `refs/heads/main`. Its config, source
directory, and optional previous registry inputs must be relative, non-link paths inside the exact
checked-out revision. Keep production inputs in reviewed repository paths; do not point this job at
generated or downloaded content.

The dispatch inputs repeat the security-sensitive release metadata so the workflow can require an
exact match with both the reviewed config and signed output:

- config path and package source directory;
- previous signed `registry.json` path, required for every sequence after `1`;
- sequence, whole-second UTC `generatedAt`, and exact `minimumHostVersion`.

Sequence `1` is the only release allowed without a predecessor. Later releases must advance exactly
one sequence and use a later timestamp. The predecessor is verified with the same configured root by
the shipped Rust `RegistryVerifier`; an unsigned, foreign-root, reused, skipped, or rolled-back
sequence fails the job.

The secret-bearing step invokes only repository-owned Node scripts. It writes keys with restrictive
permissions beneath an operation-specific `RUNNER_TEMP` directory, passes file paths to the
publisher, and removes that directory through an exit trap before verification or artifact upload.
Secrets are not job-level environment variables and are never passed to an Action, package manager,
Rust process, or uploaded tree.

After generation, the workflow:

1. compares the emitted public key byte-for-byte with the protected Environment variable;
2. verifies the new registry and predecessor through the Rust host verifier;
3. independently verifies the root and publisher Ed25519 signatures, reviewed config order and
   values, every manifest hash, every platform executable hash, and exact tree membership;
4. writes deterministic `SHA256SUMS` for every signed artifact file;
5. uploads `plugin-registry-sequence-<sequence>-<revision>` for 14 days.

The workflow has only `contents: read`. It has no GitHub Pages, Release, package publication,
OIDC, or third-party deployment permission. A successful run means the registry tree is ready for
review and external hosting; it does not mean a public endpoint exists or changed.

## Reproducibility and release checks

Signatures cover compact `JSON.stringify` payloads with `signature: null`, matching the Rust
`serde_json` field order. Files are UTF-8, two-space indented, LF-terminated JSON. Reformatting,
reordering fields, or editing any signed file invalidates the trust chain.

Before hosting a snapshot:

1. Use the protected artifact workflow, or run both
   `eng/publish-plugin-registry.test.mjs` and `eng/plugin-registry-release.test.mjs` before a local
   offline publication.
2. Compare the reviewed config, predecessor, sequence, revocations, `SHA256SUMS`, and target
   inventory with the intended release.
3. Serve the entire output tree without content transformation from one HTTPS origin.
4. Configure release builds with that origin's `registry.json` URL and the separately reviewed root
   public key.
5. Refresh from a development build configured with the same key and URL, then perform an exact
   plugin installation on every published platform.

The automated compatibility test regenerates the existing six-target fixture with fixed test keys
and requires both signed JSON files to match the Rust-verified fixtures byte for byte. Test keys and
fixture signatures are public test data and must never be reused for a real registry.
