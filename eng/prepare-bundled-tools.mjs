import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { detectExecutableTarget } from "./collect-release-artifacts.mjs";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = dirname(scriptDirectory);
const release = process.argv.includes("--release");
const profile = release ? "release" : "debug";
const rustcDetails = execFileSync("rustc", ["-vV"], {
  cwd: repositoryRoot,
  encoding: "utf8",
});
const target = rustcDetails.match(/^host: (.+)$/m)?.[1];

if (!target) {
  throw new Error("Could not determine the active Rust host target.");
}

const tools = [
  "torben-plugin-node",
  "torben-plugin-temurin",
  "torben-plugin-python",
  "torben-plugin-git",
  "torben-plugin-vscode",
  "torben-plugin-codex",
  "torben-shim",
];
const cargoArguments = ["build", "--locked"];
for (const tool of tools) {
  cargoArguments.push("-p", tool);
}
if (release) {
  cargoArguments.push("--release");
}
execFileSync("cargo", cargoArguments, {
  cwd: repositoryRoot,
  stdio: "inherit",
});

const extension = process.platform === "win32" ? ".exe" : "";
for (const tool of tools) {
  const source = join(repositoryRoot, "target", profile, `${tool}${extension}`);
  const detectedTarget = detectExecutableTarget(source);
  if (detectedTarget !== target) {
    throw new Error(`${tool} target ${detectedTarget} does not match Rust host ${target}.`);
  }
  const destination = join(
    repositoryRoot,
    "apps",
    "desktop",
    "src-tauri",
    "binaries",
    `${tool}-${target}${extension}`,
  );
  mkdirSync(dirname(destination), { recursive: true });
  copyFileSync(source, destination);
  if (process.platform !== "win32") {
    chmodSync(destination, 0o755);
  }
  console.log(`Prepared bundled tool: ${destination}`);
}
