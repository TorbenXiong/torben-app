import assert from "node:assert/strict";
import { createPrivateKey } from "node:crypto";
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

import {
  materializePluginRegistryKeys,
  validateRegistrySequence,
  verifyPluginRegistryRelease,
} from "./plugin-registry-release.mjs";
import { publishPluginRegistry } from "./publish-plugin-registry.mjs";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const fixtureRoot = join(
  repositoryRoot,
  "crates",
  "torben-core",
  "tests",
  "fixtures",
  "plugin-registry",
);
const fixtureManifestPath = join(fixtureRoot, "packages", "online-fixture", "1.2.3", "plugin.json");
const packageManifestPath = "packages/online-fixture/1.2.3/plugin.json";
const expectedRootPublicKey = "6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw=";
const pkcs8SeedPrefix = Buffer.from("302e020100300506032b657004220420", "hex");

function privateKeyPem(seed) {
  return createPrivateKey({
    key: Buffer.concat([pkcs8SeedPrefix, Buffer.alloc(32, seed)]),
    format: "der",
    type: "pkcs8",
  }).export({ format: "pem", type: "pkcs8" });
}

function writeJson(path, value) {
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`);
}

function makeFixture() {
  const root = mkdtempSync(join(tmpdir(), "torben-registry-release-"));
  const source = join(root, "source");
  const packageRoot = join(source, "packages", "online-fixture", "1.2.3");
  mkdirSync(packageRoot, { recursive: true });
  const manifest = JSON.parse(readFileSync(fixtureManifestPath, "utf8"));
  writeJson(join(packageRoot, "plugin.json"), manifest);
  for (const target of manifest.targets) {
    const executable = join(packageRoot, ...target.executable.split("/"));
    mkdirSync(dirname(executable), { recursive: true });
    copyFileSync(join(fixtureRoot, "plugin.bin"), executable);
  }
  const config = {
    schemaVersion: 1,
    sequence: 1,
    generatedAt: "2026-08-23T00:00:00Z",
    minimumHostVersion: "0.1.0",
    publishers: [
      {
        id: "fixture.publisher",
        displayName: "Fixture Publisher",
        revoked: false,
      },
    ],
    packages: [
      {
        manifestPath: packageManifestPath,
        publisherId: "fixture.publisher",
        revoked: false,
      },
    ],
  };
  const configPath = join(root, "config.json");
  const rootKeyPath = join(root, "root.pem");
  const publisherKeyPath = join(root, "publisher.pem");
  writeJson(configPath, config);
  writeFileSync(rootKeyPath, privateKeyPem(7));
  writeFileSync(publisherKeyPath, privateKeyPem(9));
  return { root, source, config, configPath, rootKeyPath, publisherKeyPath };
}

function publish(fixture, output) {
  return publishPluginRegistry({
    configPath: fixture.configPath,
    sourceDirectory: fixture.source,
    outputDirectory: output,
    rootKeyPath: fixture.rootKeyPath,
    publisherKeyPaths: new Map([["fixture.publisher", fixture.publisherKeyPath]]),
    emitRootPublicKey: true,
  });
}

function verify(fixture, output, previousRegistryPath) {
  return verifyPluginRegistryRelease({
    configPath: fixture.configPath,
    registryPath: join(output, "registry.json"),
    previousRegistryPath,
    expectedRootPublicKey,
    expectedSequence: fixture.config.sequence,
    expectedGeneratedAt: fixture.config.generatedAt,
    expectedMinimumHostVersion: fixture.config.minimumHostVersion,
    inventoryPath: join(output, "SHA256SUMS"),
  });
}

function withFixture(run) {
  const fixture = makeFixture();
  try {
    return run(fixture);
  } finally {
    assert.ok(fixture.root.startsWith(join(tmpdir(), "torben-registry-release-")));
    rmSync(fixture.root, { recursive: true, force: true });
  }
}

test("materializes exactly the reviewed private-key mapping and cleans failed output", () => {
  withFixture((fixture) => {
    const output = join(fixture.root, "keys");
    const result = materializePluginRegistryKeys({
      configPath: fixture.configPath,
      outputDirectory: output,
      rootPrivateKey: "root-private-key",
      publisherPrivateKeysJson: JSON.stringify({
        "fixture.publisher": "publisher-private-key",
      }),
    });
    assert.equal(readFileSync(result.rootKeyPath, "utf8"), "root-private-key");
    assert.deepEqual(result.publisherArguments, [
      "--publisher-key",
      `fixture.publisher=${join(output, "publisher-fixture.publisher.pem")}`,
    ]);
    assert.deepEqual(
      JSON.parse(readFileSync(result.argumentsPath, "utf8")),
      result.publisherArguments,
    );
  });

  withFixture((fixture) => {
    const output = join(fixture.root, "failed-keys");
    assert.throws(
      () =>
        materializePluginRegistryKeys({
          configPath: fixture.configPath,
          outputDirectory: output,
          rootPrivateKey: "root-private-key",
          publisherPrivateKeysJson: JSON.stringify({ unknown: "private-key" }),
        }),
      /exactly the configured publisher identifiers/u,
    );
    assert.equal(existsSync(output), false);
  });
});

test("re-verifies every signature and artifact hash before writing a deterministic inventory", () => {
  withFixture((fixture) => {
    const output = join(fixture.root, "published");
    publish(fixture, output);
    const result = verify(fixture, output);

    assert.equal(result.sequence, 1);
    assert.equal(result.publisherCount, 1);
    assert.equal(result.packageCount, 1);
    const inventory = readFileSync(result.inventoryPath, "utf8");
    assert.match(
      inventory,
      /^[0-9a-f]{64} {2}packages\/online-fixture\/1\.2\.3\/bin\/linux-aarch64\/plugin\n/mu,
    );
    assert.match(inventory, /^[0-9a-f]{64} {2}registry\.json\n/mu);
    assert.doesNotMatch(inventory, /SHA256SUMS/u);
  });
});

test("fails closed on changed bytes, unexpected files, or a different protected trust root", () => {
  withFixture((fixture) => {
    const output = join(fixture.root, "changed");
    publish(fixture, output);
    writeFileSync(
      join(output, "packages", "online-fixture", "1.2.3", "bin", "linux-x86_64", "plugin"),
      "changed",
    );
    assert.throws(() => verify(fixture, output), /executable hash does not match/u);
    assert.equal(existsSync(join(output, "SHA256SUMS")), false);
  });
  withFixture((fixture) => {
    const output = join(fixture.root, "extra");
    publish(fixture, output);
    writeFileSync(join(output, "unexpected.txt"), "unexpected");
    assert.throws(() => verify(fixture, output), /unexpected file/u);
    assert.equal(existsSync(join(output, "SHA256SUMS")), false);
  });
  withFixture((fixture) => {
    const output = join(fixture.root, "wrong-root");
    publish(fixture, output);
    assert.throws(
      () =>
        verifyPluginRegistryRelease({
          configPath: fixture.configPath,
          registryPath: join(output, "registry.json"),
          expectedRootPublicKey: Buffer.alloc(32, 4).toString("base64"),
          expectedSequence: 1,
          expectedGeneratedAt: fixture.config.generatedAt,
          expectedMinimumHostVersion: fixture.config.minimumHostVersion,
          inventoryPath: join(output, "SHA256SUMS"),
        }),
      /registry signature is invalid/u,
    );
  });
});

test("requires one continuous signed sequence step and a later generation time", () => {
  assert.doesNotThrow(() =>
    validateRegistrySequence(
      { sequence: 2, generatedAt: "2026-08-24T00:00:00Z" },
      { sequence: 1, generatedAt: "2026-08-23T00:00:00Z" },
    ),
  );
  assert.throws(
    () => validateRegistrySequence({ sequence: 2, generatedAt: "2026-08-24T00:00:00Z" }),
    /previous signed registry is required/u,
  );
  assert.throws(
    () =>
      validateRegistrySequence(
        { sequence: 3, generatedAt: "2026-08-24T00:00:00Z" },
        { sequence: 1, generatedAt: "2026-08-23T00:00:00Z" },
      ),
    /advance exactly once/u,
  );
  assert.throws(
    () =>
      validateRegistrySequence(
        { sequence: 2, generatedAt: "2026-08-23T00:00:00Z" },
        { sequence: 1, generatedAt: "2026-08-23T00:00:00Z" },
      ),
    /generatedAt must advance/u,
  );
});

test("accepts a newly signed snapshot only against its signed immediate predecessor", () => {
  withFixture((fixture) => {
    const firstOutput = join(fixture.root, "sequence-1");
    publish(fixture, firstOutput);
    fixture.config.sequence = 2;
    fixture.config.generatedAt = "2026-08-24T00:00:00Z";
    writeJson(fixture.configPath, fixture.config);
    const secondOutput = join(fixture.root, "sequence-2");
    publish(fixture, secondOutput);

    const result = verify(fixture, secondOutput, join(firstOutput, "registry.json"));
    assert.equal(result.sequence, 2);
  });
});
