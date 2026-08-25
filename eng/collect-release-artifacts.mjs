import {
  chmodSync,
  constants,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
} from "node:fs";
import { basename, dirname, extname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { supportedTargets, workspaceVersion } from "./release-metadata.mjs";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const defaultRepositoryRoot = dirname(scriptDirectory);

export const requiredPackages = Object.freeze({
  windows: [
    { format: "nsis", directory: "nsis", extension: ".exe" },
    { format: "msi", directory: "msi", extension: ".msi" },
  ],
  macos: [{ format: "dmg", directory: "dmg", extension: ".dmg" }],
  linux: [
    { format: "appimage", directory: "appimage", extension: ".AppImage" },
    { format: "deb", directory: "deb", extension: ".deb" },
    { format: "rpm", directory: "rpm", extension: ".rpm" },
  ],
});

function fail(message) {
  throw new Error(message);
}

function safeRelative(root, path) {
  const value = relative(root, path);
  if (!value || value === ".." || value.startsWith(`..${sep}`)) {
    fail(`Package path escapes its bundle root: ${path}`);
  }
  return value.split(sep).join("/");
}

function regularFile(path, description) {
  if (!existsSync(path)) {
    fail(`${description} is missing: ${path}`);
  }
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-link file: ${path}`);
  }
  return metadata;
}

function packageFiles(root, requirement) {
  const directory = join(root, requirement.directory);
  if (!existsSync(directory) || !lstatSync(directory).isDirectory()) {
    fail(`Required ${requirement.format} bundle directory is missing: ${directory}`);
  }
  const matches = [];
  const visit = (current) => {
    for (const entry of readdirSync(current, { withFileTypes: true })) {
      const path = join(current, entry.name);
      if (entry.isSymbolicLink()) {
        fail(`Bundle directories cannot contain symbolic links: ${safeRelative(root, path)}`);
      }
      if (entry.isDirectory()) {
        visit(path);
      } else if (entry.isFile()) {
        if (extname(entry.name) === requirement.extension) {
          matches.push(path);
        }
      } else {
        fail(`Bundle entries must be regular files: ${safeRelative(root, path)}`);
      }
    }
  };
  visit(directory);
  if (matches.length !== 1) {
    fail(
      `Expected exactly one ${requirement.format} package, found ${matches.length} in ${directory}.`,
    );
  }
  regularFile(matches[0], `${requirement.format} package`);
  return matches[0];
}

export function detectExecutableTarget(path) {
  regularFile(path, "CLI executable");
  const bytes = readFileSync(path);
  if (bytes.length >= 0x40 && bytes[0] === 0x4d && bytes[1] === 0x5a) {
    const header = bytes.readUInt32LE(0x3c);
    if (header + 6 > bytes.length || bytes.toString("ascii", header, header + 4) !== "PE\0\0") {
      fail("CLI executable has a malformed PE header.");
    }
    const machine = bytes.readUInt16LE(header + 4);
    if (machine === 0x8664) return "x86_64-pc-windows-msvc";
    if (machine === 0xaa64) return "aarch64-pc-windows-msvc";
    fail(`CLI executable has an unsupported PE machine type: 0x${machine.toString(16)}.`);
  }
  if (
    bytes.length >= 20 &&
    bytes[0] === 0x7f &&
    bytes[1] === 0x45 &&
    bytes[2] === 0x4c &&
    bytes[3] === 0x46
  ) {
    if (bytes[4] !== 2 || bytes[5] !== 1) {
      fail("CLI executable must be a little-endian 64-bit ELF binary.");
    }
    const machine = bytes.readUInt16LE(18);
    if (machine === 0x3e) return "x86_64-unknown-linux-gnu";
    if (machine === 0xb7) return "aarch64-unknown-linux-gnu";
    fail(`CLI executable has an unsupported ELF machine type: 0x${machine.toString(16)}.`);
  }
  if (bytes.length >= 8) {
    const littleEndian = bytes.readUInt32LE(0) === 0xfeedfacf;
    const bigEndian = bytes.readUInt32BE(0) === 0xfeedfacf;
    if (littleEndian || bigEndian) {
      const cpu = littleEndian ? bytes.readUInt32LE(4) : bytes.readUInt32BE(4);
      if (cpu === 0x01000007) return "x86_64-apple-darwin";
      if (cpu === 0x0100000c) return "aarch64-apple-darwin";
      fail(`CLI executable has an unsupported Mach-O CPU type: 0x${cpu.toString(16)}.`);
    }
  }
  fail("CLI executable is not a supported PE, ELF, or thin 64-bit Mach-O binary.");
}

function pathIsWithin(root, candidate) {
  const value = relative(root, candidate);
  return value === "" || (!isAbsolute(value) && value !== ".." && !value.startsWith(`..${sep}`));
}

function requireNewOutput(outputRoot, sourceRoot) {
  if (pathIsWithin(sourceRoot, outputRoot)) {
    fail("Release output must be outside the Tauri bundle directory.");
  }
  for (const path of [outputRoot, `${outputRoot}.next`]) {
    if (existsSync(path)) {
      fail(`Refusing to overwrite existing release output: ${path}`);
    }
  }
}

export function collectReleaseArtifacts({
  bundleRoot,
  cliBinary,
  output,
  target,
  repositoryRoot = defaultRepositoryRoot,
}) {
  const targetDetails = supportedTargets[target];
  if (!targetDetails) {
    fail(`Unsupported release target: ${target}`);
  }
  const sourceRoot = resolve(bundleRoot);
  if (!existsSync(sourceRoot) || !lstatSync(sourceRoot).isDirectory()) {
    fail(`Tauri bundle directory is missing: ${sourceRoot}`);
  }
  const cliPath = resolve(cliBinary);
  const cliMetadata = regularFile(cliPath, "CLI executable");
  const detectedTarget = detectExecutableTarget(cliPath);
  if (detectedTarget !== target) {
    fail(`CLI target ${detectedTarget} does not match requested release target ${target}.`);
  }
  const packages = requiredPackages[targetDetails.operatingSystem].map((requirement) => ({
    format: requirement.format,
    source: packageFiles(sourceRoot, requirement),
  }));
  const outputRoot = resolve(output);
  requireNewOutput(outputRoot, sourceRoot);
  const stagingRoot = `${outputRoot}.next`;
  mkdirSync(dirname(outputRoot), { recursive: true });
  mkdirSync(stagingRoot);
  const { version } = workspaceVersion(repositoryRoot);
  const cliExtension = targetDetails.operatingSystem === "windows" ? ".exe" : "";
  const cliName = `torben-${version}-${target}${cliExtension}`;
  const copied = [];
  try {
    for (const packageFile of packages) {
      const name = basename(packageFile.source);
      copyFileSync(packageFile.source, join(stagingRoot, name), constants.COPYFILE_EXCL);
      copied.push({ format: packageFile.format, path: join(outputRoot, name) });
    }
    const stagedCli = join(stagingRoot, cliName);
    copyFileSync(cliPath, stagedCli, constants.COPYFILE_EXCL);
    if (targetDetails.operatingSystem !== "windows") {
      chmodSync(stagedCli, cliMetadata.mode & 0o777);
    }
    copied.push({ format: "cli", path: join(outputRoot, cliName) });
    renameSync(stagingRoot, outputRoot);
    return copied;
  } catch (error) {
    rmSync(stagingRoot, { recursive: true, force: true });
    throw error;
  }
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
    if (Object.hasOwn(options, key)) {
      fail(`Duplicate command-line option: ${name}`);
    }
    options[key] = value;
  }
  for (const key of ["bundleRoot", "cliBinary", "output", "target"]) {
    if (!options[key]) {
      fail(`--${key.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`)} is required.`);
    }
  }
  return options;
}

function main() {
  const options = parseArguments(process.argv.slice(2));
  const copied = collectReleaseArtifacts(options);
  console.log(`Collected ${copied.length} release artifacts for ${options.target}.`);
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  try {
    main();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
