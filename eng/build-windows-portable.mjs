import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { verifyPythonManagerPackage } from "./prepare-python-manager.mjs";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = dirname(scriptDirectory);
const desktopRoot = join(repositoryRoot, "apps", "desktop");
const releaseRoot = join(repositoryRoot, "target", "release");
const outputRoot = join(repositoryRoot, "artifacts", "torben-app-portable-windows-x64");
const packageManager = process.platform === "win32" ? (process.env.ComSpec ?? "cmd.exe") : "pnpm";

function packageManagerArgs(args) {
  return process.platform === "win32" ? ["/d", "/s", "/c", "pnpm.cmd", ...args] : args;
}

if (process.platform !== "win32") {
  throw new Error("The Windows portable bundle can only be built on Windows.");
}

verifyPythonManagerPackage();

execFileSync(
  "cargo",
  [
    "build",
    "--release",
    "--locked",
    "-p",
    "torben-plugin-temurin",
    "-p",
    "torben-plugin-python",
    "-p",
    "torben-shim",
  ],
  {
    cwd: repositoryRoot,
    stdio: "inherit",
  },
);
execFileSync(
  packageManager,
  packageManagerArgs([
    "exec",
    "tauri",
    "build",
    "--ci",
    "--no-bundle",
    "--config",
    "src-tauri/tauri.bundle.conf.json",
    "--",
    "--locked",
  ]),
  {
    cwd: desktopRoot,
    stdio: "inherit",
  },
);

rmSync(outputRoot, { recursive: true, force: true });
mkdirSync(outputRoot, { recursive: true });
copyFileSync(join(releaseRoot, "torben-desktop.exe"), join(outputRoot, "TorbenApp.exe"));
// The providers, Python Install Manager package, and shim are embedded into TorbenApp.exe. The
// first run therefore contains no plugin or runtime payload; enabling Temurin or Python installs
// the verified embedded package into userData.
console.log(`Portable bundle written to ${outputRoot}`);
