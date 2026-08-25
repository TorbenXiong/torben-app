import { spawnSync } from "node:child_process";
import {
  chmodSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { detectExecutableTarget } from "./collect-release-artifacts.mjs";
import { verifyReleaseMetadata } from "./release-metadata.mjs";

const packageSuffixes = Object.freeze({
  appimage: ".AppImage",
  deb: ".deb",
  rpm: ".rpm",
});
const sidecarNames = Object.freeze([
  "torben-plugin-node",
  "torben-plugin-temurin",
  "torben-plugin-python",
  "torben-plugin-git",
  "torben-plugin-vscode",
  "torben-plugin-codex",
  "torben-shim",
]);
const maximumExtractedEntries = 20_000;
const maximumCommandOutput = 4 * 1024 * 1024;
const defaultLaunchTimeoutMs = 8_000;

function fail(message) {
  throw new Error(message);
}

function safeRelative(root, path, description) {
  const value = relative(root, path);
  if (isAbsolute(value) || value === ".." || value.startsWith(`..${sep}`)) {
    fail(`${description} escapes the extraction root: ${path}`);
  }
  return value ? value.split(sep).join("/") : ".";
}

function regularFile(path, description) {
  if (!existsSync(path)) fail(`${description} is missing: ${path}`);
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-link file: ${path}`);
  }
  return metadata;
}

function onePackage(artifactRoot, metadata, format) {
  const suffix = packageSuffixes[format];
  if (!suffix) fail(`Unsupported Linux package format: ${format}`);
  const matches = metadata.artifacts
    .map((record) => record.path)
    .filter((path) => path.endsWith(suffix));
  if (matches.length !== 1) {
    fail(`Expected exactly one ${format} package in release metadata, found ${matches.length}.`);
  }
  const path = resolve(artifactRoot, matches[0]);
  safeRelative(artifactRoot, path, `${format} package`);
  regularFile(path, `${format} package`);
  return path;
}

function executeCommand({ command, args, cwd, env, timeoutMs }) {
  const result = spawnSync(command, args, {
    cwd,
    env,
    encoding: "utf8",
    maxBuffer: maximumCommandOutput,
    timeout: timeoutMs,
    windowsHide: true,
  });
  return {
    status: result.status,
    signal: result.signal,
    stdout: result.stdout ?? "",
    stderr: result.stderr ?? "",
    error: result.error?.message,
  };
}

function requireStatus(result, expected, description) {
  if (result.error) fail(`${description} could not start: ${result.error}`);
  if (!expected.includes(result.status)) {
    const diagnostic = String(result.stderr || result.stdout)
      .trim()
      .slice(-4_000);
    fail(
      `${description} failed with status ${result.status ?? "none"}${result.signal ? ` (${result.signal})` : ""}${diagnostic ? `: ${diagnostic}` : ""}`,
    );
  }
}

function extractionCommand(format, packagePath, temporaryRoot, extractedRoot) {
  if (format === "appimage") {
    const copiedPackage = join(temporaryRoot, basename(packagePath));
    copyFileSync(packagePath, copiedPackage);
    chmodSync(copiedPackage, 0o755);
    return {
      command: copiedPackage,
      args: ["--appimage-extract"],
      cwd: temporaryRoot,
      resultRoot: join(temporaryRoot, "squashfs-root"),
      packageExecutable: copiedPackage,
    };
  }
  mkdirSync(extractedRoot);
  if (format === "deb") {
    return {
      command: "dpkg-deb",
      args: ["--extract", packagePath, extractedRoot],
      cwd: temporaryRoot,
      resultRoot: extractedRoot,
    };
  }
  return {
    command: "bash",
    args: [
      "-c",
      'set -o pipefail; rpm2cpio "$1" | cpio -idm --quiet --no-absolute-filenames',
      "torben-rpm-extract",
      packagePath,
    ],
    cwd: extractedRoot,
    resultRoot: extractedRoot,
  };
}

function installationCommand(format, packagePath, temporaryRoot, environment) {
  if (format === "deb") {
    return {
      command: "apt-get",
      args: ["--quiet=2", "install", "--yes", "--no-install-recommends", packagePath],
      cwd: temporaryRoot,
      env: { ...environment, DEBIAN_FRONTEND: "noninteractive" },
    };
  }
  if (format === "rpm") {
    return {
      command: "dnf",
      args: ["--quiet", "install", "--assumeyes", packagePath],
      cwd: temporaryRoot,
      env: environment,
    };
  }
  fail(`System installation is not defined for ${format}.`);
}

function scanExtractedTree(root) {
  if (!existsSync(root) || !lstatSync(root).isDirectory()) {
    fail(`Package extraction did not create a directory: ${root}`);
  }
  const files = [];
  const links = [];
  let entries = 0;
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      entries += 1;
      if (entries > maximumExtractedEntries) {
        fail(`Extracted package exceeds ${maximumExtractedEntries} filesystem entries.`);
      }
      const path = join(directory, entry.name);
      safeRelative(root, path, "Extracted package entry");
      if (entry.isDirectory()) visit(path);
      else if (entry.isFile()) files.push(path);
      else if (entry.isSymbolicLink()) links.push(path);
      else fail(`Extracted package contains an unsupported filesystem entry: ${path}`);
    }
  };
  visit(root);
  for (const link of links) {
    const target = realpathSync(link);
    safeRelative(root, target, "Extracted package symbolic link");
  }
  return files;
}

function desktopEntry(path) {
  const values = new Map();
  let active = false;
  for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
    if (line.startsWith("[") && line.endsWith("]")) {
      active = line === "[Desktop Entry]";
      continue;
    }
    if (!active || !line || line.startsWith("#")) continue;
    const separator = line.indexOf("=");
    if (separator <= 0) continue;
    const key = line.slice(0, separator);
    if (!values.has(key)) values.set(key, line.slice(separator + 1));
  }
  if (values.get("Name") !== "Torben App" || !values.get("Exec")) {
    fail(`Desktop entry does not expose the Torben App identity: ${path}`);
  }
  return values;
}

function execCommand(value) {
  const input = value.trim();
  if (!input) fail("Desktop entry Exec command is empty.");
  if (input[0] !== '"') return input.match(/^[^\s]+/)?.[0] ?? fail("Desktop Exec is invalid.");
  let command = "";
  let escaped = false;
  for (let index = 1; index < input.length; index += 1) {
    const character = input[index];
    if (escaped) {
      command += character;
      escaped = false;
    } else if (character === "\\") {
      escaped = true;
    } else if (character === '"') {
      return command;
    } else {
      command += character;
    }
  }
  fail("Desktop entry Exec command has an unterminated quote.");
}

function executableFile(root, path, expectedTarget, description, enforceExecutableMode) {
  if (!existsSync(path)) fail(`${description} is missing: ${path}`);
  const resolved = realpathSync(path);
  safeRelative(root, resolved, description);
  const metadata = regularFile(resolved, description);
  if (enforceExecutableMode && (metadata.mode & 0o111) === 0) {
    fail(`${description} is not executable: ${resolved}`);
  }
  const detected = detectExecutableTarget(resolved);
  if (detected !== expectedTarget) {
    fail(`${description} target ${detected} does not match release target ${expectedTarget}.`);
  }
  return resolved;
}

function inspectExtractedBundle(root, target, enforceExecutableMode) {
  const files = scanExtractedTree(root);
  const desktopEntries = files.filter((path) => path.endsWith(".desktop"));
  if (desktopEntries.length !== 1) {
    fail(`Expected exactly one desktop entry, found ${desktopEntries.length}.`);
  }
  const entry = desktopEntry(desktopEntries[0]);
  const command = execCommand(entry.get("Exec"));
  if (command.includes("%")) fail("Desktop entry executable cannot contain a field code.");
  let mainCandidate;
  if (command.startsWith("/")) {
    mainCandidate = resolve(root, command.replace(/^\/+/, ""));
  } else {
    const direct = join(root, "usr", "bin", command);
    const matches = files.filter((path) => basename(path) === basename(command));
    mainCandidate = existsSync(direct) ? direct : matches.length === 1 ? matches[0] : undefined;
  }
  if (!mainCandidate) fail(`Could not resolve desktop entry executable: ${command}`);
  const mainExecutable = executableFile(
    root,
    mainCandidate,
    target,
    "Desktop executable",
    enforceExecutableMode,
  );
  const executableDirectory = dirname(mainExecutable);
  const sidecars = sidecarNames.map((name) => {
    const matches = files.filter((path) => {
      const filename = basename(path);
      return filename === name || filename === `${name}-${target}`;
    });
    if (matches.length !== 1) {
      fail(`Expected exactly one bundled ${name} sidecar, found ${matches.length}.`);
    }
    const executable = executableFile(
      root,
      matches[0],
      target,
      `${name} sidecar`,
      enforceExecutableMode,
    );
    if (dirname(executable) !== executableDirectory) {
      fail(`${name} sidecar is not adjacent to the desktop executable.`);
    }
    return executable;
  });
  return { desktopEntry: desktopEntries[0], mainExecutable, sidecars };
}

function inspectInstalledBundle(
  systemRoot,
  extractedRoot,
  inspected,
  target,
  enforceExecutableMode,
) {
  const mapInstalledPath = (path, description) => {
    const relativePath = safeRelative(extractedRoot, path, description);
    const installed = resolve(systemRoot, relativePath);
    safeRelative(systemRoot, installed, `Installed ${description.toLowerCase()}`);
    return installed;
  };
  const installedDesktopEntry = mapInstalledPath(inspected.desktopEntry, "Desktop entry");
  if (!existsSync(installedDesktopEntry)) {
    fail(`Installed desktop entry is missing: ${installedDesktopEntry}`);
  }
  const resolvedDesktopEntry = realpathSync(installedDesktopEntry);
  safeRelative(systemRoot, resolvedDesktopEntry, "Installed desktop entry");
  regularFile(resolvedDesktopEntry, "Installed desktop entry");
  desktopEntry(resolvedDesktopEntry);
  const mainExecutable = executableFile(
    systemRoot,
    mapInstalledPath(inspected.mainExecutable, "Desktop executable"),
    target,
    "Installed desktop executable",
    enforceExecutableMode,
  );
  const sidecars = inspected.sidecars.map((path, index) =>
    executableFile(
      systemRoot,
      mapInstalledPath(path, `${sidecarNames[index]} sidecar`),
      target,
      `Installed ${sidecarNames[index]} sidecar`,
      enforceExecutableMode,
    ),
  );
  const executableDirectory = dirname(mainExecutable);
  if (sidecars.some((path) => dirname(path) !== executableDirectory)) {
    fail("Installed sidecars are not adjacent to the desktop executable.");
  }
  return { desktopEntry: installedDesktopEntry, mainExecutable, sidecars };
}

function hostArchitecture(value) {
  if (value === "x64") return "x86_64";
  if (value === "arm64") return "aarch64";
  fail(`Unsupported Linux smoke-test host architecture: ${value}`);
}

function launchEnvironment(temporaryRoot, baseEnvironment, appImageExtractAndRun) {
  const directories = {
    XDG_DATA_HOME: join(temporaryRoot, "xdg", "data"),
    XDG_CONFIG_HOME: join(temporaryRoot, "xdg", "config"),
    XDG_CACHE_HOME: join(temporaryRoot, "xdg", "cache"),
    XDG_RUNTIME_DIR: join(temporaryRoot, "xdg", "runtime"),
  };
  for (const directory of Object.values(directories)) mkdirSync(directory, { recursive: true });
  chmodSync(directories.XDG_RUNTIME_DIR, 0o700);
  const environment = { ...directories };
  for (const name of [
    "PATH",
    "LANG",
    "LC_ALL",
    "LD_LIBRARY_PATH",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
  ]) {
    if (baseEnvironment[name]) environment[name] = baseEnvironment[name];
  }
  const result = {
    ...environment,
    GDK_BACKEND: "x11",
    WEBKIT_DISABLE_COMPOSITING_MODE: "1",
  };
  if (appImageExtractAndRun) result.APPIMAGE_EXTRACT_AND_RUN = "1";
  return result;
}

export async function runLinuxPackageSmoke({
  artifacts,
  format,
  launchTimeoutMs = defaultLaunchTimeoutMs,
  repositoryRoot,
  execute = executeCommand,
  platform = process.platform,
  architecture = process.arch,
  enforceExecutableMode = process.platform === "linux",
  environment = process.env,
  effectiveUserId = typeof process.getuid === "function" ? process.getuid() : undefined,
  mode = "extract",
  systemRoot = "/",
}) {
  if (platform !== "linux") fail("Linux package smoke tests require a Linux host.");
  if (!Object.hasOwn(packageSuffixes, format)) fail(`Unsupported Linux package format: ${format}`);
  if (!new Set(["extract", "install"]).has(mode)) {
    fail("Linux package smoke-test mode must be extract or install.");
  }
  if (mode === "install" && format !== "appimage" && effectiveUserId !== 0) {
    fail("System package smoke tests require root inside a disposable Linux environment.");
  }
  if (!Number.isInteger(launchTimeoutMs) || launchTimeoutMs < 2_000 || launchTimeoutMs > 30_000) {
    fail("Launch timeout must be an integer from 2000 through 30000 milliseconds.");
  }
  const artifactRoot = resolve(artifacts);
  const metadata = await verifyReleaseMetadata({ artifacts: artifactRoot, repositoryRoot });
  if (metadata.operatingSystem !== "linux") fail("Release metadata is not for Linux.");
  if (hostArchitecture(architecture) !== metadata.architecture) {
    fail(
      `Linux smoke-test host architecture ${hostArchitecture(architecture)} does not match ${metadata.architecture}.`,
    );
  }
  const packagePath = onePackage(artifactRoot, metadata, format);
  const temporaryPrefix = join(tmpdir(), "torben-linux-package-smoke-");
  const temporaryRoot = mkdtempSync(temporaryPrefix);
  try {
    const extractedRoot = join(temporaryRoot, "root");
    const extraction = extractionCommand(format, packagePath, temporaryRoot, extractedRoot);
    const extractionResult = execute({
      ...extraction,
      stage: `extract-${format}`,
      env: environment,
      timeoutMs: 60_000,
    });
    requireStatus(extractionResult, [0], `${format} extraction`);
    const extracted = inspectExtractedBundle(
      extraction.resultRoot,
      metadata.target,
      enforceExecutableMode,
    );
    let inspected = extracted;
    let systemInstalled = false;
    if (mode === "install" && format !== "appimage") {
      const installation = installationCommand(format, packagePath, temporaryRoot, environment);
      const installationResult = execute({
        ...installation,
        stage: `install-${format}`,
        timeoutMs: 180_000,
      });
      requireStatus(installationResult, [0], `${format} system installation`);
      const resolvedSystemRoot = resolve(systemRoot);
      if (!existsSync(resolvedSystemRoot) || !lstatSync(resolvedSystemRoot).isDirectory()) {
        fail(`System root is unavailable after package installation: ${resolvedSystemRoot}`);
      }
      inspected = inspectInstalledBundle(
        resolvedSystemRoot,
        extraction.resultRoot,
        extracted,
        metadata.target,
        enforceExecutableMode,
      );
      systemInstalled = true;
    }
    const launchExecutable =
      mode === "install" && format === "appimage"
        ? extraction.packageExecutable
        : inspected.mainExecutable;
    const launchResult = execute({
      command: "timeout",
      args: [
        "--signal=TERM",
        "--kill-after=3s",
        `${launchTimeoutMs / 1_000}s`,
        "xvfb-run",
        "-a",
        "-s",
        "-screen 0 1280x800x24",
        launchExecutable,
      ],
      cwd: dirname(launchExecutable),
      env: launchEnvironment(
        temporaryRoot,
        environment,
        mode === "install" && format === "appimage",
      ),
      timeoutMs: launchTimeoutMs + 5_000,
      stage: "launch",
    });
    requireStatus(launchResult, [124], "Torben App launch probe");
    return {
      schemaVersion: 1,
      ok: true,
      format,
      mode,
      target: metadata.target,
      package: basename(packagePath),
      desktopEntry: systemInstalled
        ? inspected.desktopEntry
        : safeRelative(extraction.resultRoot, inspected.desktopEntry, "Desktop entry"),
      executable: systemInstalled
        ? inspected.mainExecutable
        : safeRelative(extraction.resultRoot, inspected.mainExecutable, "Desktop executable"),
      sidecars: inspected.sidecars.map((path) =>
        systemInstalled ? path : safeRelative(extraction.resultRoot, path, "Sidecar"),
      ),
      launch: {
        executable: systemInstalled ? launchExecutable : basename(launchExecutable),
        timeoutMs: launchTimeoutMs,
        observedStatus: 124,
      },
      systemInstalled,
    };
  } finally {
    if (!temporaryRoot.startsWith(temporaryPrefix)) {
      fail(`Refusing to remove an unexpected smoke-test directory: ${temporaryRoot}`);
    }
    rmSync(temporaryRoot, { recursive: true, force: true });
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
  if (!options.artifacts || !options.format) {
    fail(
      "Usage: linux-package-smoke.mjs --artifacts <directory> --format <appimage|deb|rpm> [--mode <extract|install>]",
    );
  }
  if (options.launchTimeoutMs !== undefined) {
    options.launchTimeoutMs = Number(options.launchTimeoutMs);
  }
  return options;
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  runLinuxPackageSmoke(parseArguments(process.argv.slice(2)))
    .then((result) => console.log(JSON.stringify(result, null, 2)))
    .catch((error) => {
      console.error(error.message);
      process.exitCode = 1;
    });
}
