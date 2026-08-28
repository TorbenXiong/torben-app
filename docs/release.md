# Release engineering

Torben App distinguishes development artifacts from official releases. A successful package build
is not enough to call an artifact official: every platform package, update artifact, and checksum
must come from the same exact version and source revision, and the platform signing gates must pass.

## Native build matrix

Each target is built on a native GitHub-hosted runner so that desktop packages and all seven native
sidecars share one architecture. `eng/prepare-bundled-tools.mjs` validates the executable header of
every plugin and shim against the Rust host target before copying it into Tauri's sidecar directory;
the workflow also passes the same explicit target to Tauri and forwards `--locked` to Cargo.

| Platform | Rust target | Expected packages |
| --- | --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` | NSIS/MSI plus CLI archive |
| Windows ARM64 | `aarch64-pc-windows-msvc` | NSIS/MSI plus CLI archive |
| macOS Intel | `x86_64-apple-darwin` | app/DMG plus CLI archive |
| macOS Apple Silicon | `aarch64-apple-darwin` | app/DMG plus CLI archive |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | AppImage, deb, rpm, and CLI archive |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | AppImage, deb, rpm, and CLI archive |

Ubuntu 24.04 is the Linux build baseline. Package installation and launch remain separate
acceptance jobs on every platform; they must not be inferred from a successful package build.

`eng/linux-package-smoke.mjs` is the shared Linux package launch probe for those acceptance jobs.
It first re-verifies the target release metadata and requires the runner architecture to match the
package target. It then extracts one AppImage, deb, or rpm into a fresh temporary directory without
installing it on the host, validates the `Torben App` desktop entry, and checks that the desktop
executable plus all six bundled application plugins and the shim are adjacent ELF files for the
same Rust target. The launch runs under `xvfb-run` with isolated XDG data, configuration, cache, and
runtime directories; success means the GUI remains alive for the bounded probe window. The child
receives only an allowlisted environment so CI credentials are not forwarded to the application.

The runner deliberately does not treat extraction as proof that deb/rpm package-manager scripts or
system installation work. `.github/workflows/linux-package-acceptance.yml` installs and launches
the packages inside disposable root containers. Its matrix covers x86_64 and ARM64 on Ubuntu
24.04 for AppImage, Debian 13 for deb, Fedora 44 for rpm, and Rocky Linux 10.2 for rpm. Rocky's
base repositories do not ship the WebKitGTK 4.1 ABI required by Tauri 2, so the Rocky acceptance
bootstrap enables the distribution's CRB repository and the community-approved EPEL repository
before installing the RPM. Ubuntu, Debian, and Fedora run the probe through Xvfb; Rocky 10 uses
EPEL's Weston with its headless backend and Pixman software renderer because Xvfb is not available
there. Both paths require the GUI process to remain alive for the bounded probe window. Required
probe tools are `timeout`, plus `xvfb-run` or `weston`, and `dpkg-deb` for deb or `bash`,
`rpm2cpio`, and `cpio` for rpm. AppImage extraction uses `--appimage-extract` and does not require
FUSE.

The default `--mode extract` never invokes a package manager. The reusable acceptance workflow is
called by both development and official releases and uses the explicit `--mode install`: AppImage
runs the verified portable package with
`APPIMAGE_EXTRACT_AND_RUN=1`, deb invokes `apt-get`, and rpm invokes `dnf`. System package modes
require root and are intended only for a disposable container. After the package manager succeeds,
the runner maps every previously inspected package path into the live container root, rechecks the
desktop identity and ELF target there, and launches the installed executable. A package-manager
failure, missing installed file, architecture mismatch, early GUI exit, or non-root invocation is
fatal. The RPM probe keeps GPG verification enabled, retains downloaded packages through the
transaction, and serializes package downloads. Only when DNF reports both unreadable cached
packages and a failed GPG check does it run one bounded recovery sequence: clear downloaded package
files, resolve missing dependencies with `dnf download` into an isolated directory, verify and
remove the byte-identical downloaded copy of the application RPM, require RPM to confirm
`signatures OK` for every remaining dependency, install that closed dependency set in one native RPM
transaction without repository access, then install the already-inspected local Torben App RPM
offline. Cleanup, download, application-copy comparison, dependency signature verification or
transaction, final package installation, and every other package-manager error fail closed. The
recovery never uses `--nogpgcheck`.

`eng/desktop-package-smoke.mjs` provides the equivalent post-install inspection and sustained launch
probe for Windows and macOS. It re-verifies release metadata, requires the runner architecture to
match the package target, validates `torben-desktop` and all seven adjacent sidecars as PE or thin
Mach-O files for that target, and launches with isolated application data plus an allowlisted
environment. On macOS it additionally verifies the bundle identifier, bundle version, executable
name, and executable mode from the copied `.app`.

`.github/workflows/desktop-package-acceptance.yml` runs six disposable hosted-runner jobs: NSIS and
MSI on Windows x64 and ARM64, plus DMG on macOS Intel and Apple Silicon. Windows invokes each
installer silently, discovers the registered installation, runs the probe, and uninstalls in a
`finally` block. macOS mounts the DMG read-only, copies its sole `.app` into a temporary
`Applications` directory to model the documented drag-to-install flow, detaches the image, and runs
the probe against the copy. The aggregate development artifact and official publishing job depend
on these six jobs and the eight Linux jobs, so a package that cannot install or remain running on
any declared target cannot be published.

When verified release metadata declares `signingStatus=signed`, the desktop probe also repeats the
platform trust checks after artifact transfer and installation. Windows requires valid
Authenticode on the downloaded MSI or NSIS package, the installed desktop executable, and all seven
installed sidecars. macOS verifies the copied application bundle with `codesign`, then revalidates
the downloaded DMG's stapled notarization ticket and Gatekeeper assessment. Unsigned development
metadata does not claim or require these checks.

```bash
node eng/linux-package-smoke.mjs \
  --artifacts artifacts/release-set/x86_64-unknown-linux-gnu \
  --format appimage \
  --mode extract
