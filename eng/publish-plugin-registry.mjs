import { createHash, createPrivateKey, createPublicKey, randomUUID, sign } from "node:crypto";
import {
  closeSync,
  copyFileSync,
  existsSync,
  lstatSync,
  mkdirSync,
  openSync,
  readFileSync,
  readSync,
  realpathSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const registrySchemaVersion = 1;
const maximumSafeSequence = Number.MAX_SAFE_INTEGER;
const maximumConfigBytes = 1024 * 1024;
const maximumManifestBytes = 1024 * 1024;
const maximumExecutableBytes = 512 * 1024 * 1024;
const maximumPortablePathLength = 4096;
const maximumPortableComponentLength = 255;
const identifierPattern = /^[a-z0-9._-]{1,128}$/u;
const publisherIdentifierPattern = /^[a-z0-9._-]{1,128}$/u;
const timestampPattern = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/u;
const semanticVersionPattern =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-((?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*)(?:\.(?:0|[1-9]\d*|\d*[A-Za-z-][0-9A-Za-z-]*))*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/u;
const capabilities = new Set([
  "version_discovery",
  "external_discovery",
  "managed_install",
  "global_selection",
  "managed_uninstall",
  "schema_ui",
]);
const filesystemRoots = new Set([
  "managed_app_library",
  "download_cache",
  "staging",
  "plugin_data",
]);
const packageManagers = new Set(["winget", "homebrew", "apt", "dnf"]);
const supportedTargets = new Set([
  "windows-x86_64",
  "windows-aarch64",
  "linux-x86_64",
  "linux-aarch64",
  "macos-x86_64",
  "macos-aarch64",
]);
const spkiPrefix = Buffer.from("302a300506032b6570032100", "hex");

function fail(message) {
  throw new Error(message);
}

function requireObject(value, label) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    fail(`${label} must be a JSON object.`);
  }
  return value;
}

function requireExactKeys(value, keys, label) {
  const expected = new Set(keys);
  for (const key of Object.keys(value)) {
    if (!expected.has(key)) fail(`${label} contains an unsupported field: ${key}.`);
  }
  for (const key of keys) {
    if (!(key in value)) fail(`${label} is missing the required field: ${key}.`);
  }
}

function requireString(value, label, { allowEmpty = false } = {}) {
  if (typeof value !== "string" || (!allowEmpty && value.trim().length === 0)) {
    fail(`${label} must be ${allowEmpty ? "a string" : "a non-empty string"}.`);
  }
  if (value.includes("\0")) fail(`${label} must not contain NUL characters.`);
  return value;
}

function requireBoolean(value, label) {
  if (typeof value !== "boolean") fail(`${label} must be a boolean.`);
  return value;
}

function requireArray(value, label) {
  if (!Array.isArray(value)) fail(`${label} must be an array.`);
  return value;
}

function requireIdentifier(value, label, pattern = identifierPattern) {
  const identifier = requireString(value, label);
  if (!pattern.test(identifier)) fail(`${label} is not a valid identifier.`);
  return identifier;
}

function requireVersion(value, label) {
  const version = requireString(value, label);
  if (!semanticVersionPattern.test(version)) fail(`${label} must be a canonical semantic version.`);
  return version;
}

function requireTimestamp(value) {
  const timestamp = requireString(value, "config.generatedAt");
  const parsed = Date.parse(timestamp);
  if (
    !timestampPattern.test(timestamp) ||
    Number.isNaN(parsed) ||
    new Date(parsed).toISOString().replace(".000Z", "Z") !== timestamp
  ) {
    fail("config.generatedAt must be a valid UTC timestamp with whole-second precision.");
  }
  return timestamp;
}

function requireSafeRelativePath(value, label) {
  const path = requireString(value, label);
  const components = path.split("/");
  if (
    path.length > maximumPortablePathLength ||
    isAbsolute(path) ||
    path.includes("\\") ||
    path.startsWith("/") ||
    path.endsWith("/") ||
    components.some((component) => !portablePathComponent(component))
  ) {
    fail(`${label} must be a safe POSIX relative path.`);
  }
  return path;
}

