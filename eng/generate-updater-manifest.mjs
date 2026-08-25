import {
  closeSync,
  existsSync,
  fsyncSync,
  lstatSync,
  openSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { basename, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { updaterTargetRequirements } from "./collect-updater-artifacts.mjs";
import { supportedTargets, workspaceVersion } from "./release-metadata.mjs";

const repository = "TorbenXiong/torben-app";

function fail(message) {
  throw new Error(message);
}

function parseJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`Could not parse ${path}: ${error.message}`);
  }
}

function hasExactKeys(value, expected) {
  return (
    value !== null &&
    typeof value === "object" &&
    !Array.isArray(value) &&
    JSON.stringify(Object.keys(value).sort()) === JSON.stringify([...expected].sort())
  );
}

function safeBasename(value) {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    value.length <= 255 &&
    value !== "." &&
    value !== ".." &&
    !/[\\/\0\r\n]/.test(value) &&
    basename(value) === value
  );
}

function validPublishedAt(value) {
  return typeof value === "string" && /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(value);
}

export function validateUpdaterTargetDirectory({ directory, target, version }) {
  const requirements = updaterTargetRequirements[target];
  if (!supportedTargets[target] || !requirements) {
    fail(`Unsupported updater target: ${target}`);
  }
  const expectedVersion = version ?? workspaceVersion().version;
  const root = resolve(directory);
  const metadata = parseJson(join(root, "release-metadata.json"));
  const mapping = parseJson(join(root, "updater-artifacts.json"));
  if (
    metadata.releaseKind !== "official" ||
    metadata.signingStatus !== "signed" ||
    metadata.version !== expectedVersion ||
    metadata.target !== target ||
    !Array.isArray(metadata.artifacts) ||
    !hasExactKeys(mapping, ["schemaVersion", "target", "artifacts"]) ||
    mapping.schemaVersion !== 1 ||
    mapping.target !== target ||
    !Array.isArray(mapping.artifacts) ||
    mapping.artifacts.length !== requirements.length
  ) {
    fail(`Updater target metadata is invalid: ${target}`);
  }
  const expectedPlatforms = new Map(
    requirements.map((requirement) => [requirement.platform, requirement.extension]),
  );
  const targetPlatforms = new Set();
  const metadataPaths = new Set(metadata.artifacts.map((record) => record?.path));
  const records = [];
  for (const record of mapping.artifacts) {
    if (!hasExactKeys(record, ["platform", "artifact", "signature"])) {
      fail(`Updater mapping record has invalid fields: ${target}`);
    }
    const expectedExtension = expectedPlatforms.get(record.platform);
    if (!expectedExtension || targetPlatforms.has(record.platform)) {
      fail(`Updater platform is unexpected or duplicated for ${target}: ${record.platform}`);
    }
    if (
      !safeBasename(record.artifact) ||
      !record.artifact.endsWith(expectedExtension) ||
      !safeBasename(record.signature) ||
      record.signature !== `${record.artifact}.sig`
    ) {
      fail(`Updater mapping file name is unsafe or inconsistent: ${target}`);
    }
    if (!metadataPaths.has(record.artifact) || !metadataPaths.has(record.signature)) {
      fail(`Updater mapping file is absent from signed release metadata: ${target}`);
    }
    const artifactPath = join(root, record.artifact);
    const signaturePath = join(root, record.signature);
    for (const path of [artifactPath, signaturePath]) {
      if (!existsSync(path) || !lstatSync(path).isFile() || lstatSync(path).isSymbolicLink()) {
        fail(`Updater file is missing or invalid: ${path}`);
      }
    }
    targetPlatforms.add(record.platform);
    records.push({ ...record, artifactPath, signaturePath });
  }
  if (targetPlatforms.size !== expectedPlatforms.size) {
    fail(`Updater platforms are incomplete for target: ${target}`);
  }
  return records;
}

