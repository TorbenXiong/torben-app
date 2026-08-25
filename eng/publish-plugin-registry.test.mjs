import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash, createPrivateKey } from "node:crypto";
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

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
  const root = mkdtempSync(join(tmpdir(), "torben-registry-publisher-"));
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
  return {
    root,
    source,
    config,
    configPath,
    rootKeyPath,
    publisherKeyPath,
    output: join(root, "published"),
  };
}

function publish(fixture, overrides = {}) {
  return publishPluginRegistry({
    configPath: fixture.configPath,
    sourceDirectory: fixture.source,
    outputDirectory: fixture.output,
    rootKeyPath: fixture.rootKeyPath,
    publisherKeyPaths: new Map([["fixture.publisher", fixture.publisherKeyPath]]),
    ...overrides,
  });
}

function withFixture(run) {
  const fixture = makeFixture();
  try {
    return run(fixture);
  } finally {
    assert.ok(fixture.root.startsWith(join(tmpdir(), "torben-registry-publisher-")));
    rmSync(fixture.root, { recursive: true, force: true });
  }
}

test("reproduces the Rust-verified registry and manifest fixtures byte for byte", () => {
  withFixture((fixture) => {
    const result = publish(fixture, { emitRootPublicKey: true });

    assert.equal(result.rootPublicKey, expectedRootPublicKey);
    assert.equal(result.publisherCount, 1);
    assert.equal(result.packageCount, 1);
    assert.deepEqual(
      readFileSync(join(fixture.output, "registry.json")),
      readFileSync(join(fixtureRoot, "registry.json")),
    );
    assert.deepEqual(
      readFileSync(join(fixture.output, ...packageManifestPath.split("/"))),
      readFileSync(fixtureManifestPath),
    );
    assert.equal(
      readFileSync(join(fixture.output, "registry-root-public-key.txt"), "utf8"),
      `${expectedRootPublicKey}\n`,
    );
    const publishedManifest = JSON.parse(
      readFileSync(join(fixture.output, ...packageManifestPath.split("/")), "utf8"),
    );
    for (const target of publishedManifest.targets) {
      assert.deepEqual(
        readFileSync(
          join(
            fixture.output,
            "packages",
            "online-fixture",
            "1.2.3",
            ...target.executable.split("/"),
          ),
        ),
        readFileSync(join(fixtureRoot, "plugin.bin")),
      );
    }
  });
});

test("derives executable and manifest hashes from the exact published bytes", () => {
  withFixture((fixture) => {
    const manifestPath = join(fixture.source, ...packageManifestPath.split("/"));
    const sourceManifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    const changedTarget = sourceManifest.targets[0];
    writeFileSync(
      join(dirname(manifestPath), ...changedTarget.executable.split("/")),
      "changed executable bytes",
    );

    publish(fixture);

    const publishedManifestBytes = readFileSync(
      join(fixture.output, ...packageManifestPath.split("/")),
    );
    const publishedManifest = JSON.parse(publishedManifestBytes);
    const registry = JSON.parse(readFileSync(join(fixture.output, "registry.json"), "utf8"));
    assert.notEqual(publishedManifest.targets[0].sha256, sourceManifest.targets[0].sha256);
    assert.equal(
      registry.entries[0].manifestSha256,
      createHash("sha256").update(publishedManifestBytes).digest("hex"),
    );
    assert.notEqual(publishedManifest.signature, sourceManifest.signature);
  });
});

test("preserves explicit publisher, package, and manifest revocation state", () => {
  withFixture((fixture) => {
    fixture.config.publishers[0].revoked = true;
    fixture.config.packages[0].revoked = true;
    writeJson(fixture.configPath, fixture.config);
    const manifestPath = join(fixture.source, ...packageManifestPath.split("/"));
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    manifest.revoked = true;
    writeJson(manifestPath, manifest);

    publish(fixture);

    const registry = JSON.parse(readFileSync(join(fixture.output, "registry.json"), "utf8"));
    const publishedManifest = JSON.parse(
      readFileSync(join(fixture.output, ...packageManifestPath.split("/")), "utf8"),
    );
    assert.equal(registry.publishers[0].revoked, true);
    assert.equal(registry.entries[0].revoked, true);
    assert.equal(publishedManifest.revoked, true);
  });
});

