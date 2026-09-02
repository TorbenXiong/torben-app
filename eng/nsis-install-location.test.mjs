import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const tauriConfig = JSON.parse(
  readFileSync(join(root, "apps", "desktop", "src-tauri", "tauri.conf.json"), "utf8"),
);
const hookPath = tauriConfig.bundle.windows.nsis.installerHooks;
const hook = readFileSync(join(root, "apps", "desktop", "src-tauri", hookPath), "utf8");

test("NSIS defaults fresh installs to the first fixed non-system drive from D through Z", () => {
  assert.equal(hookPath, "windows/nsis-hooks.nsh");
  assert.equal(tauriConfig.bundle.windows.nsis.installMode, "currentUser");
  assert.match(hook, /ReadEnvStr \$R1 "SystemDrive"/u);
  assert.match(hook, /GetDriveTypeW/u);
  assert.match(hook, /\$0 = 3 ; DRIVE_FIXED/u);
  assert.match(hook, /StrCpy \$R3 "DEFGHIJKLMNOPQRSTUVWXYZ"/u);
  assert.match(hook, /StrCpy \$R4 \$R3 1/u);
  assert.match(hook, /StrCpy \$INSTDIR "\$R4:\\TorbenApp"/u);
});

test("NSIS preserves upgrades and explicit paths and also covers silent installs", () => {
  assert.match(
    hook,
    /ReadRegStr \$R0 HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Torben App" "UninstallString"/u,
  );
  assert.match(hook, /\$\{GetOptions\} \$CMDLINE "\/D=" \$R2/u);
  assert.match(hook, /StrCpy \$INSTDIR "\$LOCALAPPDATA\\Torben App"/u);
  assert.doesNotMatch(hook, /ReadRegStr \$R0 HKCU "Software\\github\\Torben App"/u);
  assert.match(hook, /!define MUI_CUSTOMFUNCTION_GUIINIT TorbenSetDefaultInstallDirectory/u);
  assert.match(hook, /!macro NSIS_HOOK_PREINSTALL[\s\S]*SetOutPath \$INSTDIR/u);
});
