import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { verifyWindowsPortableRelease } from "./verify-windows-portable-release.mjs";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));

test("Windows desktop manifest requests administrator privileges", () => {
  const manifest = readFileSync(
    join(repositoryRoot, "apps", "desktop", "src-tauri", "windows-app-manifest.xml"),
    "utf8",
  );
  assert.match(manifest, /requestedExecutionLevel level="requireAdministrator"/u);
  assert.match(manifest, /Microsoft\.Windows\.Common-Controls/u);
});

test("Windows desktop exposes the custom title bar controls", () => {
  const config = JSON.parse(
    readFileSync(join(repositoryRoot, "apps", "desktop", "src-tauri", "tauri.conf.json"), "utf8"),
  );
  const capability = JSON.parse(
    readFileSync(
      join(repositoryRoot, "apps", "desktop", "src-tauri", "capabilities", "default.json"),
      "utf8",
    ),
  );

  assert.equal(config.app.windows[0].decorations, false);
  for (const permission of [
    "core:window:allow-close",
    "core:window:allow-minimize",
    "core:window:allow-start-dragging",
    "core:window:allow-toggle-maximize",
  ]) {
    assert.ok(capability.permissions.includes(permission), `missing ${permission}`);
  }
});

test("Windows portable builder emits only the desktop executable with embedded provider payload", () => {
  const script = readFileSync(join(repositoryRoot, "eng", "build-windows-portable.mjs"), "utf8");
  const managerPreparation = readFileSync(
    join(repositoryRoot, "eng", "prepare-python-manager.mjs"),
    "utf8",
  );
  assert.match(script, /--no-bundle/u);
  assert.match(script, /TorbenApp\.exe/u);
  assert.match(script, /torben-desktop\.exe/u);
  assert.match(script, /embedded/u);
  assert.doesNotMatch(script, /userData[\\/]+plugins/u);
  assert.match(script, /torben-plugin-node/u);
  assert.match(script, /torben-plugin-temurin/u);
  assert.match(script, /torben-plugin-python/u);
  assert.match(script, /torben-plugin-rust/u);
  assert.match(script, /torben-plugin-mysql/u);
  assert.match(script, /torben-plugin-redis/u);
  assert.match(script, /torben-plugin-postgresql/u);
  assert.match(script, /verifyPythonManagerPackage/u);
  assert.match(managerPreparation, /prepare:python-manager/u);
  assert.match(managerPreparation, /python-manager-\$\{PYTHON_MANAGER_VERSION\}\.msi/u);
  assert.match(
    managerPreparation,
    /259af5272c8f798786c1109b7ad287da519c58d43d7250ead8ed20fa3277a511/u,
  );
  assert.match(managerPreparation, /www\.python\.org\/ftp\/python\/pymanager/u);
  assert.match(managerPreparation, /redirect: "error"/u);
  assert.doesNotMatch(script, /torben-plugin-(?:git|vscode|codex)/u);
  assert.doesNotMatch(script, /copyFileSync\(join\(releaseRoot, "torben\.exe"/u);
  assert.doesNotMatch(script, /--prepare-only|--desktop-only/u);
});

test("official portable verifier accepts exactly one Windows x64 TorbenApp executable", () => {
  const root = mkdtempSync(join(tmpdir(), "torben-portable-release-"));
  try {
    const release = join(root, "release");
    mkdirSync(release);
    const executable = Buffer.alloc(512);
    executable.write("MZ", 0, "ascii");
    executable.writeUInt32LE(0x80, 0x3c);
    executable.write("PE\0\0", 0x80, "ascii");
    executable.writeUInt16LE(0x8664, 0x84);
    writeFileSync(join(release, "TorbenApp.exe"), executable);

    const verified = verifyWindowsPortableRelease({ directory: release });
    assert.equal(verified.target, "x86_64-pc-windows-msvc");
    assert.match(verified.sha256, /^[0-9A-F]{64}$/u);
    assert.equal(
      verifyWindowsPortableRelease({
        directory: release,
        expectedSha256: verified.sha256.toLowerCase(),
      }).sha256,
      verified.sha256,
    );

    writeFileSync(join(release, "SHA256SUMS"), "not published");
    assert.throws(
      () => verifyWindowsPortableRelease({ directory: release }),
      /must contain only TorbenApp\.exe/u,
    );
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("Windows release startup confirms the base and keeps data in its userData directory", () => {
  const desktop = readFileSync(
    join(repositoryRoot, "apps", "desktop", "src-tauri", "src", "lib.rs"),
    "utf8",
  );
  const paths = readFileSync(
    join(repositoryRoot, "crates", "torben-core", "src", "paths.rs"),
    "utf8",
  );
  assert.match(desktop, /prompt_for_windows_data_root/u);
  assert.match(desktop, /FolderBrowserDialog/u);
  assert.match(desktop, /TORBEN_FIRST_RUN_APPLICATION_DIR/u);
  assert.match(desktop, /default_windows_application_directory/u);
  assert.match(desktop, /for drive in b'D'\.\.=b'Z'/u);
  assert.match(desktop, /root\.join\("TorbenApp"\)/u);
  assert.match(desktop, /replace_windows_executable/u);
  assert.match(desktop, /application_directory\.join\("userData"\)/u);
  assert.match(desktop, /remove_redundant_windows_data_root_pointer/u);
  assert.doesNotMatch(desktop, /fn persist_windows_data_root/u);
  assert.match(desktop, /Command::new\(&target_executable\)/u);
  assert.match(desktop, /--torben-relocated-source/u);
  assert.match(desktop, /identical_regular_files/u);
  assert.match(desktop, /application_directory\.join\("userData"\)/u);
  assert.match(desktop, /System\.Windows\.Forms\.TextBox/u);
  assert.doesNotMatch(desktop, /\$defaultPath = \$args\[0\]/u);
  assert.match(paths, /TorbenApp\.data-root/u);
  assert.match(paths, /\["shims", "tools"\]/u);
});
