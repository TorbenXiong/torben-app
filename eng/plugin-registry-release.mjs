import { createHash, createPublicKey, verify as verifySignature } from "node:crypto";
import {
  chmodSync,
  existsSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, isAbsolute, relative, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";

const maximumJsonBytes = 1024 * 1024;
const maximumPrivateKeyBytes = 64 * 1024;
const maximumSafeSequence = 9_007_199_254_740_991;
const portableComponentPattern = /^(?![. ])(?!.*[. ]$)[A-Za-z0-9][A-Za-z0-9._-]*$/u;
const publisherIdentifierPattern = /^[a-z0-9]+(?:[.-][a-z0-9]+)*$/u;
const sha256Pattern = /^[0-9a-f]{64}$/u;
const versionPattern = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z.-]+)?$/u;
const windowsDevicePattern = /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\..*)?$/iu;
const ed25519SpkiPrefix = Buffer.from("302a300506032b6570032100", "hex");

function fail(message) {
  throw new Error(message);
}

function requireObject(value, label) {
  if (value === null || typeof value !== "object" || Array.isArray(value)) {
    fail(`${label} must be an object.`);
  }
  return value;
}

function requireExactKeys(value, expected, label) {
  const actual = Object.keys(value).sort();
  const wanted = [...expected].sort();
  if (actual.length !== wanted.length || actual.some((key, index) => key !== wanted[index])) {
    fail(`${label} has unexpected or missing fields.`);
  }
}

function requireString(value, label) {
  if (typeof value !== "string" || value.length === 0 || value.trim() !== value) {
    fail(`${label} must be a non-empty trimmed string.`);
  }
  return value;
}

function requireBoolean(value, label) {
  if (typeof value !== "boolean") fail(`${label} must be a boolean.`);
  return value;
}

function requireVersion(value, label) {
  const version = requireString(value, label);
  if (!versionPattern.test(version)) fail(`${label} must be an exact semantic version.`);
  return version;
}

function requireTimestamp(value, label) {
  const timestamp = requireString(value, label);
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/u.test(timestamp)) {
    fail(`${label} must use whole-second UTC form.`);
  }
  const parsed = new Date(timestamp);
  if (Number.isNaN(parsed.valueOf()) || parsed.toISOString().replace(".000Z", "Z") !== timestamp) {
    fail(`${label} must be a valid UTC timestamp.`);
  }
  return timestamp;
}

function requireSequence(value, label) {
  const sequence = typeof value === "string" && /^\d+$/u.test(value) ? Number(value) : value;
  if (!Number.isSafeInteger(sequence) || sequence < 1 || sequence > maximumSafeSequence) {
    fail(`${label} must be an integer between 1 and ${maximumSafeSequence}.`);
  }
  return sequence;
}

function requireSha256(value, label) {
  const hash = requireString(value, label);
  if (!sha256Pattern.test(hash)) fail(`${label} must be a lowercase SHA-256 value.`);
  return hash;
}

function requireSafeRelativePath(value, label) {
  const path = requireString(value, label);
  if (isAbsolute(path) || path.includes("\\") || path.includes(":")) {
    fail(`${label} must be a safe POSIX relative path.`);
  }
  const components = path.split("/");
  if (
    path.length > 4096 ||
    components.some(
      (component) =>
        component.length === 0 ||
        component.length > 255 ||
        component === "." ||
        component === ".." ||
        !portableComponentPattern.test(component) ||
        windowsDevicePattern.test(component),
    )
  ) {
    fail(`${label} must be a safe POSIX relative path.`);
  }
  return path;
}

function requireRegularFile(path, label, maximumBytes = Number.POSITIVE_INFINITY) {
  const metadata = lstatSync(path);
  if (metadata.isSymbolicLink() || !metadata.isFile()) {
    fail(`${label} must be a regular non-symbolic-link file.`);
  }
  if (metadata.size > maximumBytes) fail(`${label} exceeds the maximum allowed size.`);
  return path;
}

