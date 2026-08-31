import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { splitWindowsPreview } from "./split-windows-preview.mjs";

const fixturePrefix = join(tmpdir(), "torben-windows-preview-split-");
const scriptPath = fileURLToPath(new URL("./split-windows-preview.mjs", import.meta.url));

function fixture() {
  const root = mkdtempSync(fixturePrefix);
  const source = join(root, "candidate");
  mkdirSync(source);
  const files = {
    nsis: "Torben App_0.1.0_x64-setup.exe",
    msi: "Torben App_0.1.0_x64_en-US.msi",
    cli: "torben-0.1.0-x86_64-pc-windows-msvc.zip",
  };
  for (const [kind, name] of Object.entries(files)) {
    writeFileSync(join(source, name), `${kind}-fixture`);
  }
  writeFileSync(join(source, "UNSIGNED-PREVIEW.txt"), "unsigned fixture");
  return { root, source, output: join(root, "preview"), files };
}

function removeFixture(root) {
  assert.ok(root.startsWith(fixturePrefix));
  rmSync(root, { recursive: true, force: true });
}

test("splits NSIS, MSI, and CLI previews with independent checksums", async () => {
  const current = fixture();
  try {
    const selected = await splitWindowsPreview(current);
    assert.deepEqual(
      selected.map(({ kind }) => kind),
      ["nsis", "msi", "cli"],
    );
    for (const [kind, name] of Object.entries(current.files)) {
      const directory = join(current.output, kind);
      assert.deepEqual(
        readdirSync(directory).sort(),
        ["SHA256SUMS", "UNSIGNED-PREVIEW.txt", name].sort(),
      );
      const expectedHash = createHash("sha256")
        .update(readFileSync(join(current.source, name)))
        .digest("hex");
      assert.equal(
        readFileSync(join(directory, "SHA256SUMS"), "utf8"),
        `${expectedHash}  ${name}\n`,
      );
    }
  } finally {
    removeFixture(current.root);
  }
});

test("fails closed for ambiguous downloads and existing output", async () => {
  const ambiguous = fixture();
  try {
    writeFileSync(join(ambiguous.source, "duplicate-setup.exe"), "duplicate");
    await assert.rejects(splitWindowsPreview(ambiguous), /exactly one nsis preview download/);
  } finally {
    removeFixture(ambiguous.root);
  }

  const existing = fixture();
  try {
    mkdirSync(existing.output);
    await assert.rejects(splitWindowsPreview(existing), /Preview output already exists/);
  } finally {
    removeFixture(existing.root);
  }
});

test("publishes split previews through the command-line entry point", () => {
  const current = fixture();
  try {
    const result = spawnSync(
      process.execPath,
      [scriptPath, "--source", current.source, "--output", current.output],
      { encoding: "utf8", timeout: 10_000 },
    );
    assert.equal(result.status, 0, result.stderr || result.stdout);
    assert.match(result.stdout, /Prepared 3 separate Windows preview downloads/);
    assert.deepEqual(readdirSync(current.output).sort(), ["cli", "msi", "nsis"]);
  } finally {
    removeFixture(current.root);
  }
});
