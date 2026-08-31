import assert from "node:assert/strict";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  removeTemporaryRoot,
  runDesktopPackageSmoke,
  safeCanonicalRelative,
} from "./desktop-package-smoke.mjs";
import { createReleaseMetadata } from "./release-metadata.mjs";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const sidecars = [
  "torben-plugin-node",
  "torben-plugin-temurin",
  "torben-plugin-python",
  "torben-plugin-git",
  "torben-plugin-vscode",
  "torben-plugin-codex",
  "torben-shim",
];

function fixtureRoot() {
  return mkdtempSync(join(tmpdir(), "torben-desktop-smoke-test-"));
}

function removeFixture(root) {
  assert.ok(root.startsWith(join(tmpdir(), "torben-desktop-smoke-test-")));
  rmSync(root, { recursive: true, force: true });
}

function writePe(path, machine = 0x8664) {
  const bytes = Buffer.alloc(128);
  bytes.set([0x4d, 0x5a]);
  bytes.writeUInt32LE(64, 0x3c);
  bytes.write("PE\0\0", 64, "ascii");
  bytes.writeUInt16LE(machine, 68);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, bytes);
}

function writeMachO(path, cpu = 0x01000007) {
  const bytes = Buffer.alloc(64);
  bytes.writeUInt32LE(0xfeedfacf, 0);
  bytes.writeUInt32LE(cpu, 4);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, bytes);
  chmodSync(path, 0o755);
}

async function windowsFixture(root, format = "nsis", signed = false) {
  const artifacts = join(root, "artifacts");
  const installed = join(root, "installed");
  mkdirSync(artifacts);
  const packageName =
    format === "msi" ? "Torben-App_0.1.0_x64_en-US.msi" : "Torben-App_0.1.0_x64-setup.exe";
  writeFileSync(join(artifacts, packageName), `fixture-${format}`);
  writePe(join(artifacts, "torben-0.1.0-x86_64-pc-windows-msvc.exe"));
  writePe(join(installed, "torben-desktop.exe"));
  for (const sidecar of sidecars) writePe(join(installed, `${sidecar}.exe`));
  await createReleaseMetadata({
    artifacts,
    target: "x86_64-pc-windows-msvc",
    revision: "a".repeat(40),
    sourceRef: signed ? "refs/tags/v0.1.0" : "refs/heads/feature/bootstrap",
    releaseKind: signed ? "official" : "development",
    signingStatus: signed ? "signed" : "unsigned",
    repositoryRoot,
  });
  return { artifacts, installed };
}

async function macosFixture(root, signed = false) {
  const artifacts = join(root, "artifacts");
  const installed = join(root, "Torben App.app");
  const executableDirectory = join(installed, "Contents", "MacOS");
  mkdirSync(artifacts);
  writeFileSync(join(artifacts, "Torben.App_0.1.0_x64.dmg"), "fixture-dmg");
  writeMachO(join(artifacts, "torben-0.1.0-x86_64-apple-darwin"));
  mkdirSync(join(installed, "Contents"), { recursive: true });
  writeFileSync(join(installed, "Contents", "Info.plist"), "fixture-plist");
  writeMachO(join(executableDirectory, "torben-desktop"));
  for (const sidecar of sidecars) writeMachO(join(executableDirectory, sidecar));
  await createReleaseMetadata({
    artifacts,
    target: "x86_64-apple-darwin",
    revision: "a".repeat(40),
    sourceRef: signed ? "refs/tags/v0.1.0" : "refs/heads/feature/bootstrap",
    releaseKind: signed ? "official" : "development",
    signingStatus: signed ? "signed" : "unsigned",
    repositoryRoot,
  });
  return { artifacts, installed };
}

function macosPlistExecutor({ stage }) {
  const values = {
    "plist-CFBundleIdentifier": "io.github.torbenxiong.torbenapp",
    "plist-CFBundleShortVersionString": "0.1.0",
    "plist-CFBundleExecutable": "torben-desktop",
  };
  if (!Object.hasOwn(values, stage)) throw new Error(`Unexpected fixture stage: ${stage}`);
  return { status: 0, stdout: `${values[stage]}\n`, stderr: "" };
}

test("canonical desktop path checks accept root aliases and reject real escapes", () => {
  const root = fixtureRoot();
  try {
    const actualRoot = join(root, "actual");
    const aliasRoot = join(root, "alias");
    const outside = join(root, "outside");
    mkdirSync(actualRoot);
    mkdirSync(outside);
    writeFileSync(join(actualRoot, "inside"), "inside");
    writeFileSync(join(outside, "escaped"), "escaped");
    symlinkSync(actualRoot, aliasRoot, process.platform === "win32" ? "junction" : "dir");

    assert.equal(safeCanonicalRelative(aliasRoot, join(actualRoot, "inside"), "Fixture"), "inside");

    const escapeLink = join(actualRoot, "escape");
    symlinkSync(outside, escapeLink, process.platform === "win32" ? "junction" : "dir");
    assert.throws(
      () => safeCanonicalRelative(aliasRoot, join(escapeLink, "escaped"), "Fixture"),
      /Fixture escapes its validation root/,
    );
  } finally {
    removeFixture(root);
  }
});

