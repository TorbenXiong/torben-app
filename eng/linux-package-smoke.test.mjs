import assert from "node:assert/strict";
import {
  chmodSync,
  mkdirSync,
  mkdtempSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { runLinuxPackageSmoke, safeCanonicalRelative } from "./linux-package-smoke.mjs";
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
  return mkdtempSync(join(tmpdir(), "torben-linux-smoke-test-"));
}

function removeFixture(root) {
  assert.ok(root.startsWith(join(tmpdir(), "torben-linux-smoke-test-")));
  rmSync(root, { recursive: true, force: true });
}

function writeElf(path, machine = 0x3e) {
  const bytes = Buffer.alloc(64);
  bytes.set([0x7f, 0x45, 0x4c, 0x46, 2, 1]);
  bytes.writeUInt16LE(machine, 18);
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, bytes);
  chmodSync(path, 0o755);
}

function writeExtractedBundle(root, { omittedSidecar, machine = 0x3e } = {}) {
  const executableDirectory = join(root, "usr", "bin");
  const desktopDirectory = join(root, "usr", "share", "applications");
  mkdirSync(executableDirectory, { recursive: true });
  mkdirSync(desktopDirectory, { recursive: true });
  writeFileSync(
    join(desktopDirectory, "io.github.torbenxiong.torbenapp.desktop"),
    "[Desktop Entry]\nName=Torben App\nExec=torben-desktop %U\nType=Application\n",
  );
  writeElf(join(executableDirectory, "torben-desktop"), machine);
  for (const sidecar of sidecars) {
    if (sidecar !== omittedSidecar) writeElf(join(executableDirectory, sidecar), machine);
  }
}

async function artifactFixture(root, format) {
  const artifacts = join(root, "artifacts");
  mkdirSync(artifacts);
  const suffix = { appimage: ".AppImage", deb: ".deb", rpm: ".rpm" }[format];
  writeFileSync(join(artifacts, `Torben-App_0.1.0_x86_64${suffix}`), `fixture-${format}`);
  writeElf(join(artifacts, "torben-0.1.0-x86_64-unknown-linux-gnu"));
  await createReleaseMetadata({
    artifacts,
    target: "x86_64-unknown-linux-gnu",
    revision: "a".repeat(40),
    sourceRef: "refs/heads/feature/bootstrap",
    releaseKind: "development",
    signingStatus: "unsigned",
    repositoryRoot,
  });
  return artifacts;
}

function fixtureExecutor(calls, options = {}) {
  return ({ stage, command, cwd, args, env }) => {
    calls.push({ stage, command, cwd, args, env });
    if (stage === "extract-appimage") {
      writeExtractedBundle(join(cwd, "squashfs-root"), options);
      return { status: 0, stdout: "", stderr: "" };
    }
    if (stage === "extract-deb") {
      writeExtractedBundle(args[2], options);
      return { status: 0, stdout: "", stderr: "" };
    }
    if (stage === "extract-rpm") {
      writeExtractedBundle(cwd, options);
      return { status: 0, stdout: "", stderr: "" };
    }
    if (stage === "install-deb" || stage === "install-rpm") {
      if (options.installResult) return options.installResult;
      writeExtractedBundle(options.systemRoot, options);
      return { status: 0, stdout: "", stderr: "" };
    }
    if (stage === "launch") {
      return options.launchResult ?? { status: 124, stdout: "", stderr: "" };
    }
    throw new Error(`Unexpected fixture command stage: ${stage}`);
  };
}

test("canonical path checks accept root aliases and reject real escapes", () => {
  const root = fixtureRoot();
  try {
    const actualRoot = join(root, "actual");
    const aliasRoot = join(root, "alias");
    const nested = join(actualRoot, "nested");
    const outside = join(root, "outside");
    mkdirSync(nested, { recursive: true });
    mkdirSync(outside);
    writeFileSync(join(nested, "inside"), "inside");
    writeFileSync(join(outside, "escaped"), "escaped");
    symlinkSync(actualRoot, aliasRoot, process.platform === "win32" ? "junction" : "dir");

    assert.equal(
      safeCanonicalRelative(aliasRoot, join(actualRoot, "nested", "inside"), "Fixture"),
      "nested/inside",
    );

    const escapeLink = join(actualRoot, "escape");
    symlinkSync(outside, escapeLink, process.platform === "win32" ? "junction" : "dir");
    assert.throws(
      () => safeCanonicalRelative(aliasRoot, join(escapeLink, "escaped"), "Fixture"),
      /Fixture escapes the extraction root/,
    );
  } finally {
    removeFixture(root);
  }
});

