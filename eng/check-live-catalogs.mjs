import { spawnSync } from "node:child_process";
import { existsSync, lstatSync, mkdtempSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, isAbsolute, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = dirname(scriptDirectory);
const maximumOutputBytes = 16 * 1024 * 1024;
const processTimeoutMs = 180_000;
const safeEnvironmentKeys = [
  "HOME",
  "LANG",
  "LC_ALL",
  "PATH",
  "RUST_LOG",
  "XDG_CACHE_HOME",
  "XDG_CONFIG_HOME",
  "XDG_DATA_HOME",
  "XDG_RUNTIME_DIR",
];

export const officialCatalogApps = Object.freeze([
  "node",
  "temurin",
  "python",
  "git",
  "vscode",
  "codex",
]);

function fail(message) {
  throw new Error(message);
}

function regularFile(path, description) {
  if (!existsSync(path)) fail(`${description} does not exist: ${path}`);
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-symbolic-link file: ${path}`);
  }
}

function regularDirectory(path, description) {
  if (!existsSync(path)) fail(`${description} does not exist: ${path}`);
  const metadata = lstatSync(path);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-symbolic-link directory: ${path}`);
  }
}

function boundedMessage(value) {
  const text = String(value ?? "")
    .replaceAll(/\p{C}/gu, " ")
    .trim();
  return text.length > 4096 ? `${text.slice(0, 4096)}…` : text;
}

function catalogEnvironment(environment) {
  return Object.fromEntries(
    safeEnvironmentKeys
      .filter((key) => typeof environment[key] === "string" && environment[key].length > 0)
      .map((key) => [key, environment[key]]),
  );
}

function runCatalogCommand({ cliPath, app, environment }) {
  return spawnSync(cliPath, ["version", "list", app, "--json"], {
    cwd: repositoryRoot,
    encoding: "utf8",
    env: environment,
    maxBuffer: maximumOutputBytes,
    shell: false,
    timeout: processTimeoutMs,
    windowsHide: true,
  });
}

function parseEnvelope(app, result) {
  if (result.error) {
    fail(`Official ${app} catalog process failed: ${boundedMessage(result.error.message)}`);
  }
  if (result.status !== 0) {
    fail(
      `Official ${app} catalog command exited with ${String(result.status)}: ${boundedMessage(result.stderr)}`,
    );
  }
  let envelope;
  try {
    envelope = JSON.parse(result.stdout);
  } catch (error) {
    fail(`Official ${app} catalog returned invalid JSON: ${boundedMessage(error.message)}`);
  }
  if (
    envelope === null ||
    typeof envelope !== "object" ||
    envelope.schemaVersion !== 1 ||
    envelope.ok !== true ||
    envelope.error !== undefined ||
    !Array.isArray(envelope.data) ||
    envelope.data.length === 0 ||
    envelope.data.length > 10_000 ||
    (envelope.warnings !== undefined && !Array.isArray(envelope.warnings))
  ) {
    fail(`Official ${app} catalog returned an invalid Torben JSON envelope.`);
  }
  const versions = new Set();
  for (const record of envelope.data) {
    if (
      record === null ||
      typeof record !== "object" ||
      typeof record.version !== "string" ||
      record.version.length === 0 ||
      record.version.length > 256 ||
      typeof record.releasedAt !== "string" ||
      record.releasedAt.length === 0 ||
      typeof record.recommended !== "boolean" ||
      !(record.ltsName === null || typeof record.ltsName === "string") ||
      versions.has(record.version)
    ) {
      fail(`Official ${app} catalog contains an invalid or duplicate version record.`);
    }
    versions.add(record.version);
  }
  if (!envelope.data.some((record) => record.recommended)) {
    fail(`Official ${app} catalog has no recommended version.`);
  }
  return envelope;
}

function writeNew(path, content) {
  writeFileSync(path, content, { encoding: "utf8", flag: "wx" });
}

export function checkLiveCatalogs({
  cliPath,
  outputDirectory,
  execute = runCatalogCommand,
  environment = process.env,
}) {
  if (typeof cliPath !== "string" || cliPath.length === 0) {
    fail("A Torben CLI path is required.");
  }
  if (typeof outputDirectory !== "string" || outputDirectory.length === 0) {
    fail("An output directory is required.");
  }
  const cli = resolve(cliPath);
  const output = resolve(outputDirectory);
  regularFile(cli, "Torben CLI");
  if (existsSync(output)) fail(`Live catalog output must not already exist: ${output}`);
  const parent = dirname(output);
  regularDirectory(parent, "Live catalog output parent");
  const staging = mkdtempSync(join(parent, `.${basename(output)}.next-`));
  try {
    const summaries = [];
    for (const app of officialCatalogApps) {
      const result = execute({ cliPath: cli, app, environment: catalogEnvironment(environment) });
      const envelope = parseEnvelope(app, result);
      writeNew(join(staging, `${app}.json`), `${JSON.stringify(envelope, null, 2)}\n`);
      summaries.push({
        appId: app,
        versionCount: envelope.data.length,
        recommendedVersions: envelope.data
          .filter((record) => record.recommended)
          .map((record) => record.version),
      });
    }
    const summary = { schemaVersion: 1, catalogs: summaries };
    writeNew(join(staging, "catalog-summary.json"), `${JSON.stringify(summary, null, 2)}\n`);
    renameSync(staging, output);
    return summary;
  } catch (error) {
    rmSync(staging, { force: true, recursive: true });
    throw error;
  }
}

function parseArguments(arguments_) {
  const options = {};
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (argument === "--cli" || argument === "--output") {
      const value = arguments_[index + 1];
      if (!value || value.startsWith("--")) fail(`Missing value for ${argument}.`);
      options[argument.slice(2)] = value;
      index += 1;
    } else {
      fail(`Unknown argument: ${argument}`);
    }
  }
  if (!options.cli || !options.output) {
    fail("Usage: check-live-catalogs.mjs --cli <torben> --output <new-directory>");
  }
  return options;
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  try {
    const options = parseArguments(process.argv.slice(2));
    const summary = checkLiveCatalogs({
      cliPath: isAbsolute(options.cli) ? options.cli : resolve(options.cli),
      outputDirectory: isAbsolute(options.output) ? options.output : resolve(options.output),
    });
    console.log(
      `Verified ${summary.catalogs.length} official Torben application catalogs through the CLI.`,
    );
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