```

`eng/collect-release-artifacts.mjs` inspects the native `torben` executable header and rejects a PE,
ELF, or thin Mach-O architecture that differs from the requested Rust target. It also requires
exactly one package in every format listed above before copying anything into a new or empty
target-specific artifact directory. The CLI copy is named `torben-<version>-<target>` (plus `.exe`
on Windows), so it cannot be confused with a package from another matrix job. Before hashing, the
workflow additionally creates a ZIP on Windows or a `tar.gz` on macOS/Linux. The Unix archive
preserves the executable bit that GitHub Artifact transport otherwise normalizes; distributed users
should consume the archived CLI rather than the raw verification copy.

```powershell
node .\eng\collect-release-artifacts.mjs `
  --bundle-root .\target\release\bundle `
  --cli-binary .\target\release\torben.exe `
  --output .\artifacts\windows-x64 `
  --target x86_64-pc-windows-msvc
```

The collector requires a new output path outside the Tauri bundle tree. It validates every required
package and the CLI architecture before creating a sibling `.next` directory, copies and applies
Unix mode bits only there, and exposes the target directory with one rename. An existing final or
staging path is never reused; a copy, collision, mode, or rename failure removes the staging
directory so later metadata steps cannot consume a partial target.

## Reproducible metadata

`eng/release-metadata.mjs` uses only Node.js standard-library APIs. It first requires the versions
in the Cargo workspace, root package, desktop package, UI package, and Tauri configuration to be
identical. It then hashes a new target-specific artifact directory without following symbolic
links and writes deterministic, sorted files:

- `release-metadata.json`: product/application identity, exact version, Git revision/ref,
  development or official status, signing status, target OS/architecture, file sizes, and SHA-256.
- `SHA256SUMS`: every payload file plus `release-metadata.json` using the conventional two-space
  separator.

No generation timestamp is included, so identical inputs and release identity produce identical
metadata. Generation refuses to overwrite existing or staged metadata, calculates both files
before publication, fsyncs both `.next` files, and removes temporary or partially committed output
after a normal failure. Verification rejects missing, modified, additional, non-regular,
symbolic-link, wrong-target, wrong-version, or malformed files.

Example for an unsigned Windows x64 development artifact directory:

```powershell
node .\eng\release-metadata.mjs create `
  --artifacts .\artifacts\windows-x64 `
  --target x86_64-pc-windows-msvc `
  --revision 0123456789abcdef0123456789abcdef01234567 `
  --source-ref refs/heads/feature/bootstrap `
  --release-kind development `
  --signing-status unsigned

