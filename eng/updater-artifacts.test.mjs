import assert from "node:assert/strict";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { collectUpdaterArtifacts } from "./collect-updater-artifacts.mjs";
import { generateUpdaterManifest } from "./generate-updater-manifest.mjs";
import {
  prepareGithubReleaseAssets,
  verifyGithubReleaseAssets,
} from "./prepare-github-release-assets.mjs";
import { createReleaseMetadata, officialReleaseTargets } from "./release-metadata.mjs";
import { createReleaseSet, verifyReleaseSet } from "./verify-release-set.mjs";
import { verifyUpdaterArtifacts } from "./verify-updater-artifacts.mjs";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const signature = Buffer.from(
  "untrusted comment: signature from minisign secret key\nfixture\ntrusted comment: fixture\nfixture",
).toString("base64");

const requirements = {
  "x86_64-pc-windows-msvc": [
    ["nsis", ".exe"],
    ["msi", ".msi"],
  ],
  "aarch64-pc-windows-msvc": [
    ["nsis", ".exe"],
    ["msi", ".msi"],
  ],
  "x86_64-apple-darwin": [["macos", ".app.tar.gz"]],
  "aarch64-apple-darwin": [["macos", ".app.tar.gz"]],
  "x86_64-unknown-linux-gnu": [
    ["appimage", ".AppImage"],
    ["deb", ".deb"],
    ["rpm", ".rpm"],
  ],
  "aarch64-unknown-linux-gnu": [
    ["appimage", ".AppImage"],
    ["deb", ".deb"],
    ["rpm", ".rpm"],
  ],
};

function fixtureRoot() {
  return mkdtempSync(join(tmpdir(), "torben-updater-artifacts-"));
}

function removeFixture(root) {
  assert.ok(root.startsWith(join(tmpdir(), "torben-updater-artifacts-")));
  rmSync(root, { recursive: true, force: true });
}

function createBundle(root, target) {
  const bundle = join(root, "bundles", target);
  for (const [directory, extension] of requirements[target]) {
    const path = join(bundle, directory);
    mkdirSync(path, { recursive: true });
    const name =
      directory === "macos" ? `Torben App${extension}` : `Torben-App-${target}${extension}`;
    const artifact = join(path, name);
    writeFileSync(artifact, `artifact-${target}-${directory}`);
    writeFileSync(`${artifact}.sig`, signature);
  }
  return bundle;
}

async function createOfficialReleaseFixture(root) {
  const releases = join(root, "release-set");
  mkdirSync(releases);
  const mappings = new Map();
  for (const target of officialReleaseTargets) {
    const output = join(releases, target);
    mkdirSync(output);
    writeFileSync(join(output, `payload-${target}`), target);
    const bundle = createBundle(root, target);
    for (const [directory, extension] of requirements[target]) {
      if (directory === "macos") continue;
      copyFileSync(
        join(bundle, directory, `Torben-App-${target}${extension}`),
        join(output, `Torben-App-${target}${extension}`),
      );
    }
    const mapping = collectUpdaterArtifacts({ bundleRoot: bundle, output, target });
    mappings.set(target, mapping);
    await createReleaseMetadata({
      artifacts: output,
      target,
      revision: "e".repeat(40),
      sourceRef: "refs/tags/v0.1.0",
      releaseKind: "official",
      signingStatus: "signed",
      repositoryRoot,
    });
  }
  return { releases, mappings };
}

function writeMapping(releases, target, mapping) {
  writeFileSync(
    join(releases, target, "updater-artifacts.json"),
    `${JSON.stringify(mapping, null, 2)}\n`,
  );
}

test("collects every updater package/signature pair and creates latest.json", async () => {
  const root = fixtureRoot();
  try {
    const { releases, mappings } = await createOfficialReleaseFixture(root);
    for (const target of officialReleaseTargets) {
      assert.equal(mappings.get(target).artifacts.length, requirements[target].length);
    }
    const manifest = generateUpdaterManifest({
      releases,
      publishedAt: "2026-08-24T13:00:00Z",
      notes: "Official updater fixture",
    });
    assert.equal(Object.keys(manifest.platforms).length, 2);
    assert.equal(manifest.version, "0.1.0");
    assert.match(manifest.platforms["windows-x86_64-nsis"].url, /v0\.1\.0/);
    assert.equal(
      JSON.parse(readFileSync(join(releases, "latest.json"), "utf8")).pub_date,
      "2026-08-24T13:00:00Z",
    );
    await createReleaseSet({ releases, repositoryRoot });
    await verifyReleaseSet({ releases, repositoryRoot });
    const publishing = join(root, "publishing");
    const assets = await prepareGithubReleaseAssets({ releases, output: publishing });
    assert.ok(assets.includes("latest.json"));
    assert.ok(assets.includes("SHA256SUMS"));
    assert.equal(new Set(assets).size, assets.length);
    assert.match(readFileSync(join(publishing, "SHA256SUMS"), "utf8"), /latest\.json/);
    assert.deepEqual(await verifyGithubReleaseAssets({ releases, output: publishing }), assets);
  } finally {
    removeFixture(root);
  }
});

