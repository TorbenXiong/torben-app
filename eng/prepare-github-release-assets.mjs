import { createHash } from "node:crypto";
import {
  closeSync,
  constants,
  copyFileSync,
  createReadStream,
  existsSync,
  fsyncSync,
  lstatSync,
  mkdirSync,
  openSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { supportedTargets, workspaceVersion } from "./release-metadata.mjs";

const internalFiles = new Set(["release-metadata.json", "SHA256SUMS", "updater-artifacts.json"]);
const checksumsName = "SHA256SUMS";

function fail(message) {
  throw new Error(message);
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

function regularFile(path, description) {
  if (!existsSync(path)) fail(`${description} is missing: ${path}`);
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-link file: ${path}`);
  }
}

function flatRegularFiles(directory, description) {
  if (!existsSync(directory)) fail(`${description} is missing: ${directory}`);
  const metadata = lstatSync(directory);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-link directory: ${directory}`);
  }
  const files = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    if (!entry.isFile() || entry.isSymbolicLink()) {
      fail(`${description} must contain only flat regular files: ${entry.name}`);
    }
    files.push(join(directory, entry.name));
  }
  return files;
}

function atomicWrite(path, content) {
  const next = `${path}.next`;
  const descriptor = openSync(next, "wx");
  try {
    writeFileSync(descriptor, content);
    fsyncSync(descriptor);
  } finally {
    closeSync(descriptor);
  }
  renameSync(next, path);
}

function escapeRegularExpression(value) {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function releaseAssetSources(root) {
  const { version } = workspaceVersion();
  const rawCliPattern = new RegExp(`^torben-${escapeRegularExpression(version)}-[^.]+(?:\\.exe)?$`);
  const sources = [join(root, "latest.json"), join(root, "release-index.json")];
  for (const target of Object.keys(supportedTargets)) {
    sources.push(
      ...flatRegularFiles(join(root, target), `Release target ${target}`).filter((path) => {
        const name = basename(path);
        return !internalFiles.has(name) && !rawCliPattern.test(name);
      }),
    );
  }
  const names = new Set();
  for (const source of sources) {
    regularFile(source, "Release asset");
    const name = basename(source);
    if (names.has(name)) fail(`Release asset name is duplicated: ${name}`);
    names.add(name);
  }
  return { sources, names };
}

function parseChecksums(path) {
  const records = new Map();
  for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
    if (!line) continue;
    const match = line.match(/^([0-9a-f]{64}) {2}([^\r\n]+)$/);
    if (!match || records.has(match[2])) {
      fail(`Malformed or duplicate GitHub Release checksum entry: ${line}`);
    }
    records.set(match[2], match[1]);
  }
  return records;
}

async function verifyPreparedAssets(directory, sources, names) {
  const files = flatRegularFiles(directory, "GitHub Release asset directory");
  const actualNames = files
    .map((path) => basename(path))
    .sort((left, right) => left.localeCompare(right, "en"));
  const expectedNames = [...names, checksumsName].sort((left, right) =>
    left.localeCompare(right, "en"),
  );
  if (JSON.stringify(actualNames) !== JSON.stringify(expectedNames)) {
    fail("GitHub Release asset directory does not match the exact expected file set.");
  }
  const checksums = parseChecksums(join(directory, checksumsName));
  const expectedChecksumNames = [...names].sort((left, right) => left.localeCompare(right, "en"));
  if (
    JSON.stringify([...checksums.keys()].sort((left, right) => left.localeCompare(right, "en"))) !==
    JSON.stringify(expectedChecksumNames)
  ) {
    fail("GitHub Release SHA256SUMS entries do not match the expected assets.");
  }
  for (const source of sources) {
    const name = basename(source);
    const sourceDigest = await sha256(source);
    const outputDigest = await sha256(join(directory, name));
    if (sourceDigest !== outputDigest || checksums.get(name) !== outputDigest) {
      fail(`GitHub Release asset failed source or checksum verification: ${name}`);
    }
  }
  return expectedNames;
}

export async function verifyGithubReleaseAssets({ releases, output }) {
  const root = resolve(releases);
  const outputRoot = resolve(output);
  const { sources, names } = releaseAssetSources(root);
  return verifyPreparedAssets(outputRoot, sources, names);
}

export async function prepareGithubReleaseAssets({ releases, output }) {
  const root = resolve(releases);
  const outputRoot = resolve(output);
  const { sources, names } = releaseAssetSources(root);
  if (existsSync(outputRoot)) fail(`Release asset output already exists: ${outputRoot}`);
  const stagingRoot = `${outputRoot}.next`;
  if (existsSync(stagingRoot)) {
    fail(`Temporary release asset output already exists: ${stagingRoot}`);
  }
  mkdirSync(dirname(outputRoot), { recursive: true });
  mkdirSync(stagingRoot);
  try {
    for (const source of sources) {
      const name = basename(source);
      copyFileSync(source, join(stagingRoot, name), constants.COPYFILE_EXCL);
    }
    const files = flatRegularFiles(stagingRoot, "Staged GitHub Release assets").sort(
      (left, right) => basename(left).localeCompare(basename(right), "en"),
    );
    const checksums = [];
    for (const path of files) checksums.push(`${await sha256(path)}  ${basename(path)}`);
    atomicWrite(join(stagingRoot, checksumsName), `${checksums.join("\n")}\n`);
    await verifyPreparedAssets(stagingRoot, sources, names);
    renameSync(stagingRoot, outputRoot);
    return [...names, checksumsName].sort((left, right) => left.localeCompare(right, "en"));
  } catch (error) {
    rmSync(stagingRoot, { recursive: true, force: true });
    throw error;
  }
}

function parseArguments(values) {
  const [command, ...rest] = values;
  if (!["create", "verify"].includes(command)) {
    fail("Usage: prepare-github-release-assets.mjs <create|verify> --releases DIR --output DIR");
  }
  const options = {};
  for (let index = 0; index < rest.length; index += 2) {
    const name = rest[index];
    const value = rest[index + 1];
    if (!name?.startsWith("--") || !value || value.startsWith("--")) {
      fail("Usage: prepare-github-release-assets.mjs <create|verify> --releases DIR --output DIR");
    }
    const key = name.slice(2);
    if (!key || Object.hasOwn(options, key)) {
      fail(`Duplicate or invalid command-line option: ${name}`);
    }
    options[key] = value;
  }
  if (
    !options.releases ||
    !options.output ||
    JSON.stringify(Object.keys(options).sort()) !== JSON.stringify(["output", "releases"])
  ) {
    fail("--releases and --output are the only required options.");
  }
  return { command, ...options };
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  const options = parseArguments(process.argv.slice(2));
  const action =
    options.command === "create"
      ? prepareGithubReleaseAssets(options)
      : verifyGithubReleaseAssets(options);
  action
    .then((assets) =>
      console.log(
        `${options.command === "create" ? "Prepared" : "Verified"} ${assets.length} unique GitHub Release assets.`,
      ),
    )
    .catch((error) => {
      console.error(error.message);
      process.exitCode = 1;
    });
}