function portablePathComponent(component) {
  if (
    component.length === 0 ||
    component.length > maximumPortableComponentLength ||
    component.startsWith(" ") ||
    component.startsWith(".") ||
    component.endsWith(" ") ||
    component.endsWith(".") ||
    !/^[0-9A-Za-z._+ -]+$/u.test(component)
  ) {
    return false;
  }
  const base = component.split(".", 1)[0].toUpperCase();
  if (["CON", "PRN", "AUX", "NUL", "CLOCK$", "CONIN$", "CONOUT$"].includes(base)) {
    return false;
  }
  return !/^(?:COM|LPT)[1-9]$/u.test(base);
}

function isWithin(parent, child) {
  const path = relative(parent, child);
  return path === "" || (!path.startsWith(`..${sep}`) && path !== ".." && !isAbsolute(path));
}

function requireRegularFile(path, label, maximumBytes) {
  let metadata;
  try {
    metadata = lstatSync(path);
  } catch (error) {
    fail(`${label} could not be inspected: ${error.message}`);
  }
  if (metadata.isSymbolicLink() || !metadata.isFile())
    fail(`${label} must be a regular non-symbolic-link file.`);
  if (maximumBytes !== undefined && metadata.size > maximumBytes) {
    fail(`${label} exceeds the ${maximumBytes}-byte limit.`);
  }
  return metadata;
}

function resolveSourceFile(sourceRoot, relativePath, label, maximumBytes) {
  const path = resolve(sourceRoot, ...relativePath.split("/"));
  if (!isWithin(sourceRoot, path)) fail(`${label} escapes the source directory.`);
  requireRegularFile(path, label, maximumBytes);
  const real = realpathSync.native(path);
  if (!isWithin(realpathSync.native(sourceRoot), real))
    fail(`${label} resolves outside the source directory.`);
  return path;
}

function readJson(path, label, maximumBytes) {
  requireRegularFile(path, label, maximumBytes);
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    fail(`${label} is not valid JSON: ${error.message}`);
  }
}

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

function sha256File(path) {
  requireRegularFile(path, "Plugin executable", maximumExecutableBytes);
  const digest = createHash("sha256");
  const buffer = Buffer.allocUnsafe(1024 * 1024);
  const handle = openSync(path, "r");
  try {
    for (;;) {
      const bytesRead = readSync(handle, buffer, 0, buffer.length, null);
      if (bytesRead === 0) break;
      digest.update(buffer.subarray(0, bytesRead));
    }
  } finally {
    closeSync(handle);
  }
  return digest.digest("hex");
}

function loadPrivateKey(path, label) {
  requireRegularFile(path, label, 64 * 1024);
  let key;
  try {
    key = createPrivateKey(readFileSync(path));
  } catch (error) {
    fail(`${label} is not a readable private key: ${error.message}`);
  }
  if (key.asymmetricKeyType !== "ed25519") fail(`${label} must be an Ed25519 private key.`);
  return key;
}

function publicKeyBase64(privateKey) {
  const bytes = createPublicKey(privateKey).export({ format: "der", type: "spki" });
  if (
    bytes.length !== spkiPrefix.length + 32 ||
    !bytes.subarray(0, spkiPrefix.length).equals(spkiPrefix)
  ) {
    fail("Could not export the Ed25519 public key in the expected SPKI format.");
  }
  return bytes.subarray(spkiPrefix.length).toString("base64");
}

function signValue(value, privateKey) {
  return sign(null, Buffer.from(JSON.stringify(value)), privateKey).toString("base64");
}

function requireUniqueStrings(values, label, validator) {
  const seen = new Set();
  return requireArray(values, label).map((value, index) => {
    const item = requireString(value, `${label}[${index}]`);
    if (!validator(item)) fail(`${label}[${index}] is invalid: ${item}.`);
    if (seen.has(item)) fail(`${label} contains a duplicate value: ${item}.`);
    seen.add(item);
    return item;
  });
}

