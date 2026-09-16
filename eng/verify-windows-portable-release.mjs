import { createHash } from "node:crypto";
import { existsSync, lstatSync, readdirSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { detectExecutableTarget } from "./collect-release-artifacts.mjs";

const executableName = "TorbenApp.exe";

function fail(message) {
  throw new Error(message);
}

function parseArguments(values) {
  const options = {};
  for (let index = 0; index < values.length; index += 2) {
    const name = values[index];
    const value = values[index + 1];
    if (!name?.startsWith("--") || value === undefined || value.startsWith("--")) {
      fail(`Invalid command-line argument: ${name ?? "<missing>"}`);
    }
    const key = name.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
    if (Object.hasOwn(options, key)) fail(`Duplicate command-line option: ${name}`);
    options[key] = value;
  }
  if (!options.directory) fail("--directory is required.");
  return options;
}

export function verifyWindowsPortableRelease({ directory, expectedSha256 }) {
  const root = resolve(directory);
  if (!existsSync(root)) fail(`Portable release directory is missing: ${root}`);
  const rootMetadata = lstatSync(root);
  if (!rootMetadata.isDirectory() || rootMetadata.isSymbolicLink()) {
    fail(`Portable release root must be a regular directory: ${root}`);
  }

  const entries = readdirSync(root, { withFileTypes: true });
  if (
    entries.length !== 1 ||
    entries[0].name !== executableName ||
    !entries[0].isFile() ||
    entries[0].isSymbolicLink()
  ) {
    fail(`Portable release must contain only ${executableName}.`);
  }

  const executable = resolve(root, executableName);
  const metadata = lstatSync(executable);
  if (!metadata.isFile() || metadata.isSymbolicLink() || metadata.size === 0) {
    fail(`${executableName} must be a non-empty regular file.`);
  }
  const target = detectExecutableTarget(executable);
  if (target !== "x86_64-pc-windows-msvc") {
    fail(`Portable executable target ${target} is not Windows x64.`);
  }
  const sha256 = createHash("sha256").update(readFileSync(executable)).digest("hex").toUpperCase();
  if (expectedSha256 && sha256 !== expectedSha256.trim().toUpperCase()) {
    fail(`Portable executable SHA-256 ${sha256} does not match ${expectedSha256}.`);
  }
  return { executable, sha256, size: metadata.size, target };
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  const result = verifyWindowsPortableRelease({
    directory: options.directory,
    expectedSha256: options.expectedSha256,
  });
  console.log(JSON.stringify(result));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) main();