test("verifies package contents and a sustained isolated launch for every Linux format", async (t) => {
  for (const format of ["appimage", "deb", "rpm"]) {
    await t.test(format, async () => {
      const root = fixtureRoot();
      try {
        const artifacts = await artifactFixture(root, format);
        const calls = [];
        const result = await runLinuxPackageSmoke({
          artifacts,
          format,
          repositoryRoot,
          execute: fixtureExecutor(calls),
          platform: "linux",
          architecture: "x64",
          environment: {
            PATH: "/fixture/bin",
            LANG: "C.UTF-8",
            TORBEN_FIXTURE_SECRET: "must-not-reach-the-app",
          },
        });

        assert.equal(result.ok, true);
        assert.equal(result.mode, "extract");
        assert.equal(result.systemInstalled, false);
        assert.equal(result.target, "x86_64-unknown-linux-gnu");
        assert.equal(result.sidecars.length, 7);
        assert.equal(result.launch.observedStatus, 124);
        assert.deepEqual(
          calls.map((call) => call.stage),
          [`extract-${format}`, "launch"],
        );
        if (format === "deb") assert.equal(calls[0].command, "/usr/bin/dpkg-deb");
        if (format === "rpm") {
          assert.equal(calls[0].command, "/bin/bash");
          assert.match(calls[0].args[1], /\/usr\/bin\/rpm2cpio/);
          assert.match(calls[0].args[1], /\/usr\/bin\/cpio/);
        }
        const launch = calls[1];
        assert.equal(launch.command, "/usr/bin/timeout");
        assert.ok(launch.args.includes("/usr/bin/xvfb-run"));
        assert.ok(launch.args.at(-1).endsWith("torben-desktop"));
        assert.equal(launch.env.PATH, "/fixture/bin");
        assert.equal(launch.env.LANG, "C.UTF-8");
        assert.equal(launch.env.TORBEN_FIXTURE_SECRET, undefined);
        assert.match(launch.env.XDG_CONFIG_HOME, /torben-linux-package-smoke-/);
      } finally {
        removeFixture(root);
      }
    });
  }
});

test("installs deb and rpm packages only through their native manager before launch", async (t) => {
  for (const format of ["deb", "rpm"]) {
    await t.test(format, async () => {
      const root = fixtureRoot();
      try {
        const artifacts = await artifactFixture(root, format);
        const systemRoot = join(root, "system-root");
        const calls = [];
        const result = await runLinuxPackageSmoke({
          artifacts,
          format,
          mode: "install",
          repositoryRoot,
          execute: fixtureExecutor(calls, { systemRoot }),
          platform: "linux",
          architecture: "x64",
          effectiveUserId: 0,
          systemRoot,
        });

        assert.equal(result.mode, "install");
        assert.equal(result.systemInstalled, true);
        assert.ok(result.executable.startsWith(realpathSync(systemRoot)));
        assert.deepEqual(
          calls.map((call) => call.stage),
          [`extract-${format}`, `install-${format}`, "launch"],
        );
        const installation = calls[1];
        assert.equal(
          installation.command,
          format === "deb" ? "/usr/bin/apt-get" : "/usr/bin/dnf",
        );
        assert.equal(installation.args.at(-1).endsWith(`.${format}`), true);
        assert.equal(installation.args[0], format === "deb" ? "--quiet=2" : "--quiet");
        assert.ok(calls[2].args.at(-1).startsWith(realpathSync(systemRoot)));
      } finally {
        removeFixture(root);
      }
    });
  }
});

test("AppImage install mode launches the verified portable package without system mutation", async () => {
  const root = fixtureRoot();
  try {
    const artifacts = await artifactFixture(root, "appimage");
    const calls = [];
    const result = await runLinuxPackageSmoke({
      artifacts,
      format: "appimage",
      mode: "install",
      repositoryRoot,
      execute: fixtureExecutor(calls),
      platform: "linux",
      architecture: "x64",
      effectiveUserId: 1_000,
    });

    assert.equal(result.systemInstalled, false);
    assert.deepEqual(
      calls.map((call) => call.stage),
      ["extract-appimage", "launch"],
    );
    assert.ok(calls[1].args.at(-1).endsWith(".AppImage"));
    assert.equal(calls[1].env.APPIMAGE_EXTRACT_AND_RUN, "1");
  } finally {
    removeFixture(root);
  }
});

