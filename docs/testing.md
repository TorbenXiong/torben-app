# Test and acceptance evidence

Torben App separates deterministic fixture coverage, native package acceptance, and read-only live
catalog monitoring. A configured workflow is not evidence that a particular remote run passed; the
GitHub run and its artifacts remain the authoritative evidence for macOS, Linux, ARM64, signing,
notarization, and publication gates.

## Offline development gates

Ordinary tests must not contact public services. `.github/workflows/ci.yml` runs the same locked
workspace on Windows, macOS, and Ubuntu. Its required local gates are:

```powershell
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo clippy --locked -p torben-cli --all-targets --features test-fixtures -- -D warnings
cargo build --locked -p torben-shim -p torben-cli --features torben-cli/test-fixtures
cargo test --locked -p torben-cli --features test-fixtures --test node_lifecycle
cargo clippy --locked -p torben-desktop --all-targets --features test-fixtures -- -D warnings
cargo test --locked -p torben-desktop --features test-fixtures desktop_commands_complete_the_managed_node_lifecycle
pnpm run check
pnpm run test
```

`eng/workflow-policy.test.mjs` requires immutable Action revisions, frozen pnpm installation, and
`--locked` for every direct Cargo build, lint, test, or run command in workflows. The repository
does not require DCO sign-off trailers.

The Node.js first-milestone behaviors are covered by these fixture-backed tests:

| Acceptance behavior | Evidence |
| --- | --- |
| Official discovery, exact/LTS resolution, download, checksum/signature verification, safe extraction, health check, state commit, and permanent uninstall | `node::tests::local_fixture_completes_node_install_select_and_uninstall_transaction` in `crates/torben-core/src/node.rs` |
| Two installed Node.js versions and selected-line update without deleting the previous version | `node::tests::local_fixture_applies_managed_update_and_moves_the_selected_release_line` |
| Failed health check leaves the previous selection and installation intact and removes incomplete staging | `node::tests::managed_update_health_failure_keeps_the_previous_selection_and_rolls_back_staging` |
| A fresh terminal resolves `node`, `npm`, and `npx` from the single shim `PATH` entry, follows a changed persisted selection, and still works after Core and SQLite are reopened | `fresh_shim_processes_follow_persisted_node_selection` in `crates/torben-shim/tests/lifecycle.rs` |
| Selection and shim launch accept only a managed record at the standard plain app/version directory, while package-manager owners, altered paths, missing directories, and commands resolving outside that directory fail closed | `selection_rejects_a_managed_record_outside_the_standard_library`, `shim_resolution_rejects_a_package_manager_selection`, `startup_rejects_a_committed_package_manager_selection`, and `startup_rejects_a_committed_selection_with_missing_managed_files` in `crates/torben-core/src/lib.rs`; Unix additionally runs `shim_resolution_rejects_a_command_link_outside_the_managed_installation` |
| Selection shim replacement syncs an operation receipt before destination mutation; startup rolls back a receipt-bound partial commit, finishes cleanup after SQLite commit, handles receipt-only cleanup windows, and preserves missing or mismatched evidence | `selection_shim_transaction_cleans_its_receipt_after_commit`, `startup_rolls_back_a_receipt_bound_partial_selection_shim_commit`, `startup_finishes_receipt_bound_shim_cleanup_after_selection_commit`, `startup_removes_a_residual_shim_receipt_after_staging_cleanup`, `startup_closes_a_residual_shim_receipt_after_selection_rollback`, `startup_preserves_selection_shim_staging_without_a_receipt`, and `startup_preserves_selection_shim_staging_when_its_receipt_mismatches` in `crates/torben-core/src/lib.rs` |
| A selected version cannot be uninstalled; live uninstall writes an ownership receipt before state removal, restores a receipt-bound tombstone on SQLite failure, and removes its receipt after successful cleanup | `managed_uninstall_commits_only_after_receipt_bound_cleanup` and `managed_uninstall_restores_a_receipt_bound_stage_when_state_commit_fails`, plus Core lifecycle tests in `crates/torben-core/src/lib.rs` and `node.rs` |
| Uninstall startup recovery re-derives standard paths and restores or removes staging only with a matching bounded receipt; missing, altered, linked, non-directory, conflicting, or package-manager-owned paths are preserved and fail closed | `startup_restores_an_uncommitted_uninstall`, `startup_resumes_a_failed_committed_uninstall_cleanup`, `startup_preserves_an_unowned_uninstall_stage_without_a_receipt`, `startup_preserves_an_uninstall_stage_when_its_receipt_mismatches`, `startup_preserves_a_non_directory_uninstall_stage`, `startup_safely_rolls_back_an_uninstall_interrupted_before_staging`, `startup_rejects_a_package_owner_in_managed_uninstall_recovery`, and `startup_preserves_an_untracked_final_directory_after_uninstall_state_commit` in `crates/torben-core/src/lib.rs`; Unix additionally runs `startup_preserves_a_linked_uninstall_stage` |
| GUI and CLI mutations cannot overlap | `workspace_lock::tests::serializes_mutations_across_processes`; both entry points call the same workspace-locked Core APIs |
| Shell integration syncs transaction evidence before multi-target mutation, rolls back receipt-bound partial profile writes on startup, resumes receipt-only cleanup after commit, rejects altered profile ownership, and validates Windows raw `Path` ownership plans without touching the real registry | `file_transaction_commits_all_profiles_and_cleans_evidence`, `startup_rolls_back_a_receipt_bound_partial_profile_commit`, `startup_finishes_receipt_only_cleanup_after_profile_commit`, `startup_preserves_profiles_changed_outside_the_transaction`, `startup_rejects_a_transaction_bound_to_another_profile`, and `windows::tests` in `crates/torben-core/src/shell_integration.rs` |
| An in-flight desktop Core install blocks a real CLI mutation without blocking cross-process task inspection or cancellation; after rollback the waiting CLI resumes against the committed state | `desktop_core_and_real_cli_serialize_mutations_while_tasks_remain_cancellable` in `crates/torben-cli/tests/node_lifecycle.rs`; the test pauses a fixture download while the desktop Core owns the workspace lock, queries and cancels it through real `torben` subprocesses, and verifies `cancelling -> failed -> rolled_back` in durable sequence order |
| Interrupted install, uninstall, selection, plugin install, package-source operation, source migration, and library migration recover from durable journals; a successful library migration with pending old-source cleanup is also retried on startup | Startup-recovery tests in `crates/torben-core/src/lib.rs`, `operation.rs`, and `library_migration.rs`, including `terminal_cleanup_pending_migration_is_retried_during_startup_recovery` |
| Managed-install recovery deletes a final directory only with a matching bounded receipt, while a missing or mismatched receipt preserves unowned content | `startup_rolls_back_an_uncommitted_installation`, `startup_preserves_an_unowned_install_target_without_a_receipt`, and `startup_preserves_an_install_target_when_its_receipt_mismatches` in `crates/torben-core/src/lib.rs` |
| Plugin-install recovery and live rollback delete final plugin directories only with matching operation receipts, while missing or altered receipts fail closed | `startup_removes_a_receipt_bound_plugin_without_state`, `startup_preserves_an_unowned_plugin_target_without_a_receipt`, `startup_preserves_a_plugin_target_when_its_receipt_mismatches`, and `signed_network_registry_installs_through_the_shared_plugin_transaction` in `crates/torben-core/src/lib.rs` |
| Library recovery deletes only receipt-bound transaction paths, distinguishes a renamed Torben copy from an unowned target, and safely closes the pre-receipt crash window | `recovery_rejects_a_tampered_source_path_before_deleting_any_directory`, `recovery_removes_a_transaction_owned_target_before_rolling_back`, `recovery_preserves_an_unowned_nonempty_target_and_fails_closed`, and `recovery_safely_closes_a_journal_created_before_its_receipt` in `crates/torben-core/src/library_migration.rs` |
| Source-migration recovery re-derives standard managed paths, rejects journal path tampering before cleanup, requires an independent matching receipt before deleting a package-to-managed payload, and retains the package owner when removal never began | `managed_to_package_recovery_rejects_a_tampered_managed_source_path`, `package_to_managed_recovery_rejects_a_tampered_target_before_deleting_any_directory`, `package_to_managed_recovery_preserves_a_target_without_an_ownership_receipt`, `package_to_managed_recovery_preserves_the_target_when_the_receipt_mismatches`, `package_to_managed_recovery_rolls_back_before_package_removal`, and `interrupted_package_to_managed_migration_removes_payload_after_package_removal_begins` in `crates/torben-core/src/lib.rs` |
| CLI stable JSON envelope and desktop error/task states | Complete-field serialization test in `crates/torben-contracts/src/envelope.rs`, CLI unit tests, and `apps/desktop/src/test/App.test.tsx`; the desktop assertions also cover localized operation-state text, phase-specific progress-bar accessible names, and the warning shown when a committed library migration still needs old-source cleanup |
| Real CLI query and mutation paths keep one complete JSON envelope on stdout with matching process status | `query_command_emits_one_success_envelope_on_stdout`, `query_error_uses_the_complete_failure_envelope_and_exit_code`, and `mutation_error_keeps_json_stdout_separate_from_diagnostics` in `crates/torben-cli/tests/json_contract.rs` |
| CLI argument-validation failures requested with `--json` use the same complete error envelope and clap exit code `2`, while explicit help remains human-readable | `argument_error_uses_the_complete_failure_envelope_and_clap_exit_code` and `explicit_help_remains_human_readable_when_json_is_also_present` in `crates/torben-cli/tests/json_contract.rs` |
| A real `torben` subprocess discovers, installs, selects, resolves `node`/`npm`/`npx` from a fresh shim-only terminal entry, clears selection, permanently uninstalls, and reports every step through the stable JSON envelope | `real_cli_completes_node_install_select_shim_and_uninstall_lifecycle` in `crates/torben-cli/tests/node_lifecycle.rs`; its local HTTP and executable injection path is compiled only by the default-off `test-fixtures` feature and rejects non-loopback URLs |
| A second real `torben` process can inspect and cancel an in-flight Node.js download without waiting for the worker's workspace lock; the worker returns `operation_cancelled`, persists `cancelling -> failed -> rolled_back`, and removes the install, download, partial, and marker artifacts | `real_cli_cancels_an_in_flight_node_download_and_rolls_back` in `crates/torben-cli/tests/node_lifecycle.rs`; task-only CLI commands use `TorbenTaskClient`, which exposes no application mutation methods and does not run startup recovery |
| A real interrupted Node.js worker leaves a durable running journal and partial download, and the next full CLI startup safely rolls it back without creating an installation record | `real_cli_restart_recovers_an_interrupted_node_install` in `crates/torben-cli/tests/node_lifecycle.rs`; the test forcibly terminates the worker during a paused local HTTP response, verifies the pre-recovery residue, and then verifies `failed -> rolled_back` plus complete managed-artifact cleanup after restart |
| Desktop lifecycle commands discover, install, select, clear, and uninstall Node.js through the same handlers registered with Tauri, while the React API preserves the matching command names and camel-case arguments | `desktop_commands_complete_the_managed_node_lifecycle` in `apps/desktop/src-tauri/src/lib.rs` uses the default-off loopback fixture and real Core transaction; `apps/desktop/src/test/api.test.ts` verifies the frontend `invoke` mapping without mocking the command handlers |
| Built-in external discoveries start concurrently; a provider error or stopped task becomes a structured warning without dropping other providers' records, and the GUI renders that warning without hiding its current page | `external_discovery_failure_becomes_a_warning_without_dropping_other_records` and `stopped_external_discovery_task_becomes_a_structured_error` in `apps/desktop/src-tauri/src/lib.rs`, plus `keeps the desktop usable when one external discovery plugin fails` in `apps/desktop/src/test/App.test.tsx` |
| Available package-manager status probes begin concurrently without changing the stable adapter catalog order | `status_probes_start_concurrently_and_keep_catalog_order` in `crates/torben-core/src/source_adapters.rs` |
| Plugin progress is bound to the active operation and invalid or unexpected notifications fail closed | JSON-RPC fixture and validation tests in `crates/torben-plugin-host/src/lib.rs`; the event-aware Core callbacks are compiled by the workspace gates |
| Plugin methods require declared capabilities and malformed permission declarations fail before process use | `scoped_plugin_denies_methods_without_the_declared_capability` and `manifest_rejects_unsafe_and_duplicate_permission_declarations` in `crates/torben-plugin-host/src/lib.rs` |
| Registry publication produces Rust-compatible signatures and no partial output | `eng/publish-plugin-registry.test.mjs` regenerates the six-target Rust fixture byte for byte and covers exact hashes, revocation, key separation, strict release metadata, CLI use, and staging cleanup |
| Protected registry artifacts cannot bypass trust-root, sequence, inventory, or workflow boundaries | `eng/plugin-registry-release.test.mjs` independently re-verifies both signature levels, every target hash, exact tree membership, deterministic inventory, protected root equality, and a signed immediate predecessor; `eng/workflow-policy.test.mjs` requires a manual main-only Environment job, immutable Actions, read-only permissions, scoped secrets, locked Cargo verification, and no deployment capability |
| Registry paths cannot exploit platform-specific aliases | `registry_paths_reject_cross_platform_filesystem_aliases`, `registry_package_directories_are_unique_across_windows_case_folding`, and `manifest_targets_cannot_reuse_case_folded_executable_paths` in `crates/torben-plugin-host/src/lib.rs`, with matching publisher failure cases |
| Each native target appears only after all required packages and its matching CLI have been collected | `eng/collect-release-artifacts.test.mjs` covers all six targets, missing package formats, wrong executable architectures, staging rollback after a late name collision, existing/stale output refusal, and output containment |
| Updater publication requires exactly 12 safe and unambiguous platform assets, validates paths before invoking Rust signature verification, and never uploads an unchecked or partial flattened directory | `eng/updater-artifacts.test.mjs` covers missing and extra platforms, cross-platform traversal before process launch, duplicate GitHub asset URLs, same-name/different-byte packages, macOS architecture disambiguation, preflight-before-copy behavior, staging cleanup, exact flat asset membership, source-byte divergence, and publishing-checksum verification |
| Aggregate release metadata binds official `latest.json` semantics and exposes neither an unsigned development updater nor a one-file aggregate transaction | `eng/updater-artifacts.test.mjs` rejects a modified updater URL before aggregate creation; `eng/verify-release-set.test.mjs` covers deterministic paired metadata, development-manifest rejection, and stale staging refusal before partial output |
| Per-target release metadata publishes `release-metadata.json` and `SHA256SUMS` as one recoverable pair | `eng/release-metadata.test.mjs` proves deterministic byte-for-byte output and rejects stale staging before exposing either final metadata file |
| Desktop command search supports platform shortcuts, focus, arrow selection, filtering, and keyboard navigation | `searches pages and applications from the keyboard command palette` and platform-label coverage in `apps/desktop/src/test/App.test.tsx`; local browser QA checks the 700×600 layout |
| Keyboard users can bypass repeated navigation and system reduced-motion preferences suppress decorative movement | App shell skip-control focus assertions in `apps/desktop/src/test/App.test.tsx`; the reduced-motion rules are included in the Biome-checked desktop stylesheet |
| Desktop startup, task polling, and settings errors recover without stale, overlapping, or duplicate notices | `retries a failed initial snapshot without restarting the desktop`, `clears a recovered task polling error without affecting the main snapshot`, `does not overlap slow task polling requests`, and `shows one local alert when saving settings fails` in `apps/desktop/src/test/App.test.tsx` |
| Local diagnostic logs are bounded and exclude free-form operation messages | `diagnostic_log::tests::operation_log_excludes_free_form_message`, `diagnostic_log::tests::rotates_to_one_bounded_backup`, and `tests::doctor_reports_the_local_diagnostic_log` in `crates/torben-core/src` |
| Doctor does not report optional terminal integration or package-manager absence as a broken first-run state, but still rejects outdated Shell ownership and validates shims after a terminal selection exists | `shell_integration_actions_are_idempotent_and_update_doctor`, `doctor_distinguishes_optional_configuration_from_broken_configuration`, and `doctor_detects_outdated_command_shims` in `crates/torben-core/src/lib.rs` |
| Fresh SQLite state records every embedded migration, the previous bootstrap ledger gap is repaired, and an older Core rejects a future schema before creating or changing application tables | `fresh_database_records_every_embedded_migration`, `repairs_the_preexisting_schema_two_receipt_gap`, and `rejects_a_database_from_a_newer_schema_before_creating_application_tables` in `crates/torben-core/src/store.rs` |

