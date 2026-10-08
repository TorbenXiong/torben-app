import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  createReleaseMetadata,
  officialReleaseTargets,
  supportedTargets,
} from "./release-metadata.mjs";
import { createReleaseSet, verifyReleaseSet } from "./verify-release-set.mjs";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const revision = "c".repeat(40);

function fixtureRoot() {
  return mkdtempSync(join(tmpdir(), "torben-release-set-"));
}

function removeFixture(root) {
  const expectedPrefix = join(tmpdir(), "torben-release-set-");
  assert.ok(root.startsWith(expectedPrefix));
  rmSync(root, { recursive: true, force: true });
}

async function populateReleaseSet(root, options = {}) {
  const omittedTarget = options.omittedTarget;
  const alternateRevisionTarget = options.alternateRevisionTarget;
  for (const target of Object.keys(supportedTargets)) {
    if (target === omittedTarget) continue;
    const directory = join(root, target);
    mkdirSync(directory);
    writeFileSync(join(directory, `torben-${target}.fixture`), `payload-${target}`);
    await createReleaseMetadata({
      artifacts: directory,
      target,
      revision: target === alternateRevisionTarget ? "d".repeat(40) : revision,
      sourceRef: "refs/heads/feature/bootstrap",
      releaseKind: "development",
      signingStatus: "unsigned",
      repositoryRoot,
    });
  }
}

test("creates and verifies a deterministic complete six-target release set", async () => {
  const first = fixtureRoot();
  const second = fixtureRoot();
  try {
    await populateReleaseSet(first);
    await populateReleaseSet(second);
    const index = await createReleaseSet({ releases: first, repositoryRoot });
    await createReleaseSet({ releases: second, repositoryRoot });
    assert.equal(index.targets.length, 6);
    assert.deepEqual(
      index.targets.map((target) => target.target),
      Object.keys(supportedTargets),
    );
    assert.equal(
      readFileSync(join(first, "release-index.json"), "utf8"),
      readFileSync(join(second, "release-index.json"), "utf8"),
    );
    assert.equal(
      readFileSync(join(first, "SHA256SUMS"), "utf8"),
      readFileSync(join(second, "SHA256SUMS"), "utf8"),
    );
    const verified = await verifyReleaseSet({ releases: first, repositoryRoot });
    assert.equal(verified.sourceRevision, revision);
  } finally {
    removeFixture(first);
    removeFixture(second);
  }
});

test("rejects an incomplete target matrix", async () => {
  const root = fixtureRoot();
  try {
    await populateReleaseSet(root, { omittedTarget: "aarch64-unknown-linux-gnu" });
    await assert.rejects(
      createReleaseSet({ releases: root, repositoryRoot }),
      /targets are incomplete or duplicated/,
    );
  } finally {
    removeFixture(root);
  }
});

test("rejects mixed revisions before creating an aggregate index", async () => {
  const root = fixtureRoot();
  try {
    await populateReleaseSet(root, { alternateRevisionTarget: "aarch64-apple-darwin" });
    await assert.rejects(
      createReleaseSet({ releases: root, repositoryRoot }),
      /do not share one version, revision, ref, and release kind/,
    );
  } finally {
    removeFixture(root);
  }
});

test("development release sets reject an updater manifest before writing aggregate metadata", async () => {
  const root = fixtureRoot();
  try {
    await populateReleaseSet(root);
    writeFileSync(join(root, "latest.json"), "{}\n");
    await assert.rejects(
      createReleaseSet({ releases: root, repositoryRoot }),
      /Development release sets cannot contain latest\.json/,
    );
    assert.equal(existsSync(join(root, "release-index.json")), false);
    assert.equal(existsSync(join(root, "SHA256SUMS")), false);
  } finally {
    removeFixture(root);
  }
});

test("stale aggregate staging metadata fails before creating a partial index", async () => {
  const root = fixtureRoot();
  try {
    await populateReleaseSet(root);
    writeFileSync(join(root, "SHA256SUMS.next"), "stale\n");
    await assert.rejects(
      createReleaseSet({ releases: root, repositoryRoot }),
      /Refusing to overwrite existing release-set metadata/,
    );
    assert.equal(existsSync(join(root, "release-index.json")), false);
    assert.equal(existsSync(join(root, "release-index.json.next")), false);
  } finally {
    removeFixture(root);
  }
});

test("post-transfer verification detects a modified target payload", async () => {
  const root = fixtureRoot();
  try {
    await populateReleaseSet(root);
    await createReleaseSet({ releases: root, repositoryRoot });
    writeFileSync(
      join(root, "x86_64-pc-windows-msvc", "torben-x86_64-pc-windows-msvc.fixture"),
      "modified-payload",
    );
    await assert.rejects(
      verifyReleaseSet({ releases: root, repositoryRoot }),
      /failed verification/,
    );
  } finally {
    removeFixture(root);
  }
});

test("official release targets remain Windows x64 only", () => {
  assert.deepEqual(officialReleaseTargets, ["x86_64-pc-windows-msvc"]);
});