function readJson(path, label, maximumBytes = maximumJsonBytes) {
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

function publicKeyFromBase64(encoded, label) {
  const value = requireString(encoded, label);
  const raw = Buffer.from(value, "base64");
  if (raw.length !== 32 || raw.toString("base64") !== value) {
    fail(`${label} must be a canonical Base64 Ed25519 public key.`);
  }
  try {
    return createPublicKey({
      key: Buffer.concat([ed25519SpkiPrefix, raw]),
      format: "der",
      type: "spki",
    });
  } catch (error) {
    fail(`${label} is not a valid Ed25519 public key: ${error.message}`);
  }
}

function verifySignedValue(value, publicKey, label) {
  const signature = requireString(value.signature, `${label}.signature`);
  const signatureBytes = Buffer.from(signature, "base64");
  if (signatureBytes.length !== 64 || signatureBytes.toString("base64") !== signature) {
    fail(`${label}.signature must be a canonical Base64 Ed25519 signature.`);
  }
  const unsigned = { ...value, signature: null };
  if (!verifySignature(null, Buffer.from(JSON.stringify(unsigned)), publicKey, signatureBytes)) {
    fail(`${label} signature is invalid.`);
  }
}

function isWithin(root, path) {
  const child = relative(resolve(root), resolve(path));
  return child === "" || (!child.startsWith(`..${sep}`) && child !== ".." && !isAbsolute(child));
}

function resolveArtifactFile(root, path, label, maximumBytes = Number.POSITIVE_INFINITY) {
  const candidate = resolve(root, ...requireSafeRelativePath(path, label).split("/"));
  if (!isWithin(root, candidate)) fail(`${label} escapes the registry artifact root.`);
  return requireRegularFile(candidate, label, maximumBytes);
}

function normalizeConfig(value) {
  const config = requireObject(value, "config");
  requireExactKeys(
    config,
    ["schemaVersion", "sequence", "generatedAt", "minimumHostVersion", "publishers", "packages"],
    "config",
  );
  if (config.schemaVersion !== 1) fail("config.schemaVersion must be 1.");
  if (!Array.isArray(config.publishers) || !Array.isArray(config.packages)) {
    fail("config publishers and packages must be arrays.");
  }
  const publisherIds = new Set();
  const publishers = config.publishers.map((value, index) => {
    const label = `config.publishers[${index}]`;
    const publisher = requireObject(value, label);
    requireExactKeys(publisher, ["id", "displayName", "revoked"], label);
    const id = requireString(publisher.id, `${label}.id`);
    if (!publisherIdentifierPattern.test(id) || publisherIds.has(id)) {
      fail(`${label}.id must be a unique publisher identifier.`);
    }
    publisherIds.add(id);
    return {
      id,
      displayName: requireString(publisher.displayName, `${label}.displayName`),
      revoked: requireBoolean(publisher.revoked, `${label}.revoked`),
    };
  });
  const manifestPaths = new Set();
  const packages = config.packages.map((value, index) => {
    const label = `config.packages[${index}]`;
    const packageConfig = requireObject(value, label);
    requireExactKeys(packageConfig, ["manifestPath", "publisherId", "revoked"], label);
    const manifestPath = requireSafeRelativePath(
      packageConfig.manifestPath,
      `${label}.manifestPath`,
    );
    const folded = manifestPath.toLowerCase();
    if (!manifestPath.endsWith("/plugin.json") || manifestPaths.has(folded)) {
      fail(`${label}.manifestPath must be a unique version-specific plugin.json path.`);
    }
    manifestPaths.add(folded);
    const publisherId = requireString(packageConfig.publisherId, `${label}.publisherId`);
    if (!publisherIds.has(publisherId)) fail(`${label} references an unknown publisher.`);
    return {
      manifestPath,
      publisherId,
      revoked: requireBoolean(packageConfig.revoked, `${label}.revoked`),
    };
  });
  return {
    schemaVersion: 1,
    sequence: requireSequence(config.sequence, "config.sequence"),
    generatedAt: requireTimestamp(config.generatedAt, "config.generatedAt"),
    minimumHostVersion: requireVersion(config.minimumHostVersion, "config.minimumHostVersion"),
    publishers,
    packages,
  };
}

export function materializePluginRegistryKeys({
  configPath,
  outputDirectory,
  rootPrivateKey,
  publisherPrivateKeysJson,
}) {
  const config = normalizeConfig(readJson(resolve(configPath), "Registry publishing config"));
  const outputRoot = resolve(outputDirectory);
  if (existsSync(outputRoot)) fail("The private-key output directory already exists.");
  const parent = dirname(outputRoot);
  const parentMetadata = lstatSync(parent);
  if (parentMetadata.isSymbolicLink() || !parentMetadata.isDirectory()) {
    fail("The private-key output parent must be a regular non-symbolic-link directory.");
  }
  const rootKey = requireString(rootPrivateKey, "Registry root private key");
  if (Buffer.byteLength(rootKey) > maximumPrivateKeyBytes) {
    fail("Registry root private key exceeds the maximum allowed size.");
  }
  if (Buffer.byteLength(publisherPrivateKeysJson ?? "") > maximumJsonBytes) {
    fail("Publisher private-key mapping exceeds the maximum allowed size.");
  }
  let mappings;
  try {
    mappings = requireObject(JSON.parse(publisherPrivateKeysJson), "Publisher private-key mapping");
  } catch (error) {
    fail(`Publisher private-key mapping is not valid JSON: ${error.message}`);
  }
  const expectedIds = config.publishers.map((publisher) => publisher.id).sort();
  const actualIds = Object.keys(mappings).sort();
  if (
    actualIds.length !== expectedIds.length ||
    actualIds.some((publisherId, index) => publisherId !== expectedIds[index])
  ) {
    fail(
      "Publisher private-key mapping must contain exactly the configured publisher identifiers.",
    );
  }
  mkdirSync(outputRoot, { mode: 0o700 });
  try {
    const rootKeyPath = resolve(outputRoot, "registry-root.pem");
    writeFileSync(rootKeyPath, rootKey, { mode: 0o600 });
    chmodSync(rootKeyPath, 0o600);
    const publisherArguments = [];
    for (const publisher of config.publishers) {
      const privateKey = requireString(
        mappings[publisher.id],
        `Publisher ${publisher.id} private key`,
      );
      if (Buffer.byteLength(privateKey) > maximumPrivateKeyBytes) {
        fail(`Publisher ${publisher.id} private key exceeds the maximum allowed size.`);
      }
      const keyPath = resolve(outputRoot, `publisher-${publisher.id}.pem`);
      writeFileSync(keyPath, privateKey, { mode: 0o600 });
      chmodSync(keyPath, 0o600);
      publisherArguments.push("--publisher-key", `${publisher.id}=${keyPath}`);
    }
    const argumentsPath = resolve(outputRoot, "publisher-arguments.json");
    writeFileSync(argumentsPath, `${JSON.stringify(publisherArguments)}\n`, { mode: 0o600 });
    chmodSync(argumentsPath, 0o600);
    return { rootKeyPath, publisherArguments, argumentsPath };
  } catch (error) {
    rmSync(outputRoot, { recursive: true, force: true });
    throw error;
  }
}

function validateRegistryShape(registry, label) {
  const value = requireObject(registry, label);
  requireExactKeys(
    value,
    [
      "schemaVersion",
      "sequence",
      "generatedAt",
      "minimumHostVersion",
      "publishers",
      "entries",
      "signature",
    ],
    label,
  );
  if (value.schemaVersion !== 1) fail(`${label}.schemaVersion must be 1.`);
  if (!Array.isArray(value.publishers) || !Array.isArray(value.entries)) {
    fail(`${label} publishers and entries must be arrays.`);
  }
  requireSequence(value.sequence, `${label}.sequence`);
  requireTimestamp(value.generatedAt, `${label}.generatedAt`);
  requireVersion(value.minimumHostVersion, `${label}.minimumHostVersion`);
  return value;
}

export function validateRegistrySequence(current, previous) {
  const currentSequence = requireSequence(current.sequence, "Current registry sequence");
  if (previous === undefined) {
    if (currentSequence !== 1) fail("A previous signed registry is required after sequence 1.");
    return;
  }
  const previousSequence = requireSequence(previous.sequence, "Previous registry sequence");
  if (currentSequence !== previousSequence + 1) {
    fail(
      `Registry sequence must advance exactly once from ${previousSequence} to ${previousSequence + 1}.`,
    );
  }
  const currentGeneratedAt = requireTimestamp(current.generatedAt, "Current registry generatedAt");
  const previousGeneratedAt = requireTimestamp(
    previous.generatedAt,
    "Previous registry generatedAt",
  );
  if (currentGeneratedAt <= previousGeneratedAt) {
    fail("Registry generatedAt must advance beyond the previous signed snapshot.");
  }
}

function walkTree(root, current = root, paths = { files: [], directories: [] }) {
  for (const entry of readdirSync(current, { withFileTypes: true })) {
    const path = resolve(current, entry.name);
    const artifactPath = relative(root, path).split(sep).join("/");
    if (entry.isSymbolicLink()) fail(`Registry artifact contains a symbolic link: ${artifactPath}`);
    if (entry.isDirectory()) {
      paths.directories.push(artifactPath);
      walkTree(root, path, paths);
    } else if (entry.isFile()) {
      paths.files.push(artifactPath);
    } else {
      fail(`Registry artifact contains a non-regular entry: ${artifactPath}`);
    }
  }
  return paths;
}

function expectedDirectories(files) {
  const directories = new Set();
  for (const file of files) {
    const parts = file.split("/");
    parts.pop();
    while (parts.length > 0) {
      directories.add(parts.join("/"));
      parts.pop();
    }
  }
  return directories;
}

export function verifyPluginRegistryRelease({
  configPath,
  registryPath,
  previousRegistryPath,
  expectedRootPublicKey,
  expectedSequence,
  expectedGeneratedAt,
  expectedMinimumHostVersion,
  inventoryPath,
}) {
  const config = normalizeConfig(readJson(resolve(configPath), "Registry publishing config"));
  const registryFile = resolve(registryPath);
  const registryRoot = dirname(registryFile);
  requireRegularFile(registryFile, "Published registry", maximumJsonBytes);
  const registry = validateRegistryShape(readJson(registryFile, "Published registry"), "registry");
  const rootPublicKey = requireString(expectedRootPublicKey, "Expected registry root public key");
  const rootKey = publicKeyFromBase64(rootPublicKey, "Expected registry root public key");
  verifySignedValue(registry, rootKey, "registry");

  const expected = {
    sequence: requireSequence(expectedSequence, "Expected sequence"),
    generatedAt: requireTimestamp(expectedGeneratedAt, "Expected generatedAt"),
    minimumHostVersion: requireVersion(expectedMinimumHostVersion, "Expected minimumHostVersion"),
  };
  for (const [key, value] of Object.entries(expected)) {
    if (config[key] !== value || registry[key] !== value) {
      fail(`Reviewed ${key} does not match both config and published registry.`);
    }
  }
  if (config.schemaVersion !== registry.schemaVersion)
    fail("Registry schema does not match config.");

  const rootKeyPath = resolve(registryRoot, "registry-root-public-key.txt");
  requireRegularFile(rootKeyPath, "Published registry root public key", 256);
  if (readFileSync(rootKeyPath, "utf8") !== `${rootPublicKey}\n`) {
    fail("Published registry root public key does not match the protected trust root.");
  }

  let previous;
  if (previousRegistryPath) {
    const previousPath = resolve(previousRegistryPath);
    previous = validateRegistryShape(
      readJson(previousPath, "Previous signed registry"),
      "previousRegistry",
    );
    verifySignedValue(previous, rootKey, "previousRegistry");
  }
  validateRegistrySequence(registry, previous);

  if (registry.publishers.length !== config.publishers.length) {
    fail("Published publisher count does not match config.");
  }
  const publishers = new Map();
  registry.publishers.forEach((value, index) => {
    const label = `registry.publishers[${index}]`;
    const publisher = requireObject(value, label);
    requireExactKeys(publisher, ["id", "displayName", "publicKey", "revoked"], label);
    const configured = config.publishers[index];
    if (
      publisher.id !== configured?.id ||
      publisher.displayName !== configured.displayName ||
      publisher.revoked !== configured.revoked
    ) {
      fail(`${label} does not match the reviewed config order and values.`);
    }
    if (publishers.has(publisher.id)) fail(`${label} reuses a publisher identifier.`);
    publishers.set(publisher.id, {
      ...publisher,
      key: publicKeyFromBase64(publisher.publicKey, `${label}.publicKey`),
    });
  });

  if (registry.entries.length !== config.packages.length) {
    fail("Published registry entry count does not match config.");
  }
  const expectedFiles = new Set(["registry.json", "registry-root-public-key.txt"]);
  const identities = new Set();
  registry.entries.forEach((value, index) => {
    const label = `registry.entries[${index}]`;
    const entry = requireObject(value, label);
    requireExactKeys(
      entry,
      ["pluginId", "version", "publisherId", "manifestPath", "manifestSha256", "revoked"],
      label,
    );
    const configured = config.packages[index];
    const manifestPath = requireSafeRelativePath(entry.manifestPath, `${label}.manifestPath`);
    if (
      manifestPath !== configured?.manifestPath ||
      entry.publisherId !== configured.publisherId ||
      entry.revoked !== configured.revoked
    ) {
      fail(`${label} does not match the reviewed config order and values.`);
    }
    const publisher = publishers.get(entry.publisherId);
    if (!publisher) fail(`${label} references an unknown publisher.`);
    const version = requireVersion(entry.version, `${label}.version`);
    const identity = `${requireString(entry.pluginId, `${label}.pluginId`)}@${version}`;
    if (identities.has(identity)) fail(`${label} reuses plugin identity ${identity}.`);
    identities.add(identity);
    requireBoolean(entry.revoked, `${label}.revoked`);
    const manifestFile = resolveArtifactFile(
      registryRoot,
      manifestPath,
      `${label} manifest`,
      maximumJsonBytes,
    );
    const manifestBytes = readFileSync(manifestFile);
    if (sha256(manifestBytes) !== requireSha256(entry.manifestSha256, `${label}.manifestSha256`)) {
      fail(`${label} manifest hash does not match the published bytes.`);
    }
    const manifest = requireObject(JSON.parse(manifestBytes), `${label} manifest`);
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
      `${label} manifest`,
    );
    if (
      manifest.id !== entry.pluginId ||
      manifest.version !== version ||
      manifest.publisher !== publisher.displayName
    ) {
      fail(`${label} manifest identity does not match the signed registry entry.`);
    }
    verifySignedValue(manifest, publisher.key, `${label} manifest`);
    if (!Array.isArray(manifest.targets) || manifest.targets.length === 0) {
      fail(`${label} manifest targets must be a non-empty array.`);
    }
    const manifestDirectory = dirname(manifestFile);
    const targets = new Set();
    const executablePaths = new Set();
    manifest.targets.forEach((targetValue, targetIndex) => {
      const targetLabel = `${label} manifest.targets[${targetIndex}]`;
      const target = requireObject(targetValue, targetLabel);
      requireExactKeys(target, ["target", "executable", "sha256"], targetLabel);
      const targetName = requireString(target.target, `${targetLabel}.target`);
      if (targets.has(targetName)) fail(`${targetLabel} reuses a target name.`);
      targets.add(targetName);
      const executable = requireSafeRelativePath(target.executable, `${targetLabel}.executable`);
      const foldedExecutable = executable.toLowerCase();
      if (executablePaths.has(foldedExecutable)) fail(`${targetLabel} reuses an executable path.`);
      executablePaths.add(foldedExecutable);
      const executableFile = resolveArtifactFile(
        manifestDirectory,
        executable,
        `${targetLabel} executable`,
      );
      if (
        sha256(readFileSync(executableFile)) !==
        requireSha256(target.sha256, `${targetLabel}.sha256`)
      ) {
        fail(`${targetLabel} executable hash does not match the published bytes.`);
      }
      expectedFiles.add(relative(registryRoot, executableFile).split(sep).join("/"));
    });
    expectedFiles.add(manifestPath);
  });

  const inventoryFile = resolve(inventoryPath);
  if (dirname(inventoryFile) !== registryRoot || inventoryFile === registryFile) {
    fail("SHA-256 inventory must be a new file directly inside the registry artifact root.");
  }
  if (existsSync(inventoryFile)) fail("SHA-256 inventory already exists.");
  const inventoryName = relative(registryRoot, inventoryFile).split(sep).join("/");
  if (!portableComponentPattern.test(inventoryName))
    fail("SHA-256 inventory name is not portable.");

  const tree = walkTree(registryRoot);
  const actualFiles = new Set(tree.files);
  for (const file of actualFiles) {
    if (!expectedFiles.has(file)) fail(`Registry artifact contains an unexpected file: ${file}`);
  }
  for (const file of expectedFiles) {
    if (!actualFiles.has(file)) fail(`Registry artifact is missing an expected file: ${file}`);
  }
  const directories = expectedDirectories(expectedFiles);
  for (const directory of tree.directories) {
    if (!directories.has(directory))
      fail(`Registry artifact contains an unexpected directory: ${directory}`);
  }

  const inventory = [...expectedFiles]
    .sort()
    .map((path) => `${sha256(readFileSync(resolve(registryRoot, ...path.split("/"))))}  ${path}`)
    .join("\n");
  writeFileSync(inventoryFile, `${inventory}\n`, { flag: "wx" });
  return {
    sequence: registry.sequence,
    publisherCount: registry.publishers.length,
    packageCount: registry.entries.length,
    fileCount: expectedFiles.size,
    inventoryPath: inventoryFile,
  };
}

