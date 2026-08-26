import assert from "node:assert/strict";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { createReleaseMetadata, supportedTargets } from "./release-metadata.mjs";
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

test("development workflow pins approved Actions and cannot publish an official release", () => {
  const workflow = readFileSync(
    join(repositoryRoot, ".github", "workflows", "release.yml"),
    "utf8",
  );
  assert.match(workflow, /^on:\r?\n {2}workflow_dispatch:\s*$/m);
  assert.doesNotMatch(workflow, /^\s+push:\s*$/m);
  assert.doesNotMatch(workflow, /contents:\s*write/);
  assert.doesNotMatch(workflow, /gh release|--release-kind official/);
  assert.match(workflow, /--release-kind development/);
  assert.match(workflow, /--signing-status unsigned/);
  assert.match(workflow, /prepare:bundled-tools:release/);
  assert.match(workflow, /--target \$\{\{ matrix\.target \}\}/);
  assert.match(workflow, /-- --locked/);
  assert.match(workflow, /Compress-Archive/);
  assert.match(workflow, /tar --create --gzip/);
  assert.match(workflow, /^permissions:\r?\n {2}contents: read$/m);
  assert.match(
    workflow,
    /^ {2}linux-package-acceptance:\r?\n {4}name: Install and launch Linux packages\r?\n {4}needs: build\r?\n {4}uses: \.\/\.github\/workflows\/linux-package-acceptance\.yml$/m,
  );
  assert.match(
    workflow,
    /^ {2}desktop-package-acceptance:\r?\n {4}name: Install and launch Windows and macOS packages\r?\n {4}needs: build\r?\n {4}uses: \.\/\.github\/workflows\/desktop-package-acceptance\.yml$/m,
  );
  assert.match(
    workflow,
    /^ {2}aggregate:\r?\n {4}name: Verify six-target release set\r?\n {4}needs: \[build, desktop-package-acceptance, linux-package-acceptance\]$/m,
  );
  assert.equal((workflow.match(/retention-days: 14/g) ?? []).length, 2);
  assert.equal((workflow.match(/if-no-files-found: error/g) ?? []).length, 2);

  const expectedActions = new Map([
    ["actions/checkout", "3d3c42e5aac5ba805825da76410c181273ba90b1"],
    ["actions/setup-node", "820762786026740c76f36085b0efc47a31fe5020"],
    ["actions/upload-artifact", "043fb46d1a93c77aae656e7c1c64a875d1fc6a0a"],
    ["actions/download-artifact", "3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c"],
  ]);
  const continuousIntegration = readFileSync(
    join(repositoryRoot, ".github", "workflows", "ci.yml"),
    "utf8",
  );
  const officialWorkflow = readFileSync(
    join(repositoryRoot, ".github", "workflows", "official-release.yml"),
    "utf8",
  );
  const acceptanceWorkflow = readFileSync(
    join(repositoryRoot, ".github", "workflows", "linux-package-acceptance.yml"),
    "utf8",
  );
  const desktopAcceptanceWorkflow = readFileSync(
    join(repositoryRoot, ".github", "workflows", "desktop-package-acceptance.yml"),
    "utf8",
  );
  const usesLines =
    `${continuousIntegration}\n${workflow}\n${officialWorkflow}\n${acceptanceWorkflow}\n${desktopAcceptanceWorkflow}`
      .split(/\r?\n/)
      .map((line) => line.trim())
      .filter((line) => line.startsWith("uses:"));
  assert.ok(usesLines.length > 0);
  const reusableCalls = usesLines.filter((line) => line.startsWith("uses: ./"));
  assert.deepEqual(reusableCalls, [
    "uses: ./.github/workflows/linux-package-acceptance.yml",
    "uses: ./.github/workflows/desktop-package-acceptance.yml",
    "uses: ./.github/workflows/linux-package-acceptance.yml",
    "uses: ./.github/workflows/desktop-package-acceptance.yml",
  ]);
  for (const line of usesLines) {
    if (line.startsWith("uses: ./")) continue;
    const match = line.match(/^uses: ([^@\s]+)@([0-9a-f]{40})(?:\s+#.*)?$/);
    assert.ok(match, `Action is not pinned to a full commit SHA: ${line}`);
    assert.equal(match[2], expectedActions.get(match[1]), `Unexpected Action revision: ${line}`);
  }

  assert.match(acceptanceWorkflow, /^on:\r?\n {2}workflow_call:$/m);
  assert.match(acceptanceWorkflow, /^permissions:\r?\n {2}contents: read$/m);
  assert.match(acceptanceWorkflow, /^ {6}fail-fast: false$/m);
  assert.match(acceptanceWorkflow, /^ {6}options: --user 0$/m);
  assert.match(acceptanceWorkflow, /--mode install/);
  assert.match(acceptanceWorkflow, /apt-get install --yes --no-install-recommends/);
  assert.match(acceptanceWorkflow, /dnf install --assumeyes/);
  assert.match(acceptanceWorkflow, /microdnf install --assumeyes/);
  assert.match(acceptanceWorkflow, /shell: \/bin\/sh -e \{0\}/);
  const rockyBootstrap = acceptanceWorkflow.match(/ {12}rocky\)\r?\n([\s\S]*?) {14};;/)?.[1];
  assert.ok(rockyBootstrap, "Rocky Linux acceptance bootstrap is missing");
  const rockyPackages = rockyBootstrap.match(/microdnf install --assumeyes \\\r?\n([\s\S]*)/)?.[1];
  assert.ok(rockyPackages, "Rocky Linux microdnf package list is missing");
  assert.doesNotMatch(rockyPackages, /(?:^|\s)coreutils(?:\s|\\|$)/m);
  assert.doesNotMatch(acceptanceWorkflow, /continue-on-error|secrets\./);
  const acceptanceImages = [...acceptanceWorkflow.matchAll(/^\s+image: (\S+)$/gm)]
    .map((match) => match[1])
    .filter((image) => !image.startsWith("${{"));
  assert.deepEqual(acceptanceImages, [
    "ubuntu:24.04",
    "debian:13-slim",
    "fedora:44",
    "rockylinux/rockylinux:9.8-minimal",
  ]);
  const acceptanceTargets = [...acceptanceWorkflow.matchAll(/^\s+target: (\S+)$/gm)].map(
    (match) => match[1],
  );
  assert.deepEqual(acceptanceTargets, ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"]);
  const acceptanceRunners = [...acceptanceWorkflow.matchAll(/^\s+- runner: (\S+)$/gm)].map(
    (match) => match[1],
  );
  assert.deepEqual(acceptanceRunners, ["ubuntu-24.04", "ubuntu-24.04-arm"]);

  assert.match(desktopAcceptanceWorkflow, /^on:\r?\n {2}workflow_call:$/m);
  assert.match(desktopAcceptanceWorkflow, /^permissions:\r?\n {2}contents: read$/m);
  assert.match(desktopAcceptanceWorkflow, /^ {6}fail-fast: false$/m);
  assert.match(desktopAcceptanceWorkflow, /desktop-package-smoke\.mjs/);
  assert.match(desktopAcceptanceWorkflow, /Sort-Object -Property FullName -Unique/);
  assert.match(desktopAcceptanceWorkflow, /Start-Process msiexec\.exe/);
  assert.match(desktopAcceptanceWorkflow, /Start-Process \$package/);
  assert.match(desktopAcceptanceWorkflow, /Start-Process \$uninstallers\[0\]\.FullName/);
  assert.equal(
    (desktopAcceptanceWorkflow.match(/-Wait -PassThru -WindowStyle Hidden/g) ?? []).length,
    4,
  );
  assert.doesNotMatch(desktopAcceptanceWorkflow, /& msiexec\.exe|& \$package \/S/);
  assert.match(desktopAcceptanceWorkflow, /hdiutil attach -readonly -nobrowse/);
  assert.match(desktopAcceptanceWorkflow, /ditto "\$\{applications\[0\]\}" "\$installed_app"/);
  assert.doesNotMatch(desktopAcceptanceWorkflow, /continue-on-error|secrets\./);
  const desktopNames = [
    ...desktopAcceptanceWorkflow.matchAll(/^\s+- name: (.+ (?:NSIS|MSI|DMG))$/gm),
  ].map((match) => match[1]);
  assert.deepEqual(desktopNames, [
    "Windows x64 NSIS",
    "Windows x64 MSI",
    "Windows ARM64 NSIS",
    "Windows ARM64 MSI",
    "macOS Intel DMG",
    "macOS Apple Silicon DMG",
  ]);

  const matrixBlock = workflow.match(/matrix:\r?\n([\s\S]*?) {4}runs-on:/)?.[1] ?? "";
  const targets = [...matrixBlock.matchAll(/^\s+target: (\S+)$/gm)].map((match) => match[1]);
  assert.deepEqual(targets, Object.keys(supportedTargets));
  assert.doesNotMatch(workflow, /--bundle-root target\/release\/bundle/);
  for (const target of Object.keys(supportedTargets)) {
    assert.match(workflow, new RegExp(`bundle: target/${target}/release/bundle`));
  }

  assert.match(officialWorkflow, /^on:\r?\n {2}push:\r?\n {4}tags: \["v\*\.\*\.\*"\]$/m);
  assert.match(
    officialWorkflow,
    /process\.env\.GITHUB_REF !== `refs\/tags\/v\$\{version\}`/,
    "official release must bind the tag to the exact workspace version",
  );
  assert.equal((officialWorkflow.match(/^ {4}environment: official-release$/gm) ?? []).length, 2);
  assert.equal((officialWorkflow.match(/contents: write/g) ?? []).length, 1);
  assert.match(
    officialWorkflow,
    /^ {2}linux-package-acceptance:\r?\n {4}name: Install and launch signed Linux packages\r?\n {4}needs: build\r?\n {4}uses: \.\/\.github\/workflows\/linux-package-acceptance\.yml\r?\n {4}with:\r?\n {6}artifact-prefix: official-$/m,
  );
  assert.match(
    officialWorkflow,
    /^ {2}desktop-package-acceptance:\r?\n {4}name: Install and launch signed Windows and macOS packages\r?\n {4}needs: build\r?\n {4}uses: \.\/\.github\/workflows\/desktop-package-acceptance\.yml\r?\n {4}with:\r?\n {6}artifact-prefix: official-$/m,
  );
  assert.match(
    officialWorkflow,
    /^ {2}publish:\r?\n {4}name: Verify and publish official release\r?\n {4}needs: \[build, desktop-package-acceptance, linux-package-acceptance\]$/m,
  );
  assert.match(officialWorkflow, /TAURI_SIGNING_PRIVATE_KEY/);
  assert.match(officialWorkflow, /TORBEN_UPDATER_PUBLIC_KEY/);
  assert.match(officialWorkflow, /Get-AuthenticodeSignature/);
  assert.match(officialWorkflow, /xcrun stapler validate/);
  assert.match(officialWorkflow, /verify-updater/);
  assert.match(officialWorkflow, /verify-updater-artifacts\.mjs/);
  assert.doesNotMatch(
    officialWorkflow,
    /JSON\.parse\(readFileSync\(join\(root, "updater-artifacts\.json"/,
  );
  assert.ok(
    officialWorkflow.indexOf("- name: Generate signed official metadata") <
      officialWorkflow.indexOf("- name: Verify every updater signature"),
    "signed target metadata must exist before the strict updater mapping verifier runs",
  );
  assert.match(officialWorkflow, /generate-updater-manifest\.mjs/);
  assert.match(
    officialWorkflow,
    /prepare-github-release-assets\.mjs\r?\n\s+create\r?\n\s+--releases artifacts\/release-set/,
  );
  assert.match(
    officialWorkflow,
    /prepare-github-release-assets\.mjs\r?\n\s+verify\r?\n\s+--releases artifacts\/release-set/,
  );
  assert.match(officialWorkflow, /gh release create/);
  assert.match(
    officialWorkflow,
    /gh release create "\$\{GITHUB_REF_NAME\}" artifacts\/publishing\/\*/,
  );
  assert.match(officialWorkflow, /--verify-tag/);
  assert.match(officialWorkflow, /--release-kind official/);
  assert.match(officialWorkflow, /--signing-status signed/);
  assert.doesNotMatch(officialWorkflow, /--skip-stapling|--no-sign|continue-on-error/);
  const officialMatrixBlock =
    officialWorkflow.match(/matrix:\r?\n([\s\S]*?) {4}runs-on:/)?.[1] ?? "";
  const officialTargets = [...officialMatrixBlock.matchAll(/^\s+target: (\S+)$/gm)].map(
    (match) => match[1],
  );
  assert.deepEqual(officialTargets, Object.keys(supportedTargets));
  assert.match(
    officialWorkflow,
    /for target in x86_64-pc-windows-msvc aarch64-pc-windows-msvc x86_64-apple-darwin aarch64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do/,
  );
  assert.match(
    officialWorkflow,
    /mv "artifacts\/downloaded\/official-\$target" "artifacts\/release-set\/\$target"/,
  );
  assert.match(
    officialWorkflow,
    /test -z "\$\(find artifacts\/downloaded -mindepth 1 -print -quit\)"/,
  );

  const publicationGates = [
    "Download all signed targets",
    "Normalize exact target directories",
    "Re-verify every downloaded target and updater signature",
    "Generate signed static updater manifest",
    "Create and re-verify official six-target release set",
    "Prepare unique flat GitHub Release assets",
    "Re-verify flat GitHub Release assets",
    "Create immutable GitHub Release",
  ];
  let previousGate = -1;
  for (const gate of publicationGates) {
    const position = officialWorkflow.indexOf(`- name: ${gate}`);
    assert.ok(
      position > previousGate,
      `official publication gate is missing or out of order: ${gate}`,
    );
    previousGate = position;
  }
});