function validNetworkDomain(value) {
  return (
    value === value.toLowerCase() &&
    value.length <= 253 &&
    !value.startsWith(".") &&
    !value.endsWith(".") &&
    value
      .split(".")
      .every(
        (label) =>
          label.length > 0 &&
          label.length <= 63 &&
          !label.startsWith("-") &&
          !label.endsWith("-") &&
          /^[a-z0-9-]+$/u.test(label),
      )
  );
}

function validExternalCommand(value) {
  return (
    value !== "." && value !== ".." && value.length <= 253 && /^[0-9A-Za-z._+-]+$/u.test(value)
  );
}

function normalizePermissions(value, label) {
  const permissions = requireObject(value, label);
  requireExactKeys(
    permissions,
    ["networkDomains", "filesystemRoots", "externalCommands", "packageManagers"],
    label,
  );
  const bounded = (items, itemLabel, validator) => {
    const result = requireUniqueStrings(items, itemLabel, validator);
    if (result.length > 64) fail(`${itemLabel} contains more than 64 values.`);
    return result;
  };
  return {
    networkDomains: bounded(
      permissions.networkDomains,
      `${label}.networkDomains`,
      validNetworkDomain,
    ),
    filesystemRoots: bounded(permissions.filesystemRoots, `${label}.filesystemRoots`, (item) =>
      filesystemRoots.has(item),
    ),
    externalCommands: bounded(
      permissions.externalCommands,
      `${label}.externalCommands`,
      validExternalCommand,
    ),
    packageManagers: bounded(permissions.packageManagers, `${label}.packageManagers`, (item) =>
      packageManagers.has(item),
    ),
  };
}

function normalizeManifest(source, label, publisher, sourceRoot, manifestPath, stagingRoot) {
  const manifest = requireObject(source, label);
  requireExactKeys(
    manifest,
    [
      "id",
      "displayName",
      "version",
      "protocolVersion",
      "minimumHostVersion",
      "publisher",
      "capabilities",
      "permissions",
      "targets",
      "signature",
      "revoked",
    ],
    label,
  );
  const publisherName = requireString(manifest.publisher, `${label}.publisher`);
  if (publisherName !== publisher.displayName) {
    fail(`${label}.publisher must match publisher ${publisher.id}'s displayName.`);
  }
  const manifestDirectory = manifestPath.split("/").slice(0, -1).join("/");
  const seenTargets = new Set();
  const seenExecutables = new Set();
  const targets = requireArray(manifest.targets, `${label}.targets`).map((targetValue, index) => {
    const targetLabel = `${label}.targets[${index}]`;
    const target = requireObject(targetValue, targetLabel);
    requireExactKeys(target, ["target", "executable", "sha256"], targetLabel);
    requireString(target.sha256, `${targetLabel}.sha256`, { allowEmpty: true });
    const targetName = requireString(target.target, `${targetLabel}.target`);
    if (!supportedTargets.has(targetName))
      fail(`${targetLabel}.target is not supported: ${targetName}.`);
    if (seenTargets.has(targetName))
      fail(`${label}.targets contains duplicate target ${targetName}.`);
    seenTargets.add(targetName);
    const executable = requireSafeRelativePath(target.executable, `${targetLabel}.executable`);
    const foldedExecutable = executable.toLowerCase();
    if (seenExecutables.has(foldedExecutable)) {
      fail(`${label}.targets reuses an executable path: ${executable}.`);
    }
    seenExecutables.add(foldedExecutable);
    const sourceExecutablePath = `${manifestDirectory}/${executable}`;
    const sourceExecutable = resolveSourceFile(
      sourceRoot,
      sourceExecutablePath,
      `${targetLabel}.executable file`,
    );
    const outputExecutable = resolve(stagingRoot, ...sourceExecutablePath.split("/"));
    mkdirSync(dirname(outputExecutable), { recursive: true });
    copyFileSync(sourceExecutable, outputExecutable);
    return { target: targetName, executable, sha256: sha256File(outputExecutable) };
  });
  if (targets.length === 0) fail(`${label}.targets must not be empty.`);

  const normalized = {
    id: requireIdentifier(manifest.id, `${label}.id`),
    displayName: requireString(manifest.displayName, `${label}.displayName`),
    version: requireVersion(manifest.version, `${label}.version`),
    protocolVersion: manifest.protocolVersion,
    minimumHostVersion: requireVersion(manifest.minimumHostVersion, `${label}.minimumHostVersion`),
    publisher: publisherName,
    capabilities: requireUniqueStrings(manifest.capabilities, `${label}.capabilities`, (item) =>
      capabilities.has(item),
    ),
    permissions: normalizePermissions(manifest.permissions, `${label}.permissions`),
    targets,
    signature: null,
    revoked: requireBoolean(manifest.revoked, `${label}.revoked`),
  };
  if (manifest.signature !== null && typeof manifest.signature !== "string") {
    fail(`${label}.signature must be a string or null.`);
  }
  if (normalized.protocolVersion !== 1) fail(`${label}.protocolVersion must be 1.`);
  normalized.signature = signValue(normalized, publisher.privateKey);
  return normalized;
}

