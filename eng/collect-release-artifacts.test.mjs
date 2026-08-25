import assert from "node:assert/strict";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { basename, dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  collectReleaseArtifacts,
  detectExecutableTarget,
  requiredPackages,
} from "./collect-release-artifacts.mjs";
import {
  createReleaseMetadata,
  supportedTargets,
  verifyReleaseMetadata,
} from "./release-metadata.mjs";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const revision = "b".repeat(40);

function fixtureRoot() {
  return mkdtempSync(join(tmpdir(), "torben-release-collector-"));
}

function removeFixture(root) {
  const expectedPrefix = join(tmpdir(), "torben-release-collector-");
  assert.ok(root.startsWith(expectedPrefix));
  rmSync(root, { recursive: true, force: true });
}

function executableFixture(target) {
  if (target.endsWith("windows-msvc")) {
    const bytes = Buffer.alloc(128);
    bytes.write("MZ", 0, "ascii");
    bytes.writeUInt32LE(64, 0x3c);
    bytes.write("PE\0\0", 64, "ascii");
    bytes.writeUInt16LE(target.startsWith("x86_64") ? 0x8664 : 0xaa64, 68);
    return bytes;
  }
  if (target.endsWith("linux-gnu")) {
    const bytes = Buffer.alloc(64);
    bytes.set([0x7f, 0x45, 0x4c, 0x46, 2, 1]);
    bytes.writeUInt16LE(target.startsWith("x86_64") ? 0x3e : 0xb7, 18);
    return bytes;
  }
  const bytes = Buffer.alloc(32);
  bytes.writeUInt32LE(0xfeedfacf, 0);
  bytes.writeUInt32LE(target.startsWith("x86_64") ? 0x01000007 : 0x0100000c, 4);
  return bytes;
}

function createBundle(root, target, omittedFormat) {
  const operatingSystem = supportedTargets[target].operatingSystem;
  const bundleRoot = join(root, "bundle");
  for (const requirement of requiredPackages[operatingSystem]) {
    if (requirement.format === omittedFormat) {
      continue;
    }
    const directory = join(bundleRoot, requirement.directory);
    mkdirSync(directory, { recursive: true });
    writeFileSync(
      join(directory, `Torben-App-${requirement.format}-${target}${requirement.extension}`),
      `${requirement.format}-${target}`,
    );
  }
  return bundleRoot;
}

function createCli(root, target) {
  const extension = target.includes("windows") ? ".exe" : "";
  const path = join(root, `torben${extension}`);
  writeFileSync(path, executableFixture(target));
  if (!extension) {
    chmodSync(path, 0o755);
  }
  return path;
}

test("collects every required package and a matching native CLI for all six targets", async (t) => {
  for (const target of Object.keys(supportedTargets)) {
    await t.test(target, async () => {
      const root = fixtureRoot();
      try {
        const bundleRoot = createBundle(root, target);
        const cliBinary = createCli(root, target);
        const output = join(root, "artifacts");
        const copied = collectReleaseArtifacts({
          bundleRoot,
          cliBinary,
          output,
          target,
          repositoryRoot,
        });
        assert.equal(
          copied.length,
          requiredPackages[supportedTargets[target].operatingSystem].length + 1,
        );
        const copiedCli = copied.find((entry) => entry.format === "cli");
        assert.ok(copiedCli);
        assert.equal(detectExecutableTarget(copiedCli.path), target);
        assert.equal(readdirSync(output).length, copied.length);

        await createReleaseMetadata({
          artifacts: output,
          target,
          revision,
          sourceRef: "refs/heads/feature/bootstrap",
          releaseKind: "development",
          signingStatus: "unsigned",
          repositoryRoot,
        });
        const metadata = await verifyReleaseMetadata({ artifacts: output, repositoryRoot });
        assert.equal(metadata.target, target);
      } finally {
        removeFixture(root);
      }
    });
  }
});