function parseArguments(arguments_) {
  const command = arguments_[0];
  const options = {};
  for (let index = 1; index < arguments_.length; index += 2) {
    const name = arguments_[index];
    const value = arguments_[index + 1];
    if (!name?.startsWith("--") || value === undefined || value.startsWith("--")) {
      fail(`Malformed argument near ${name ?? "end of command"}.`);
    }
    const key = name.slice(2).replace(/-([a-z])/gu, (_, letter) => letter.toUpperCase());
    if (Object.hasOwn(options, key)) fail(`Duplicate argument: ${name}`);
    options[key] = value;
  }
  return { command, options };
}

function requireArguments(options, names, usage) {
  for (const name of names) if (!options[name]) fail(usage);
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  try {
    const { command, options } = parseArguments(process.argv.slice(2));
    if (command === "materialize-keys") {
      requireArguments(
        options,
        ["config", "output"],
        "Usage: plugin-registry-release.mjs materialize-keys --config <file> --output <directory>",
      );
      const result = materializePluginRegistryKeys({
        configPath: options.config,
        outputDirectory: options.output,
        rootPrivateKey: process.env.TORBEN_PLUGIN_REGISTRY_ROOT_PRIVATE_KEY,
        publisherPrivateKeysJson: process.env.TORBEN_PLUGIN_REGISTRY_PUBLISHER_PRIVATE_KEYS_JSON,
      });
      console.log(`Materialized ${result.publisherArguments.length / 2} publisher key mapping(s).`);
    } else if (command === "verify") {
      requireArguments(
        options,
        [
          "config",
          "registry",
          "expectedRootKey",
          "sequence",
          "generatedAt",
          "minimumHostVersion",
          "inventory",
        ],
        "Usage: plugin-registry-release.mjs verify --config <file> --registry <file> --expected-root-key <base64> --sequence <number> --generated-at <UTC> --minimum-host-version <version> [--previous-registry <file>] --inventory <file>",
      );
      const result = verifyPluginRegistryRelease({
        configPath: options.config,
        registryPath: options.registry,
        previousRegistryPath: options.previousRegistry,
        expectedRootPublicKey: options.expectedRootKey,
        expectedSequence: options.sequence,
        expectedGeneratedAt: options.generatedAt,
        expectedMinimumHostVersion: options.minimumHostVersion,
        inventoryPath: options.inventory,
      });
      console.log(
        `Verified registry sequence ${result.sequence}: ${result.packageCount} package(s), ${result.fileCount} signed artifact file(s).`,
      );
    } else {
      fail("Expected materialize-keys or verify command.");
    }
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
