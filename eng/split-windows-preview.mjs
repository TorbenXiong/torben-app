import { createHash, randomUUID } from "node:crypto";
import {
  copyFileSync,
  createReadStream,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const downloads = [
  { kind: "nsis", matches: (name) => name.endsWith("-setup.exe") },
  { kind: "msi", matches: (name) => name.endsWith(".msi") },
  {
    kind: "cli",
    matches: (name) => /^torben-.+-x86_64-pc-windows-msvc\.zip$/u.test(name),
  },
];

function fail(message) {
  throw new Error(message);
}

function requirePlainFile(path, label) {
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail(`${label} must be a plain file: ${path}`);
  }
}

async function sha256(path) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest("hex");
}

export async function splitWindowsPreview({ source, output }) {
  const sourceRoot = resolve(source);
  const outputRoot = resolve(output);
  const outputRelation = relative(sourceRoot, outputRoot);
  if (
    !outputRelation ||
    (!isAbsolute(outputRelation) &&
      outputRelation !== ".." &&
      !outputRelation.startsWith(`..${sep}`))
  ) {
    fail("Preview output must be outside the accepted candidate directory.");
  }
  const sourceMetadata = lstatSync(sourceRoot);
  if (!sourceMetadata.isDirectory() || sourceMetadata.isSymbolicLink()) {
    fail(`Accepted candidate must be a plain directory: ${sourceRoot}`);
  }
  if (existsSync(outputRoot)) fail(`Preview output already exists: ${outputRoot}`);

  const warning = join(sourceRoot, "UNSIGNED-PREVIEW.txt");
  requirePlainFile(warning, "Unsigned preview warning");
  const entries = readdirSync(sourceRoot, { withFileTypes: true });
  const selected = downloads.map(({ kind, matches }) => {
    const candidates = entries.filter((entry) => entry.isFile() && matches(entry.name));
    if (candidates.length !== 1) {
      fail(`Expected exactly one ${kind} preview download, found ${candidates.length}.`);
    }
    return { kind, name: candidates[0].name };
  });

  mkdirSync(dirname(outputRoot), { recursive: true });
  const staging = `${outputRoot}.staging-${process.pid}-${randomUUID()}`;
  mkdirSync(staging);
  try {
    for (const { kind, name } of selected) {
      const sourcePath = join(sourceRoot, name);
      requirePlainFile(sourcePath, `${kind} preview download`);
      const destination = join(staging, kind);
      mkdirSync(destination);
      copyFileSync(sourcePath, join(destination, basename(sourcePath)));
      copyFileSync(warning, join(destination, basename(warning)));
      writeFileSync(join(destination, "SHA256SUMS"), `${await sha256(sourcePath)}  ${name}\n`);
    }
    renameSync(staging, outputRoot);
  } catch (error) {
    rmSync(staging, { recursive: true, force: true });
    throw error;
  }
  return selected;
}

function parseArguments(values) {
  const options = {};
  for (let index = 0; index < values.length; index += 2) {
    const name = values[index];
    const value = values[index + 1];
    if (!name?.startsWith("--") || !value) fail(`Invalid argument: ${name ?? "<missing>"}`);
    options[name.slice(2)] = value;
  }
  if (!options.source || !options.output) fail("Usage: --source <directory> --output <directory>");
  return options;
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  splitWindowsPreview(parseArguments(process.argv.slice(2)))
    .then((selected) => {
      console.log(`Prepared ${selected.length} separate Windows preview downloads.`);
    })
    .catch((error) => {
      console.error(error.message);
      process.exitCode = 1;
    });
}
