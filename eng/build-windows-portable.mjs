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
const nodeExecutable = process.execPath;
const tauriCli = join(desktopRoot, "node_modules", "@tauri-apps", "cli", "tauri.js");
const typescriptCli = join(desktopRoot, "node_modules", "typescript", "bin", "tsc");
const viteCli = join(desktopRoot, "node_modules", "vite", "bin", "vite.js");

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
    "--offline",
    "-p",
    "torben-plugin-node",
    "-p",
    "torben-plugin-temurin",
    "-p",
    "torben-plugin-python",
    "-p",
    "torben-plugin-rust",
    "-p",
    "torben-plugin-mysql",
    "-p",
    "torben-plugin-redis",
    "-p",
    "torben-plugin-postgresql",
    "-p",
    "torben-shim",
  ],
  {
    cwd: repositoryRoot,
    stdio: "inherit",
  },
);
execFileSync(nodeExecutable, [typescriptCli, "--noEmit"], {
  cwd: desktopRoot,
  stdio: "inherit",
});
execFileSync(nodeExecutable, [viteCli, "build"], {
  cwd: desktopRoot,
  stdio: "inherit",
});
execFileSync(
  nodeExecutable,
  [
    tauriCli,
    "build",
    "--ci",
    "--no-bundle",
    "--config",
    "src-tauri/tauri.bundle.conf.json",
    "--config",
    '{"build":{"beforeBuildCommand":null}}',
    "--",
    "--locked",
    "--offline",
  ],
  {
    cwd: desktopRoot,
    stdio: "inherit",
  },
);

rmSync(outputRoot, { recursive: true, force: true });
mkdirSync(outputRoot, { recursive: true });
copyFileSync(join(releaseRoot, "torben-desktop.exe"), join(outputRoot, "TorbenApp.exe"));
// The providers, Python Install Manager package, and shim are embedded into TorbenApp.exe. The
// first run therefore contains no plugin or runtime payload; enabling Node.js, Temurin, Python,
// Rust, MySQL, Redis, or PostgreSQL installs
// the verified embedded package into userData.
console.log(`Portable bundle written to ${outputRoot}`);
