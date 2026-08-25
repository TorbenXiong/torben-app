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
import { dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const defaultRepositoryRoot = dirname(scriptDirectory);
const metadataName = "release-metadata.json";
const checksumsName = "SHA256SUMS";
const maximumArtifacts = 1024;
const comparePaths = (left, right) => left.localeCompare(right, "en");

export const supportedTargets = Object.freeze({
  "x86_64-pc-windows-msvc": { operatingSystem: "windows", architecture: "x86_64" },
  "aarch64-pc-windows-msvc": { operatingSystem: "windows", architecture: "aarch64" },
  "x86_64-apple-darwin": { operatingSystem: "macos", architecture: "x86_64" },
  "aarch64-apple-darwin": { operatingSystem: "macos", architecture: "aarch64" },
  "x86_64-unknown-linux-gnu": { operatingSystem: "linux", architecture: "x86_64" },
  "aarch64-unknown-linux-gnu": { operatingSystem: "linux", architecture: "aarch64" },
});

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

function cargoWorkspaceVersion(repositoryRoot) {
  const path = join(repositoryRoot, "Cargo.toml");
  const content = readFileSync(path, "utf8");
  const sectionStart = content.search(/^\[workspace\.package\]\s*$/m);
  if (sectionStart < 0) {
    fail(`${path} has no [workspace.package] section.`);
  }
  const afterHeader = content.slice(sectionStart).replace(/^\[workspace\.package\]\s*\r?\n/, "");
  const nextSection = afterHeader.search(/^\[/m);
  const section = nextSection < 0 ? afterHeader : afterHeader.slice(0, nextSection);
  const version = section.match(/^version\s*=\s*"([^"]+)"\s*$/m)?.[1];
  if (!version) {
    fail(`${path} has no workspace package version.`);
  }
  return version;
}

export function workspaceVersion(repositoryRoot = defaultRepositoryRoot) {
  const root = resolve(repositoryRoot);
  const versions = {
    cargoWorkspace: cargoWorkspaceVersion(root),
    rootPackage: parseJson(join(root, "package.json")).version,
    desktopPackage: parseJson(join(root, "apps", "desktop", "package.json")).version,
    desktopTauri: parseJson(join(root, "apps", "desktop", "src-tauri", "tauri.conf.json")).version,
    uiPackage: parseJson(join(root, "packages", "ui", "package.json")).version,
  };
  const unique = new Set(Object.values(versions));
  if (unique.size !== 1 || unique.has(undefined)) {
    fail(`Workspace versions do not match: ${JSON.stringify(versions)}`);
  }
  const [version] = unique;
  if (!/^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?$/.test(version)) {
    fail(`Workspace version is not a supported semantic version: ${version}`);
  }
  return { version, sources: versions };
}

function safeRelative(root, path) {
  const value = relative(root, path);
  if (!value || isAbsolute(value) || value === ".." || value.startsWith(`..${sep}`)) {
    fail(`Artifact path escapes its root: ${path}`);
  }
  const normalized = value.split(sep).join("/");
  if (normalized.includes("\n") || normalized.includes("\r")) {
    fail(`Artifact path cannot be represented safely in SHA256SUMS: ${normalized}`);
  }
  return normalized;
}

function collectArtifactPaths(root) {
  if (!existsSync(root) || !lstatSync(root).isDirectory()) {
    fail(`Artifact directory does not exist: ${root}`);
  }
  const paths = [];
  const visit = (directory) => {
    const entries = readdirSync(directory, { withFileTypes: true }).sort((left, right) =>
      left.name.localeCompare(right.name, "en"),
    );
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) {
        fail(`Release artifacts cannot contain symbolic links: ${safeRelative(root, path)}`);
      }
      if (entry.isDirectory()) {
        visit(path);
      } else if (entry.isFile()) {
        const relativePath = safeRelative(root, path);
        if (relativePath !== metadataName && relativePath !== checksumsName) {
          paths.push({ absolute: path, relative: relativePath });
        }
      } else {
        fail(`Release artifacts must be regular files: ${safeRelative(root, path)}`);
      }
      if (paths.length > maximumArtifacts) {
        fail(`Release artifact count exceeds ${maximumArtifacts}.`);
      }
    }
  };
  visit(root);
  return paths.sort((left, right) => comparePaths(left.relative, right.relative));
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