test("system installation fails closed for non-root and package-manager errors", async (t) => {
  await t.test("non-root", async () => {
    const root = fixtureRoot();
    try {
      const artifacts = await artifactFixture(root, "deb");
      let executed = false;
      await assert.rejects(
        runLinuxPackageSmoke({
          artifacts,
          format: "deb",
          mode: "install",
          repositoryRoot,
          execute: () => {
            executed = true;
          },
          platform: "linux",
          architecture: "x64",
          effectiveUserId: 1_000,
        }),
        /require root inside a disposable Linux environment/,
      );
      assert.equal(executed, false);
    } finally {
      removeFixture(root);
    }
  });

  await t.test("package manager failure", async () => {
    const root = fixtureRoot();
    try {
      const artifacts = await artifactFixture(root, "rpm");
      const systemRoot = join(root, "system-root");
      const calls = [];
      await assert.rejects(
        runLinuxPackageSmoke({
          artifacts,
          format: "rpm",
          mode: "install",
          repositoryRoot,
          execute: fixtureExecutor(calls, {
            systemRoot,
            installResult: { status: 1, stdout: "", stderr: "dependency resolution failed" },
          }),
          platform: "linux",
          architecture: "x64",
          effectiveUserId: 0,
          systemRoot,
        }),
        /rpm system installation failed with status 1: dependency resolution failed/,
      );
      assert.deepEqual(
        calls.map((call) => call.stage),
        ["extract-rpm", "install-rpm"],
      );
    } finally {
      removeFixture(root);
    }
  });

  await t.test("package manager omitted installed payload", async () => {
    const root = fixtureRoot();
    try {
      const artifacts = await artifactFixture(root, "deb");
      const systemRoot = join(root, "system-root");
      mkdirSync(systemRoot);
      const calls = [];
      await assert.rejects(
        runLinuxPackageSmoke({
          artifacts,
          format: "deb",
          mode: "install",
          repositoryRoot,
          execute: fixtureExecutor(calls, {
            systemRoot,
            installResult: { status: 0, stdout: "", stderr: "" },
          }),
          platform: "linux",
          architecture: "x64",
          effectiveUserId: 0,
          systemRoot,
        }),
        /Installed desktop entry is missing/,
      );
      assert.deepEqual(
        calls.map((call) => call.stage),
        ["extract-deb", "install-deb"],
      );
    } finally {
      removeFixture(root);
    }
  });
});

test("fails before extraction when the native host architecture does not match", async () => {
  const root = fixtureRoot();
  try {
    const artifacts = await artifactFixture(root, "deb");
    let executed = false;
    await assert.rejects(
      runLinuxPackageSmoke({
        artifacts,
        format: "deb",
        repositoryRoot,
        execute: () => {
          executed = true;
        },
        platform: "linux",
        architecture: "arm64",
      }),
      /host architecture aarch64 does not match x86_64/,
    );
    assert.equal(executed, false);
  } finally {
    removeFixture(root);
  }
});

test("rejects missing sidecars and an application that exits before the launch window", async (t) => {
  await t.test("missing sidecar", async () => {
    const root = fixtureRoot();
    try {
      const artifacts = await artifactFixture(root, "rpm");
      const calls = [];
      await assert.rejects(
        runLinuxPackageSmoke({
          artifacts,
          format: "rpm",
          repositoryRoot,
          execute: fixtureExecutor(calls, { omittedSidecar: "torben-plugin-codex" }),
          platform: "linux",
          architecture: "x64",
        }),
        /Expected exactly one bundled torben-plugin-codex sidecar, found 0/,
      );
      assert.deepEqual(
        calls.map((call) => call.stage),
        ["extract-rpm"],
      );
    } finally {
      removeFixture(root);
    }
  });

  await t.test("early process exit", async () => {
    const root = fixtureRoot();
    try {
      const artifacts = await artifactFixture(root, "appimage");
      await assert.rejects(
        runLinuxPackageSmoke({
          artifacts,
          format: "appimage",
          repositoryRoot,
          execute: fixtureExecutor([], {
            launchResult: { status: 1, stdout: "", stderr: "fixture startup failure" },
          }),
          platform: "linux",
          architecture: "x64",
        }),
        /launch probe failed with status 1: fixture startup failure/,
      );
    } finally {
      removeFixture(root);
    }
  });
});
