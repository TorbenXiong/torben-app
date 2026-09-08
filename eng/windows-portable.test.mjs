import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));

test("Windows desktop manifest requests administrator privileges", () => {
  const manifest = readFileSync(
    join(repositoryRoot, "apps", "desktop", "src-tauri", "windows-app-manifest.xml"),
    "utf8",
  );
  assert.match(manifest, /requestedExecutionLevel level="requireAdministrator"/u);
  assert.match(manifest, /Microsoft\.Windows\.Common-Controls/u);
});

test("Windows portable builder emits only the desktop executable with embedded provider payload", () => {
  const script = readFileSync(join(repositoryRoot, "eng", "build-windows-portable.mjs"), "utf8");
  assert.match(script, /--no-bundle/u);
  assert.match(script, /TorbenApp\.exe/u);
  assert.match(script, /torben-desktop\.exe/u);
  assert.match(script, /embedded/u);
  assert.doesNotMatch(script, /userData[\\/]+plugins/u);
  assert.doesNotMatch(script, /torben-plugin-(?:node|python|git|vscode|codex)/u);
  assert.doesNotMatch(script, /copyFileSync\(join\(releaseRoot, "torben\.exe"/u);
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