node .\eng\release-metadata.mjs verify `
  --artifacts .\artifacts\windows-x64
```

After all six matrix artifacts have been transferred into directories named by their Rust target,
`eng/verify-release-set.mjs` re-verifies every target and requires exactly one copy of every
supported target. All six must share one version, Git revision/ref, and release kind. It produces a
deterministic `release-index.json` plus a top-level `SHA256SUMS` covering every target payload,
target manifest, target checksum file, and the aggregate index. Both aggregate files are completely
calculated and fsynced as `.next` files before either final name is exposed; a normal write or
rename failure removes temporary and partially committed aggregate metadata. Official sets must
contain a semantically valid `latest.json` whose 12 platform records exactly reproduce the signed
mapping files, local signatures, version, and fixed GitHub URLs. Development sets must not contain
`latest.json`. The final publishing job must run `verify` after artifact download and before
creating a GitHub Release.

```powershell
node .\eng\verify-release-set.mjs create --releases .\artifacts\release-set
node .\eng\verify-release-set.mjs verify --releases .\artifacts\release-set
```

Run the dependency-free regression tests with:

```powershell
node --test `
  .\eng\release-metadata.test.mjs `
  .\eng\collect-release-artifacts.test.mjs `
  .\eng\verify-release-set.test.mjs `
  .\eng\updater-artifacts.test.mjs `
  .\eng\linux-package-smoke.test.mjs `
  .\eng\desktop-package-smoke.test.mjs
```

## Official-release gates

The metadata tool accepts `official` only for the exact `refs/tags/v<version>` ref and only when
the signing status is `signed`. The release workflow must independently prove that declaration:

- Windows packages have a valid configured publisher signature.
- macOS packages are signed with the configured Developer ID and notarization succeeds.
- update artifacts and metadata are signed with the configured Tauri updater key.
- every target metadata file verifies after artifact transfer and before GitHub Release creation.
- desktop, CLI, sidecars, package metadata, tag, and update metadata all report the same version.

If any signing credential is absent, the workflow may publish a clearly named development artifact
for manual testing, but it must not create or update an official GitHub Release.

## Current GitHub workflow

`.github/workflows/release.yml` is intentionally manual and development-only. It has no tag or push
trigger, requests read-only repository contents, never calls `gh release`, and records every target
as `releaseKind=development` and `signingStatus=unsigned`. It uploads six short-lived target
artifacts, runs the fourteen-job combined package acceptance matrix, downloads every target by exact
name, runs the aggregate verification, and uploads one 14-day six-target development artifact.
GitHub-owned Actions are pinned to complete reviewed commit SHAs rather than mutable tags.

`.github/workflows/windows-preview.yml` is the smaller Windows-first feedback path. It is also
manual and read-only, but builds only `x86_64-pc-windows-msvc`. The candidate contains NSIS, MSI,
the archived CLI, an explicit `UNSIGNED-PREVIEW.txt` warning, and deterministic unsigned development
metadata. A reusable two-job acceptance matrix installs, launches, and uninstalls both Windows
packages before a 14-day `torben-app-<version>-windows-x64-unsigned-preview` artifact is exposed.
The workflow does not enter a protected Environment, read signing credentials, create a tag or
GitHub Release, or claim that the preview is an official release.

