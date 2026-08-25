import { spawnSync } from "node:child_process";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { validateUpdaterTargetDirectory } from "./generate-updater-manifest.mjs";

function fail(message) {
  throw new Error(message);
}

export function verifyUpdaterArtifacts({
  directory,
  target,
  publicKey = process.env.TORBEN_UPDATER_PUBLIC_KEY,
  spawn = spawnSync,
}) {
  if (!publicKey) fail("TORBEN_UPDATER_PUBLIC_KEY is required.");
  const records = validateUpdaterTargetDirectory({ directory, target });
  for (const record of records) {
    const result = spawn(
      "cargo",
      [
        "run",
        "--release",
        "--locked",
        "-p",
        "torben-release-tools",
        "--",
        "verify-updater",
        record.artifactPath,
        record.signaturePath,
        publicKey,
      ],
      { stdio: "inherit" },
    );
    if (result.error) {
      fail(`Could not start updater signature verification: ${result.error.message}`);
    }
    if (result.status !== 0) {
      fail(`Updater signature verification failed for ${record.artifact}.`);
    }
  }
  return records.length;
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
  if (!options.directory || !options.target) {
    fail("--directory and --target are required.");
  }
  return options;
}

if (process.argv[1] && resolve(process.argv[1]) === resolve(fileURLToPath(import.meta.url))) {
  try {
    const options = parseArguments(process.argv.slice(2));
    const count = verifyUpdaterArtifacts(options);
    console.log(`Verified ${count} updater signatures for ${options.target}.`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
