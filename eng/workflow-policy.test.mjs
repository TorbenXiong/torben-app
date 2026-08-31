import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const workflowDirectory = join(repositoryRoot, ".github", "workflows");

function workflows() {
  return readdirSync(workflowDirectory)
    .filter((name) => name.endsWith(".yml") || name.endsWith(".yaml"))
    .map((name) => ({
      name,
      content: readFileSync(join(workflowDirectory, name), "utf8"),
    }));
}

test("all external GitHub Actions use immutable commit revisions", () => {
  for (const workflow of workflows()) {
    for (const match of workflow.content.matchAll(/^\s*(?:-\s*)?uses:\s*(\S+)/gmu)) {
      const reference = match[1];
      if (reference.startsWith("./")) continue;
      assert.match(
        reference,
        /^[^@\s]+@[0-9a-f]{40}$/u,
        `${workflow.name} has an unpinned Action: ${reference}`,
      );
    }
  }
});

test("workflow package installation and Cargo commands honor lock files", () => {
  for (const workflow of workflows()) {
    const lines = workflow.content.split(/\r?\n/gu);
    for (const line of lines) {
      if (line.includes("pnpm install")) {
        assert.match(line, /pnpm install\s+--frozen-lockfile/u, workflow.name);
      }
      if (/\bcargo\s+(?:build|clippy|run|test)\b/u.test(line)) {
        assert.match(line, /--locked\b/u, `${workflow.name}: ${line.trim()}`);
      }
    }
  }
});

test("ordinary CI runs locked lint and test gates", () => {
  const ci = readFileSync(join(workflowDirectory, "ci.yml"), "utf8");

  assert.match(ci, /name: Test Windows x64[\s\S]*?runs-on: windows-latest/u);
  assert.doesNotMatch(ci, /matrix:\s*\n\s+os:/u);
  assert.doesNotMatch(ci, /(?:macos-latest|ubuntu-24\.04)/u);
  assert.match(ci, /cargo clippy --workspace --all-targets --locked -- -D warnings/u);
  assert.match(ci, /cargo test --workspace --locked/u);
});

test("official catalog checks allow scheduled evidence and an equivalent manual preflight", () => {
  const ci = readFileSync(join(workflowDirectory, "ci.yml"), "utf8");

  assert.match(ci, /^\s{2}workflow_dispatch:$/mu);
  assert.match(ci, /^\s{2}schedule:$/mu);
  assert.match(
    ci,
    /if: github\.event_name == 'schedule' \|\| github\.event_name == 'workflow_dispatch'/u,
  );
  assert.match(ci, /name: Verify all official provider catalogs/u);
  assert.match(ci, /name: Verify all official provider catalogs[\s\S]*?runs-on: windows-latest/u);
  assert.match(ci, /--cli target\/debug\/torben\.exe/u);
  assert.match(ci, /node eng\/check-live-catalogs\.mjs/u);
  assert.match(ci, /name: live-official-catalogs/u);
  assert.match(ci, /^permissions:\n\s{2}contents: read$/mu);
});

test("release workflows preserve Cargo arguments and native package prerequisites", () => {
  for (const name of ["release.yml", "official-release.yml"]) {
    const release = readFileSync(join(workflowDirectory, name), "utf8");

    assert.match(release, /working-directory: apps\/desktop/u, name);
    assert.match(
      release,
      /node node_modules\/@tauri-apps\/cli\/tauri\.js build[\s\S]*?-- --locked/u,
      name,
    );
    assert.doesNotMatch(release, /pnpm[^\n]*exec tauri build/u, name);
    assert.match(release, /patchelf xdg-utils/u, name);
    assert.doesNotMatch(release, /require\("\.\/package\.json"\)/u, name);
    assert.match(release, /join\(process\.env\.GITHUB_WORKSPACE, "package\.json"\)/u, name);
  }
});