test("temporary cleanup removes only validated smoke roots", async () => {
  const prefix = join(tmpdir(), "torben-desktop-package-smoke-");
  const root = mkdtempSync(prefix);
  mkdirSync(join(root, "profile", "AppData", "Roaming"), { recursive: true });
  await assert.rejects(
    removeTemporaryRoot(root, join(tmpdir(), "unrelated-prefix-")),
    /Refusing to remove an unexpected smoke-test directory/,
  );
  let recreated = false;
  const recreateTimer = setInterval(() => {
    if (!recreated && !existsSync(root)) {
      mkdirSync(join(root, "profile", "AppData", "Roaming"), { recursive: true });
      recreated = true;
    }
  }, 10);
  try {
    await removeTemporaryRoot(root, prefix);
  } finally {
    clearInterval(recreateTimer);
  }
  assert.equal(recreated, true);
  assert.equal(existsSync(root), false);
});

test("temporary cleanup retries recoverable Windows handle errors", async () => {
  const prefix = join(tmpdir(), "torben-desktop-package-smoke-");
  const root = mkdtempSync(prefix);
  let attempts = 0;
  try {
    await removeTemporaryRoot(root, prefix, {
      remove: (path, options) => {
        attempts += 1;
        if (attempts === 1) {
          const error = new Error("fixture directory is still in use");
          error.code = "EPERM";
          throw error;
        }
        rmSync(path, options);
      },
      wait: async () => {},
    });
  } finally {
    if (existsSync(root)) removeFixture(root);
  }
  assert.equal(attempts, 2);
  assert.equal(existsSync(root), false);
});

test("verifies an installed Windows package and sustained isolated launch", async (t) => {
  for (const format of ["nsis", "msi"]) {
    await t.test(format, async () => {
      const root = fixtureRoot();
      try {
        const fixture = await windowsFixture(root, format);
        let launchCall;
        const result = await runDesktopPackageSmoke({
          ...fixture,
          format,
          installedRoot: fixture.installed,
          repositoryRoot,
          platform: "win32",
          architecture: "x64",
          environment: {
            Path: "C:\\fixture",
            SystemRoot: "C:\\Windows",
            TORBEN_FIXTURE_SECRET: "must-not-reach-the-app",
          },
          launch: async (options) => {
            launchCall = options;
            return { sustained: true, processId: 42 };
          },
        });

        assert.equal(result.ok, true);
        assert.equal(result.format, format);
        assert.equal(result.target, "x86_64-pc-windows-msvc");
        assert.equal(result.executable, "torben-desktop.exe");
        assert.equal(result.sidecars.length, 7);
        assert.deepEqual(result.signing, { status: "unsigned", verified: false });
        assert.equal(launchCall.env.Path, "C:\\fixture");
        assert.equal(launchCall.env.SystemRoot, "C:\\Windows");
        assert.equal(launchCall.env.TORBEN_FIXTURE_SECRET, undefined);
        assert.match(launchCall.env.LOCALAPPDATA, /torben-desktop-package-smoke-/);
      } finally {
        removeFixture(root);
      }
    });
  }
});

test("verifies a copied macOS application bundle, identity, and launch", async () => {
  const root = fixtureRoot();
  try {
    const fixture = await macosFixture(root);
    let launchCall;
    const result = await runDesktopPackageSmoke({
      artifacts: fixture.artifacts,
      format: "dmg",
      installedRoot: fixture.installed,
      repositoryRoot,
      execute: macosPlistExecutor,
      platform: "darwin",
      architecture: "x64",
      enforceExecutableMode: false,
      environment: {
        PATH: "/fixture/bin",
        LANG: "C.UTF-8",
        TORBEN_FIXTURE_SECRET: "must-not-reach-the-app",
      },
      launch: async (options) => {
        launchCall = options;
        return { sustained: true, processId: 43 };
      },
    });

    assert.equal(result.ok, true);
    assert.equal(result.applicationRoot, fixture.installed);
    assert.equal(result.executable, "Contents/MacOS/torben-desktop");
    assert.deepEqual(result.signing, { status: "unsigned", verified: false });
    assert.equal(launchCall.env.PATH, "/fixture/bin");
    assert.equal(launchCall.env.LANG, "C.UTF-8");
    assert.equal(launchCall.env.TORBEN_FIXTURE_SECRET, undefined);
    assert.match(launchCall.env.HOME, /torben-desktop-package-smoke-/);
  } finally {
    removeFixture(root);
  }
});