function normalizeConfig(value) {
  const config = requireObject(value, "config");
  requireExactKeys(
    config,
    ["schemaVersion", "sequence", "generatedAt", "minimumHostVersion", "publishers", "packages"],
    "config",
  );
  if (config.schemaVersion !== registrySchemaVersion) {
    fail(`config.schemaVersion must be ${registrySchemaVersion}.`);
  }
  if (
    !Number.isSafeInteger(config.sequence) ||
    config.sequence < 1 ||
    config.sequence > maximumSafeSequence
  ) {
    fail(`config.sequence must be an integer between 1 and ${maximumSafeSequence}.`);
  }
  return {
    schemaVersion: config.schemaVersion,
    sequence: config.sequence,
    generatedAt: requireTimestamp(config.generatedAt),
    minimumHostVersion: requireVersion(config.minimumHostVersion, "config.minimumHostVersion"),
    publishers: requireArray(config.publishers, "config.publishers"),
    packages: requireArray(config.packages, "config.packages"),
  };
}

export function publishPluginRegistry({
  configPath,
  sourceDirectory,
  outputDirectory,
  rootKeyPath,
  publisherKeyPaths,
  emitRootPublicKey = false,
}) {
  const configFile = resolve(configPath);
  const sourceRoot = resolve(sourceDirectory);
  const outputRoot = resolve(outputDirectory);
  const rootPrivateKeyPath = resolve(rootKeyPath);
  requireRegularFile(configFile, "Registry publishing config", maximumConfigBytes);
  const sourceMetadata = lstatSync(sourceRoot);
  if (sourceMetadata.isSymbolicLink() || !sourceMetadata.isDirectory()) {
    fail("The source directory must be a regular non-symbolic-link directory.");
  }
  if (existsSync(outputRoot)) fail("The output directory already exists.");
  if (isWithin(sourceRoot, outputRoot))
    fail("The output directory must not be inside the source directory.");
  const outputParent = dirname(outputRoot);
  const parentMetadata = lstatSync(outputParent);
  if (parentMetadata.isSymbolicLink() || !parentMetadata.isDirectory()) {
    fail("The output parent must be a regular non-symbolic-link directory.");
  }

  const config = normalizeConfig(
    readJson(configFile, "Registry publishing config", maximumConfigBytes),
  );
  const rootKey = loadPrivateKey(rootPrivateKeyPath, "Registry root key");
  const rootPublicKey = publicKeyBase64(rootKey);
  const keyMappings =
    publisherKeyPaths instanceof Map ? publisherKeyPaths : new Map(publisherKeyPaths);
  const publisherIds = new Set();
  const publisherPublicKeys = new Set();
  const publisherRecords = new Map();
  for (const [index, publisherValue] of config.publishers.entries()) {
    const label = `config.publishers[${index}]`;
    const publisher = requireObject(publisherValue, label);
    requireExactKeys(publisher, ["id", "displayName", "revoked"], label);
    const id = requireIdentifier(publisher.id, `${label}.id`, publisherIdentifierPattern);
    if (publisherIds.has(id)) fail(`config.publishers contains duplicate publisher ${id}.`);
    publisherIds.add(id);
    const keyPath = keyMappings.get(id);
    if (!keyPath) fail(`No private key was provided for publisher ${id}.`);
    const privateKey = loadPrivateKey(resolve(keyPath), `Publisher key ${id}`);
    const publicKey = publicKeyBase64(privateKey);
    if (publicKey === rootPublicKey) {
      fail(`Publisher ${id} must not reuse the registry root key.`);
    }
    if (publisherPublicKeys.has(publicKey)) {
      fail(`Publisher ${id} reuses another publisher's key.`);
    }
    publisherPublicKeys.add(publicKey);
    publisherRecords.set(id, {
      id,
      displayName: requireString(publisher.displayName, `${label}.displayName`),
      publicKey,
      revoked: requireBoolean(publisher.revoked, `${label}.revoked`),
      privateKey,
    });
  }
  for (const id of keyMappings.keys()) {
    if (!publisherIds.has(id)) fail(`A private key was provided for unknown publisher ${id}.`);
  }

  const stagingRoot = resolve(
    outputParent,
    `.${outputRoot.split(sep).at(-1)}.staging-${randomUUID()}`,
  );
  mkdirSync(stagingRoot);
  try {
    const entries = [];
    const pluginVersions = new Set();
    const packageDirectories = new Set();
    for (const [index, packageValue] of config.packages.entries()) {
      const label = `config.packages[${index}]`;
      const packageConfig = requireObject(packageValue, label);
      requireExactKeys(packageConfig, ["manifestPath", "publisherId", "revoked"], label);
      const manifestPath = requireSafeRelativePath(
        packageConfig.manifestPath,
        `${label}.manifestPath`,
      );
      if (!manifestPath.endsWith("/plugin.json")) {
        fail(
          `${label}.manifestPath must end with /plugin.json inside a version-specific directory.`,
        );
      }
      const packageDirectory = manifestPath.slice(0, -"/plugin.json".length).toLowerCase();
      if (packageDirectories.has(packageDirectory))
        fail(`config.packages reuses ${packageDirectory}.`);
      packageDirectories.add(packageDirectory);
      const publisherId = requireIdentifier(
        packageConfig.publisherId,
        `${label}.publisherId`,
        publisherIdentifierPattern,
      );
      const publisher = publisherRecords.get(publisherId);
      if (!publisher) fail(`${label} references unknown publisher ${publisherId}.`);
      const sourceManifest = resolveSourceFile(
        sourceRoot,
        manifestPath,
        `${label} manifest`,
        maximumManifestBytes,
      );
      const normalizedManifest = normalizeManifest(
        readJson(sourceManifest, `${label} manifest`, maximumManifestBytes),
        `${label} manifest`,
        publisher,
        sourceRoot,
        manifestPath,
        stagingRoot,
      );
      const identity = `${normalizedManifest.id}@${normalizedManifest.version}`;
      if (pluginVersions.has(identity))
        fail(`config.packages contains duplicate plugin version ${identity}.`);
      pluginVersions.add(identity);
      const manifestDirectoryVersion = manifestPath.split("/").at(-2);
      if (manifestDirectoryVersion !== normalizedManifest.version) {
        fail(
          `${label}.manifestPath must use the exact manifest version directory ${normalizedManifest.version}.`,
        );
      }
      const manifestBytes = Buffer.from(`${JSON.stringify(normalizedManifest, null, 2)}\n`);
      const outputManifest = resolve(stagingRoot, ...manifestPath.split("/"));
      mkdirSync(dirname(outputManifest), { recursive: true });
      writeFileSync(outputManifest, manifestBytes);
      entries.push({
        pluginId: normalizedManifest.id,
        version: normalizedManifest.version,
        publisherId,
        manifestPath,
        manifestSha256: sha256(manifestBytes),
        revoked: requireBoolean(packageConfig.revoked, `${label}.revoked`),
      });
    }

    const registry = {
      schemaVersion: config.schemaVersion,
      sequence: config.sequence,
      generatedAt: config.generatedAt,
      minimumHostVersion: config.minimumHostVersion,
      publishers: [...publisherRecords.values()].map((publisher) => ({
        id: publisher.id,
        displayName: publisher.displayName,
        publicKey: publisher.publicKey,
        revoked: publisher.revoked,
      })),
      entries,
      signature: null,
    };
    registry.signature = signValue(registry, rootKey);
    writeFileSync(resolve(stagingRoot, "registry.json"), `${JSON.stringify(registry, null, 2)}\n`);
    if (emitRootPublicKey) {
      writeFileSync(resolve(stagingRoot, "registry-root-public-key.txt"), `${rootPublicKey}\n`);
    }
    renameSync(stagingRoot, outputRoot);
    return {
      outputDirectory: outputRoot,
      registryPath: resolve(outputRoot, "registry.json"),
      rootPublicKey,
      publisherCount: registry.publishers.length,
      packageCount: registry.entries.length,
    };
  } catch (error) {
    if (existsSync(stagingRoot)) rmSync(stagingRoot, { recursive: true, force: true });
    throw error;
  }
}

