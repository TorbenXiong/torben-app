import assert from "node:assert/strict";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { checkLiveCatalogs, officialCatalogApps } from "./check-live-catalogs.mjs";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));

function fixture() {
  const root = mkdtempSync(join(tmpdir(), "torben-live-catalogs-"));
  const cli = join(root, "torben-fixture");
  const parent = join(root, "artifacts");
  writeFileSync(cli, "fixture");
  mkdirSync(parent);
  return { root, cli, output: join(parent, "live-catalogs") };
}

function removeFixture(root) {
  assert.ok(root.startsWith(join(tmpdir(), "torben-live-catalogs-")));
  rmSync(root, { force: true, recursive: true });
}

function successEnvelope(app) {
  return JSON.stringify({
    schemaVersion: 1,
    ok: true,
    data: [
      {
        version: `${officialCatalogApps.indexOf(app) + 1}.2.3`,
        ltsName: app === "node" ? "LTS" : null,
        releasedAt: "2026-08-24",
        recommended: true,
      },
    ],
  });
}

test("validates all six Torben provider catalogs and commits one complete artifact", () => {
  const current = fixture();
  const calls = [];
  try {
    const summary = checkLiveCatalogs({
      cliPath: current.cli,
      outputDirectory: current.output,
      environment: { PATH: "/fixture/bin", SECRET_TOKEN: "must-not-be-forwarded" },
      execute: ({ cliPath, app, environment }) => {
        calls.push({ cliPath, app, environment });
        return { status: 0, stdout: successEnvelope(app), stderr: "" };
      },
    });

    assert.deepEqual(
      calls.map((call) => call.app),
      officialCatalogApps,
    );
    assert.equal(calls[0].environment.SECRET_TOKEN, undefined);
    assert.equal(calls[0].environment.PATH, "/fixture/bin");
    assert.equal(summary.catalogs.length, 6);
    assert.equal(
      JSON.parse(readFileSync(join(current.output, "catalog-summary.json"), "utf8")).catalogs
        .length,
      6,
    );
    for (const app of officialCatalogApps) {
      assert.equal(JSON.parse(readFileSync(join(current.output, `${app}.json`), "utf8")).ok, true);
    }
  } finally {
    removeFixture(current.root);
  }
});

test("rejects an error envelope without publishing a partial artifact", () => {
  const current = fixture();
  try {
    assert.throws(
      () =>
        checkLiveCatalogs({
          cliPath: current.cli,
          outputDirectory: current.output,
          execute: ({ app }) => ({
            status: 0,
            stdout:
              app === "python"
                ? JSON.stringify({ schemaVersion: 1, ok: false, error: { code: "fixture" } })
                : successEnvelope(app),
            stderr: "",
          }),
        }),
      /Official python catalog returned an invalid Torben JSON envelope/,
    );
    assert.throws(() => readFileSync(current.output), /ENOENT/);
  } finally {
    removeFixture(current.root);
  }
});

test("rejects empty, duplicate, and unrecommended catalog data", () => {
  for (const data of [
    [],
    [
      { version: "1.2.3", ltsName: null, releasedAt: "2026-08-24", recommended: true },
      { version: "1.2.3", ltsName: null, releasedAt: "2026-08-24", recommended: true },
    ],
    [{ version: "1.2.3", ltsName: null, releasedAt: "2026-08-24", recommended: false }],
  ]) {
    const current = fixture();
    try {
      assert.throws(() =>
        checkLiveCatalogs({
          cliPath: current.cli,
          outputDirectory: current.output,
          execute: () => ({
            status: 0,
            stdout: JSON.stringify({ schemaVersion: 1, ok: true, data }),
            stderr: "",
          }),
        }),
      );
    } finally {
      removeFixture(current.root);
    }
  }
});

test("scheduled CI builds and verifies every official provider through the real CLI", () => {
  const workflow = readFileSync(join(repositoryRoot, ".github", "workflows", "ci.yml"), "utf8");
  assert.match(
    workflow,
    /^ {2}live-official-catalogs:\r?\n {4}if: github\.event_name == 'schedule'$/m,
  );
  assert.match(
    workflow,
    /node eng\/check-live-catalogs\.mjs\s+--cli target\/debug\/torben\s+--output artifacts\/live-catalogs/,
  );
  for (const app of officialCatalogApps) {
    assert.match(workflow, new RegExp(`-p torben-plugin-${app}`));
  }
  assert.doesNotMatch(workflow, /live-node-metadata|curl .*nodejs\.org/);
  assert.match(
    workflow,
    /actions\/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7\.0\.1/,
  );
});