test("rejects unsafe manifest and executable paths without leaving staging output", () => {
  withFixture((fixture) => {
    for (const invalidPath of [
      "../plugin.json",
      "packages//plugin.json",
      "packages/CON/1.2.3/plugin.json",
      "packages/com1.txt/1.2.3/plugin.json",
      "packages/example./1.2.3/plugin.json",
      "packages/插件/1.2.3/plugin.json",
      "packages/example/1.2.3/plugin.json:payload",
    ]) {
      fixture.config.packages[0].manifestPath = invalidPath;
      writeJson(fixture.configPath, fixture.config);
      assert.throws(() => publish(fixture), /safe POSIX relative path/u);
      assert.equal(existsSync(fixture.output), false);
    }

    fixture.config.packages[0].manifestPath = packageManifestPath;
    writeJson(fixture.configPath, fixture.config);
    const manifestPath = join(fixture.source, ...packageManifestPath.split("/"));
    const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
    for (const invalidPath of [
      "../../outside.exe",
      "bin/NUL.exe",
      "bin/plugin.exe:payload",
      "bin/plugin. ",
      "bin/插件",
    ]) {
      manifest.targets[0].executable = invalidPath;
      writeJson(manifestPath, manifest);
      assert.throws(() => publish(fixture), /safe POSIX relative path/u);
      assert.equal(existsSync(fixture.output), false);
    }
    assert.deepEqual(
      readdirSync(fixture.root).filter((name) => name.includes(".staging-")),
      [],
    );
  });
});

test("rejects duplicate packages and missing or surplus publisher keys", () => {
  withFixture((fixture) => {
    fixture.config.packages.push({ ...fixture.config.packages[0] });
    writeJson(fixture.configPath, fixture.config);
    assert.throws(() => publish(fixture), /reuses packages\/online-fixture\/1.2.3/u);
  });
  withFixture((fixture) => {
    assert.throws(
      () => publish(fixture, { publisherKeyPaths: new Map() }),
      /No private key was provided/u,
    );
    assert.throws(
      () =>
        publish(fixture, {
          publisherKeyPaths: new Map([
            ["fixture.publisher", fixture.publisherKeyPath],
            ["unknown.publisher", fixture.publisherKeyPath],
          ]),
        }),
      /unknown publisher unknown.publisher/u,
    );
  });
});

test("requires a new output directory outside the source tree", () => {
  withFixture((fixture) => {
    mkdirSync(fixture.output);
    assert.throws(() => publish(fixture), /output directory already exists/u);
  });
  withFixture((fixture) => {
    assert.throws(
      () => publish(fixture, { outputDirectory: join(fixture.source, "published") }),
      /must not be inside the source directory/u,
    );
  });
});

test("keeps registry-root and publisher signing keys separate", () => {
  withFixture((fixture) => {
    assert.throws(
      () =>
        publish(fixture, {
          publisherKeyPaths: new Map([["fixture.publisher", fixture.rootKeyPath]]),
        }),
      /must not reuse the registry root key/u,
    );
    assert.equal(existsSync(fixture.output), false);
  });
});

test("requires explicit valid release metadata and an exact version directory", () => {
  withFixture((fixture) => {
    fixture.config.generatedAt = "2026-02-31T00:00:00Z";
    writeJson(fixture.configPath, fixture.config);
    assert.throws(() => publish(fixture), /valid UTC timestamp/u);
  });
  withFixture((fixture) => {
    const oldManifest = join(fixture.source, ...packageManifestPath.split("/"));
    const wrongPath = "packages/online-fixture/not-the-version/plugin.json";
    const newManifest = join(fixture.source, ...wrongPath.split("/"));
    mkdirSync(dirname(newManifest), { recursive: true });
    const manifest = JSON.parse(readFileSync(oldManifest, "utf8"));
    writeJson(newManifest, manifest);
    for (const target of manifest.targets) {
      const oldExecutable = join(dirname(oldManifest), ...target.executable.split("/"));
      const newExecutable = join(dirname(newManifest), ...target.executable.split("/"));
      mkdirSync(dirname(newExecutable), { recursive: true });
      copyFileSync(oldExecutable, newExecutable);
    }
    fixture.config.packages[0].manifestPath = wrongPath;
    writeJson(fixture.configPath, fixture.config);
    assert.throws(() => publish(fixture), /exact manifest version directory 1\.2\.3/u);
    assert.equal(existsSync(fixture.output), false);
  });
});

test("publishes through the documented command-line interface", () => {
  withFixture((fixture) => {
    const result = spawnSync(
      process.execPath,
      [
        join(repositoryRoot, "eng", "publish-plugin-registry.mjs"),
        "--config",
        fixture.configPath,
        "--source",
        fixture.source,
        "--output",
        fixture.output,
        "--root-key",
        fixture.rootKeyPath,
        "--publisher-key",
        `fixture.publisher=${fixture.publisherKeyPath}`,
        "--emit-root-public-key",
      ],
      { encoding: "utf8", shell: false, windowsHide: true },
    );

    assert.equal(result.status, 0, result.stderr);
    assert.match(result.stdout, /Published 1 plugin package\(s\)/u);
    assert.match(result.stdout, new RegExp(expectedRootPublicKey.replaceAll("+", "\\+"), "u"));
    assert.equal(existsSync(join(fixture.output, "registry.json")), true);
  });
});
