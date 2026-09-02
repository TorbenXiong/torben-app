# Torben App project rules

The global Codex rules remain in force. These rules apply specifically to this repository.

## Product boundaries

- Torben App is a local-first application manager. The current delivery and support target is
  Windows x64.
- Windows ARM64, macOS, and Linux are deferred. Existing implementations and release tooling may
  remain for future milestones, but feature parity, packaging, and native acceptance on those
  targets are not current merge or release gates.
- New product work must complete and verify the Windows x64 path first. Do not expand deferred
  platform scope unless the product milestone is explicitly changed.
- Preserve shared contracts, Core boundaries, and existing platform abstractions so deferred
  targets can resume without a rewrite; do not delete cross-platform code merely because it is not
  in the current support scope.
- Do not introduce accounts, cloud synchronization, telemetry, project-level version pinning, or a resident background service without an explicit product decision.
- Do not import SoftPilot code or state formats. Security and transaction ideas may be independently reimplemented.
- Native plugins are trusted code. Never describe process isolation as a security sandbox.

## Architecture

- Keep platform and persistence implementations in `torben-core`; shared wire types belong in `torben-contracts`.
- GUI and CLI must call the same Core APIs and expose the same operation states and error codes.
- Plugins communicate only through versioned JSON-RPC over stdio. They must not access the Core database.
- A managed installation has one immutable source owner. Source changes require an explicit uninstall/reinstall migration plan.
- Installation mutations require the workspace lock and the order download, verify, stage, health-check, atomic commit, state commit.

## Quality

- Rust code must pass `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo test --workspace`.
- Frontend code must pass `pnpm run check` and `pnpm run test`.
- Network-dependent tests must use local fixtures by default. Live official metadata checks belong in an explicit scheduled CI job.
- Do not edit generated Tauri output, `target`, `node_modules`, `dist`, or coverage artifacts.
