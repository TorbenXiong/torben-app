import { spawn, spawnSync } from "node:child_process";
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  realpathSync,
  rmSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

import { detectExecutableTarget } from "./collect-release-artifacts.mjs";
import { verifyReleaseMetadata } from "./release-metadata.mjs";

const sidecarNames = Object.freeze([
  "torben-plugin-node",
  "torben-plugin-temurin",
  "torben-plugin-python",
  "torben-plugin-git",
  "torben-plugin-vscode",
  "torben-plugin-codex",
  "torben-shim",
]);
const maximumEntries = 20_000;
const maximumCommandOutput = 4 * 1024 * 1024;
const defaultLaunchTimeoutMs = 8_000;

function fail(message) {
  throw new Error(message);
}

function safeRelative(root, path, description) {
  const value = relative(root, path);
  if (isAbsolute(value) || value === ".." || value.startsWith(`..${sep}`)) {
    fail(`${description} escapes its validation root: ${path}`);
  }
  return value ? value.split(sep).join("/") : ".";
}

export function safeCanonicalRelative(root, path, description) {
  return safeRelative(realpathSync(root), realpathSync(path), description);
}

function regularFile(path, description) {
  if (!existsSync(path)) fail(`${description} is missing: ${path}`);
  const metadata = lstatSync(path);
  if (!metadata.isFile() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-link file: ${path}`);
  }
  return metadata;
}

function regularDirectory(path, description) {
  if (!existsSync(path)) fail(`${description} is missing: ${path}`);
  const metadata = lstatSync(path);
  if (!metadata.isDirectory() || metadata.isSymbolicLink()) {
    fail(`${description} must be a regular non-link directory: ${path}`);
  }
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

function requireCommand(result, description) {
  if (result.error) fail(`${description} could not start: ${result.error}`);
  if (result.status !== 0) {
    const diagnostic = String(result.stderr || result.stdout)
      .trim()
      .slice(-4_000);
    fail(
      `${description} failed with status ${result.status ?? "none"}${result.signal ? ` (${result.signal})` : ""}${diagnostic ? `: ${diagnostic}` : ""}`,
    );
  }
  return result.stdout.trim();
}

function hostTarget(platform, architecture) {
  const cpu = architecture === "x64" ? "x86_64" : architecture === "arm64" ? "aarch64" : null;
  if (!cpu) fail(`Unsupported desktop smoke-test host architecture: ${architecture}`);
  if (platform === "win32") return `${cpu}-pc-windows-msvc`;
  if (platform === "darwin") return `${cpu}-apple-darwin`;
  fail(`Desktop package smoke tests require Windows or macOS, received ${platform}.`);
}

function packagePath(artifactRoot, metadata, format) {
  const expectedOperatingSystem = format === "dmg" ? "macos" : "windows";
  if (metadata.operatingSystem !== expectedOperatingSystem) {
    fail(`${format} is not valid for ${metadata.operatingSystem} release metadata.`);
  }
  const suffix = format === "msi" ? ".msi" : format === "dmg" ? ".dmg" : ".exe";
  const rawCliName = `torben-${metadata.version}-${metadata.target}.exe`;
  const matches = metadata.artifacts
    .map((record) => record.path)
    .filter(
      (path) =>
        path.toLowerCase().endsWith(suffix) &&
        (format !== "nsis" || basename(path).toLowerCase() !== rawCliName.toLowerCase()),
    );
  if (matches.length !== 1) {
    fail(`Expected exactly one ${format} package in release metadata, found ${matches.length}.`);
  }
  const path = resolve(artifactRoot, matches[0]);
  safeRelative(artifactRoot, path, `${format} package`);
  regularFile(path, `${format} package`);
  return path;
}

function scanWindowsRoot(root) {
  const files = [];
  let entries = 0;
  const visit = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      entries += 1;
      if (entries > maximumEntries) {
        fail(`Installed application exceeds ${maximumEntries} filesystem entries.`);
      }
      const path = join(directory, entry.name);
      safeRelative(root, path, "Installed application entry");
      if (entry.isSymbolicLink()) {
        fail(`Installed application contains a symbolic link: ${path}`);
      }
      if (entry.isDirectory()) visit(path);
      else if (entry.isFile()) files.push(path);
      else fail(`Installed application contains an unsupported filesystem entry: ${path}`);
    }
  };
  visit(root);
  return files;
}

function executableFile(root, path, target, description, enforceExecutableMode = false) {
  const resolved = realpathSync(path);
  safeCanonicalRelative(root, resolved, description);
  const metadata = regularFile(resolved, description);
  if (enforceExecutableMode && (metadata.mode & 0o111) === 0) {
    fail(`${description} is not executable: ${resolved}`);
  }
  const detected = detectExecutableTarget(resolved);
  if (detected !== target) {
    fail(`${description} target ${detected} does not match release target ${target}.`);
  }
  return resolved;
}

function inspectWindowsInstallation(root, target) {
  regularDirectory(root, "Windows installation root");
  const files = scanWindowsRoot(root);
  const mainCandidates = files.filter(
    (path) => basename(path).toLowerCase() === "torben-desktop.exe",
  );
  if (mainCandidates.length !== 1) {
    fail(`Expected exactly one installed torben-desktop.exe, found ${mainCandidates.length}.`);
  }
  const mainExecutable = executableFile(
    root,
    mainCandidates[0],
    target,
    "Installed desktop executable",
  );
  const executableDirectory = dirname(mainExecutable);
  const sidecars = sidecarNames.map((name) => {
    const accepted = new Set(
      [`${name}.exe`, `${name}-${target}.exe`].map((value) => value.toLowerCase()),
    );
    const matches = files.filter((path) => accepted.has(basename(path).toLowerCase()));
    if (matches.length !== 1) {
      fail(`Expected exactly one installed ${name} sidecar, found ${matches.length}.`);
    }
    const executable = executableFile(root, matches[0], target, `Installed ${name} sidecar`);
    if (dirname(executable) !== executableDirectory) {
      fail(`${name} sidecar is not adjacent to the desktop executable.`);
    }
    return executable;
  });
  return { applicationRoot: root, mainExecutable, sidecars };
}

function readPlistValue(infoPath, key, execute) {
  const result = execute({
    command: "plutil",
    args: ["-extract", key, "raw", "-o", "-", infoPath],
    cwd: dirname(infoPath),
    env: process.env,
    timeoutMs: 10_000,
    stage: `plist-${key}`,
  });
  const value = requireCommand(result, `Read macOS ${key}`);
  if (!value || value.length > 1_024) fail(`macOS ${key} is empty or unexpectedly large.`);
  return value;
}

function inspectMacosInstallation(root, target, expectedVersion, execute, enforceExecutableMode) {
  regularDirectory(root, "macOS application bundle");
  if (!root.toLowerCase().endsWith(".app")) {
    fail(`macOS installation root must be an application bundle: ${root}`);
  }
  const infoPath = join(root, "Contents", "Info.plist");
  regularFile(infoPath, "macOS Info.plist");
  const identifier = readPlistValue(infoPath, "CFBundleIdentifier", execute);
  if (identifier !== "io.github.torbenxiong.torbenapp") {
    fail(`Unexpected macOS application identifier: ${identifier}`);
  }
  const version = readPlistValue(infoPath, "CFBundleShortVersionString", execute);
  if (version !== expectedVersion) {
    fail(`macOS application version ${version} does not match release version ${expectedVersion}.`);
  }
  const executableName = readPlistValue(infoPath, "CFBundleExecutable", execute);
  if (basename(executableName) !== executableName || executableName !== "torben-desktop") {
    fail(`Unexpected macOS desktop executable name: ${executableName}`);
  }
  const executableDirectory = join(root, "Contents", "MacOS");
  regularDirectory(executableDirectory, "macOS executable directory");
  const mainExecutable = executableFile(
    root,
    join(executableDirectory, executableName),
    target,
    "Installed desktop executable",
    enforceExecutableMode,
  );
  const entries = readdirSync(executableDirectory, { withFileTypes: true });
  const sidecars = sidecarNames.map((name) => {
    const accepted = new Set([name, `${name}-${target}`]);
    const matches = entries.filter((entry) => entry.isFile() && accepted.has(entry.name));
    if (matches.length !== 1) {
      fail(`Expected exactly one installed ${name} sidecar, found ${matches.length}.`);
    }
    return executableFile(
      root,
      join(executableDirectory, matches[0].name),
      target,
      `Installed ${name} sidecar`,
      enforceExecutableMode,
    );
  });
  return { applicationRoot: root, mainExecutable, sidecars };
}

function verifySignedInstallation({ inspected, packageFile, platform, execute, environment }) {
  if (platform === "win32") {
    const signaturePaths = [packageFile, inspected.mainExecutable, ...inspected.sidecars];
    const signatureScript = [
      "$ErrorActionPreference = 'Stop'",
      "$SignatureTargets = ConvertFrom-Json -InputObject $env:TORBEN_SIGNATURE_PATHS",
      "if ($SignatureTargets.Count -ne 9) { throw 'Expected one package and eight installed executables.' }",
      "$Thumbprints = @()",
      "foreach ($TargetPath in $SignatureTargets) {",
      "  $Signature = Get-AuthenticodeSignature -LiteralPath $TargetPath",
      "  if ($Signature.Status -ne 'Valid') {",
      '    throw "Invalid Authenticode signature: $TargetPath ($($Signature.Status))"',
      "  }",
      "  $Thumbprints += $Signature.SignerCertificate.Thumbprint",
      "}",
      "$Publishers = @($Thumbprints | Sort-Object -Unique)",
      "if ($Publishers.Count -ne 1) { throw 'Installed files do not share one Authenticode publisher.' }",
    ].join("\n");
    const encodedSignatureScript = Buffer.from(signatureScript, "utf16le").toString("base64");
    requireCommand(
      execute({
        command: "powershell.exe",
        args: [
          "-NoLogo",
          "-NoProfile",
          "-NonInteractive",
          "-EncodedCommand",
          encodedSignatureScript,
        ],
        cwd: inspected.applicationRoot,
        env: { ...environment, TORBEN_SIGNATURE_PATHS: JSON.stringify(signaturePaths) },
        timeoutMs: 30_000,
        stage: "windows-authenticode",
      }),
      "Windows Authenticode verification",
    );
    return;
  }
  requireCommand(
    execute({
      command: "codesign",
      args: ["--verify", "--deep", "--strict", "--verbose=2", inspected.applicationRoot],
      cwd: dirname(inspected.applicationRoot),
      env: environment,
      timeoutMs: 30_000,
      stage: "macos-codesign",
    }),
    "macOS application signature verification",
  );
  requireCommand(
    execute({
      command: "xcrun",
      args: ["stapler", "validate", packageFile],
      cwd: dirname(packageFile),
      env: environment,
      timeoutMs: 30_000,
      stage: "macos-stapler",
    }),
    "macOS notarization ticket verification",
  );
  requireCommand(
    execute({
      command: "spctl",
      args: ["--assess", "--verbose=4", "--type", "install", packageFile],
      cwd: dirname(packageFile),
      env: environment,
      timeoutMs: 30_000,
      stage: "macos-gatekeeper",
    }),
    "macOS Gatekeeper assessment",
  );
}

function isolatedLaunchEnvironment(root, platform, environment) {
  const profile = join(root, "profile");
  const directories =
    platform === "win32"
      ? {
          APPDATA: join(profile, "AppData", "Roaming"),
          LOCALAPPDATA: join(profile, "AppData", "Local"),
          TEMP: join(profile, "Temp"),
          TMP: join(profile, "Temp"),
          USERPROFILE: profile,
        }
      : {
          HOME: profile,
          TMPDIR: join(profile, "tmp"),
        };
  for (const path of new Set(Object.values(directories))) mkdirSync(path, { recursive: true });
  const allowed =
    platform === "win32"
      ? [
          "PATH",
          "Path",
          "PATHEXT",
          "SystemRoot",
          "WINDIR",
          "COMSPEC",
          "ProgramFiles",
          "ProgramFiles(x86)",
          "CommonProgramFiles",
          "CommonProgramFiles(x86)",
          "NUMBER_OF_PROCESSORS",
          "PROCESSOR_ARCHITECTURE",
        ]
      : ["PATH", "LANG", "LC_ALL", "SSL_CERT_FILE", "SSL_CERT_DIR"];
  const result = { ...directories };
  for (const name of allowed) {
    if (environment[name]) result[name] = environment[name];
  }
  return result;
}

async function sustainedLaunch({ executable, cwd, env, timeoutMs, platform }) {
  return await new Promise((resolveProbe) => {
    const child = spawn(executable, [], {
      cwd,
      env,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    let output = "";
    let settled = false;
    let timer;
    let terminationTimer;
    let launchWindowReached = false;
    const collect = (chunk) => {
      output = `${output}${chunk.toString()}`.slice(-maximumCommandOutput);
    };
    child.stdout.on("data", collect);
    child.stderr.on("data", collect);
    const finish = (result) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      if (terminationTimer) clearTimeout(terminationTimer);
      resolveProbe(result);
    };
    child.once("error", (error) => finish({ sustained: false, error: error.message, output }));
    child.once("exit", (code, signal) =>
      finish({ sustained: launchWindowReached, code, signal, output }),
    );
    timer = setTimeout(() => {
      if (settled) return;
      launchWindowReached = true;
      const processId = child.pid;
      if (platform === "win32" && processId) {
        spawnSync("taskkill.exe", ["/PID", String(processId), "/T", "/F"], {
          encoding: "utf8",
          windowsHide: true,
          timeout: 10_000,
        });
      } else {
        child.kill("SIGTERM");
      }
      terminationTimer = setTimeout(
        () =>
          finish({
            sustained: true,
            processId,
            error: "Torben App did not terminate after the sustained launch probe.",
            output,
          }),
        10_000,
      );
    }, timeoutMs);
  });
}

export async function removeTemporaryRoot(
  root,
  expectedPrefix,
  {
    remove = rmSync,
    exists = existsSync,
    wait = (milliseconds) => new Promise((resolveDelay) => setTimeout(resolveDelay, milliseconds)),
  } = {},
) {
  if (!root.startsWith(expectedPrefix)) {
    fail(`Refusing to remove an unexpected smoke-test directory: ${root}`);
  }
  const recoverableCodes = new Set(["EBUSY", "ENOTEMPTY", "EPERM"]);
  for (let attempt = 0; attempt < 20; attempt += 1) {
    try {
      remove(root, {
        recursive: true,
        force: true,
        maxRetries: 2,
        retryDelay: 100,
      });
    } catch (error) {
      if (!recoverableCodes.has(error?.code)) throw error;
    }
    await wait(250);
    if (!exists(root)) {
      await wait(500);
      if (!exists(root)) return;
    }
  }
  fail(`Smoke-test directory was recreated during process cleanup: ${root}`);
}

export async function runDesktopPackageSmoke({
  artifacts,
  format,
  installedRoot,
  launchTimeoutMs = defaultLaunchTimeoutMs,
  repositoryRoot,
  execute = executeCommand,
  launch = sustainedLaunch,
  platform = process.platform,
  architecture = process.arch,
  environment = process.env,
  enforceExecutableMode = process.platform === "darwin",
}) {
  if (!new Set(["nsis", "msi", "dmg"]).has(format)) {
    fail(`Unsupported desktop package format: ${format}`);
  }
  if (!Number.isInteger(launchTimeoutMs) || launchTimeoutMs < 2_000 || launchTimeoutMs > 30_000) {
    fail("Launch timeout must be an integer from 2000 through 30000 milliseconds.");
  }
  const artifactRoot = resolve(artifacts);
  const metadata = await verifyReleaseMetadata({ artifacts: artifactRoot, repositoryRoot });
  const expectedTarget = hostTarget(platform, architecture);
  if (metadata.target !== expectedTarget) {
    fail(`Desktop smoke-test host target ${expectedTarget} does not match ${metadata.target}.`);
  }
  const packageFile = packagePath(artifactRoot, metadata, format);
  const applicationRoot = resolve(installedRoot);
  const inspected =
    platform === "win32"
      ? inspectWindowsInstallation(applicationRoot, metadata.target)
      : inspectMacosInstallation(
          applicationRoot,
          metadata.target,
          metadata.version,
          execute,
          enforceExecutableMode,
        );
  const signatureRequired = metadata.signingStatus === "signed";
  if (signatureRequired) {
    verifySignedInstallation({ inspected, packageFile, platform, execute, environment });
  }
  const temporaryPrefix = join(tmpdir(), "torben-desktop-package-smoke-");
  const temporaryRoot = mkdtempSync(temporaryPrefix);
  try {
    const probe = await launch({
      executable: inspected.mainExecutable,
      cwd: dirname(inspected.mainExecutable),
      env: isolatedLaunchEnvironment(temporaryRoot, platform, environment),
      timeoutMs: launchTimeoutMs,
      platform,
    });
    if (!probe?.sustained) {
      const diagnostic = String(probe?.error || probe?.output || "")
        .trim()
        .slice(-4_000);
      fail(
        `Torben App exited before the ${launchTimeoutMs}ms launch window${probe?.code !== undefined ? ` with code ${String(probe.code)}` : ""}${diagnostic ? `: ${diagnostic}` : ""}`,
      );
    }
    if (probe.error) {
      const diagnostic = String(probe.error).trim().slice(-4_000);
      fail(
        `Torben App survived the ${launchTimeoutMs}ms launch window but cleanup failed${diagnostic ? `: ${diagnostic}` : ""}`,
      );
    }
    return {
      schemaVersion: 1,
      ok: true,
      format,
      target: metadata.target,
      package: basename(packageFile),
      applicationRoot,
      executable: safeCanonicalRelative(
        applicationRoot,
        inspected.mainExecutable,
        "Desktop executable",
      ),
      sidecars: inspected.sidecars.map((path) =>
        safeCanonicalRelative(applicationRoot, path, "Sidecar"),
      ),
      signing: { status: metadata.signingStatus, verified: signatureRequired },
      launch: { timeoutMs: launchTimeoutMs, sustained: true },
    };
  } finally {
    await removeTemporaryRoot(temporaryRoot, temporaryPrefix);
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
  for (const name of ["artifacts", "format", "installedRoot"]) {
    if (!options[name])
      fail(`--${name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`)} is required.`);
  }
  if (options.launchTimeoutMs !== undefined) {
    options.launchTimeoutMs = Number(options.launchTimeoutMs);
  }
  return options;
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  runDesktopPackageSmoke(parseArguments(process.argv.slice(2)))
    .then((result) => console.log(JSON.stringify(result, null, 2)))
    .catch((error) => {
      console.error(error.message);
      process.exitCode = 1;
    });
}
