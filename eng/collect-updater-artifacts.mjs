import {
  closeSync,
  constants,
  copyFileSync,
  existsSync,
  fsyncSync,
  lstatSync,
  openSync,
  readdirSync,
  readFileSync,
  readSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { supportedTargets } from "./release-metadata.mjs";

const mappingName = "updater-artifacts.json";

export const updaterTargetRequirements = Object.freeze({
  "x86_64-pc-windows-msvc": [
    { directory: "nsis", extension: ".exe", platform: "windows-x86_64-nsis" },
    { directory: "msi", extension: ".msi", platform: "windows-x86_64-msi" },
  ],
  "aarch64-pc-windows-msvc": [
    { directory: "nsis", extension: ".exe", platform: "windows-aarch64-nsis" },
    { directory: "msi", extension: ".msi", platform: "windows-aarch64-msi" },
  ],
  "x86_64-apple-darwin": [
    { directory: "macos", extension: ".app.tar.gz", platform: "darwin-x86_64-app" },
  ],
  "aarch64-apple-darwin": [
    { directory: "macos", extension: ".app.tar.gz", platform: "darwin-aarch64-app" },
  ],
  "x86_64-unknown-linux-gnu": [
    { directory: "appimage", extension: ".AppImage", platform: "linux-x86_64-appimage" },
    { directory: "deb", extension: ".deb", platform: "linux-x86_64-deb" },
    { directory: "rpm", extension: ".rpm", platform: "linux-x86_64-rpm" },
  ],
  "aarch64-unknown-linux-gnu": [
    { directory: "appimage", extension: ".AppImage", platform: "linux-aarch64-appimage" },
    { directory: "deb", extension: ".deb", platform: "linux-aarch64-deb" },
    { directory: "rpm", extension: ".rpm", platform: "linux-aarch64-rpm" },
  ],
});

function fail(message) {
  throw new Error(message);
}

function regularFile(path, description) {
  if (!existsSync(path)) fail(`${description} is missing: ${path}`);
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-link file: ${path}`);
  }
  return metadata;
}

function filesAreEqual(left, right) {
  const leftMetadata = regularFile(left, "Updater source artifact");
  const rightMetadata = regularFile(right, "Existing release artifact");
  if (leftMetadata.size !== rightMetadata.size) return false;
  const leftDescriptor = openSync(left, "r");
  const rightDescriptor = openSync(right, "r");
  const leftBuffer = Buffer.allocUnsafe(1024 * 1024);
  const rightBuffer = Buffer.allocUnsafe(1024 * 1024);
  try {
    let position = 0;
    while (position < leftMetadata.size) {
      const length = Math.min(leftBuffer.length, leftMetadata.size - position);
      const leftRead = readSync(leftDescriptor, leftBuffer, 0, length, position);
      const rightRead = readSync(rightDescriptor, rightBuffer, 0, length, position);
      if (
        leftRead !== length ||
        rightRead !== length ||
        !leftBuffer.subarray(0, length).equals(rightBuffer.subarray(0, length))
      ) {
        return false;
      }
      position += length;
    }
    return true;
  } finally {
    closeSync(leftDescriptor);
    closeSync(rightDescriptor);
  }
}

function updaterArtifactName(artifact, requirement, target) {
  const name = basename(artifact);
  if (requirement.directory !== "macos") return name;
  const stem = name.slice(0, -requirement.extension.length);
  return `${stem}-${target}${requirement.extension}`;
}

function oneFile(directory, suffix, description) {
  if (!existsSync(directory) || !lstatSync(directory).isDirectory()) {
    fail(`${description} directory is missing: ${directory}`);
  }
  const matches = readdirSync(directory)
    .filter((name) => name.endsWith(suffix))
    .sort((left, right) => left.localeCompare(right, "en"));
  if (matches.length !== 1) {
    fail(`Expected exactly one ${description}, found ${matches.length} in ${directory}.`);
  }
  const path = join(directory, matches[0]);
  regularFile(path, description);
  return path;
}

function validateEncodedSignature(path) {
  const encoded = readFileSync(path, "utf8").trim();
  let decoded;
  try {
    decoded = Buffer.from(encoded, "base64").toString("utf8");
  } catch {
    fail(`Updater signature is not valid Base64: ${path}`);
  }
  if (
    Buffer.from(decoded, "utf8").toString("base64") !== encoded ||
    !decoded.startsWith("untrusted comment: ") ||
    !decoded.includes("\ntrusted comment: ")
  ) {
    fail(`Updater signature does not contain an encoded minisign signature: ${path}`);
  }
}

export function collectUpdaterArtifacts({ bundleRoot, output, target }) {
  if (!supportedTargets[target] || !updaterTargetRequirements[target]) {
    fail(`Unsupported updater target: ${target}`);
  }
  const sourceRoot = resolve(bundleRoot);
  const outputRoot = resolve(output);
  if (
    !existsSync(outputRoot) ||
    !lstatSync(outputRoot).isDirectory() ||
    lstatSync(outputRoot).isSymbolicLink()
  ) {
    fail(`Release output directory is missing: ${outputRoot}`);
  }
  const mappingPath = join(outputRoot, mappingName);
  if (existsSync(mappingPath)) {
    fail(`Refusing to overwrite updater mapping: ${mappingPath}`);
  }
  const mappingTemporaryPath = `${mappingPath}.next`;
  if (existsSync(mappingTemporaryPath)) {
    fail(`Refusing to overwrite temporary updater mapping: ${mappingTemporaryPath}`);
  }
  const candidates = updaterTargetRequirements[target].map((requirement) => {
    const directory = join(sourceRoot, requirement.directory);
    const artifact = oneFile(directory, requirement.extension, `${requirement.platform} artifact`);
    const signature = `${artifact}.sig`;
    regularFile(signature, `${requirement.platform} signature`);
    validateEncodedSignature(signature);

    const artifactDestination = join(
      outputRoot,
      updaterArtifactName(artifact, requirement, target),
    );
    if (existsSync(artifactDestination)) {
      if (!filesAreEqual(artifact, artifactDestination)) {
        fail(`Existing release artifact differs from updater artifact: ${artifactDestination}`);
      }
    }
    const signatureDestination = `${artifactDestination}.sig`;
    if (existsSync(signatureDestination)) {
      fail(`Refusing to overwrite updater signature: ${signatureDestination}`);
    }
    return { requirement, artifact, signature, artifactDestination, signatureDestination };
  });
  const created = [];
  try {
    const records = [];
    for (const candidate of candidates) {
      if (!existsSync(candidate.artifactDestination)) {
        copyFileSync(candidate.artifact, candidate.artifactDestination, constants.COPYFILE_EXCL);
        created.push(candidate.artifactDestination);
      }
      copyFileSync(candidate.signature, candidate.signatureDestination, constants.COPYFILE_EXCL);
      created.push(candidate.signatureDestination);
      records.push({
        platform: candidate.requirement.platform,
        artifact: basename(candidate.artifactDestination),
        signature: basename(candidate.signatureDestination),
      });
    }
    const mapping = { schemaVersion: 1, target, artifacts: records };
    const descriptor = openSync(mappingTemporaryPath, "wx");
    try {
      writeFileSync(descriptor, `${JSON.stringify(mapping, null, 2)}\n`);
      fsyncSync(descriptor);
    } finally {
      closeSync(descriptor);
    }
    renameSync(mappingTemporaryPath, mappingPath);
    return mapping;
  } catch (error) {
    if (existsSync(mappingTemporaryPath)) unlinkSync(mappingTemporaryPath);
    for (const path of created.reverse()) {
      if (existsSync(path)) unlinkSync(path);
    }
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
    if (Object.hasOwn(options, key)) fail(`Duplicate command-line option: ${name}`);
    options[key] = value;
  }
  for (const key of ["bundleRoot", "output", "target"]) {
    if (!options[key]) fail(`Missing required option: ${key}`);
  }
  return options;
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  try {
    const options = parseArguments(process.argv.slice(2));
    const mapping = collectUpdaterArtifacts(options);
    console.log(
      `Collected ${mapping.artifacts.length} signed updater artifacts for ${mapping.target}.`,
    );
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