Formal publishing is defined but remains operationally unavailable until Windows signing, macOS
Developer ID/notarization, the Tauri updater signing path, and protected-environment review are
configured. The development workflow must not be renamed or treated as an official release.

The application-side updater uses the fixed GitHub Release `latest.json` endpoint and accepts its
Base64-encoded minisign verification key only through the compile-time
`TORBEN_UPDATER_PUBLIC_KEY` environment
variable. Development builds omit the variable and therefore never query the endpoint. The official
workflow generates Tauri updater artifacts, signs them with the matching private key, embeds only
the public key, verifies every `.sig`, and includes the signed `latest.json` in the six-target
publication transaction.

`.github/workflows/official-release.yml` is the only publishing workflow. It runs only for an exact
`v<workspace-version>` tag and is bound to the protected `official-release` GitHub environment. The
environment must require review and provide all relevant secrets:

- updater: `TORBEN_UPDATER_PUBLIC_KEY` (Base64-encoded minisign public-key text),
  `TAURI_SIGNING_PRIVATE_KEY`, and `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`;
- Windows: `WINDOWS_CERTIFICATE` (Base64 PFX), `WINDOWS_CERTIFICATE_PASSWORD`, the exact
  `WINDOWS_CERTIFICATE_SUBJECT`, and an HTTPS `WINDOWS_TIMESTAMP_URL`;
- macOS: `APPLE_CERTIFICATE` (Base64 P12), `APPLE_CERTIFICATE_PASSWORD`, `KEYCHAIN_PASSWORD`,
  `APPLE_ID`, `APPLE_PASSWORD`, and `APPLE_TEAM_ID`.

Each native job verifies the tag and secrets before building. Windows imports the PFX into the
ephemeral user store, requires its Subject to match the protected environment, Authenticode-signs
the CLI and sidecars, lets Tauri sign MSI/NSIS, and then requires every signature to report `Valid`
from the same publisher. macOS imports a `Developer ID Application` certificate into an ephemeral
keychain, signs CLI/sidecars and the application, and requires both `stapler validate` and Gatekeeper
assessment for the DMG. Tauri produces minisign updater signatures for every supported installer.
All signed Windows, macOS, and Linux artifacts must then pass the same fourteen-job installation and
sustained GUI launch matrix before the publishing job can receive write permission.

`torben-release-tools` streams each downloaded artifact through `minisign-verify` using the public
key compiled into the application. The publish job repeats those checks after artifact transfer,
requires the exact 12-platform updater mapping, generates `latest.json`, re-verifies the complete
release set, flattens only unique public assets, and creates the GitHub Release once with
`--verify-tag`. Mapping records accept only the fixed target/platform pairs, safe basenames, exact
package suffixes, matching `.sig` names, and files already covered by signed target metadata.
macOS updater tarballs receive a deterministic Rust-target suffix so Intel and Apple Silicon can
never resolve to the same GitHub asset URL. Existing target packages must be byte-identical to the
Tauri updater input; they are not silently reused by name alone. The same structural validator
resolves every verifier path before `torben-release-tools` reads an artifact, both in the native
build job and after artifact transfer. Manifest and mapping files are atomically written, while
flattened assets are staged in a sibling directory and renamed only after every copy and checksum
succeeds. A separate upload gate then re-enumerates the flat directory, rejects links and nested or
extra entries, compares every public asset with its verified release-set source, and validates the
exact publishing `SHA256SUMS` before `gh release create` can run. Missing credentials, duplicate
asset names, signature mismatch, notarization failure, partial target matrices, unsafe mappings,
source divergence, or an existing Release stop publication without leaving a publishable partial
directory.
