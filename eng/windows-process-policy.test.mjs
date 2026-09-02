import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));

test("Windows background process policy is applied at shared Core and plugin-host boundaries", () => {
  const coreProcess = readFileSync(
    join(repositoryRoot, "crates", "torben-core", "src", "process.rs"),
    "utf8",
  );
  const pluginHost = readFileSync(
    join(repositoryRoot, "crates", "torben-plugin-host", "src", "lib.rs"),
    "utf8",
  );
  assert.match(coreProcess, /CREATE_NO_WINDOW: u32 = 0x0800_0000/);
  assert.equal((coreProcess.match(/creation_flags\(CREATE_NO_WINDOW\)/g) ?? []).length, 2);
  assert.match(pluginHost, /command\.creation_flags\(0x0800_0000\)/);
});
