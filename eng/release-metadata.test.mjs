import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  createReleaseMetadata,
  verifyReleaseMetadata,
  workspaceVersion,
} from "./release-metadata.mjs";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const revision = "a".repeat(40);

function fixtureDirectory() {
  const root = mkdtempSync(join(tmpdir(), "torben-release-metadata-"));
  mkdirSync(join(root, "cli"));
  writeFileSync(join(root, "Torben-App_0.1.0_x64-setup.exe"), "desktop-fixture");
  writeFileSync(join(root, "cli", "torben.exe"), "cli-fixture");
  return root;
}

function removeFixture(root) {
  const expectedPrefix = join(tmpdir(), "torben-release-metadata-");
  assert.ok(root.startsWith(expectedPrefix));
  rmSync(root, { recursive: true, force: true });
}

function developmentOptions(artifacts) {
  return {
    artifacts,
    target: "x86_64-pc-windows-msvc",
    revision,
    sourceRef: "refs/heads/feature/bootstrap",
    releaseKind: "development",
    signingStatus: "unsigned",
    repositoryRoot,
  };
}

test("workspace user-facing versions remain aligned", () => {
  const result = workspaceVersion(repositoryRoot);
  assert.equal(result.version, "0.1.0");
  assert.equal(new Set(Object.values(result.sources)).size, 1);
});

test("creates and verifies deterministic SHA-256 release metadata", async () => {
  const first = fixtureDirectory();
  const second = fixtureDirectory();
  try {
    const metadata = await createReleaseMetadata(developmentOptions(first));
    await createReleaseMetadata(developmentOptions(second));
    assert.equal(metadata.operatingSystem, "windows");
    assert.equal(metadata.architecture, "x86_64");
    assert.equal(metadata.releaseKind, "development");
    assert.equal(metadata.signingStatus, "unsigned");
    assert.deepEqual(
      metadata.artifacts.map((artifact) => artifact.path),
      ["cli/torben.exe", "Torben-App_0.1.0_x64-setup.exe"],
    );
    assert.equal(
      readFileSync(join(first, "release-metadata.json"), "utf8"),
      readFileSync(join(second, "release-metadata.json"), "utf8"),
    );
    assert.equal(
      readFileSync(join(first, "SHA256SUMS"), "utf8"),
      readFileSync(join(second, "SHA256SUMS"), "utf8"),
    );
    const verified = await verifyReleaseMetadata({ artifacts: first, repositoryRoot });
    assert.equal(verified.sourceRevision, revision);
  } finally {
    removeFixture(first);
    removeFixture(second);
  }
});

test("verification detects modified and unexpected artifacts", async () => {
  const root = fixtureDirectory();
  try {
    await createReleaseMetadata(developmentOptions(root));
    writeFileSync(join(root, "cli", "torben.exe"), "modified-cli-fixture");
    await assert.rejects(
      verifyReleaseMetadata({ artifacts: root, repositoryRoot }),
      /failed verification/,
    );

    const second = fixtureDirectory();
    try {
      await createReleaseMetadata(developmentOptions(second));
      writeFileSync(join(second, "unexpected.txt"), "unexpected");
      await assert.rejects(
        verifyReleaseMetadata({ artifacts: second, repositoryRoot }),
        /contents do not match/,
      );
    } finally {
      removeFixture(second);
    }
  } finally {
    removeFixture(root);
  }
});

test("official metadata fails closed without a signed version tag", async () => {
  const root = fixtureDirectory();
  try {
    await assert.rejects(
      createReleaseMetadata({
        ...developmentOptions(root),
        sourceRef: "refs/tags/v0.1.0",
        releaseKind: "official",
        signingStatus: "unsigned",
      }),
      /require signed artifacts/,
    );
    await assert.rejects(
      createReleaseMetadata({
        ...developmentOptions(root),
        sourceRef: "refs/heads/main",
        releaseKind: "official",
        signingStatus: "signed",
      }),
      /require source ref refs\/tags\/v0\.1\.0/,
    );
  } finally {
    removeFixture(root);
  }
});

test("stale target metadata staging fails before exposing a partial metadata pair", async () => {
  const root = fixtureDirectory();
  try {
    writeFileSync(join(root, "SHA256SUMS.next"), "stale\n");
    await assert.rejects(
      createReleaseMetadata(developmentOptions(root)),
      /Refusing to overwrite existing release metadata/,
    );
    assert.equal(existsSync(join(root, "release-metadata.json")), false);
    assert.equal(existsSync(join(root, "release-metadata.json.next")), false);
  } finally {
    removeFixture(root);
  }
});

test("command-line create and verify entry points round-trip", () => {
  const root = fixtureDirectory();
  const script = join(repositoryRoot, "eng", "release-metadata.mjs");
  try {
    const created = spawnSync(
      process.execPath,
      [
        script,
        "create",
        "--artifacts",
        root,
        "--target",
        "x86_64-pc-windows-msvc",
        "--revision",
        revision,
        "--source-ref",
        "refs/heads/feature/bootstrap",
        "--release-kind",
        "development",
        "--signing-status",
        "unsigned",
      ],
      { encoding: "utf8" },
    );
    assert.equal(created.status, 0, created.stderr);
    assert.match(created.stdout, /Created Torben App 0\.1\.0 x86_64-pc-windows-msvc/);

    const verified = spawnSync(process.execPath, [script, "verify", "--artifacts", root], {
      encoding: "utf8",
    });
    assert.equal(verified.status, 0, verified.stderr);
    assert.match(verified.stdout, /Verified Torben App 0\.1\.0 x86_64-pc-windows-msvc/);
  } finally {
    removeFixture(root);
  }
});
