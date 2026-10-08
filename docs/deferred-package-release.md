# Deferred package release engineering

Windows ARM64, macOS, Linux, installer packages, and the signed updater channel are retained
engineering paths. They are not current product support or release gates. The supported Windows
x64 portable release is documented in [release engineering](release.md).

## Native build matrix

Build each deferred package target in a native environment so that desktop packages and
every target-supported native sidecar share one architecture. The package matrix includes ten
Windows providers plus the command shim; deferred Unix targets retain the original six providers
plus the shim. The official portable executable instead embeds the seven currently supported
Windows providers and the shim. `eng/prepare-bundled-tools.mjs` validates the executable header of
every selected package sidecar against the Rust host target before copying it into Tauri's sidecar
directory. Pass the same explicit target to Tauri and forward `--locked` to Cargo when building.

| Platform | Rust target | Expected packages |
| --- | --- | --- |
| Windows x64 | `x86_64-pc-windows-msvc` | NSIS/MSI plus CLI archive |
| Windows ARM64 | `aarch64-pc-windows-msvc` | NSIS/MSI plus CLI archive |
| macOS Intel | `x86_64-apple-darwin` | app/DMG plus CLI archive |
| macOS Apple Silicon | `aarch64-apple-darwin` | app/DMG plus CLI archive |
| Linux x86_64 | `x86_64-unknown-linux-gnu` | AppImage, deb, rpm, and CLI archive |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | AppImage, deb, rpm, and CLI archive |

Ubuntu 24.04 is the Linux build baseline. Package installation and launch remain separate
acceptance steps on every platform; they must not be inferred from a successful package build.

`eng/linux-package-smoke.mjs` is the shared Linux package launch probe for local native acceptance.
It first re-verifies the target release metadata and requires the runner architecture to match the
package target. It then extracts one AppImage, deb, or rpm into a fresh temporary directory without
installing it on the host, validates the `Torben App` desktop entry, and checks that the desktop
executable plus all six bundled application plugins and the shim are adjacent ELF files for the
same Rust target. The launch runs under `xvfb-run` with isolated XDG data, configuration, cache, and
runtime directories; success means the GUI remains alive for the bounded probe window. The child
receives only an allowlisted environment so unrelated credentials are not forwarded to the application.

The runner deliberately does not treat extraction as proof that deb/rpm package-manager scripts or
system installation work. Run installation and launch checks manually in disposable native
containers when resuming those milestones. Required probe tools are `timeout`, plus `xvfb-run` or
`weston`, and `dpkg-deb` for deb or `bash`, `rpm2cpio`, and `cpio` for rpm. AppImage extraction
uses `--appimage-extract` and does not require FUSE.

The default `--mode extract` never invokes a package manager. Explicit local installation
acceptance uses `--mode install`: AppImage
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

For local Windows acceptance, install NSIS/MSI in a disposable environment, discover the
registered installation, run the probe, and uninstall after the check. For macOS acceptance, mount
the DMG read-only, copy its sole `.app` into a temporary `Applications` directory, detach the
image, and run the probe against the copy. These deferred package checks are separate from current
Windows x64 portable acceptance.

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
on Windows), so it cannot be confused with a package for another target. Before hashing, create a
ZIP on Windows or a `tar.gz` on macOS/Linux. The Unix archive preserves executable mode bits during
transfer; distributed users should consume the archived CLI rather than the raw verification copy.

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
before creating a GitHub Release. The current portable release uses the stricter one-file verifier
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