function updaterPlatforms(root, version) {
  const platforms = {};
  const artifactNames = new Set();
  for (const target of Object.keys(supportedTargets)) {
    const directory = join(root, target);
    const records = validateUpdaterTargetDirectory({ directory, target, version });
    for (const record of records) {
      if (platforms[record.platform]) fail(`Duplicate updater platform: ${record.platform}`);
      if (artifactNames.has(record.artifact)) {
        fail(`Duplicate updater artifact name would create one ambiguous URL: ${record.artifact}`);
      }
      artifactNames.add(record.artifact);
      platforms[record.platform] = {
        signature: readFileSync(record.signaturePath, "utf8").trim(),
        url: `https://github.com/${repository}/releases/download/v${version}/${encodeURIComponent(basename(record.artifactPath))}`,
      };
    }
  }
  const expectedPlatformCount = Object.values(updaterTargetRequirements).reduce(
    (count, requirements) => count + requirements.length,
    0,
  );
  if (Object.keys(platforms).length !== expectedPlatformCount) {
    fail(`Updater platform set is incomplete: expected ${expectedPlatformCount}.`);
  }
  return Object.fromEntries(
    Object.entries(platforms).sort(([left], [right]) => left.localeCompare(right, "en")),
  );
}

export function verifyUpdaterManifest({ releases, version }) {
  const root = resolve(releases);
  const expectedVersion = version ?? workspaceVersion().version;
  const manifestPath = join(root, "latest.json");
  if (
    !existsSync(manifestPath) ||
    !lstatSync(manifestPath).isFile() ||
    lstatSync(manifestPath).isSymbolicLink()
  ) {
    fail(`Updater manifest must be a regular non-link file: ${manifestPath}`);
  }
  const manifest = parseJson(manifestPath);
  if (
    !hasExactKeys(manifest, ["version", "notes", "pub_date", "platforms"]) ||
    manifest.version !== expectedVersion ||
    typeof manifest.notes !== "string" ||
    !validPublishedAt(manifest.pub_date) ||
    manifest.platforms === null ||
    typeof manifest.platforms !== "object" ||
    Array.isArray(manifest.platforms)
  ) {
    fail("Updater manifest schema, version, or publication timestamp is invalid.");
  }
  const expectedPlatforms = updaterPlatforms(root, expectedVersion);
  const actualPlatformNames = Object.keys(manifest.platforms).sort((left, right) =>
    left.localeCompare(right, "en"),
  );
  if (JSON.stringify(actualPlatformNames) !== JSON.stringify(Object.keys(expectedPlatforms))) {
    fail("Updater manifest platform keys do not match the signed updater mapping.");
  }
  for (const platform of actualPlatformNames) {
    if (
      !hasExactKeys(manifest.platforms[platform], ["signature", "url"]) ||
      manifest.platforms[platform].signature !== expectedPlatforms[platform].signature ||
      manifest.platforms[platform].url !== expectedPlatforms[platform].url
    ) {
      fail(`Updater manifest platform record is invalid: ${platform}`);
    }
  }
  return manifest;
}

export function generateUpdaterManifest({ releases, publishedAt, notes = "" }) {
  const root = resolve(releases);
  if (!validPublishedAt(publishedAt)) {
    fail("--published-at must be an RFC 3339 UTC timestamp without fractional seconds.");
  }
  if (typeof notes !== "string") fail("Updater notes must be a string.");
  const { version } = workspaceVersion();
  const platforms = updaterPlatforms(root, version);
  const manifest = {
    version,
    notes,
    pub_date: publishedAt,
    platforms,
  };
  const destination = join(root, "latest.json");
  const temporaryDestination = `${destination}.next`;
  if (existsSync(destination) || existsSync(temporaryDestination)) {
    fail(`Refusing to overwrite updater manifest: ${destination}`);
  }
  const descriptor = openSync(temporaryDestination, "wx");
  try {
    try {
      writeFileSync(descriptor, `${JSON.stringify(manifest, null, 2)}\n`);
      fsyncSync(descriptor);
    } finally {
      closeSync(descriptor);
    }
    renameSync(temporaryDestination, destination);
  } catch (error) {
    if (existsSync(temporaryDestination)) unlinkSync(temporaryDestination);
    throw error;
  }
  return manifest;
}

function parseArguments(values) {
  const options = { notes: "" };
  for (let index = 0; index < values.length; index += 2) {
    const name = values[index];
    const value = values[index + 1];
    if (!name?.startsWith("--") || value === undefined || value.startsWith("--")) {
      fail(`Invalid command-line argument: ${name ?? "<missing>"}`);
    }
    const key = name.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
    options[key] = value;
  }
  if (!options.releases || !options.publishedAt)
    fail("--releases and --published-at are required.");
  return options;
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  try {
    const manifest = generateUpdaterManifest(parseArguments(process.argv.slice(2)));
    console.log(`Generated latest.json for Torben App ${manifest.version}.`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