function removeIfPresent(path) {
  if (existsSync(path)) unlinkSync(path);
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

function requireNewMetadataPaths(paths) {
  for (const path of paths) {
    for (const candidate of [path, `${path}.next`]) {
      if (existsSync(candidate)) {
        fail(`Refusing to overwrite existing release metadata: ${candidate}`);
      }
    }
  }
}

function validateReleaseIdentity({ version, revision, sourceRef, releaseKind, signingStatus }) {
  if (!/^[0-9a-f]{40}$/.test(revision)) {
    fail("Source revision must be a lowercase 40-character Git commit SHA.");
  }
  const refSegments = typeof sourceRef === "string" ? sourceRef.split("/") : [];
  if (
    !/^refs\/[0-9A-Za-z._/-]+$/.test(sourceRef ?? "") ||
    refSegments[0] !== "refs" ||
    !["heads", "tags", "pull"].includes(refSegments[1]) ||
    refSegments.length < 3 ||
    refSegments.some((segment) => segment === "" || segment === "." || segment === "..")
  ) {
    fail("Source ref must be a canonical Git ref without traversal segments.");
  }
  if (!["development", "official"].includes(releaseKind)) {
    fail("Release kind must be development or official.");
  }
  if (!["unsigned", "signed"].includes(signingStatus)) {
    fail("Signing status must be unsigned or signed.");
  }
  if (releaseKind === "official") {
    if (sourceRef !== `refs/tags/v${version}`) {
      fail(`Official releases require source ref refs/tags/v${version}.`);
    }
    if (signingStatus !== "signed") {
      fail("Official releases require signed artifacts.");
    }
  }
}

export async function createReleaseMetadata({
  artifacts,
  target,
  revision,
  sourceRef,
  releaseKind,
  signingStatus,
  repositoryRoot = defaultRepositoryRoot,
}) {
  const artifactRoot = resolve(artifacts);
  const targetDetails = supportedTargets[target];
  if (!targetDetails) {
    fail(`Unsupported release target: ${target}`);
  }
  const { version, sources } = workspaceVersion(repositoryRoot);
  validateReleaseIdentity({ version, revision, sourceRef, releaseKind, signingStatus });
  const metadataPath = join(artifactRoot, metadataName);
  const checksumsPath = join(artifactRoot, checksumsName);
  requireNewMetadataPaths([metadataPath, checksumsPath]);
  const paths = collectArtifactPaths(artifactRoot);
  if (paths.length === 0) {
    fail("Release artifact directory is empty.");
  }
  const artifactRecords = [];
  for (const path of paths) {
    const metadata = lstatSync(path.absolute);
    artifactRecords.push({
      path: path.relative,
      size: metadata.size,
      sha256: await sha256(path.absolute),
    });
  }
  const metadata = {
    schemaVersion: 1,
    productName: "Torben App",
    applicationId: "io.github.torbenxiong.torbenapp",
    version,
    versionSources: sources,
    sourceRevision: revision,
    sourceRef,
    releaseKind,
    signingStatus,
    target,
    ...targetDetails,
    artifacts: artifactRecords,
  };
  const metadataContent = `${JSON.stringify(metadata, null, 2)}\n`;
  const checksums = [
    ...artifactRecords,
    {
      path: metadataName,
      sha256: sha256Content(metadataContent),
    },
  ]
    .sort((left, right) => comparePaths(left.path, right.path))
    .map((record) => `${record.sha256}  ${record.path}`)
    .join("\n");
  let metadataTemporaryPath;
  let checksumsTemporaryPath;
  let metadataCommitted = false;
  let checksumsCommitted = false;
  try {
    metadataTemporaryPath = writeTemporary(metadataPath, metadataContent);
    checksumsTemporaryPath = writeTemporary(checksumsPath, `${checksums}\n`);
    renameSync(metadataTemporaryPath, metadataPath);
    metadataCommitted = true;
    renameSync(checksumsTemporaryPath, checksumsPath);
    checksumsCommitted = true;
  } catch (error) {
    if (metadataTemporaryPath) removeIfPresent(metadataTemporaryPath);
    if (checksumsTemporaryPath) removeIfPresent(checksumsTemporaryPath);
    if (metadataCommitted) removeIfPresent(metadataPath);
    if (checksumsCommitted) removeIfPresent(checksumsPath);
    throw error;
  }
  return metadata;
}

function parseChecksums(path) {
  const records = new Map();
  const lines = readFileSync(path, "utf8").split(/\r?\n/);
  for (const line of lines) {
    if (!line) {
      continue;
    }
    const match = line.match(/^([0-9a-f]{64}) {2}([^\r\n]+)$/);
    if (!match || records.has(match[2])) {
      fail(`Malformed or duplicate SHA256SUMS entry: ${line}`);
    }
    records.set(match[2], match[1]);
  }
  return records;
}

export async function verifyReleaseMetadata({ artifacts, repositoryRoot = defaultRepositoryRoot }) {
  const artifactRoot = resolve(artifacts);
  const metadataPath = join(artifactRoot, metadataName);
  const checksumsPath = join(artifactRoot, checksumsName);
  for (const path of [metadataPath, checksumsPath]) {
    if (!existsSync(path) || !lstatSync(path).isFile() || lstatSync(path).isSymbolicLink()) {
      fail(`Release metadata file is missing or invalid: ${path}`);
    }
  }
  const metadata = parseJson(metadataPath);
  if (
    metadata.schemaVersion !== 1 ||
    metadata.productName !== "Torben App" ||
    metadata.applicationId !== "io.github.torbenxiong.torbenapp"
  ) {
    fail("Release metadata schema or product identity is invalid.");
  }
  const targetDetails = supportedTargets[metadata.target];
  if (
    !targetDetails ||
    metadata.operatingSystem !== targetDetails.operatingSystem ||
    metadata.architecture !== targetDetails.architecture
  ) {
    fail("Release metadata target, operating system, or architecture is inconsistent.");
  }
  const workspace = workspaceVersion(repositoryRoot);
  if (
    metadata.version !== workspace.version ||
    JSON.stringify(metadata.versionSources) !== JSON.stringify(workspace.sources)
  ) {
    fail("Release metadata does not match the current workspace version.");
  }
  validateReleaseIdentity({
    version: metadata.version,
    revision: metadata.sourceRevision,
    sourceRef: metadata.sourceRef,
    releaseKind: metadata.releaseKind,
    signingStatus: metadata.signingStatus,
  });
  if (!Array.isArray(metadata.artifacts) || metadata.artifacts.length === 0) {
    fail("Release metadata has no artifacts.");
  }
  const discovered = collectArtifactPaths(artifactRoot);
  const expectedPaths = metadata.artifacts.map((record) => record.path);
  const discoveredPaths = discovered.map((record) => record.relative);
  if (JSON.stringify(expectedPaths) !== JSON.stringify(discoveredPaths)) {
    fail("Release directory contents do not match the metadata artifact list.");
  }
  const checksums = parseChecksums(checksumsPath);
  const expectedChecksumPaths = [...expectedPaths, metadataName].sort(comparePaths);
  if (
    JSON.stringify([...checksums.keys()].sort(comparePaths)) !==
    JSON.stringify(expectedChecksumPaths)
  ) {
    fail("SHA256SUMS entries do not match the release metadata.");
  }
  for (let index = 0; index < discovered.length; index += 1) {
    const path = discovered[index];
    const record = metadata.artifacts[index];
    const fileMetadata = lstatSync(path.absolute);
    const digest = await sha256(path.absolute);
    if (
      record.path !== path.relative ||
      record.size !== fileMetadata.size ||
      record.sha256 !== digest ||
      checksums.get(record.path) !== digest
    ) {
      fail(`Release artifact failed verification: ${path.relative}`);
    }
  }
  const metadataDigest = await sha256(metadataPath);
  if (checksums.get(metadataName) !== metadataDigest) {
    fail("release-metadata.json failed SHA-256 verification.");
  }
  return metadata;
}

function parseArguments(values) {
  const [command, ...rest] = values;
  if (!["create", "verify"].includes(command)) {
    fail("Usage: release-metadata.mjs <create|verify> --artifacts <directory> [...options]");
  }
  const options = { command };
  for (let index = 0; index < rest.length; index += 2) {
    const name = rest[index];
    const value = rest[index + 1];
    if (!name?.startsWith("--") || value === undefined || value.startsWith("--")) {
      fail(`Invalid command-line argument: ${name ?? "<missing>"}`);
    }
    const key = name.slice(2).replace(/-([a-z])/g, (_, letter) => letter.toUpperCase());
    if (Object.hasOwn(options, key)) {
      fail(`Duplicate command-line option: ${name}`);
    }
    options[key] = value;
  }
  if (!options.artifacts) {
    fail("--artifacts is required.");
  }
  return options;
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  const result =
    options.command === "create"
      ? await createReleaseMetadata(options)
      : await verifyReleaseMetadata(options);
  console.log(
    `${options.command === "create" ? "Created" : "Verified"} Torben App ${result.version} ${result.target} ${result.releaseKind} metadata.`,
  );
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