test("Windows preview remains manual, unsigned, read-only, and Windows x64 scoped", () => {
  const preview = readFileSync(join(workflowDirectory, "windows-preview.yml"), "utf8");

  assert.match(preview, /^name: Windows x64 unsigned preview$/mu);
  assert.match(preview, /^\s{2}workflow_dispatch:$/mu);
  assert.doesNotMatch(preview, /^\s{2}(?:push|pull_request|schedule|release):/mu);
  assert.match(preview, /^permissions:\n\s{2}contents: read$/mu);
  assert.match(preview, /runs-on: windows-latest/u);
  assert.match(preview, /--target x86_64-pc-windows-msvc/u);
  assert.match(preview, /--release-kind development/u);
  assert.match(preview, /--signing-status unsigned/u);
  assert.match(preview, /UNSIGNED-PREVIEW\.txt/u);
  assert.match(preview, /acceptance_matrix:/u);
  assert.match(preview, /"format":"nsis"/u);
  assert.match(preview, /"format":"msi"/u);
  assert.match(
    preview,
    /name: torben-app-\$\{\{ steps\.version\.outputs\.version \}\}-windows-x64-nsis-unsigned-preview/u,
  );
  assert.match(
    preview,
    /name: torben-app-\$\{\{ steps\.version\.outputs\.version \}\}-windows-x64-msi-unsigned-preview/u,
  );
  assert.match(
    preview,
    /name: torben-\$\{\{ steps\.version\.outputs\.version \}\}-windows-x64-cli-unsigned-preview/u,
  );
  assert.match(preview, /Split accepted Windows x64 downloads/u);
  assert.match(preview, /node eng\/split-windows-preview\.mjs/u);
  assert.doesNotMatch(preview, /windows-x64-unsigned-preview\s*$/mu);
  assert.doesNotMatch(preview, /(?:macos|ubuntu|aarch64)/u);
  assert.doesNotMatch(preview, /(?:environment:|secrets\.|vars\.)/u);
  assert.doesNotMatch(
    preview,
    /\b(?:gh release|npm publish|cargo publish|pages: write|contents: write|id-token: write)\b/u,
  );
});

test("plugin registry artifacts require a protected manual main-branch release", () => {
  const release = readFileSync(join(workflowDirectory, "plugin-registry-release.yml"), "utf8");

  assert.match(release, /^\s{2}workflow_dispatch:/mu);
  assert.doesNotMatch(release, /^\s{2}(?:push|pull_request|schedule|release):/mu);
  assert.match(release, /^permissions:\n\s{2}contents: read$/mu);
  assert.match(release, /if: github\.ref == 'refs\/heads\/main'/u);
  assert.match(release, /environment: official-plugin-registry/u);
  assert.match(release, /TORBEN_PLUGIN_REGISTRY_ROOT_PRIVATE_KEY: \$\{\{ secrets\./u);
  assert.match(release, /TORBEN_PLUGIN_REGISTRY_PUBLISHER_PRIVATE_KEYS_JSON: \$\{\{ secrets\./u);
  assert.match(release, /EXPECTED_ROOT_PUBLIC_KEY: \$\{\{ vars\./u);
  assert.match(
    release,
    /cargo run --locked -p torben-plugin-host --example verify-plugin-registry/u,
  );
  assert.match(release, /--previous-registry/u);
  assert.match(release, /--inventory "\$ARTIFACT_ROOT\/SHA256SUMS"/u);
  assert.match(release, /actions\/upload-artifact@[0-9a-f]{40}/u);
  assert.doesNotMatch(release, /actions\/(?:deploy-pages|upload-pages-artifact|create-release)/u);
  assert.doesNotMatch(
    release,
    /\b(?:gh release|npm publish|cargo publish|pages: write|id-token: write)\b/u,
  );

  const secretStep = release.match(
    /- name: Materialize private keys and create immutable registry tree[\s\S]*?(?=\n\s{6}- name:)/u,
  )?.[0];
  assert.ok(secretStep, "private-key step is missing");
  assert.equal(
    [...release.matchAll(/\$\{\{ secrets\.[^}]+\}\}/gu)].length,
    2,
    "registry private-key secrets must be scoped to the trusted publisher step",
  );
  assert.doesNotMatch(secretStep, /\b(?:uses:|npm|pnpm|cargo|curl|wget)\b/u);
});