test("rejects incomplete, extra, and unsafe updater mappings", async () => {
  for (const mutation of ["missing", "extra", "traversal"]) {
    const root = fixtureRoot();
    try {
      const { releases, mappings } = await createOfficialReleaseFixture(root);
      const target = "x86_64-pc-windows-msvc";
      const mapping = structuredClone(mappings.get(target));
      if (mutation === "missing") mapping.artifacts.pop();
      if (mutation === "extra") {
        mapping.artifacts.push({
          platform: "windows-x86_64-portable",
          artifact: mapping.artifacts[0].artifact,
          signature: mapping.artifacts[0].signature,
        });
      }
      if (mutation === "traversal") {
        mapping.artifacts[0].artifact = "../outside.exe";
        mapping.artifacts[0].signature = "../outside.exe.sig";
      }
      writeMapping(releases, target, mapping);
      assert.throws(
        () =>
          generateUpdaterManifest({
            releases,
            publishedAt: "2026-08-24T13:00:00Z",
          }),
        /Updater (target metadata|mapping file name)/,
      );
      assert.equal(existsSync(join(releases, "latest.json")), false);
    } finally {
      removeFixture(root);
    }
  }
});

test("validates a signed mapping before invoking the Rust verifier", async () => {
  const root = fixtureRoot();
  try {
    const { releases } = await createOfficialReleaseFixture(root);
    const target = "x86_64-pc-windows-msvc";
    const calls = [];
    const count = verifyUpdaterArtifacts({
      directory: join(releases, target),
      target,
      publicKey: "fixture-public-key",
      spawn(command, args, options) {
        calls.push({ command, args, options });
        return { status: 0 };
      },
    });
    assert.equal(count, 2);
    assert.equal(calls.length, 2);
    for (const call of calls) {
      assert.equal(call.command, "cargo");
      assert.deepEqual(call.args.slice(0, 7), [
        "run",
        "--release",
        "--locked",
        "-p",
        "torben-release-tools",
        "--",
        "verify-updater",
      ]);
      assert.equal(call.args.at(-1), "fixture-public-key");
      assert.deepEqual(call.options, { stdio: "inherit" });
    }
    const mappingPath = join(releases, target, "updater-artifacts.json");
    const unsafeMapping = JSON.parse(readFileSync(mappingPath, "utf8"));
    unsafeMapping.artifacts[0].artifact = "../outside.exe";
    unsafeMapping.artifacts[0].signature = "../outside.exe.sig";
    writeFileSync(mappingPath, `${JSON.stringify(unsafeMapping, null, 2)}\n`);
    let unsafeSpawnCount = 0;
    assert.throws(
      () =>
        verifyUpdaterArtifacts({
          directory: join(releases, target),
          target,
          publicKey: "fixture-public-key",
          spawn() {
            unsafeSpawnCount += 1;
            return { status: 0 };
          },
        }),
      /mapping file name is unsafe/,
    );
    assert.equal(unsafeSpawnCount, 0);
  } finally {
    removeFixture(root);
  }
});

test("does not require deferred platform updater directories", async () => {
  const root = fixtureRoot();
  try {
    const { releases } = await createOfficialReleaseFixture(root);
    const manifest = generateUpdaterManifest({
      releases,
      publishedAt: "2026-08-24T13:00:00Z",
    });
    assert.deepEqual(Object.keys(manifest.platforms), [
      "windows-x86_64-msi",
      "windows-x86_64-nsis",
    ]);
  } finally {
    removeFixture(root);
  }
});

test("official release sets reject deferred platform targets", async () => {
  const root = fixtureRoot();
  try {
    const { releases } = await createOfficialReleaseFixture(root);
    generateUpdaterManifest({ releases, publishedAt: "2026-08-24T13:00:00Z" });
    const deferredTarget = "aarch64-pc-windows-msvc";
    const deferredDirectory = join(releases, deferredTarget);
    mkdirSync(deferredDirectory);
    writeFileSync(join(deferredDirectory, "deferred.fixture"), deferredTarget);
    await createReleaseMetadata({
      artifacts: deferredDirectory,
      target: deferredTarget,
      revision: "e".repeat(40),
      sourceRef: "refs/tags/v0.1.0",
      releaseKind: "official",
      signingStatus: "signed",
      repositoryRoot,
    });
    await assert.rejects(
      createReleaseSet({ releases, repositoryRoot }),
      /targets are incomplete or duplicated/,
    );
  } finally {
    removeFixture(root);
  }
});

