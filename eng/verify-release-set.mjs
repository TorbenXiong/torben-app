import { createHash } from "node:crypto";
import {
  closeSync,
  createReadStream,
  existsSync,
  fsyncSync,
  lstatSync,
  openSync,
  readdirSync,
  readFileSync,
  renameSync,
  unlinkSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { verifyUpdaterManifest } from "./generate-updater-manifest.mjs";
import {
  officialReleaseTargets,
  supportedTargets,
  verifyReleaseMetadata,
} from "./release-metadata.mjs";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const defaultRepositoryRoot = dirname(scriptDirectory);
const indexName = "release-index.json";
const checksumsName = "SHA256SUMS";
const updaterManifestName = "latest.json";
const developmentTargetOrder = Object.keys(supportedTargets);
const comparePaths = (left, right) => left.localeCompare(right, "en");

function fail(message) {
  throw new Error(message);
}

function regularFile(path, description) {
  if (!existsSync(path)) {
    fail(`${description} is missing: ${path}`);
  }
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-link file: ${path}`);
  }
}

function releaseDirectories(root, allowMetadataFiles) {
  if (!existsSync(root)) {
    fail(`Release set directory is missing: ${root}`);
  }
  const rootMetadata = lstatSync(root);
  if (!rootMetadata.isDirectory() || rootMetadata.isSymbolicLink()) {
    fail(`Release set must be a regular directory: ${root}`);
  }
  const directories = [];
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const path = join(root, entry.name);
    if (entry.isSymbolicLink()) {
      fail(`Release set cannot contain symbolic links: ${entry.name}`);
    }
    if (entry.isDirectory()) {
      if (!/^[0-9A-Za-z._-]+$/.test(entry.name)) {
        fail(`Release target directory has an unsafe name: ${entry.name}`);
      }
      directories.push({ name: entry.name, path });
    } else if (
      !allowMetadataFiles ||
      !entry.isFile() ||
      ![indexName, checksumsName, updaterManifestName].includes(entry.name)
    ) {
      fail(`Unexpected release-set root entry: ${entry.name}`);
    }
  }
  return directories.sort((left, right) => comparePaths(left.name, right.name));
}

function listRegularFiles(root) {
  const files = [];
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) {
        fail(`Release set cannot contain symbolic links: ${relative(root, path)}`);
      }
      if (entry.isDirectory()) {
        visit(path);
      } else if (entry.isFile()) {
        files.push({
          absolute: path,
          relative: relative(root, path).split(sep).join("/"),
        });
      } else {
        fail(`Release set entries must be regular files: ${relative(root, path)}`);
      }
    }
  };
  visit(root);
  return files.sort((left, right) => comparePaths(left.relative, right.relative));
}

async function sha256(path) {
  const hash = createHash("sha256");
  await new Promise((accept, reject) => {
    const stream = createReadStream(path);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("error", reject);
    stream.on("end", accept);
  });
  return hash.digest("hex");
}

function sha256Content(content) {
  return createHash("sha256").update(content).digest("hex");
}

function requireNewMetadataPaths(paths) {
  for (const path of paths) {
    if (existsSync(path) || existsSync(`${path}.next`)) {
      fail(`Refusing to overwrite existing release-set metadata: ${path}`);
    }
  }
}

function writeTemporary(path, content) {
  const next = `${path}.next`;
  const descriptor = openSync(next, "wx");
  try {
    try {
      writeFileSync(descriptor, content);
      fsyncSync(descriptor);
    } finally {
      closeSync(descriptor);
    }
    return next;
  } catch (error) {
    removeIfPresent(next);
    throw error;
  }
}

function removeIfPresent(path) {
  if (existsSync(path)) unlinkSync(path);
}

function parseJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`Could not parse ${path}: ${error.message}`);
  }
}

function parseChecksums(path) {
  const records = new Map();
  for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
    if (!line) continue;
    const match = line.match(/^([0-9a-f]{64}) {2}([^\r\n]+)$/);
    if (!match || records.has(match[2])) {
      fail(`Malformed or duplicate release-set checksum entry: ${line}`);
    }
    records.set(match[2], match[1]);
  }
  return records;
}

function assertCompleteTargets(records, targetOrder) {
  const actual = records.map((record) => record.metadata.target).sort(comparePaths);
  const expected = [...targetOrder].sort(comparePaths);
  if (JSON.stringify(actual) !== JSON.stringify(expected)) {
    fail(`Release set targets are incomplete or duplicated: ${actual.join(", ")}`);
  }
  for (const record of records) {
    if (record.directory !== record.metadata.target) {
      fail(`Release directory ${record.directory} must match target ${record.metadata.target}.`);
    }
  }
}

function assertSharedIdentity(records) {
  const first = records[0]?.metadata;
  if (!first) {
    fail("Release set contains no target directories.");
  }
  for (const record of records.slice(1)) {
    const metadata = record.metadata;
    if (
      metadata.version !== first.version ||
      metadata.sourceRevision !== first.sourceRevision ||
      metadata.sourceRef !== first.sourceRef ||
      metadata.releaseKind !== first.releaseKind
    ) {
      fail("Release targets do not share one version, revision, ref, and release kind.");
    }
  }
  return first;
}

async function verifiedTargets(root, allowMetadataFiles, repositoryRoot) {
  const directories = releaseDirectories(root, allowMetadataFiles);
  const records = [];
  for (const directory of directories) {
    records.push({
      directory: directory.name,
      path: directory.path,
      metadata: await verifyReleaseMetadata({
        artifacts: directory.path,
        repositoryRoot,
      }),
    });
  }
  const identity = assertSharedIdentity(records);
  const targetOrder =
    identity.releaseKind === "official" ? officialReleaseTargets : developmentTargetOrder;
  assertCompleteTargets(records, targetOrder);
  return { records, identity, targetOrder };
}

function verifyUpdaterPolicy(root, identity) {
  const manifestPath = join(root, updaterManifestName);
  if (identity.releaseKind === "official") {
    regularFile(manifestPath, "Official updater manifest");
    verifyUpdaterManifest({ releases: root, version: identity.version });
  } else if (existsSync(manifestPath)) {
    fail("Development release sets cannot contain latest.json.");
  }
}

export async function createReleaseSet({ releases, repositoryRoot = defaultRepositoryRoot }) {
  const root = resolve(releases);
  const indexPath = join(root, indexName);
  const checksumsPath = join(root, checksumsName);
  requireNewMetadataPaths([indexPath, checksumsPath]);
  const { records, identity, targetOrder } = await verifiedTargets(root, true, repositoryRoot);
  verifyUpdaterPolicy(root, identity);
  const targetRecords = [];
  for (const target of targetOrder) {
    const record = records.find((candidate) => candidate.metadata.target === target);
    targetRecords.push({
      target,
      operatingSystem: record.metadata.operatingSystem,
      architecture: record.metadata.architecture,
      directory: record.directory,
      signingStatus: record.metadata.signingStatus,
      artifactCount: record.metadata.artifacts.length,
      metadataSha256: await sha256(join(record.path, "release-metadata.json")),
      checksumsSha256: await sha256(join(record.path, "SHA256SUMS")),
    });
  }
  const index = {
    schemaVersion: 1,
    productName: "Torben App",
    applicationId: "io.github.torbenxiong.torbenapp",
    version: identity.version,
    sourceRevision: identity.sourceRevision,
    sourceRef: identity.sourceRef,
    releaseKind: identity.releaseKind,
    targets: targetRecords,
  };
  const indexContent = `${JSON.stringify(index, null, 2)}\n`;
  const files = listRegularFiles(root).filter(
    (file) => ![indexName, checksumsName].includes(file.relative),
  );
  const checksumRecords = [];
  for (const file of files) {
    checksumRecords.push({ digest: await sha256(file.absolute), path: file.relative });
  }
  checksumRecords.push({ digest: sha256Content(indexContent), path: indexName });
  checksumRecords.sort((left, right) => comparePaths(left.path, right.path));
  const checksumsContent = `${checksumRecords
    .map((record) => `${record.digest}  ${record.path}`)
    .join("\n")}\n`;
  let indexTemporaryPath;
  let checksumsTemporaryPath;
  let indexCommitted = false;
  let checksumsCommitted = false;
  try {
    indexTemporaryPath = writeTemporary(indexPath, indexContent);
    checksumsTemporaryPath = writeTemporary(checksumsPath, checksumsContent);
    renameSync(indexTemporaryPath, indexPath);
    indexCommitted = true;
    renameSync(checksumsTemporaryPath, checksumsPath);
    checksumsCommitted = true;
  } catch (error) {
    if (indexTemporaryPath) removeIfPresent(indexTemporaryPath);
    if (checksumsTemporaryPath) removeIfPresent(checksumsTemporaryPath);
    if (indexCommitted) removeIfPresent(indexPath);
    if (checksumsCommitted) removeIfPresent(checksumsPath);
    throw error;
  }
  return index;
}

export async function verifyReleaseSet({ releases, repositoryRoot = defaultRepositoryRoot }) {
  const root = resolve(releases);
  const indexPath = join(root, indexName);
  const checksumsPath = join(root, checksumsName);
  regularFile(indexPath, "release-index.json");
  regularFile(checksumsPath, "release-set SHA256SUMS");
  const index = parseJson(indexPath);
  if (
    index.schemaVersion !== 1 ||
    index.productName !== "Torben App" ||
    index.applicationId !== "io.github.torbenxiong.torbenapp" ||
    !Array.isArray(index.targets)
  ) {
    fail("Release index schema or product identity is invalid.");
  }
  const { records, identity, targetOrder } = await verifiedTargets(root, true, repositoryRoot);
  verifyUpdaterPolicy(root, identity);
  if (
    index.version !== identity.version ||
    index.sourceRevision !== identity.sourceRevision ||
    index.sourceRef !== identity.sourceRef ||
    index.releaseKind !== identity.releaseKind ||
    index.targets.length !== targetOrder.length
  ) {
    fail("Release index does not match the verified target identity.");
  }
  for (let indexPosition = 0; indexPosition < targetOrder.length; indexPosition += 1) {
    const target = targetOrder[indexPosition];
    const indexed = index.targets[indexPosition];
    const record = records.find((candidate) => candidate.metadata.target === target);
    if (
      indexed.target !== target ||
      indexed.operatingSystem !== record.metadata.operatingSystem ||
      indexed.architecture !== record.metadata.architecture ||
      indexed.directory !== record.directory ||
      indexed.signingStatus !== record.metadata.signingStatus ||
      indexed.artifactCount !== record.metadata.artifacts.length ||
      indexed.metadataSha256 !== (await sha256(join(record.path, "release-metadata.json"))) ||
      indexed.checksumsSha256 !== (await sha256(join(record.path, "SHA256SUMS")))
    ) {
      fail(`Release index target record is invalid: ${target}`);
    }
  }
  const files = listRegularFiles(root).filter((file) => file.relative !== checksumsName);
  const checksums = parseChecksums(checksumsPath);
  if (
    JSON.stringify([...checksums.keys()].sort(comparePaths)) !==
    JSON.stringify(files.map((file) => file.relative))
  ) {
    fail("Release-set SHA256SUMS entries do not match its files.");
  }
  for (const file of files) {
    if (checksums.get(file.relative) !== (await sha256(file.absolute))) {
      fail(`Release-set checksum failed verification: ${file.relative}`);
    }
  }
  return index;
}

function parseArguments(values) {
  const [command, flag, releases, ...extra] = values;
  if (
    !["create", "verify"].includes(command) ||
    flag !== "--releases" ||
    !releases ||
    extra.length !== 0
  ) {
    fail("Usage: verify-release-set.mjs <create|verify> --releases <directory>");
  }
  return { command, releases };
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  const result =
    options.command === "create"
      ? await createReleaseSet(options)
      : await verifyReleaseSet(options);
  console.log(
    `${options.command === "create" ? "Created" : "Verified"} Torben App ${result.version} ${result.releaseKind} ${result.targets.length}-target release set.`,
  );
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