The cross-process lock test starts a separate copy of the Rust test process, holds the real fs4
workspace file lock there, and proves that a contender cannot enter until the holder exits. Its
ignored test is only a subprocess entry point; the parent test runs it explicitly and is not an
unexecuted acceptance scenario.

## Native package acceptance

Development and official release workflows call two reusable acceptance workflows only after all
native packages are built:

- `.github/workflows/desktop-package-acceptance.yml` installs NSIS/MSI on Windows x64 and ARM64,
  copies the application from DMG on macOS Intel and Apple Silicon, validates the application and
  seven adjacent sidecars, re-verifies installed signatures for signed metadata, and requires a
  sustained isolated GUI launch.
- `.github/workflows/linux-package-acceptance.yml` installs or runs AppImage, deb, and rpm artifacts
  on x86_64 and ARM64 across Ubuntu, Debian, Fedora, and Rocky Linux containers, then performs the
  same content and launch checks.

The development aggregate and official publishing job depend on all fourteen jobs. Local fixture
coverage for the probes lives in `eng/desktop-package-smoke.test.mjs` and
`eng/linux-package-smoke.test.mjs`; those tests validate fail-closed behavior but do not substitute
for a native package workflow run.

Signed desktop fixtures additionally prove that verified metadata activates Authenticode checks
for one package plus all eight installed executables, and activates `codesign`, stapler, and
Gatekeeper checks for macOS. A signature command failure stops the launch probe. The Windows
fixture asserts the encoded PowerShell command and its nine-path JSON input; only native workflow
runs with protected signing credentials can provide positive trust evidence.

## Live and official-only evidence

The weekly `live-official-catalogs` job is the only ordinary workflow allowed to query public
provider catalogs. It builds the real CLI and all six bundled provider plugins, validates their
stable JSON results, and atomically uploads one complete snapshot artifact. It performs no install,
selection, package-manager, or system mutation.

An official release additionally requires the protected `official-release` environment, Windows
Authenticode credentials, Apple Developer ID/notarization credentials, and the matching Tauri
updater signing key. Missing remote run evidence or credentials means the corresponding release
criterion remains unverified; an unsigned development artifact must never be described as an
official release.