test("rejects a modified latest.json before creating official aggregate metadata", async () => {
  const root = fixtureRoot();
  try {
    const { releases } = await createOfficialReleaseFixture(root);
    const manifest = generateUpdaterManifest({
      releases,
      publishedAt: "2026-08-24T13:00:00Z",
    });
    manifest.platforms["windows-x86_64-nsis"].url = "https://example.invalid/untrusted-updater.exe";
    writeFileSync(join(releases, "latest.json"), `${JSON.stringify(manifest, null, 2)}\n`);
    await assert.rejects(
      createReleaseSet({ releases, repositoryRoot }),
      /Updater manifest platform record is invalid/,
    );
    assert.equal(existsSync(join(releases, "release-index.json")), false);
    assert.equal(existsSync(join(releases, "SHA256SUMS")), false);
  } finally {
    removeFixture(root);
  }
});

test("rejects a conflicting pre-existing updater artifact", () => {
  const root = fixtureRoot();
  try {
    const target = "x86_64-pc-windows-msvc";
    const bundle = createBundle(root, target);
    const output = join(root, "release-set", target);
    mkdirSync(output, { recursive: true });
    writeFileSync(join(output, `Torben-App-${target}.exe`), "different bytes");
    assert.throws(
      () => collectUpdaterArtifacts({ bundleRoot: bundle, output, target }),
      /differs from updater artifact/,
    );
    assert.equal(existsSync(join(output, "updater-artifacts.json")), false);
  } finally {
    removeFixture(root);
  }
});

test("preflights every updater pair before copying any signature", () => {
  const root = fixtureRoot();
  try {
    const target = "x86_64-pc-windows-msvc";
    const bundle = createBundle(root, target);
    const output = join(root, "release-set", target);
    mkdirSync(output, { recursive: true });
    for (const [directory, extension] of requirements[target]) {
      copyFileSync(
        join(bundle, directory, `Torben-App-${target}${extension}`),
        join(output, `Torben-App-${target}${extension}`),
      );
    }
    const firstSignature = join(output, `Torben-App-${target}.exe.sig`);
    writeFileSync(join(output, `Torben-App-${target}.msi.sig`), "stale signature");
    assert.throws(
      () => collectUpdaterArtifacts({ bundleRoot: bundle, output, target }),
      /Refusing to overwrite updater signature/,
    );
    assert.equal(existsSync(firstSignature), false);
    assert.equal(existsSync(join(output, "updater-artifacts.json")), false);
  } finally {
    removeFixture(root);
  }
});

test("rejects flattened publication when the current release target is missing", async () => {
  const root = fixtureRoot();
  try {
    const releases = join(root, "release-set");
    mkdirSync(releases);
    writeFileSync(join(releases, "latest.json"), "{}\n");
    writeFileSync(join(releases, "release-index.json"), "{}\n");
    const output = join(root, "publishing");
    await assert.rejects(
      prepareGithubReleaseAssets({ releases, output }),
      /Release target x86_64-pc-windows-msvc is missing/,
    );
    assert.equal(existsSync(output), false);
    assert.equal(existsSync(`${output}.next`), false);
  } finally {
    removeFixture(root);
  }
});

test("flat release verification rejects extra and source-divergent assets", async () => {
  const root = fixtureRoot();
  try {
    const { releases } = await createOfficialReleaseFixture(root);
    generateUpdaterManifest({
      releases,
      publishedAt: "2026-08-24T13:00:00Z",
    });
    await createReleaseSet({ releases, repositoryRoot });
    const publishing = join(root, "publishing");
    await prepareGithubReleaseAssets({ releases, output: publishing });

    const unexpected = join(publishing, "unexpected.bin");
    writeFileSync(unexpected, "unexpected");
    await assert.rejects(
      verifyGithubReleaseAssets({ releases, output: publishing }),
      /does not match the exact expected file set/,
    );
    rmSync(unexpected);

    writeFileSync(join(publishing, "latest.json"), "{}\n");
    await assert.rejects(
      verifyGithubReleaseAssets({ releases, output: publishing }),
      /failed source or checksum verification: latest\.json/,
    );
  } finally {
    removeFixture(root);
  }
});

test("rejects missing updater signatures and unsigned target metadata", async () => {
  const root = fixtureRoot();
  try {
    const target = "x86_64-pc-windows-msvc";
    const bundle = createBundle(root, target);
    rmSync(join(bundle, "nsis", `Torben-App-${target}.exe.sig`));
    const output = join(root, "release-set", target);
    mkdirSync(output, { recursive: true });
    assert.throws(
      () => collectUpdaterArtifacts({ bundleRoot: bundle, output, target }),
      /signature is missing/,
    );
  } finally {
    removeFixture(root);
  }
});