function parseArguments(arguments_) {
  const options = { publisherKeyPaths: new Map(), emitRootPublicKey: false };
  const valued = new Set(["--config", "--source", "--output", "--root-key", "--publisher-key"]);
  for (let index = 0; index < arguments_.length; index += 1) {
    const argument = arguments_[index];
    if (argument === "--emit-root-public-key") {
      options.emitRootPublicKey = true;
      continue;
    }
    if (!valued.has(argument)) fail(`Unknown argument: ${argument}`);
    const value = arguments_[index + 1];
    if (!value || value.startsWith("--")) fail(`Missing value for ${argument}.`);
    index += 1;
    if (argument === "--publisher-key") {
      const separatorIndex = value.indexOf("=");
      if (separatorIndex < 1 || separatorIndex === value.length - 1) {
        fail("--publisher-key must use <publisher-id>=<private-key-path>.");
      }
      const id = value.slice(0, separatorIndex);
      if (options.publisherKeyPaths.has(id)) fail(`Duplicate --publisher-key mapping for ${id}.`);
      options.publisherKeyPaths.set(id, value.slice(separatorIndex + 1));
    } else {
      const name =
        argument === "--root-key"
          ? "rootKeyPath"
          : `${argument.slice(2).replace(/-([a-z])/gu, (_, letter) => letter.toUpperCase())}${
              argument === "--source" || argument === "--output" ? "Directory" : "Path"
            }`;
      options[name] = value;
    }
  }
  for (const name of ["configPath", "sourceDirectory", "outputDirectory", "rootKeyPath"]) {
    if (!options[name]) {
      fail(
        "Usage: publish-plugin-registry.mjs --config <file> --source <directory> --output <new-directory> --root-key <pem> --publisher-key <id>=<pem> [--emit-root-public-key]",
      );
    }
  }
  return options;
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  try {
    const result = publishPluginRegistry(parseArguments(process.argv.slice(2)));
    console.log(
      `Published ${result.packageCount} plugin package(s) from ${result.publisherCount} publisher(s) to ${result.outputDirectory}.`,
    );
    console.log(`Registry root public key: ${result.rootPublicKey}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