test("fails before output when a required package format is missing", () => {
  const root = fixtureRoot();
  try {
    const target = "x86_64-unknown-linux-gnu";
    const output = join(root, "artifacts");
    assert.throws(
      () =>
        collectReleaseArtifacts({
          bundleRoot: createBundle(root, target, "rpm"),
          cliBinary: createCli(root, target),
          output,
          target,
          repositoryRoot,
        }),
      /rpm bundle directory is missing/,
    );
    assert.equal(existsSync(output), false);
  } finally {
    removeFixture(root);
  }
});

test("rejects a CLI built for another architecture", () => {
  const root = fixtureRoot();
  try {
    const target = "x86_64-pc-windows-msvc";
    assert.throws(
      () =>
        collectReleaseArtifacts({
          bundleRoot: createBundle(root, target),
          cliBinary: createCli(root, "aarch64-pc-windows-msvc"),
          output: join(root, "artifacts"),
          target,
          repositoryRoot,
        }),
      /does not match requested release target/,
    );
  } finally {
    removeFixture(root);
  }
});

test("copied CLI name includes the workspace version and target", () => {
  const root = fixtureRoot();
  try {
    const target = "aarch64-apple-darwin";
    const copied = collectReleaseArtifacts({
      bundleRoot: createBundle(root, target),
      cliBinary: createCli(root, target),
      output: join(root, "artifacts"),
      target,
      repositoryRoot,
    });
    assert.equal(
      basename(copied.find((entry) => entry.format === "cli").path),
      "torben-0.1.0-aarch64-apple-darwin",
    );
  } finally {
    removeFixture(root);
  }
});

test("rolls back staged packages when a later destination collides", () => {
  const root = fixtureRoot();
  try {
    const target = "x86_64-pc-windows-msvc";
    const bundleRoot = createBundle(root, target);
    renameSync(
      join(bundleRoot, "nsis", `Torben-App-nsis-${target}.exe`),
      join(bundleRoot, "nsis", `torben-0.1.0-${target}.exe`),
    );
    const output = join(root, "artifacts");
    assert.throws(
      () =>
        collectReleaseArtifacts({
          bundleRoot,
          cliBinary: createCli(root, target),
          output,
          target,
          repositoryRoot,
        }),
      /exist/i,
    );
    assert.equal(existsSync(output), false);
    assert.equal(existsSync(`${output}.next`), false);
  } finally {
    removeFixture(root);
  }
});

test("refuses existing, staged, or bundle-contained release output", () => {
  const root = fixtureRoot();
  try {
    const target = "x86_64-pc-windows-msvc";
    const bundleRoot = createBundle(root, target);
    const cliBinary = createCli(root, target);
    const output = join(root, "artifacts");
    mkdirSync(output);
    assert.throws(
      () =>
        collectReleaseArtifacts({
          bundleRoot,
          cliBinary,
          output,
          target,
          repositoryRoot,
        }),
      /Refusing to overwrite existing release output/,
    );
    assert.deepEqual(readdirSync(output), []);
    rmSync(output, { recursive: true });

    mkdirSync(`${output}.next`);
    writeFileSync(join(`${output}.next`, "sentinel"), "preserve");
    assert.throws(
      () =>
        collectReleaseArtifacts({
          bundleRoot,
          cliBinary,
          output,
          target,
          repositoryRoot,
        }),
      /Refusing to overwrite existing release output/,
    );
    assert.equal(readFileSync(join(`${output}.next`, "sentinel"), "utf8"), "preserve");
    rmSync(`${output}.next`, { recursive: true });

    const contained = join(bundleRoot, "collected");
    assert.throws(
      () =>
        collectReleaseArtifacts({
          bundleRoot,
          cliBinary,
          output: contained,
          target,
          repositoryRoot,
        }),
      /must be outside the Tauri bundle directory/,
    );
    assert.equal(existsSync(contained), false);
  } finally {
    removeFixture(root);
  }
});
