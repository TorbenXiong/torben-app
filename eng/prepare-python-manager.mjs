import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

export const PYTHON_MANAGER_VERSION = "26.3";
export const PYTHON_MANAGER_SHA256 =
  "259af5272c8f798786c1109b7ad287da519c58d43d7250ead8ed20fa3277a511";
export const PYTHON_MANAGER_URL = `https://www.python.org/ftp/python/pymanager/python-manager-${PYTHON_MANAGER_VERSION}.msi`;

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const repositoryRoot = dirname(scriptDirectory);
export const pythonManagerPackage = join(
  repositoryRoot,
  ".tools",
  "python-manager",
  `python-manager-${PYTHON_MANAGER_VERSION}.msi`,
);

function sha256(bytes) {
  return createHash("sha256").update(bytes).digest("hex");
}

export function verifyPythonManagerPackage() {
  if (!existsSync(pythonManagerPackage)) {
    throw new Error(
      "The pinned Python Install Manager package is missing. Run `pnpm run prepare:python-manager` after reviewing the documented download, then retry.",
    );
  }
  const actual = sha256(readFileSync(pythonManagerPackage));
  if (actual !== PYTHON_MANAGER_SHA256) {
    throw new Error(
      `Existing Python Install Manager package has SHA-256 ${actual}; expected ${PYTHON_MANAGER_SHA256}. Remove it before retrying.`,
    );
  }
  return pythonManagerPackage;
}

export async function preparePythonManager() {
  if (existsSync(pythonManagerPackage)) {
    verifyPythonManagerPackage();
    console.log(`Python Install Manager ${PYTHON_MANAGER_VERSION} is already verified.`);
    return pythonManagerPackage;
  }

  console.log(
    `Downloading Python Install Manager ${PYTHON_MANAGER_VERSION} from ${PYTHON_MANAGER_URL}`,
  );
  const response = await fetch(PYTHON_MANAGER_URL, { redirect: "error" });
  if (!response.ok) {
    throw new Error(`Python Install Manager download failed with HTTP ${response.status}.`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  const actual = sha256(bytes);
  if (actual !== PYTHON_MANAGER_SHA256) {
    throw new Error(
      `Downloaded Python Install Manager has SHA-256 ${actual}; expected ${PYTHON_MANAGER_SHA256}.`,
    );
  }

  mkdirSync(dirname(pythonManagerPackage), { recursive: true });
  const partial = `${pythonManagerPackage}.partial`;
  rmSync(partial, { force: true });
  writeFileSync(partial, bytes, { flag: "wx" });
  renameSync(partial, pythonManagerPackage);
  verifyPythonManagerPackage();
  console.log(`Verified Python Install Manager written to ${pythonManagerPackage}`);
  return pythonManagerPackage;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  await preparePythonManager();
}
