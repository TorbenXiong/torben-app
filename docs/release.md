# Release engineering

Torben App distinguishes development artifacts from official releases. A successful build is not
enough to call an artifact official: the portable executable must come from the exact tagged source
revision and pass the applicable build, launch, transfer, and hash gates. The current preview, CI,
and official release target is Windows x64. The broader package matrix below is retained as future
release-engineering design and is not a current support commitment.

## Native build matrix

Each deferred package target is built on a native GitHub-hosted runner so that desktop packages and
every target-supported native sidecar share one architecture. The package matrix includes ten
Windows providers plus the command shim; deferred Unix targets retain the original six providers
plus the shim. The official portable executable instead embeds the seven currently supported
Windows providers and the shim. `eng/prepare-bundled-tools.mjs` validates the executable header of
every selected package sidecar against the Rust host target before copying it into Tauri's sidecar
directory; the package workflow also passes the same explicit target to Tauri and forwards
`--locked` to Cargo.

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

The default `--mode extract` never invokes a package manager. The reusable development acceptance
workflow uses the explicit `--mode install`: AppImage
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
match the package target, validates `torben-desktop` and every adjacent sidecar as PE or thin
Mach-O files for that target (eleven sidecars on Windows x64, seven on deferred macOS), and
launches with isolated application data plus an allowlisted environment. On macOS it additionally
verifies the bundle identifier, bundle version, executable name, and executable mode from the
copied `.app`.

`.github/workflows/desktop-package-acceptance.yml` runs six disposable hosted-runner jobs: NSIS and
MSI on Windows x64 and ARM64, plus DMG on macOS Intel and Apple Silicon. Windows invokes each
installer silently, discovers the registered installation, runs the probe, and uninstalls in a
`finally` block. macOS mounts the DMG read-only, copies its sole `.app` into a temporary
`Applications` directory to model the documented drag-to-install flow, detaches the image, and runs
the probe against the copy. The manual cross-platform development aggregate depends on these six
jobs and the eight Linux jobs. The official portable workflow does not invoke this deferred package
matrix.

When verified release metadata declares `signingStatus=signed`, the desktop probe also repeats the
platform trust checks after artifact transfer and installation. Windows requires valid
Authenticode on the downloaded MSI or NSIS package, the installed desktop executable, and all eleven
installed Windows sidecars. macOS verifies the copied application bundle with `codesign`, then revalidates
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

`eng/verify-release-set.mjs` re-verifies deferred package/update target directories and applies a
release-kind-specific inventory: development sets retain all six known targets, while its legacy
official mode requires only Windows x64. Every included target must share one version, Git
revision/ref, and release kind.
The tool produces a
deterministic `release-index.json` plus a top-level `SHA256SUMS` covering every target payload,
target manifest, target checksum file, and the aggregate index. Both aggregate files are completely
calculated and fsynced as `.next` files before either final name is exposed; a normal write or
rename failure removes temporary and partially committed aggregate metadata. Official sets must
contain a semantically valid `latest.json` whose two Windows x64 installer records exactly reproduce
the signed mapping files, local signatures, version, and fixed GitHub URLs. Development sets must not contain
`latest.json`. A future package/update publishing job must run `verify` after artifact download and
before creating a GitHub Release. The current portable workflow uses the stricter one-file verifier
instead.

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

The official deliverable is one Windows x64 file named `TorbenApp.exe`. The tag workflow requires:

- the exact `refs/tags/v<workspace-version>` ref and a matching version-specific release-notes file;
- exact ProductVersion, Windows x64 PE target, and a single-file release directory;
- a ten-second launch against a fresh isolated `userData`, creation of `state.db`, and no recursive
  `tools/shims/userData` directory;
- byte-identical SHA-256 verification after GitHub Artifact transfer.

The current portable release is intentionally not Authenticode-signed. Windows can therefore show
an unknown-publisher or SmartScreen warning. The release notes must disclose that limitation; adding
publisher signing later is a separate release-engineering decision.

## Current GitHub workflow

`.github/workflows/release.yml` is intentionally manual and development-only. It has no tag or push
trigger, requests read-only repository contents, never calls `gh release`, and records every target
as `releaseKind=development` and `signingStatus=unsigned`. It uploads six short-lived target
artifacts, runs the fourteen-job combined package acceptance matrix, downloads every target by exact
name, runs the aggregate verification, and uploads one 14-day six-target development artifact.
GitHub-owned Actions are pinned to complete reviewed commit SHAs rather than mutable tags.

`.github/workflows/windows-preview.yml` is the current supported release feedback path. It is manual
and read-only, and builds only `x86_64-pc-windows-msvc`. The internal candidate contains NSIS, MSI,
the archived CLI, an explicit `UNSIGNED-PREVIEW.txt` warning, and deterministic unsigned development
metadata. A reusable two-job acceptance matrix installs, launches, and uninstalls both Windows
packages before three separate 14-day downloads are exposed: an NSIS preview, an MSI preview, and a
CLI preview. Each download contains only its selected distributable, the unsigned-build warning, and
a SHA-256 checksum, so users do not need to download duplicate installer formats. The workflow does
not enter a protected Environment, read signing credentials, create a tag or GitHub Release, or claim
that the preview is an official release.

`.github/workflows/official-release.yml` is the Windows x64 formal publishing path. It emits only
`TorbenApp.exe`; installer packages, CLI archives, updater manifests, checksum files, and release
metadata are not public assets. Cross-platform build and package-acceptance definitions remain in
the manual development workflow for future work and are not part of the current support scope. The
protected-environment review remains the manual publication approval gate. The preview workflow
must not be renamed or treated as an official release.

The application-side updater uses the fixed GitHub Release `latest.json` endpoint and accepts its
Base64-encoded minisign verification key only through the compile-time
`TORBEN_UPDATER_PUBLIC_KEY` environment
variable. Development and official portable builds omit the variable and therefore never query the
endpoint. The updater implementation and its signed metadata tooling remain for a future explicit
installer/update-channel milestone; the current portable release is upgraded by replacing
`TorbenApp.exe` while preserving `userData`.

`.github/workflows/official-release.yml` is the only publishing workflow. It runs only for an exact
`v<workspace-version>` tag. Its publishing job is bound to the protected `official-release` GitHub
environment, which requires one review but no Windows signing secrets. The Windows x64 build job
verifies the tag, release-notes template, source, and tests before building. It verifies the
executable version and target, the exact one-file inventory, and a sustained launch from a fresh
`TorbenApp.exe` plus `userData` directory.

The publish job downloads only `TorbenApp.exe`, compares its SHA-256 with the build-job output,
rechecks ProductVersion, then generates the approved release-note format from
`docs/releases/<version>.md`. It appends the verified SHA-256 and tag-specific changelog URL and
creates the GitHub Release once with `--verify-tag`. An unexpected file, hash mismatch, failed
launch, version mismatch, or an existing Release stops publication.