test("re-verifies signed Windows packages and every installed executable", async () => {
  const root = fixtureRoot();
  try {
    const fixture = await windowsFixture(root, "nsis", true);
    let signatureCall;
    const result = await runDesktopPackageSmoke({
      ...fixture,
      format: "nsis",
      installedRoot: fixture.installed,
      repositoryRoot,
      platform: "win32",
      architecture: "x64",
      execute: (options) => {
        signatureCall = options;
        return { status: 0, stdout: "", stderr: "" };
      },
      launch: async () => ({ sustained: true, processId: 44 }),
    });

    assert.equal(signatureCall.stage, "windows-authenticode");
    assert.equal(signatureCall.command, "powershell.exe");
    assert.ok(signatureCall.args.includes("-EncodedCommand"));
    const verifiedPaths = JSON.parse(signatureCall.env.TORBEN_SIGNATURE_PATHS);
    assert.equal(verifiedPaths.length, 9);
    assert.ok(verifiedPaths.some((path) => path.endsWith("-setup.exe")));
    assert.ok(verifiedPaths.some((path) => path.endsWith("torben-desktop.exe")));
    for (const sidecar of sidecars) {
      assert.ok(verifiedPaths.some((path) => path.endsWith(`${sidecar}.exe`)));
    }
    assert.deepEqual(result.signing, { status: "signed", verified: true });
  } finally {
    removeFixture(root);
  }
});

test("re-verifies a signed macOS application, notarization ticket, and Gatekeeper", async () => {
  const root = fixtureRoot();
  try {
    const fixture = await macosFixture(root, true);
    const signatureStages = [];
    const result = await runDesktopPackageSmoke({
      artifacts: fixture.artifacts,
      format: "dmg",
      installedRoot: fixture.installed,
      repositoryRoot,
      platform: "darwin",
      architecture: "x64",
      enforceExecutableMode: false,
      execute: (options) => {
        if (options.stage.startsWith("plist-")) return macosPlistExecutor(options);
        signatureStages.push(options.stage);
        return { status: 0, stdout: "", stderr: "" };
      },
      launch: async () => ({ sustained: true, processId: 45 }),
    });

    assert.deepEqual(signatureStages, ["macos-codesign", "macos-stapler", "macos-gatekeeper"]);
    assert.deepEqual(result.signing, { status: "signed", verified: true });
  } finally {
    removeFixture(root);
  }
});

test("fails closed when signed package verification fails", async () => {
  const root = fixtureRoot();
  try {
    const fixture = await windowsFixture(root, "msi", true);
    await assert.rejects(
      runDesktopPackageSmoke({
        ...fixture,
        format: "msi",
        installedRoot: fixture.installed,
        repositoryRoot,
        platform: "win32",
        architecture: "x64",
        execute: () => ({ status: 1, stdout: "", stderr: "fixture invalid signature" }),
        launch: async () => ({ sustained: true, processId: 46 }),
      }),
      /Windows Authenticode verification failed with status 1: fixture invalid signature/,
    );
  } finally {
    removeFixture(root);
  }
});

test("fails closed for host mismatch, missing sidecars, and early exit", async (t) => {
  await t.test("host mismatch", async () => {
    const root = fixtureRoot();
    try {
      const fixture = await windowsFixture(root);
      await assert.rejects(
        runDesktopPackageSmoke({
          artifacts: fixture.artifacts,
          format: "nsis",
          installedRoot: fixture.installed,
          repositoryRoot,
          platform: "win32",
          architecture: "arm64",
        }),
        /host target aarch64-pc-windows-msvc does not match x86_64-pc-windows-msvc/,
      );
    } finally {
      removeFixture(root);
    }
  });

  await t.test("missing sidecar", async () => {
    const root = fixtureRoot();
    try {
      const fixture = await windowsFixture(root);
      rmSync(join(fixture.installed, "torben-plugin-codex.exe"));
      await assert.rejects(
        runDesktopPackageSmoke({
          artifacts: fixture.artifacts,
          format: "nsis",
          installedRoot: fixture.installed,
          repositoryRoot,
          platform: "win32",
          architecture: "x64",
        }),
        /Expected exactly one installed torben-plugin-codex sidecar, found 0/,
      );
    } finally {
      removeFixture(root);
    }
  });

  await t.test("early exit", async () => {
    const root = fixtureRoot();
    try {
      const fixture = await windowsFixture(root);
      await assert.rejects(
        runDesktopPackageSmoke({
          artifacts: fixture.artifacts,
          format: "nsis",
          installedRoot: fixture.installed,
          repositoryRoot,
          platform: "win32",
          architecture: "x64",
          launch: async () => ({ sustained: false, code: 1, output: "fixture startup failure" }),
        }),
        /exited before the 8000ms launch window with code 1: fixture startup failure/,
      );
    } finally {
      removeFixture(root);
    }
  });

  await t.test("cleanup failure after the launch window", async () => {
    const root = fixtureRoot();
    try {
      const fixture = await windowsFixture(root, "msi");
      await assert.rejects(
        runDesktopPackageSmoke({
          artifacts: fixture.artifacts,
          format: "msi",
          installedRoot: fixture.installed,
          repositoryRoot,
          platform: "win32",
          architecture: "x64",
          launch: async () => ({
            sustained: true,
            error: "Torben App did not terminate after the sustained launch probe.",
          }),
        }),
        /survived the 8000ms launch window but cleanup failed: Torben App did not terminate/,
      );
    } finally {
      removeFixture(root);
    }
  });
});
