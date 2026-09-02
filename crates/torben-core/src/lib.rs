#![allow(clippy::missing_errors_doc, clippy::needless_pass_by_value)]

mod bundled_shim;
mod catalog;
mod codex;
mod diagnostic_log;
mod git;
mod git_signature;
mod library_migration;
mod managed_updates;
mod node;
mod node_plugin;
mod node_signature;
mod operation;
mod paths;
mod plugin_registry;
mod process;
mod python;
#[cfg(any(unix, test))]
mod python_sigstore;
mod schema_ui;
mod shell_integration;
mod source_adapters;
mod store;
mod temurin;
mod temurin_signature;
#[cfg(feature = "test-fixtures")]
pub mod test_fixtures;
mod vscode;
mod workspace_lock;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

pub use codex::{CodexDistribution, CodexProvider, CodexSigstoreBundle};
pub use git::{GitDistribution, GitInstallKind, GitProvider};
pub use node::{NodeDistribution, NodeProvider};
pub use paths::TorbenPaths;
pub use python::{PythonDistribution, PythonInstallKind, PythonProvider, PythonSourceArchive};
use sha2::{Digest, Sha256};
pub use store::{PluginRecord, StateStore};
pub use temurin::{TemurinDistribution, TemurinProvider};
use torben_contracts::plugin::{SchemaActionParams, SchemaActionResult, SchemaPage};
use torben_contracts::{
    AppId, ApplicationDescriptor, ExactVersion, InstallRecord, InstallScope,
    ManagedLibraryMigrationResult, ManagedLibraryStatus, ManagedToPackageMigrationPlan,
    ManagedToPackageMigrationResult, ManagedUpdateCheck, ManagedUpdateResult, ManagedUpdateWarning,
    OperationEvent, OperationId, OperationKind, OperationState, PackageCoordinate,
    PackageInstallationRecord, PackageToManagedMigrationPlan, PackageToManagedMigrationRequest,
    PackageToManagedMigrationResult, PluginId, SelectionRecord, ShellIntegrationState,
    ShellIntegrationStatus, SourceAction, SourceAdapterAvailability, SourceAdapterKind,
    SourceAdapterStatus, SourceExecutionOutcome, SourceExecutionRequest, SourceExecutionResult,
    SourceMigrationPlan, SourceMigrationRequest, SourceMigrationResult, SourceOperationPlan,
    SourcePackageKind, SourcePackageState, SourcePackageVersion, TorbenError, TorbenResult,
    UserSettings, VersionDescriptor,
    plugin::{
        InstallPlan, PluginManifest, PluginOrigin, PluginRegistryStatus, PluginSummary,
        UninstallPlan,
    },
};
pub use vscode::{VsCodeDistribution, VsCodeProvider};

#[cfg(feature = "test-fixtures")]
#[derive(Debug, Clone)]
pub struct NodeFixtureConfiguration {
    pub base_url: String,
    pub checksum_signature: Vec<u8>,
    pub plugin_executable: PathBuf,
    pub shim_executable: PathBuf,
}

use crate::{
    bundled_shim::BundledShim,
    operation::{CancellationProbe, OperationJournal, SourceMigrationSubject},
    shell_integration::ShellIntegrationBackend,
    workspace_lock::WorkspaceLock,
};

const BUNDLED_NODE_PLUGIN_ID: &str = "app.torben.plugin.node";
const BUNDLED_NODE_PLUGIN_MANIFEST: &str =
    include_str!("../../../plugins/node/plugin.manifest.template.json");
const BUNDLED_TEMURIN_PLUGIN_ID: &str = "app.torben.plugin.temurin";
const BUNDLED_PYTHON_PLUGIN_ID: &str = "app.torben.plugin.python";
const BUNDLED_GIT_PLUGIN_ID: &str = "app.torben.plugin.git";
const BUNDLED_VSCODE_PLUGIN_ID: &str = "app.torben.plugin.vscode";
const BUNDLED_CODEX_PLUGIN_ID: &str = "app.torben.plugin.codex";
const MANAGED_INSTALL_RECEIPT_SCHEMA_VERSION: u32 = 1;
const MANAGED_INSTALL_RECEIPT_MAX_BYTES: u64 = 16 * 1024;
const MANAGED_UNINSTALL_RECEIPT_SCHEMA_VERSION: u32 = 1;
const MANAGED_UNINSTALL_RECEIPT_MAX_BYTES: u64 = 16 * 1024;
const SELECTION_SHIM_RECEIPT_SCHEMA_VERSION: u32 = 1;
const SELECTION_SHIM_RECEIPT_MAX_BYTES: u64 = 32 * 1024;
const PLUGIN_INSTALL_RECEIPT_SCHEMA_VERSION: u32 = 1;
const PLUGIN_INSTALL_RECEIPT_MAX_BYTES: u64 = 16 * 1024;
const SOURCE_MIGRATION_RECEIPT_SCHEMA_VERSION: u32 = 1;
const SOURCE_MIGRATION_RECEIPT_MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManagedInstallReceipt {
    schema_version: u32,
    operation_id: OperationId,
    app_id: AppId,
    version: ExactVersion,
    final_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManagedUninstallReceipt {
    schema_version: u32,
    operation_id: OperationId,
    app_id: AppId,
    version: ExactVersion,
    source_path: PathBuf,
    staged_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SelectionShimReceipt {
    schema_version: u32,
    operation_id: OperationId,
    app_id: AppId,
    version: ExactVersion,
    staging_path: PathBuf,
    backup_path: PathBuf,
    source_sha256: String,
    destinations: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PluginInstallReceipt {
    schema_version: u32,
    operation_id: OperationId,
    plugin_id: PluginId,
    version: ExactVersion,
    final_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PackageToManagedReceipt {
    schema_version: u32,
    operation_id: OperationId,
    app_id: AppId,
    app_version: ExactVersion,
    managed_target_path: PathBuf,
    approval_token: String,
}

#[derive(Clone)]
pub struct TorbenCore {
    paths: TorbenPaths,
    store: Arc<StateStore>,
    node: NodeProvider,
    temurin: TemurinProvider,
    python: PythonProvider,
    git: GitProvider,
    vscode: VsCodeProvider,
    codex: CodexProvider,
    node_plugin: node_plugin::BundledPlugin,
    temurin_plugin: node_plugin::BundledPlugin,
    python_plugin: node_plugin::BundledPlugin,
    git_plugin: node_plugin::BundledPlugin,
    vscode_plugin: node_plugin::BundledPlugin,
    codex_plugin: node_plugin::BundledPlugin,
    bundled_shim: BundledShim,
    shell_integration: Arc<dyn ShellIntegrationBackend>,
    official_registry_key: Option<String>,
    official_registry_url: Option<String>,
    #[cfg(test)]
    official_registry_fixture_mode: bool,
}

#[derive(Clone)]
pub struct TorbenTaskClient {
    paths: TorbenPaths,
    store: Arc<StateStore>,
}

struct PreparedPluginInstall {
    manifest_path: PathBuf,
    destination: PathBuf,
    record: PluginRecord,
    summary: PluginSummary,
}

struct PreparedSourceOperation {
    before: SourcePackageState,
    plan: SourceOperationPlan,
    owner: Option<PackageInstallationRecord>,
    executable_path: Option<PathBuf>,
}

struct SourceMigrationCommands {
    uninstall_current: SourceOperationPlan,
    install_target: SourceOperationPlan,
    cleanup_target: SourceOperationPlan,
    restore_current: SourceOperationPlan,
    warnings: Vec<String>,
}

struct PreparedMigrationTarget {
    state: SourcePackageState,
    version: SourcePackageVersion,
    executable: PathBuf,
}

const fn shell_integration_is_healthy(state: ShellIntegrationState) -> bool {
    !matches!(state, ShellIntegrationState::Outdated)
}

const fn source_adapter_is_healthy(availability: SourceAdapterAvailability) -> bool {
    matches!(
        availability,
        SourceAdapterAvailability::Available | SourceAdapterAvailability::Missing
    )
}

impl TorbenTaskClient {
    pub fn open_default() -> TorbenResult<Self> {
        Self::open(TorbenPaths::discover()?)
    }

    pub fn open(paths: TorbenPaths) -> TorbenResult<Self> {
        paths.ensure_base_layout()?;
        let store = Arc::new(StateStore::open(paths.state_database())?);
        Ok(Self { paths, store })
    }

    pub fn operation_events(&self) -> TorbenResult<Vec<OperationEvent>> {
        OperationJournal::list(&self.store)
    }

    pub fn cancel_operation(&self, operation_id: OperationId) -> TorbenResult<()> {
        OperationJournal::request_cancellation(&self.paths, &self.store, operation_id)
    }
}

impl TorbenCore {
    pub fn open_default() -> TorbenResult<Self> {
        Self::open(TorbenPaths::discover()?)
    }

    pub fn open(paths: TorbenPaths) -> TorbenResult<Self> {
        paths.ensure_base_layout()?;
        let store = Arc::new(StateStore::open(paths.state_database())?);
        if let Some(app_library) = store.managed_library_path()? {
            if !app_library.is_absolute() {
                return Err(TorbenError::new(
                    "managed_library_state_invalid",
                    "The saved managed application library path is not absolute.",
                )
                .with_detail("path", app_library.display().to_string()));
            }
            paths.set_app_library(app_library);
            let metadata = paths.app_library().symlink_metadata().map_err(|error| {
                TorbenError::new(
                    "managed_library_unavailable",
                    "The configured managed application library is unavailable.",
                )
                .with_detail("path", paths.app_library().display().to_string())
                .with_detail("reason", error.to_string())
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(TorbenError::new(
                    "managed_library_unavailable",
                    "The configured managed application library is not a regular directory.",
                )
                .with_detail("path", paths.app_library().display().to_string()));
            }
        } else {
            paths.ensure_layout()?;
        }
        let applications = catalog::applications()?;
        let sources = catalog::sources(&applications)?;
        store.sync_catalog(&applications, &sources)?;
        let shell_integration = shell_integration::platform_backend(&paths);
        {
            let _lock = WorkspaceLock::acquire(paths.workspace_lock())?;
            shell_integration.recover(&paths.shim_dir())?;
            recover_interrupted_operations(&paths, Arc::clone(&store))?;
        }
        let core = Self {
            paths,
            store,
            node: NodeProvider::official()?,
            temurin: TemurinProvider::official()?,
            python: PythonProvider::official()?,
            git: GitProvider::official()?,
            vscode: VsCodeProvider::official()?,
            codex: CodexProvider::official()?,
            node_plugin: node_plugin::BundledPlugin::node()?,
            temurin_plugin: node_plugin::BundledPlugin::temurin()?,
            python_plugin: node_plugin::BundledPlugin::python()?,
            git_plugin: node_plugin::BundledPlugin::git()?,
            vscode_plugin: node_plugin::BundledPlugin::vscode()?,
            codex_plugin: node_plugin::BundledPlugin::codex()?,
            bundled_shim: BundledShim::discover()?,
            shell_integration,
            official_registry_key: option_env!("TORBEN_OFFICIAL_PLUGIN_REGISTRY_KEY")
                .map(str::to_owned),
            official_registry_url: option_env!("TORBEN_OFFICIAL_PLUGIN_REGISTRY_URL")
                .map(str::to_owned),
            #[cfg(test)]
            official_registry_fixture_mode: false,
        };
        let _ = diagnostic_log::record_core_started(core.paths.log_dir());
        Ok(core)
    }

    #[cfg(feature = "test-fixtures")]
    pub fn open_node_fixture(
        paths: TorbenPaths,
        configuration: NodeFixtureConfiguration,
    ) -> TorbenResult<Self> {
        let base_url = url::Url::parse(&configuration.base_url)
            .map_err(|error| invalid_node_fixture_configuration("baseUrl", error.to_string()))?;
        let loopback = match base_url.host() {
            Some(url::Host::Ipv4(address)) => address == std::net::Ipv4Addr::LOCALHOST,
            Some(url::Host::Ipv6(address)) => address == std::net::Ipv6Addr::LOCALHOST,
            _ => false,
        };
        if base_url.scheme() != "http"
            || !loopback
            || base_url.cannot_be_a_base()
            || !base_url.path().ends_with('/')
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(invalid_node_fixture_configuration(
                "baseUrl",
                "The fixture URL must be an HTTP loopback base URL ending in '/'.",
            ));
        }
        if configuration.checksum_signature.is_empty() {
            return Err(invalid_node_fixture_configuration(
                "checksumSignatureHex",
                "The fixture checksum signature cannot be empty.",
            ));
        }
        for (field, executable) in [
            ("pluginExecutable", &configuration.plugin_executable),
            ("shimExecutable", &configuration.shim_executable),
        ] {
            if !executable.is_absolute() || !executable.is_file() {
                return Err(invalid_node_fixture_configuration(
                    field,
                    "The fixture executable must be an existing absolute file path.",
                )
                .with_detail("path", executable.display().to_string()));
            }
        }

        let mut core = Self::open(paths)?;
        core.node =
            NodeProvider::with_fixture_base_url(base_url, configuration.checksum_signature)?;
        core.node_plugin =
            node_plugin::BundledPlugin::node_from_executable(configuration.plugin_executable);
        core.bundled_shim = BundledShim::from_executable(configuration.shim_executable);
        Ok(core)
    }

    pub fn paths(&self) -> &TorbenPaths {
        &self.paths
    }

    pub fn applications(&self) -> TorbenResult<Vec<ApplicationDescriptor>> {
        self.store.list_applications()
    }

    pub fn search_applications(&self, query: &str) -> TorbenResult<Vec<ApplicationDescriptor>> {
        let query = query.trim().to_ascii_lowercase();
        Ok(self
            .applications()?
            .into_iter()
            .filter(|application| {
                query.is_empty()
                    || application.id.as_str().contains(&query)
                    || application
                        .display_name
                        .to_ascii_lowercase()
                        .contains(&query)
                    || application.summary.to_ascii_lowercase().contains(&query)
            })
            .collect())
    }

    pub fn application(&self, app_id: &AppId) -> TorbenResult<ApplicationDescriptor> {
        self.applications()?
            .into_iter()
            .find(|application| &application.id == app_id)
            .ok_or_else(|| {
                TorbenError::new(
                    "app_not_found",
                    "The requested application is not registered.",
                )
                .with_detail("appId", app_id.to_string())
            })
    }

    pub async fn versions(&self, app_id: &AppId) -> TorbenResult<Vec<VersionDescriptor>> {
        Self::ensure_supported_app(app_id)?;
        let mut plugin = self.bundled_plugin(app_id)?.connect().await?;
        let versions = plugin.versions(app_id).await?;
        plugin.shutdown().await?;
        Ok(versions)
    }

    pub fn installed(&self) -> TorbenResult<Vec<InstallRecord>> {
        self.store.list_installations()
    }

    pub async fn external_installations(&self, app_id: &AppId) -> TorbenResult<Vec<InstallRecord>> {
        Self::ensure_supported_app(app_id)?;
        let mut plugin = self.bundled_plugin(app_id)?.connect().await?;
        let installations = plugin
            .external_installations(app_id, self.paths.data_dir())
            .await?;
        plugin.shutdown().await?;
        Ok(installations)
    }

    pub fn selected_version(&self, app_id: &AppId) -> TorbenResult<Option<ExactVersion>> {
        self.store.selected_version(app_id)
    }

    pub fn selections(&self) -> TorbenResult<Vec<SelectionRecord>> {
        self.store.list_selections()
    }

    pub async fn install(
        &self,
        app_id: &AppId,
        requested_version: &str,
    ) -> TorbenResult<InstallRecord> {
        Self::ensure_supported_app(app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let mut journal = OperationJournal::start(
            &self.paths,
            Arc::clone(&self.store),
            torben_contracts::OperationKind::Install,
            app_id,
            None,
        )?;
        journal.record(
            OperationState::Running,
            "resolve",
            format!("Resolving {app_id} version {requested_version}"),
            Some(0.02),
        )?;
        let mut plugin = match self.bundled_plugin(app_id)?.connect().await {
            Ok(plugin) => plugin,
            Err(error) => {
                journal.fail_and_rollback(&error)?;
                return Err(error);
            }
        };
        let resolved = match plugin.resolve_version(app_id, requested_version).await {
            Ok(version) => version,
            Err(error) => {
                let _ = plugin.shutdown().await;
                journal.fail_and_rollback(&error)?;
                return Err(error);
            }
        };
        journal.set_version(&resolved)?;
        if let Err(error) = journal.cancellation_probe().check() {
            let _ = plugin.shutdown().await;
            journal.acknowledge_cancellation()?;
            journal.fail_and_rollback(&error)?;
            return Err(error);
        }
        if let Some(existing) = self.store.get_installation(app_id, &resolved)? {
            plugin.shutdown().await?;
            journal.succeed(format!("{app_id} {resolved} is already installed"))?;
            return Ok(existing);
        }
        let operation_id = journal.operation_id();
        let plan = match plugin
            .install_plan_with_events(operation_id, app_id, &resolved, |event| {
                journal.record(
                    OperationState::Running,
                    format!("plugin.{}", event.phase),
                    event.message,
                    event.progress.map(|progress| 0.02 + progress * 0.07),
                )
            })
            .await
        {
            Ok(plan) => plan,
            Err(error) => {
                let _ = plugin.shutdown().await;
                journal.fail_and_rollback(&error)?;
                return Err(error);
            }
        };
        if let Err(error) = plugin.shutdown().await {
            journal.fail_and_rollback(&error)?;
            return Err(error);
        }
        let result = self
            .install_managed_payload(app_id, &resolved, &plan, &mut journal)
            .await;

        self.finish_install_transaction(app_id, &mut journal, result)
    }

    async fn install_managed_payload(
        &self,
        app_id: &AppId,
        version: &ExactVersion,
        plan: &InstallPlan,
        journal: &mut OperationJournal,
    ) -> TorbenResult<InstallRecord> {
        match app_id.as_str() {
            "node" => {
                self.node
                    .install(&self.paths, app_id, version, plan, journal)
                    .await
            }
            "temurin" => {
                self.temurin
                    .install(&self.paths, app_id, version, plan, journal)
                    .await
            }
            "python" => {
                self.python
                    .install(&self.paths, app_id, version, plan, journal)
                    .await
            }
            "git" => {
                self.git
                    .install(&self.paths, app_id, version, plan, journal)
                    .await
            }
            "vscode" => {
                self.vscode
                    .install(&self.paths, app_id, version, plan, journal)
                    .await
            }
            "codex" => {
                self.codex
                    .install(&self.paths, app_id, version, plan, journal)
                    .await
            }
            _ => unreachable!("supported applications are checked before install"),
        }
    }

    fn finish_install_transaction(
        &self,
        app_id: &AppId,
        journal: &mut OperationJournal,
        result: TorbenResult<InstallRecord>,
    ) -> TorbenResult<InstallRecord> {
        match result {
            Ok(record) => {
                if let Err(error) = write_managed_install_receipt(&self.paths, journal, &record) {
                    journal.fail(&error)?;
                    return Err(error);
                }
                if let Err(error) = journal.cancellation_probe().check() {
                    journal.acknowledge_cancellation()?;
                    match cleanup_managed_install_artifacts(
                        &self.paths,
                        app_id,
                        &record.version,
                        journal,
                    ) {
                        Ok(()) => {
                            journal.fail_and_rollback(&error)?;
                            return Err(error);
                        }
                        Err(cleanup_error) => {
                            let pending = install_rollback_pending(&error, &cleanup_error);
                            journal.fail(&pending)?;
                            return Err(pending);
                        }
                    }
                }
                if let Err(error) = self.store.add_installation(&record) {
                    match cleanup_managed_install_artifacts(
                        &self.paths,
                        app_id,
                        &record.version,
                        journal,
                    ) {
                        Ok(()) => {
                            journal.fail_and_rollback(&error)?;
                            return Err(error);
                        }
                        Err(cleanup_error) => {
                            let pending = TorbenError::new(
                                "install_rollback_pending",
                                "Installation state failed and filesystem rollback is incomplete.",
                            )
                            .with_detail("stateErrorCode", &error.code)
                            .with_detail("stateError", &error.message)
                            .with_detail("cleanupErrorCode", &cleanup_error.code)
                            .with_detail("cleanupError", &cleanup_error.message)
                            .with_remediation(
                                "Restart Torben App to resume recovery before retrying the installation.",
                            );
                            journal.fail(&pending)?;
                            return Err(pending);
                        }
                    }
                }
                remove_managed_install_receipt_if_present(&self.paths, journal.operation_id())?;
                journal.succeed("Installation committed")?;
                Ok(record)
            }
            Err(error) => {
                acknowledge_cancellation_error(journal, &error)?;
                let cleanup = match journal.version().cloned() {
                    None => {
                        let staging = self.paths.staging_dir().join(format!(
                            "install-{}-{}",
                            app_id,
                            journal.operation_id()
                        ));
                        remove_managed_directory_if_exists(&staging, journal)
                    }
                    Some(version) => {
                        cleanup_managed_install_artifacts(&self.paths, app_id, &version, journal)
                    }
                };
                match cleanup {
                    Ok(()) => {
                        journal.fail_and_rollback(&error)?;
                        Err(error)
                    }
                    Err(cleanup_error) => {
                        let pending = install_rollback_pending(&error, &cleanup_error);
                        journal.fail(&pending)?;
                        Err(pending)
                    }
                }
            }
        }
    }

    pub async fn managed_update_check(
        &self,
        app_filter: Option<&AppId>,
    ) -> TorbenResult<ManagedUpdateCheck> {
        if let Some(app_id) = app_filter {
            Self::ensure_supported_app(app_id)?;
        }
        let installations = self.store.list_installations()?;
        let selections = self.store.list_selections()?;
        let settings = self.store.user_settings()?;
        let automatic = settings
            .updates
            .automatically_update_apps
            .into_iter()
            .collect::<BTreeSet<_>>();
        let app_ids = installations
            .iter()
            .filter(|record| record.scope == InstallScope::Managed)
            .map(|record| record.app_id.clone())
            .filter(|app_id| app_filter.is_none_or(|filter| filter == app_id))
            .collect::<BTreeSet<_>>();
        let mut candidates = Vec::new();
        let mut warnings = Vec::new();
        for app_id in &app_ids {
            match self.versions(app_id).await {
                Ok(versions) => candidates.extend(managed_updates::candidates_for_app(
                    app_id,
                    &installations,
                    &selections,
                    &versions,
                    automatic.contains(app_id),
                )),
                Err(error) => warnings.push(ManagedUpdateWarning {
                    app_id: app_id.clone(),
                    code: error.code,
                    message: error.message,
                    details: error.details,
                    remediation: error.remediation,
                }),
            }
        }
        candidates.sort_by(|left, right| {
            left.app_id
                .cmp(&right.app_id)
                .then_with(|| left.channel.cmp(&right.channel))
        });
        Ok(ManagedUpdateCheck {
            checked_apps: app_ids.len(),
            candidates,
            warnings,
        })
    }

    pub fn set_managed_auto_update(&self, app_id: &AppId, enabled: bool) -> TorbenResult<()> {
        Self::ensure_supported_app(app_id)?;
        let mut settings = self.store.user_settings()?;
        let mut app_ids = settings
            .updates
            .automatically_update_apps
            .into_iter()
            .collect::<BTreeSet<_>>();
        if enabled {
            app_ids.insert(app_id.clone());
        } else {
            app_ids.remove(app_id);
        }
        settings.updates.automatically_update_apps = app_ids.into_iter().collect();
        self.store.save_user_settings(&settings)
    }

    pub async fn apply_managed_update(
        &self,
        app_id: &AppId,
        installed_version: &ExactVersion,
        available_version: &ExactVersion,
    ) -> TorbenResult<ManagedUpdateResult> {
        let check = self.managed_update_check(Some(app_id)).await?;
        let candidate = check
            .candidates
            .into_iter()
            .find(|candidate| {
                candidate.installed_version == *installed_version
                    && candidate.available_version == *available_version
            })
            .ok_or_else(|| {
                TorbenError::new(
                    "managed_update_stale",
                    "The requested managed update is no longer the current update candidate.",
                )
                .with_detail("appId", app_id.to_string())
                .with_detail("installedVersion", installed_version.to_string())
                .with_detail("availableVersion", available_version.to_string())
                .with_remediation("Check for managed application updates again before retrying.")
            })?;
        let expected_selection = candidate.selected_version.clone();
        let installation = self
            .install(app_id, &candidate.available_version.to_string())
            .await?;
        let selection_updated = if let Some(previous) = expected_selection {
            self.select_if_current(app_id, &previous, &candidate.available_version)
                .await?
        } else {
            false
        };
        Ok(ManagedUpdateResult {
            candidate,
            installation,
            selection_updated,
        })
    }

    pub async fn select(&self, app_id: &AppId, version: &ExactVersion) -> TorbenResult<()> {
        Self::ensure_supported_app(app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        self.select_locked(app_id, version).await
    }

    async fn select_if_current(
        &self,
        app_id: &AppId,
        expected: &ExactVersion,
        version: &ExactVersion,
    ) -> TorbenResult<bool> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        if self.store.selected_version(app_id)?.as_ref() != Some(expected) {
            return Ok(false);
        }
        self.select_locked(app_id, version).await?;
        Ok(true)
    }

    async fn select_locked(&self, app_id: &AppId, version: &ExactVersion) -> TorbenResult<()> {
        let record = self
            .store
            .get_installation(app_id, version)?
            .ok_or_else(|| {
                TorbenError::new(
                    "version_not_installed",
                    "Install the version before selecting it.",
                )
                .with_detail("appId", app_id.to_string())
                .with_detail("version", version.to_string())
            })?;
        if record.scope != InstallScope::Managed {
            return Err(TorbenError::new(
                "installation_not_selectable",
                "Only Torben-managed archive installations can be selected for terminal use.",
            )
            .with_detail("appId", app_id.to_string())
            .with_detail("version", version.to_string())
            .with_remediation(
                "Install the version from the Torben application catalog before selecting it.",
            ));
        }
        validate_selected_installation(&self.paths, &record)?;
        let mut journal = OperationJournal::start(
            &self.paths,
            Arc::clone(&self.store),
            torben_contracts::OperationKind::Select,
            app_id,
            Some(version),
        )?;
        journal.record(
            torben_contracts::OperationState::Running,
            "health_check",
            format!("Checking {app_id} {version} before selection"),
            Some(0.4),
        )?;
        if let Err(error) = self.plugin_health_check(&record).await {
            journal.fail_and_rollback(&error)?;
            return Err(error);
        }
        journal.record(
            torben_contracts::OperationState::Running,
            "shim",
            "Installing managed terminal command shims",
            Some(0.7),
        )?;
        let Some(shim_binary) = self.bundled_shim.executable() else {
            let error = self.bundled_shim.missing_error();
            journal.fail_and_rollback(&error)?;
            return Err(error);
        };
        if let Err(error) = install_selection_shims_locked(&self.paths, shim_binary, &journal) {
            if error.code == "selection_shim_rollback_pending" {
                journal.fail(&error)?;
            } else {
                journal.fail_and_rollback(&error)?;
            }
            return Err(error);
        }
        journal.record(
            torben_contracts::OperationState::Running,
            "commit",
            "Committing terminal selection",
            Some(0.8),
        )?;
        if let Err(error) = self.store.set_selection(app_id, version) {
            journal.fail_and_rollback(&error)?;
            return Err(error);
        }
        journal.succeed(format!("Selected {app_id} {version}"))
    }

    pub async fn uninstall(&self, app_id: &AppId, version: &ExactVersion) -> TorbenResult<()> {
        Self::ensure_supported_app(app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let record = self
            .store
            .get_installation(app_id, version)?
            .ok_or_else(|| {
                TorbenError::new("version_not_installed", "The version is not installed.")
            })?;
        if record.scope == InstallScope::PackageManager {
            return Err(TorbenError::new(
                "package_manager_uninstall_required",
                "Package-manager installations must be removed through their source adapter.",
            )
            .with_detail("appId", app_id.to_string())
            .with_detail("version", version.to_string())
            .with_remediation(
                "Use `torben source execute uninstall` or the package source operation in Diagnostics.",
            ));
        }
        if self.store.selected_version(app_id)?.as_ref() == Some(version) {
            return Err(TorbenError::new(
                "version_is_selected",
                "The selected version cannot be uninstalled.",
            )
            .with_remediation("Select another version or clear the selection first."));
        }
        let mut journal = OperationJournal::start(
            &self.paths,
            Arc::clone(&self.store),
            torben_contracts::OperationKind::Uninstall,
            app_id,
            Some(version),
        )?;
        let expected_source = self
            .paths
            .app_version_dir(app_id.as_str(), &version.to_string());
        let source = PathBuf::from(&record.install_path);
        if record.scope != InstallScope::Managed || source != expected_source {
            let error = TorbenError::new(
                "managed_install_path_invalid",
                "The installation record is not a standard managed installation.",
            )
            .with_detail("expectedPath", expected_source.display().to_string())
            .with_detail("actualPath", source.display().to_string());
            journal.fail_and_rollback(&error)?;
            return Err(error);
        }
        journal.record(
            torben_contracts::OperationState::Running,
            "plugin_plan",
            "Requesting the application uninstall plan",
            Some(0.05),
        )?;
        let plan = match self
            .plugin_uninstall_plan_with_events(&record, &mut journal)
            .await
        {
            Ok(plan) => plan,
            Err(error) => {
                journal.fail_and_rollback(&error)?;
                return Err(error);
            }
        };
        if let Err(error) = validate_uninstall_plan(&record, &plan) {
            journal.fail_and_rollback(&error)?;
            return Err(error);
        }
        let staged = self.paths.staging_dir().join(format!(
            "uninstall-{}-{}",
            app_id,
            journal.operation_id()
        ));
        execute_uninstall_transaction(
            &self.paths,
            &self.store,
            &record,
            &source,
            &staged,
            &mut journal,
        )
    }

    pub fn clear_selection(&self, app_id: &AppId) -> TorbenResult<()> {
        Self::ensure_supported_app(app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let Some(previous) = self.store.selected_version(app_id)? else {
            return Ok(());
        };
        let mut journal = OperationJournal::start(
            &self.paths,
            Arc::clone(&self.store),
            torben_contracts::OperationKind::Select,
            app_id,
            None,
        )?;
        journal.record(
            torben_contracts::OperationState::Running,
            "commit",
            format!("Clearing selected {app_id} {previous}"),
            Some(0.8),
        )?;
        if let Err(error) = self.store.clear_selection(app_id) {
            journal.fail_and_rollback(&error)?;
            return Err(error);
        }
        journal.succeed(format!("Cleared selected {app_id} {previous}"))
    }

    pub fn operation_events(&self) -> TorbenResult<Vec<OperationEvent>> {
        OperationJournal::list(&self.store)
    }

    pub fn cancel_operation(
        &self,
        operation_id: torben_contracts::OperationId,
    ) -> TorbenResult<()> {
        OperationJournal::request_cancellation(&self.paths, &self.store, operation_id)
    }

    pub fn user_settings(&self) -> TorbenResult<UserSettings> {
        self.store.user_settings()
    }

    pub fn update_user_settings(&self, settings: &UserSettings) -> TorbenResult<()> {
        self.store.save_user_settings(settings)
    }

    pub fn managed_library_status(&self) -> TorbenResult<ManagedLibraryStatus> {
        library_migration::status(&self.paths)
    }

    pub async fn source_adapter_statuses(&self) -> TorbenResult<Vec<SourceAdapterStatus>> {
        source_adapters::SourceAdapterService::discover()
            .statuses()
            .await
    }

    pub async fn inspect_source_package(
        &self,
        adapter: SourceAdapterKind,
        coordinate: PackageCoordinate,
        package_kind: SourcePackageKind,
    ) -> TorbenResult<SourcePackageState> {
        source_adapters::SourceAdapterService::discover()
            .inspect(adapter, coordinate, package_kind)
            .await
    }

    pub async fn plan_source_operation(
        &self,
        action: SourceAction,
        adapter: SourceAdapterKind,
        coordinate: PackageCoordinate,
        package_kind: SourcePackageKind,
        package_version: Option<SourcePackageVersion>,
    ) -> TorbenResult<SourceOperationPlan> {
        source_adapters::SourceAdapterService::discover()
            .reviewed_plan(action, adapter, coordinate, package_kind, package_version)
            .await
    }

    pub async fn execute_source_operation(
        &self,
        request: SourceExecutionRequest,
    ) -> TorbenResult<SourceExecutionResult> {
        let service = source_adapters::SourceAdapterService::discover();
        self.execute_source_operation_with_service(request, &service)
            .await
    }

    pub async fn plan_source_migration(
        &self,
        request: SourceMigrationRequest,
    ) -> TorbenResult<SourceMigrationPlan> {
        let service = source_adapters::SourceAdapterService::discover();
        self.plan_source_migration_with_service(request, &service)
            .await
    }

    async fn plan_source_migration_with_service(
        &self,
        request: SourceMigrationRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<SourceMigrationPlan> {
        self.application(&request.app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        self.prepare_source_migration_plan(&request, service).await
    }

    pub async fn execute_source_migration(
        &self,
        request: SourceMigrationRequest,
    ) -> TorbenResult<SourceMigrationResult> {
        let service = source_adapters::SourceAdapterService::discover();
        self.execute_source_migration_with_service(request, &service)
            .await
    }

    async fn execute_source_migration_with_service(
        &self,
        request: SourceMigrationRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<SourceMigrationResult> {
        if !request.accept_system_changes {
            return Err(TorbenError::new(
                "source_migration_confirmation_required",
                "Source migration requires explicit acceptance of system changes.",
            )
            .with_remediation(
                "Review the complete migration plan, then repeat with --accept-system-changes.",
            ));
        }
        let approved = request.approved_plan_token.as_deref().ok_or_else(|| {
            TorbenError::new(
                "source_migration_plan_approval_required",
                "Source migration requires the exact token from a reviewed plan.",
            )
            .with_remediation("Generate and review a new source migration plan.")
        })?;
        self.application(&request.app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let plan = self
            .prepare_source_migration_plan(&request, service)
            .await?;
        if approved != plan.approval_token {
            return Err(TorbenError::new(
                "source_migration_plan_approval_required",
                "The source migration plan changed after it was reviewed.",
            )
            .with_detail("approvalToken", &plan.approval_token)
            .with_remediation("Review the new migration plan before executing it."));
        }
        self.execute_prepared_source_migration(plan, service).await
    }

    pub async fn plan_managed_to_package_migration(
        &self,
        request: SourceMigrationRequest,
    ) -> TorbenResult<ManagedToPackageMigrationPlan> {
        let service = source_adapters::SourceAdapterService::discover();
        self.plan_managed_to_package_migration_with_service(request, &service)
            .await
    }

    async fn plan_managed_to_package_migration_with_service(
        &self,
        request: SourceMigrationRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<ManagedToPackageMigrationPlan> {
        self.application(&request.app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        self.prepare_managed_to_package_plan(&request, service)
            .await
    }

    pub async fn execute_managed_to_package_migration(
        &self,
        request: SourceMigrationRequest,
    ) -> TorbenResult<ManagedToPackageMigrationResult> {
        let service = source_adapters::SourceAdapterService::discover();
        self.execute_managed_to_package_migration_with_service(request, &service)
            .await
    }

    async fn execute_managed_to_package_migration_with_service(
        &self,
        request: SourceMigrationRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<ManagedToPackageMigrationResult> {
        validate_managed_to_package_approval(&request)?;
        self.application(&request.app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let plan = self
            .prepare_managed_to_package_plan(&request, service)
            .await?;
        if request.approved_plan_token.as_deref() != Some(&plan.approval_token) {
            return Err(TorbenError::new(
                "source_migration_plan_approval_required",
                "The managed-to-package migration plan changed after review.",
            )
            .with_detail("approvalToken", &plan.approval_token)
            .with_remediation("Review the new migration plan before executing it."));
        }
        self.execute_prepared_managed_to_package(plan, service)
            .await
    }

    pub async fn plan_package_to_managed_migration(
        &self,
        request: PackageToManagedMigrationRequest,
    ) -> TorbenResult<PackageToManagedMigrationPlan> {
        let service = source_adapters::SourceAdapterService::discover();
        self.plan_package_to_managed_migration_with_service(request, &service)
            .await
    }

    async fn plan_package_to_managed_migration_with_service(
        &self,
        request: PackageToManagedMigrationRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<PackageToManagedMigrationPlan> {
        self.application(&request.app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        self.prepare_package_to_managed_plan(&request, service)
            .await
    }

    pub async fn execute_package_to_managed_migration(
        &self,
        request: PackageToManagedMigrationRequest,
    ) -> TorbenResult<PackageToManagedMigrationResult> {
        let service = source_adapters::SourceAdapterService::discover();
        self.execute_package_to_managed_migration_with_service(request, &service)
            .await
    }

    async fn execute_package_to_managed_migration_with_service(
        &self,
        request: PackageToManagedMigrationRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<PackageToManagedMigrationResult> {
        validate_package_to_managed_approval(&request)?;
        self.application(&request.app_id)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let plan = self
            .prepare_package_to_managed_plan(&request, service)
            .await?;
        if request.approved_plan_token.as_deref() != Some(&plan.approval_token) {
            return Err(TorbenError::new(
                "source_migration_plan_approval_required",
                "The package-to-managed migration plan changed after review.",
            )
            .with_detail("approvalToken", &plan.approval_token)
            .with_remediation("Review the new migration plan before executing it."));
        }
        self.execute_prepared_package_to_managed(plan, service)
            .await
    }

    async fn prepare_package_to_managed_plan(
        &self,
        request: &PackageToManagedMigrationRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<PackageToManagedMigrationPlan> {
        let current_owner = self
            .store
            .package_installation(&request.app_id, &request.app_version)?
            .ok_or_else(|| {
                TorbenError::new(
                    "source_migration_owner_required",
                    "Only a Torben-owned package-manager installation can migrate to managed storage.",
                )
            })?;
        let current_state = service
            .inspect(
                current_owner.adapter,
                current_owner.coordinate.clone(),
                current_owner.package_kind,
            )
            .await?;
        if !current_state.installed
            || current_state.installed_version.as_ref() != Some(&current_owner.package_version)
        {
            return Err(TorbenError::new(
                "source_package_state_drifted",
                "The current package state no longer matches Torben's source owner.",
            ));
        }
        let managed_target = self
            .paths
            .app_version_dir(request.app_id.as_str(), &request.app_version.to_string());
        if managed_target.exists() {
            return Err(TorbenError::new(
                "managed_install_path_conflict",
                "The official managed target path already exists.",
            )
            .with_detail("path", managed_target.display().to_string()));
        }
        let uninstall_current = service
            .reviewed_plan(
                SourceAction::Uninstall,
                current_owner.adapter,
                current_owner.coordinate.clone(),
                current_owner.package_kind,
                Some(current_owner.package_version.clone()),
            )
            .await?;
        let restore_current = service
            .reviewed_plan(
                SourceAction::Install,
                current_owner.adapter,
                current_owner.coordinate.clone(),
                current_owner.package_kind,
                Some(current_owner.package_version.clone()),
            )
            .await?;
        let install_managed = self
            .official_install_plan(&request.app_id, &request.app_version)
            .await?;
        let mut warnings = vec![
            "The official managed archive is installed and verified before the current package is removed.".to_owned(),
            "Application configuration is not read, copied, or migrated.".to_owned(),
            "Package-manager removal and restoration may affect shared dependencies and are not filesystem-atomic.".to_owned(),
        ];
        if !restore_current.exact_version_guaranteed {
            warnings.push(
                "The current package manager cannot guarantee restoration of an arbitrary historical version."
                    .to_owned(),
            );
        }
        let mut plan = PackageToManagedMigrationPlan {
            app_id: request.app_id.clone(),
            app_version: request.app_version.clone(),
            current_owner,
            current_state,
            uninstall_current,
            restore_current,
            install_managed,
            managed_target_path: managed_target.display().to_string(),
            approval_token: String::new(),
            warnings,
        };
        plan.approval_token = package_to_managed_token(&plan)?;
        Ok(plan)
    }

    async fn official_install_plan(
        &self,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<InstallPlan> {
        let mut plugin = self.bundled_plugin(app_id)?.connect().await?;
        let result = async {
            let resolved = plugin.resolve_version(app_id, &version.to_string()).await?;
            if &resolved != version {
                return Err(TorbenError::new(
                    "source_migration_version_changed",
                    "The official plugin resolved a different exact version.",
                )
                .with_detail("requestedVersion", version.to_string())
                .with_detail("resolvedVersion", resolved.to_string()));
            }
            plugin
                .install_plan(OperationId::new(), app_id, version)
                .await
        }
        .await;
        let shutdown = plugin.shutdown().await;
        match (result, shutdown) {
            (Ok(plan), Ok(())) => Ok(plan),
            (Err(error), _) | (Ok(_), Err(error)) => Err(error),
        }
    }

    async fn prepare_managed_to_package_plan(
        &self,
        request: &SourceMigrationRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<ManagedToPackageMigrationPlan> {
        self.validate_package_migration_target(request)?;
        let current = self.managed_migration_installation(request)?;
        let target_state = service
            .inspect(
                request.target_adapter,
                request.target_coordinate.clone(),
                request.target_package_kind,
            )
            .await?;
        if target_state.installed {
            return Err(TorbenError::new(
                "source_migration_target_present",
                "The target package is already installed and cannot be adopted during migration.",
            ));
        }
        let uninstall_current = self
            .plugin_uninstall_plan(OperationId::new(), &current)
            .await?;
        validate_uninstall_plan(&current, &uninstall_current)?;
        let install_target = service
            .reviewed_plan(
                SourceAction::Install,
                request.target_adapter,
                request.target_coordinate.clone(),
                request.target_package_kind,
                request.target_package_version.clone(),
            )
            .await?;
        let cleanup_target = cleanup_plan_for_target(service, &install_target)?;
        let mut plan = ManagedToPackageMigrationPlan {
            app_id: request.app_id.clone(),
            app_version: request.app_version.clone(),
            current_installation: current,
            uninstall_current,
            target_state,
            install_target,
            cleanup_target,
            target_executable_path: request.target_executable_path.clone(),
            approval_token: String::new(),
            warnings: vec![
                "The managed installation is staged for rollback before the reviewed package-manager command runs.".to_owned(),
                "Application configuration outside the managed version directory is not read or migrated.".to_owned(),
                "Package-manager changes may affect shared dependencies and cannot be made filesystem-atomic.".to_owned(),
            ],
        };
        plan.approval_token = managed_to_package_token(&plan)?;
        Ok(plan)
    }

    fn managed_migration_installation(
        &self,
        request: &SourceMigrationRequest,
    ) -> TorbenResult<InstallRecord> {
        let record = self
            .store
            .get_installation(&request.app_id, &request.app_version)?
            .ok_or_else(|| {
                TorbenError::new("version_not_installed", "The version is not installed.")
            })?;
        let expected = self
            .paths
            .app_version_dir(request.app_id.as_str(), &request.app_version.to_string());
        let actual = PathBuf::from(&record.install_path);
        if record.scope != InstallScope::Managed || actual != expected {
            return Err(TorbenError::new(
                "managed_install_path_invalid",
                "Managed source migration requires a standard managed installation.",
            ));
        }
        if self.store.selected_version(&request.app_id)?.as_ref() == Some(&request.app_version) {
            return Err(TorbenError::new(
                "version_is_selected",
                "The selected managed version cannot migrate to a package source.",
            )
            .with_remediation("Select another version or clear the selection first."));
        }
        let metadata = actual.symlink_metadata().map_err(|error| {
            TorbenError::new(
                "managed_install_path_invalid",
                "The managed installation directory is unavailable.",
            )
            .with_detail("reason", error.to_string())
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(TorbenError::new(
                "managed_install_path_invalid",
                "The managed installation path must be a regular directory.",
            ));
        }
        Ok(record)
    }

    async fn prepare_source_migration_plan(
        &self,
        request: &SourceMigrationRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<SourceMigrationPlan> {
        self.validate_package_migration_target(request)?;
        let current_owner = self.source_migration_owner(request)?;
        let (current_state, target_state) =
            Self::inspect_source_migration(request, &current_owner, service).await?;
        let commands = Self::source_migration_commands(request, &current_owner, service).await?;
        let mut plan = SourceMigrationPlan {
            app_id: request.app_id.clone(),
            app_version: request.app_version.clone(),
            current_owner,
            current_state,
            target_state,
            uninstall_current: commands.uninstall_current,
            install_target: commands.install_target,
            cleanup_target: commands.cleanup_target,
            restore_current: commands.restore_current,
            target_executable_path: request.target_executable_path.clone(),
            approval_token: String::new(),
            warnings: commands.warnings,
        };
        plan.approval_token = source_migration_token(&plan)?;
        Ok(plan)
    }

    fn source_migration_owner(
        &self,
        request: &SourceMigrationRequest,
    ) -> TorbenResult<PackageInstallationRecord> {
        let current_owner = self
            .store
            .package_installation(&request.app_id, &request.app_version)?
            .ok_or_else(|| {
                TorbenError::new(
                    "source_migration_owner_required",
                    "Only a Torben-owned package-manager installation can migrate sources.",
                )
            })?;
        if current_owner.adapter == request.target_adapter
            && current_owner.coordinate == request.target_coordinate
            && current_owner.package_kind == request.target_package_kind
        {
            return Err(TorbenError::new(
                "source_migration_target_unchanged",
                "The migration target is the current immutable source owner.",
            ));
        }
        Ok(current_owner)
    }

    fn validate_package_migration_target(
        &self,
        request: &SourceMigrationRequest,
    ) -> TorbenResult<()> {
        if !Path::new(&request.target_executable_path).is_absolute() {
            return Err(TorbenError::new(
                "source_health_path_invalid",
                "A source migration target executable path must be absolute.",
            ));
        }
        if let Some(conflict) =
            self.store
                .list_package_installations()?
                .into_iter()
                .find(|record| {
                    record.adapter == request.target_adapter
                        && record.coordinate == request.target_coordinate
                        && record.package_kind == request.target_package_kind
                        && (record.app_id != request.app_id
                            || record.app_version != request.app_version)
                })
        {
            return Err(TorbenError::new(
                "package_source_owner_conflict",
                "The migration target is owned by another Torben application record.",
            )
            .with_detail("ownerAppId", conflict.app_id.to_string())
            .with_detail("ownerVersion", conflict.app_version.to_string()));
        }
        Ok(())
    }

    async fn inspect_source_migration(
        request: &SourceMigrationRequest,
        current_owner: &PackageInstallationRecord,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<(SourcePackageState, SourcePackageState)> {
        let current_state = service
            .inspect(
                current_owner.adapter,
                current_owner.coordinate.clone(),
                current_owner.package_kind,
            )
            .await?;
        if !current_state.installed
            || current_state.installed_version.as_ref() != Some(&current_owner.package_version)
        {
            return Err(TorbenError::new(
                "source_package_state_drifted",
                "The current package state no longer matches Torben's source owner.",
            )
            .with_detail("ownedVersion", current_owner.package_version.to_string())
            .with_detail(
                "installedVersion",
                current_state
                    .installed_version
                    .as_ref()
                    .map_or("absent", SourcePackageVersion::as_str),
            ));
        }
        let target_state = service
            .inspect(
                request.target_adapter,
                request.target_coordinate.clone(),
                request.target_package_kind,
            )
            .await?;
        if target_state.installed {
            return Err(TorbenError::new(
                "source_migration_target_present",
                "The target package is already installed and cannot be adopted during migration.",
            )
            .with_remediation(
                "Keep the target external, or remove it with its package manager before planning migration.",
            ));
        }
        Ok((current_state, target_state))
    }

    async fn source_migration_commands(
        request: &SourceMigrationRequest,
        current_owner: &PackageInstallationRecord,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<SourceMigrationCommands> {
        let uninstall_current = service
            .reviewed_plan(
                SourceAction::Uninstall,
                current_owner.adapter,
                current_owner.coordinate.clone(),
                current_owner.package_kind,
                Some(current_owner.package_version.clone()),
            )
            .await?;
        let install_target = service
            .reviewed_plan(
                SourceAction::Install,
                request.target_adapter,
                request.target_coordinate.clone(),
                request.target_package_kind,
                request.target_package_version.clone(),
            )
            .await?;
        let cleanup_target = cleanup_plan_for_target(service, &install_target)?;
        let restore_current = service
            .reviewed_plan(
                SourceAction::Install,
                current_owner.adapter,
                current_owner.coordinate.clone(),
                current_owner.package_kind,
                Some(current_owner.package_version.clone()),
            )
            .await?;
        let mut warnings = vec![
            "Migration uninstalls the current package and reinstalls it from the reviewed target source; application configuration is not read or migrated.".to_owned(),
            "Package-manager changes are not filesystem-atomic and may modify shared dependencies.".to_owned(),
            "If target installation fails, Torben attempts the reviewed cleanup and source restore plans; failed compensation requires manual reconciliation.".to_owned(),
        ];
        if !restore_current.exact_version_guaranteed {
            warnings.push(
                "The current package manager cannot guarantee restoration of an arbitrary historical version."
                    .to_owned(),
            );
        }
        Ok(SourceMigrationCommands {
            uninstall_current,
            install_target,
            cleanup_target,
            restore_current,
            warnings,
        })
    }

    async fn execute_prepared_source_migration(
        &self,
        plan: SourceMigrationPlan,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<SourceMigrationResult> {
        let mut journal =
            OperationJournal::start_source_migration(&self.paths, Arc::clone(&self.store), &plan)?;
        journal.record(
            OperationState::Running,
            "preview",
            "Executing the approved source migration plan",
            Some(0.1),
        )?;
        if let Err(error) = Self::remove_source_migration_current(&plan, service).await {
            if error.code == "source_migration_reconciliation_required" {
                self.store
                    .remove_package_installation(&plan.app_id, &plan.app_version)?;
                journal.fail_reconciliation_required(&error)?;
            } else {
                journal.fail_reconciled(&error)?;
            }
            return Err(error);
        }
        let target = match Self::prepare_package_migration_target(
            &plan.app_id,
            &plan.app_version,
            &plan.install_target,
            &plan.target_executable_path,
            service,
            &mut journal,
        )
        .await
        {
            Ok(target) => target,
            Err(error) => {
                return self
                    .rollback_source_migration(plan, service, &mut journal, error)
                    .await;
            }
        };
        let request = SourceExecutionRequest {
            app_id: plan.app_id.clone(),
            app_version: plan.app_version.clone(),
            action: SourceAction::Install,
            adapter: plan.install_target.adapter,
            coordinate: plan.install_target.coordinate.clone(),
            package_kind: plan.install_target.package_kind,
            package_version: plan.install_target.package_version.clone(),
            executable_path: Some(plan.target_executable_path.clone()),
            approved_execution_identity: plan.install_target.execution_identity.clone(),
            accept_system_changes: true,
        };
        let (installation, package) = Self::source_installation_records(
            &request,
            target.version,
            &target.executable,
            &target.state,
        )?;
        journal.record(
            OperationState::Running,
            "state_commit",
            "Atomically replacing the immutable source owner",
            Some(0.92),
        )?;
        if let Err(error) =
            self.store
                .replace_package_installation(&plan.current_owner, &installation, &package)
        {
            return self
                .rollback_source_migration(plan, service, &mut journal, error)
                .await;
        }
        journal.succeed("Package source migration committed")?;
        Ok(SourceMigrationResult {
            operation_id: journal.operation_id(),
            plan,
            installation: package,
        })
    }

    async fn remove_source_migration_current(
        plan: &SourceMigrationPlan,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<()> {
        let removal = service.execute(&plan.uninstall_current).await;
        let current_after = service
            .inspect(
                plan.current_owner.adapter,
                plan.current_owner.coordinate.clone(),
                plan.current_owner.package_kind,
            )
            .await
            .map_err(|error| source_migration_reconciliation_error(&error))?;
        if !current_after.installed {
            return Ok(());
        }
        if current_after.installed_version.as_ref() != Some(&plan.current_owner.package_version) {
            return Err(TorbenError::new(
                "source_migration_reconciliation_required",
                "The current source changed to an unexpected version during migration.",
            )
            .with_remediation("Inspect the current package source before retrying."));
        }
        Err(removal.err().unwrap_or_else(|| {
            TorbenError::new(
                "source_reconciliation_failed",
                "The current package remains installed after the migration uninstall step.",
            )
        }))
    }

    async fn prepare_package_migration_target(
        app_id: &AppId,
        app_version: &ExactVersion,
        install_target: &SourceOperationPlan,
        target_executable_path: &str,
        service: &source_adapters::SourceAdapterService,
        journal: &mut OperationJournal,
    ) -> TorbenResult<PreparedMigrationTarget> {
        journal.record(
            OperationState::Running,
            "install_target",
            "Installing the reviewed target package",
            Some(0.45),
        )?;
        let execution = service.execute(install_target).await;
        let state = service
            .inspect(
                install_target.adapter,
                install_target.coordinate.clone(),
                install_target.package_kind,
            )
            .await?;
        execution?;
        let version = state.installed_version.clone().ok_or_else(|| {
            TorbenError::new(
                "source_reconciliation_failed",
                "The target package manager returned success but the package is absent.",
            )
        })?;
        if install_target
            .package_version
            .as_ref()
            .is_some_and(|expected| expected != &version)
        {
            return Err(TorbenError::new(
                "source_reconciliation_version_mismatch",
                "The migration target installed a different raw package version.",
            )
            .with_detail("installedVersion", version.to_string()));
        }
        journal.record(
            OperationState::Running,
            "health_check",
            "Checking the target application executable",
            Some(0.75),
        )?;
        let executable = service
            .health_check(
                app_id.as_str(),
                &app_version.to_string(),
                Path::new(target_executable_path),
            )
            .await?;
        Ok(PreparedMigrationTarget {
            state,
            version,
            executable,
        })
    }

    async fn execute_prepared_managed_to_package(
        &self,
        plan: ManagedToPackageMigrationPlan,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<ManagedToPackageMigrationResult> {
        let mut journal = OperationJournal::start_managed_to_package_migration(
            &self.paths,
            Arc::clone(&self.store),
            &plan,
        )?;
        let backup = source_migration_backup(&self.paths, journal.operation_id());
        if let Err(error) = stage_managed_source(&plan, &backup, &mut journal) {
            journal.fail_and_rollback(&error)?;
            return Err(error);
        }
        let target = match Self::prepare_package_migration_target(
            &plan.app_id,
            &plan.app_version,
            &plan.install_target,
            &plan.target_executable_path,
            service,
            &mut journal,
        )
        .await
        {
            Ok(target) => target,
            Err(error) => {
                return self
                    .rollback_managed_to_package(plan, service, &backup, &mut journal, error)
                    .await;
            }
        };
        let request = package_request_from_managed_plan(&plan);
        let (installation, package) = Self::source_installation_records(
            &request,
            target.version,
            &target.executable,
            &target.state,
        )?;
        journal.record(
            OperationState::Running,
            "state_commit",
            "Atomically replacing managed ownership with the package source",
            Some(0.92),
        )?;
        if let Err(error) = self.store.replace_managed_with_package(
            &plan.current_installation,
            &installation,
            &package,
        ) {
            return self
                .rollback_managed_to_package(plan, service, &backup, &mut journal, error)
                .await;
        }
        if let Err(error) = remove_source_migration_backup(&backup) {
            journal.record(
                OperationState::Running,
                "cleanup_pending",
                format!("{}: {}", error.code, error.message),
                Some(0.98),
            )?;
            return Err(error);
        }
        journal.succeed("Managed installation migrated to the package source")?;
        Ok(ManagedToPackageMigrationResult {
            operation_id: journal.operation_id(),
            plan,
            installation: package,
        })
    }

    async fn rollback_managed_to_package(
        &self,
        plan: ManagedToPackageMigrationPlan,
        service: &source_adapters::SourceAdapterService,
        backup: &Path,
        journal: &mut OperationJournal,
        cause: TorbenError,
    ) -> TorbenResult<ManagedToPackageMigrationResult> {
        journal.record(
            OperationState::Running,
            "compensate",
            "Cleaning the package target and restoring the managed installation",
            Some(0.84),
        )?;
        let cleanup = cleanup_migration_package_target(&plan.cleanup_target, service).await;
        let restore = restore_managed_source(&plan.current_installation, backup);
        let health = match restore {
            Ok(()) => self.plugin_health_check(&plan.current_installation).await,
            Err(error) => Err(error),
        };
        if let Err(error) = health {
            let _ = self
                .store
                .remove_installation(&plan.app_id, &plan.app_version);
            let failure = TorbenError::new(
                "source_migration_reconciliation_required",
                "The managed source could not be restored to a verified state.",
            )
            .with_detail("causeCode", cause.code)
            .with_detail("restoreCode", error.code)
            .with_remediation(
                "Inspect the managed library and package manager; Torben removed unverified ownership.",
            );
            journal.fail_reconciliation_required(&failure)?;
            return Err(failure);
        }
        if let Err(error) = cleanup {
            let failure = TorbenError::new(
                "source_migration_reconciliation_required",
                "The managed source was restored, but the package target could not be cleaned.",
            )
            .with_detail("causeCode", cause.code)
            .with_detail("cleanupCode", error.code)
            .with_remediation(
                "The managed installation remains owned. Inspect the target package as external state.",
            );
            journal.fail_reconciliation_required(&failure)?;
            return Err(failure);
        }
        journal.fail_and_rollback(&cause)?;
        Err(cause
            .with_detail("sourceRestored", "true")
            .with_remediation("The managed installation was restored and verified."))
    }

    async fn execute_prepared_package_to_managed(
        &self,
        plan: PackageToManagedMigrationPlan,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<PackageToManagedMigrationResult> {
        let mut journal = OperationJournal::start_package_to_managed_migration(
            &self.paths,
            Arc::clone(&self.store),
            &plan,
        )?;
        journal.record(
            OperationState::Running,
            "install_managed",
            "Installing and verifying the official managed archive",
            Some(0.2),
        )?;
        let managed = self
            .prepare_managed_payload_for_migration(&plan, &mut journal)
            .await?;
        self.commit_package_to_managed_payload(plan, service, &mut journal, managed)
            .await
    }

    async fn commit_package_to_managed_payload(
        &self,
        plan: PackageToManagedMigrationPlan,
        service: &source_adapters::SourceAdapterService,
        journal: &mut OperationJournal,
        managed: InstallRecord,
    ) -> TorbenResult<PackageToManagedMigrationResult> {
        journal.record(
            OperationState::Running,
            "remove_package",
            "Removing the reviewed package-manager source",
            Some(0.72),
        )?;
        if let Err(error) = remove_package_for_managed_migration(&plan, service).await {
            let cleanup = cleanup_package_to_managed_payload(
                &self.paths,
                &plan,
                journal.operation_id(),
                journal,
            );
            if error.code == "source_migration_reconciliation_required" {
                let _ = self
                    .store
                    .remove_package_installation(&plan.app_id, &plan.app_version);
                let failure = match cleanup {
                    Ok(()) => error,
                    Err(cleanup_error) => source_migration_cleanup_error(&error, &cleanup_error),
                };
                journal.fail_reconciliation_required(&failure)?;
                return Err(failure);
            }
            return match cleanup {
                Ok(()) => {
                    journal.fail_and_rollback(&error)?;
                    Err(error)
                }
                Err(cleanup_error) => {
                    let failure = source_migration_cleanup_error(&error, &cleanup_error);
                    journal.fail_reconciliation_required(&failure)?;
                    Err(failure)
                }
            };
        }
        journal.record(
            OperationState::Running,
            "state_commit",
            "Atomically replacing package ownership with managed ownership",
            Some(0.94),
        )?;
        if let Err(error) = self
            .store
            .replace_package_with_managed(&plan.current_owner, &managed)
        {
            return self
                .rollback_package_to_managed(plan, service, journal, error)
                .await;
        }
        remove_package_to_managed_receipt_if_present(&self.paths, journal.operation_id())?;
        journal.succeed("Package source migrated to the official managed archive")?;
        Ok(PackageToManagedMigrationResult {
            operation_id: journal.operation_id(),
            plan,
            installation: managed,
        })
    }

    async fn prepare_managed_payload_for_migration(
        &self,
        plan: &PackageToManagedMigrationPlan,
        journal: &mut OperationJournal,
    ) -> TorbenResult<InstallRecord> {
        let managed = match self
            .install_managed_payload(
                &plan.app_id,
                &plan.app_version,
                &plan.install_managed,
                journal,
            )
            .await
        {
            Ok(record) => record,
            Err(error) => return self.discard_package_to_managed_payload(plan, journal, error),
        };
        if let Err(error) = write_package_to_managed_receipt(&self.paths, plan, journal) {
            journal.fail_reconciliation_required(&error)?;
            return Err(error);
        }
        if let Err(error) = validate_package_to_managed_installation(plan, &managed) {
            return self.discard_package_to_managed_payload(plan, journal, error);
        }
        if let Err(error) = journal.cancellation_probe().check() {
            journal.acknowledge_cancellation()?;
            return self.discard_package_to_managed_payload(plan, journal, error);
        }
        Ok(managed)
    }

    fn discard_package_to_managed_payload(
        &self,
        plan: &PackageToManagedMigrationPlan,
        journal: &mut OperationJournal,
        error: TorbenError,
    ) -> TorbenResult<InstallRecord> {
        let cleanup =
            cleanup_package_to_managed_payload(&self.paths, plan, journal.operation_id(), journal);
        match cleanup {
            Ok(()) => {
                journal.fail_and_rollback(&error)?;
                Err(error)
            }
            Err(cleanup_error) => {
                let failure = source_migration_cleanup_error(&error, &cleanup_error);
                journal.fail_reconciliation_required(&failure)?;
                Err(failure)
            }
        }
    }

    async fn rollback_package_to_managed(
        &self,
        plan: PackageToManagedMigrationPlan,
        service: &source_adapters::SourceAdapterService,
        journal: &mut OperationJournal,
        cause: TorbenError,
    ) -> TorbenResult<PackageToManagedMigrationResult> {
        journal.record(
            OperationState::Running,
            "compensate",
            "Restoring the reviewed package source and removing the managed archive",
            Some(0.82),
        )?;
        let restore = restore_package_after_managed_failure(&plan, service).await;
        let cleanup =
            cleanup_package_to_managed_payload(&self.paths, &plan, journal.operation_id(), journal);
        if let (Ok(()), Ok(())) = (&restore, &cleanup) {
            journal.fail_and_rollback(&cause)?;
            return Err(cause
                .with_detail("sourceRestored", "true")
                .with_remediation("The package source was restored and verified."));
        }
        let _ = self
            .store
            .remove_package_installation(&plan.app_id, &plan.app_version);
        let failure = TorbenError::new(
            "source_migration_reconciliation_required",
            "Package-to-managed migration compensation could not be fully verified.",
        )
        .with_detail("causeCode", cause.code)
        .with_detail(
            "restoreCode",
            restore
                .as_ref()
                .err()
                .map_or("none", |error| error.code.as_str()),
        )
        .with_detail(
            "cleanupCode",
            cleanup
                .as_ref()
                .err()
                .map_or("none", |error| error.code.as_str()),
        )
        .with_remediation(
            "Inspect the package manager and managed library; Torben removed unverified ownership.",
        );
        journal.fail_reconciliation_required(&failure)?;
        Err(failure)
    }

    async fn rollback_source_migration(
        &self,
        plan: SourceMigrationPlan,
        service: &source_adapters::SourceAdapterService,
        journal: &mut OperationJournal,
        cause: TorbenError,
    ) -> TorbenResult<SourceMigrationResult> {
        journal.record(
            OperationState::Running,
            "compensate",
            "Cleaning the target package before restoring the previous source",
            Some(0.82),
        )?;
        let compensation = async {
            let target = service
                .inspect(
                    plan.cleanup_target.adapter,
                    plan.cleanup_target.coordinate.clone(),
                    plan.cleanup_target.package_kind,
                )
                .await?;
            if target.installed {
                let cleanup_result = service.execute(&plan.cleanup_target).await;
                let cleaned = service
                    .inspect(
                        plan.cleanup_target.adapter,
                        plan.cleanup_target.coordinate.clone(),
                        plan.cleanup_target.package_kind,
                    )
                    .await?;
                if cleaned.installed {
                    return Err(cleanup_result.err().unwrap_or_else(|| {
                        TorbenError::new(
                            "source_migration_cleanup_failed",
                            "The failed migration target remains installed.",
                        )
                    }));
                }
            }
            let restore_result = service.execute(&plan.restore_current).await;
            let restored = service
                .inspect(
                    plan.current_owner.adapter,
                    plan.current_owner.coordinate.clone(),
                    plan.current_owner.package_kind,
                )
                .await?;
            if !restored.installed
                || restored.installed_version.as_ref() != Some(&plan.current_owner.package_version)
            {
                return Err(restore_result.err().unwrap_or_else(|| {
                    TorbenError::new(
                        "source_migration_restore_failed",
                        "The previous package source could not be restored exactly.",
                    )
                }));
            }
            service
                .health_check(
                    plan.app_id.as_str(),
                    &plan.app_version.to_string(),
                    Path::new(&plan.current_owner.executable_path),
                )
                .await?;
            Ok::<(), TorbenError>(())
        }
        .await;
        if compensation.is_ok() {
            journal.fail_and_rollback(&cause)?;
            return Err(cause
                .with_detail("sourceRestored", "true")
                .with_remediation(
                    "The reviewed target migration failed, but Torben restored and verified the previous source.",
                ));
        }
        let compensation_error = compensation.unwrap_err();
        if let Err(state_error) = self
            .store
            .remove_package_installation(&plan.app_id, &plan.app_version)
        {
            journal.fail_reconciliation_required(&state_error)?;
            return Err(source_migration_reconciliation_error(&state_error));
        }
        let error = TorbenError::new(
            "source_migration_reconciliation_required",
            "Source migration and compensation did not reach a verified owned state.",
        )
        .with_detail("causeCode", cause.code)
        .with_detail("compensationCode", compensation_error.code)
        .with_remediation(
            "Inspect both package sources manually. Torben removed ownership and will treat any remaining package as external.",
        );
        journal.fail_reconciliation_required(&error)?;
        Err(error)
    }

    async fn execute_source_operation_with_service(
        &self,
        request: SourceExecutionRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<SourceExecutionResult> {
        self.validate_source_execution_request(&request)?;
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let prepared = self.prepare_source_operation(&request, service).await?;
        self.execute_prepared_source_operation(&request, service, prepared)
            .await
    }

    fn validate_source_execution_request(
        &self,
        request: &SourceExecutionRequest,
    ) -> TorbenResult<()> {
        if !request.accept_system_changes {
            return Err(TorbenError::new(
                "source_operation_confirmation_required",
                "Package-manager execution requires explicit acceptance of system changes.",
            )
            .with_remediation(
                "Review `torben source plan` output, then repeat with --accept-system-changes.",
            ));
        }
        self.application(&request.app_id).map(|_| ())
    }

    async fn prepare_source_operation(
        &self,
        request: &SourceExecutionRequest,
        service: &source_adapters::SourceAdapterService,
    ) -> TorbenResult<PreparedSourceOperation> {
        let existing_installation = self
            .store
            .get_installation(&request.app_id, &request.app_version)?;
        let existing_owner = self
            .store
            .package_installation(&request.app_id, &request.app_version)?;
        if let Some(owner) = self
            .store
            .list_package_installations()?
            .into_iter()
            .find(|record| {
                record.adapter == request.adapter
                    && record.coordinate == request.coordinate
                    && record.package_kind == request.package_kind
                    && (record.app_id != request.app_id
                        || record.app_version != request.app_version)
            })
        {
            return Err(TorbenError::new(
                "package_source_owner_conflict",
                "The package coordinate is already owned by another Torben application record.",
            )
            .with_detail("coordinate", request.coordinate.to_string())
            .with_detail("ownerAppId", owner.app_id.to_string())
            .with_detail("ownerVersion", owner.app_version.to_string()));
        }
        let before = service
            .inspect(
                request.adapter,
                request.coordinate.clone(),
                request.package_kind,
            )
            .await?;
        match request.action {
            SourceAction::Install => {
                Self::prepare_source_install(
                    request,
                    service,
                    before,
                    existing_installation,
                    existing_owner,
                )
                .await
            }
            SourceAction::Uninstall => {
                Self::prepare_source_uninstall(request, service, before, existing_owner).await
            }
        }
    }

    async fn prepare_source_install(
        request: &SourceExecutionRequest,
        service: &source_adapters::SourceAdapterService,
        before: SourcePackageState,
        existing_installation: Option<InstallRecord>,
        existing_owner: Option<PackageInstallationRecord>,
    ) -> TorbenResult<PreparedSourceOperation> {
        if before.installed {
            return Err(TorbenError::new(
                "source_external_installation_present",
                "The package is already installed and Torben will not take ownership of it.",
            )
            .with_detail("adapter", request.adapter.to_string())
            .with_detail("coordinate", request.coordinate.to_string())
            .with_detail(
                "installedVersion",
                before
                    .installed_version
                    .as_ref()
                    .map_or("unknown", SourcePackageVersion::as_str),
            )
            .with_remediation(
                "Keep the package external, or remove it with its package manager before asking Torben to install it.",
            ));
        }
        if let Some(existing) = existing_installation {
            return Err(TorbenError::new(
                "installation_source_conflict",
                "The application version is already owned by another source.",
            )
            .with_detail("sourceId", existing.source_id.to_string())
            .with_detail(
                "scope",
                format!("{:?}", existing.scope).to_ascii_lowercase(),
            ));
        }
        if existing_owner.is_some() {
            return Err(TorbenError::new(
                "package_ownership_state_invalid",
                "Package ownership exists without a matching installation record.",
            ));
        }
        let executable = request.executable_path.as_deref().ok_or_else(|| {
            TorbenError::new(
                "source_health_path_required",
                "A package-manager install requires the expected application executable path.",
            )
            .with_remediation("Pass --executable-path with an absolute application path.")
        })?;
        let plan = service
            .reviewed_plan(
                SourceAction::Install,
                request.adapter,
                request.coordinate.clone(),
                request.package_kind,
                request.package_version.clone(),
            )
            .await?;
        Self::validate_source_plan_approval(request, &plan)?;
        Ok(PreparedSourceOperation {
            before,
            plan,
            owner: None,
            executable_path: Some(PathBuf::from(executable)),
        })
    }

    async fn prepare_source_uninstall(
        request: &SourceExecutionRequest,
        service: &source_adapters::SourceAdapterService,
        before: SourcePackageState,
        existing_owner: Option<PackageInstallationRecord>,
    ) -> TorbenResult<PreparedSourceOperation> {
        if request.executable_path.is_some() {
            return Err(TorbenError::new(
                "source_uninstall_executable_unexpected",
                "Package-manager uninstall uses the executable recorded at ownership commit.",
            ));
        }
        let owner = existing_owner.ok_or_else(|| {
            TorbenError::new(
                "package_ownership_not_found",
                "Torben will not uninstall a package without a matching ownership record.",
            )
            .with_detail("appId", request.app_id.to_string())
            .with_detail("version", request.app_version.to_string())
        })?;
        validate_source_owner_matches_request(&owner, request)?;
        if !owner.owned_by_torben {
            return Err(TorbenError::new(
                "package_ownership_not_found",
                "The package record is not owned by Torben.",
            ));
        }
        if before.installed && before.installed_version.as_ref() != Some(&owner.package_version) {
            return Err(TorbenError::new(
                "source_package_state_drifted",
                "The installed package version changed outside Torben.",
            )
            .with_detail("ownedVersion", owner.package_version.to_string())
            .with_detail(
                "installedVersion",
                before
                    .installed_version
                    .as_ref()
                    .map_or("unknown", SourcePackageVersion::as_str),
            )
            .with_remediation(
                "Reconcile the package manually; Torben will not remove a changed external package.",
            ));
        }
        let plan_version =
            (request.adapter != SourceAdapterKind::Homebrew).then(|| owner.package_version.clone());
        let plan = service
            .reviewed_plan(
                SourceAction::Uninstall,
                request.adapter,
                request.coordinate.clone(),
                request.package_kind,
                plan_version,
            )
            .await?;
        Self::validate_source_plan_approval(request, &plan)?;
        Ok(PreparedSourceOperation {
            before,
            plan,
            owner: Some(owner),
            executable_path: None,
        })
    }

    fn validate_source_plan_approval(
        request: &SourceExecutionRequest,
        plan: &SourceOperationPlan,
    ) -> TorbenResult<()> {
        let Some(identity) = plan.execution_identity.as_deref() else {
            return Ok(());
        };
        if request.approved_execution_identity.as_deref() == Some(identity) {
            return Ok(());
        }
        Err(TorbenError::new(
            "source_plan_approval_required",
            "The resolved package identity does not match the reviewed execution plan.",
        )
        .with_detail("executionIdentity", identity)
        .with_remediation(
            "Review a new source plan and pass its exact executionIdentity when executing.",
        ))
    }

    async fn execute_prepared_source_operation(
        &self,
        request: &SourceExecutionRequest,
        service: &source_adapters::SourceAdapterService,
        prepared: PreparedSourceOperation,
    ) -> TorbenResult<SourceExecutionResult> {
        let mut journal =
            OperationJournal::start_source(&self.paths, Arc::clone(&self.store), request)?;
        journal.record(
            OperationState::Running,
            "preview",
            format!(
                "Approved plan: {} {}",
                prepared.plan.executable,
                prepared.plan.execute_arguments.join(" ")
            ),
            Some(0.2),
        )?;
        if request.action == SourceAction::Uninstall && !prepared.before.installed {
            self.store
                .remove_package_installation(&request.app_id, &request.app_version)?;
            journal
                .succeed("Removed stale Torben ownership after confirming the package is absent")?;
            return Ok(SourceExecutionResult {
                operation_id: journal.operation_id(),
                plan: prepared.plan,
                before: prepared.before.clone(),
                after: prepared.before,
                outcome: SourceExecutionOutcome::OwnershipRemoved,
                installation: None,
            });
        }
        journal.record(
            OperationState::Running,
            "execute",
            "Executing the approved package-manager mutation",
            Some(0.45),
        )?;
        let execution = service.execute(&prepared.plan).await;
        journal.record(
            OperationState::Running,
            "reconcile",
            "Re-inspecting package-manager state",
            Some(0.72),
        )?;
        let after = match service
            .inspect(
                request.adapter,
                request.coordinate.clone(),
                request.package_kind,
            )
            .await
        {
            Ok(state) => state,
            Err(error) => {
                journal.fail_reconciliation_required(&error)?;
                return Err(error.with_remediation(
                    "Torben could not verify external state and did not change ownership. Inspect the package manager before retrying.",
                ));
            }
        };
        if let Err(error) = execution {
            return self.finish_failed_source_execution(
                request,
                prepared,
                after,
                error,
                &mut journal,
            );
        }
        match request.action {
            SourceAction::Install => {
                self.commit_source_install(request, service, prepared, after, &mut journal)
                    .await
            }
            SourceAction::Uninstall => self.commit_source_uninstall(prepared, after, &mut journal),
        }
    }

    fn finish_failed_source_execution(
        &self,
        request: &SourceExecutionRequest,
        prepared: PreparedSourceOperation,
        after: SourcePackageState,
        error: TorbenError,
        journal: &mut OperationJournal,
    ) -> TorbenResult<SourceExecutionResult> {
        if request.action == SourceAction::Uninstall && !after.installed {
            self.store
                .remove_package_installation(&request.app_id, &request.app_version)?;
            journal.succeed(
                "The package manager reported an error, but reconciliation confirmed removal",
            )?;
            return Ok(SourceExecutionResult {
                operation_id: journal.operation_id(),
                plan: prepared.plan,
                before: prepared.before,
                after,
                outcome: SourceExecutionOutcome::OwnershipRemoved,
                installation: None,
            });
        }
        journal.fail_reconciled(&error)?;
        Err(error)
    }

    async fn commit_source_install(
        &self,
        request: &SourceExecutionRequest,
        service: &source_adapters::SourceAdapterService,
        prepared: PreparedSourceOperation,
        after: SourcePackageState,
        journal: &mut OperationJournal,
    ) -> TorbenResult<SourceExecutionResult> {
        let Some(installed_version) = after.installed_version.clone() else {
            let error = TorbenError::new(
                "source_reconciliation_failed",
                "The package manager returned success but the package is not installed.",
            );
            journal.fail_reconciled(&error)?;
            return Err(error);
        };
        if let Some(expected) = &request.package_version
            && expected != &installed_version
        {
            let error = TorbenError::new(
                "source_reconciliation_version_mismatch",
                "The package manager installed a different raw version.",
            )
            .with_detail("expectedVersion", expected.to_string())
            .with_detail("installedVersion", installed_version.to_string());
            journal.fail_reconciled(&error)?;
            return Err(error);
        }
        journal.record(
            OperationState::Running,
            "health_check",
            "Checking the installed application executable",
            Some(0.86),
        )?;
        let requested_executable = prepared.executable_path.as_ref().expect("validated above");
        let executable = match service
            .health_check(
                request.app_id.as_str(),
                &request.app_version.to_string(),
                requested_executable,
            )
            .await
        {
            Ok(path) => path,
            Err(error) => {
                journal.fail_reconciled(&error)?;
                return Err(error);
            }
        };
        let (installation, package) =
            Self::source_installation_records(request, installed_version, &executable, &after)?;
        journal.record(
            OperationState::Running,
            "state_commit",
            "Committing reconciled package ownership",
            Some(0.95),
        )?;
        self.store
            .commit_package_installation(&installation, &package)?;
        journal.succeed("Package-manager installation reconciled and ownership committed")?;
        Ok(SourceExecutionResult {
            operation_id: journal.operation_id(),
            plan: prepared.plan,
            before: prepared.before,
            after,
            outcome: SourceExecutionOutcome::OwnershipCommitted,
            installation: Some(package),
        })
    }

    fn source_installation_records(
        request: &SourceExecutionRequest,
        installed_version: SourcePackageVersion,
        executable: &Path,
        after: &SourcePackageState,
    ) -> TorbenResult<(InstallRecord, PackageInstallationRecord)> {
        let install_path = executable.parent().ok_or_else(|| {
            TorbenError::new(
                "source_health_path_invalid",
                "The application executable has no parent directory.",
            )
        })?;
        let installed_at = source_timestamp();
        let installation = InstallRecord {
            app_id: request.app_id.clone(),
            version: request.app_version.clone(),
            source_id: after.source_id.clone(),
            scope: InstallScope::PackageManager,
            install_path: install_path.display().to_string(),
            installed_at: installed_at.clone(),
            health: "healthy".to_owned(),
        };
        let package = PackageInstallationRecord {
            app_id: request.app_id.clone(),
            app_version: request.app_version.clone(),
            source_id: after.source_id.clone(),
            adapter: request.adapter,
            coordinate: request.coordinate.clone(),
            package_kind: request.package_kind,
            package_version: installed_version,
            architecture: after
                .architecture
                .clone()
                .unwrap_or_else(|| std::env::consts::ARCH.to_owned()),
            executable_path: executable.display().to_string(),
            owned_by_torben: true,
            installed_at,
            health: "healthy".to_owned(),
        };
        Ok((installation, package))
    }

    fn commit_source_uninstall(
        &self,
        prepared: PreparedSourceOperation,
        after: SourcePackageState,
        journal: &mut OperationJournal,
    ) -> TorbenResult<SourceExecutionResult> {
        if after.installed {
            let error = TorbenError::new(
                "source_reconciliation_failed",
                "The package manager returned success but the package remains installed.",
            );
            journal.fail_reconciled(&error)?;
            return Err(error);
        }
        let owner = prepared.owner.expect("validated above");
        self.store
            .remove_package_installation(&owner.app_id, &owner.app_version)?;
        journal.succeed("Package removal reconciled and Torben ownership removed")?;
        Ok(SourceExecutionResult {
            operation_id: journal.operation_id(),
            plan: prepared.plan,
            before: prepared.before,
            after,
            outcome: SourceExecutionOutcome::OwnershipRemoved,
            installation: None,
        })
    }

    pub fn package_installations(&self) -> TorbenResult<Vec<PackageInstallationRecord>> {
        self.store.list_package_installations()
    }

    pub fn migrate_managed_library(
        &self,
        target: &Path,
    ) -> TorbenResult<ManagedLibraryMigrationResult> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        library_migration::migrate(&self.paths, Arc::clone(&self.store), target)
    }

    pub fn shell_integration_status(&self) -> TorbenResult<ShellIntegrationStatus> {
        self.shell_integration.status(&self.paths.shim_dir())
    }

    pub fn enable_shell_integration(&self) -> TorbenResult<ShellIntegrationStatus> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let previous = self.shell_integration.status(&self.paths.shim_dir())?;
        let mut status = self.shell_integration.enable(&self.paths.shim_dir())?;
        status.new_terminal_required = previous.state != status.state;
        Ok(status)
    }

    pub fn disable_shell_integration(&self) -> TorbenResult<ShellIntegrationStatus> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let previous = self.shell_integration.status(&self.paths.shim_dir())?;
        let mut status = self.shell_integration.disable(&self.paths.shim_dir())?;
        status.new_terminal_required = previous.state != status.state;
        Ok(status)
    }

    pub fn executable_for(&self, app_id: &AppId, command: &str) -> TorbenResult<PathBuf> {
        let selected = self.store.selected_version(app_id)?.ok_or_else(|| {
            TorbenError::new("no_selected_version", "No terminal version is selected.")
                .with_detail("appId", app_id.to_string())
        })?;
        let record = self
            .store
            .get_installation(app_id, &selected)?
            .ok_or_else(|| {
                TorbenError::new(
                    "selection_broken",
                    "The selected installation is missing from state.",
                )
            })?;
        let install_path = validate_selected_installation(&self.paths, &record)?;
        let command_path = match app_id.as_str() {
            "node" => self.node.command_path(&install_path, command),
            "temurin" => self.temurin.command_path(&install_path, command),
            "python" => self.python.command_path(&install_path, command),
            "git" => self.git.command_path(&install_path, command),
            "vscode" => self.vscode.command_path(&install_path, command),
            "codex" => self.codex.command_path(&install_path, command),
            _ => Err(TorbenError::new(
                "capability_not_available",
                "This application does not expose managed terminal commands.",
            )
            .with_detail("appId", app_id.to_string())),
        }?;
        validate_selected_command_path(&install_path, &command_path)
    }

    #[allow(clippy::too_many_lines)]
    pub fn doctor(&self) -> TorbenResult<Vec<DoctorCheck>> {
        let mut checks = vec![DoctorCheck {
            id: "data_directory".to_owned(),
            healthy: self.paths.data_dir().is_dir(),
            message: self.paths.data_dir().display().to_string(),
        }];
        checks.push(DoctorCheck {
            id: "managed_library".to_owned(),
            healthy: self.paths.app_library().is_dir(),
            message: self.paths.app_library().display().to_string(),
        });
        let (logs_healthy, logs_message) = diagnostic_log::probe(self.paths.log_dir()).map_or_else(
            |error| {
                (
                    false,
                    format!("{}: {}", self.paths.log_dir().display(), error),
                )
            },
            |path| (true, path.display().to_string()),
        );
        checks.push(DoctorCheck {
            id: "diagnostic_log".to_owned(),
            healthy: logs_healthy,
            message: logs_message,
        });
        let selections = self.store.list_selections()?;
        for selection in &selections {
            let validation = self
                .store
                .get_installation(&selection.app_id, &selection.version)?
                .ok_or_else(|| {
                    TorbenError::new(
                        "selection_state_invalid",
                        "Selection points to a missing installation.",
                    )
                })
                .and_then(|record| validate_selected_installation(&self.paths, &record));
            checks.push(DoctorCheck {
                id: format!("selection.{}", selection.app_id),
                healthy: validation.is_ok(),
                message: validation.map_or_else(
                    |error| format!("{}: {}", error.code, error.message),
                    |_| format!("{} is selected", selection.version),
                ),
            });
        }
        let (shims_healthy, shims_message) = if selections.is_empty() {
            (
                true,
                format!(
                    "{} (not required until a terminal version is selected)",
                    self.paths.shim_dir().display()
                ),
            )
        } else {
            self.bundled_shim.executable().map_or_else(
                || {
                    let error = self.bundled_shim.missing_error();
                    (
                        false,
                        error
                            .details
                            .get("searchedPaths")
                            .cloned()
                            .unwrap_or_else(|| error.message.clone()),
                    )
                },
                |binary| match shims_match_source(&self.paths, binary) {
                    Ok(true) => (true, self.paths.shim_dir().display().to_string()),
                    Ok(false) => (
                        false,
                        format!("{} (missing or outdated)", self.paths.shim_dir().display()),
                    ),
                    Err(error) => (false, format!("{}: {}", error.code, error.message)),
                },
            )
        };
        checks.push(DoctorCheck {
            id: "terminal_shims".to_owned(),
            healthy: shims_healthy,
            message: shims_message,
        });
        let (shell_healthy, shell_message) = match self.shell_integration_status() {
            Ok(status) => (
                shell_integration_is_healthy(status.state),
                format!(
                    "{} ({})",
                    status.shim_path,
                    format!("{:?}", status.state).to_ascii_lowercase()
                ),
            ),
            Err(error) => (false, format!("{}: {}", error.code, error.message)),
        };
        checks.push(DoctorCheck {
            id: "shell_integration".to_owned(),
            healthy: shell_healthy,
            message: shell_message,
        });
        let (plugin_healthy, plugin_message) = self.node_plugin.diagnostic();
        checks.push(DoctorCheck {
            id: "bundled_plugin.node".to_owned(),
            healthy: plugin_healthy,
            message: plugin_message,
        });
        let (plugin_healthy, plugin_message) = self.temurin_plugin.diagnostic();
        checks.push(DoctorCheck {
            id: "bundled_plugin.temurin".to_owned(),
            healthy: plugin_healthy,
            message: plugin_message,
        });
        let (plugin_healthy, plugin_message) = self.python_plugin.diagnostic();
        checks.push(DoctorCheck {
            id: "bundled_plugin.python".to_owned(),
            healthy: plugin_healthy,
            message: plugin_message,
        });
        let (plugin_healthy, plugin_message) = self.git_plugin.diagnostic();
        checks.push(DoctorCheck {
            id: "bundled_plugin.git".to_owned(),
            healthy: plugin_healthy,
            message: plugin_message,
        });
        let (plugin_healthy, plugin_message) = self.vscode_plugin.diagnostic();
        checks.push(DoctorCheck {
            id: "bundled_plugin.vscode".to_owned(),
            healthy: plugin_healthy,
            message: plugin_message,
        });
        let (plugin_healthy, plugin_message) = self.codex_plugin.diagnostic();
        checks.push(DoctorCheck {
            id: "bundled_plugin.codex".to_owned(),
            healthy: plugin_healthy,
            message: plugin_message,
        });
        for status in source_adapters::SourceAdapterService::discover()
            .discovered_statuses()?
            .into_iter()
            .filter(|status| status.availability != SourceAdapterAvailability::Unsupported)
        {
            checks.push(DoctorCheck {
                id: format!("source_adapter.{}", status.adapter),
                healthy: source_adapter_is_healthy(status.availability),
                message: status.message,
            });
        }
        Ok(checks)
    }

    pub fn install_shims(&self, shim_binary: &std::path::Path) -> TorbenResult<Vec<PathBuf>> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        install_shims_locked(&self.paths, shim_binary)
    }

    pub fn plugins(&self) -> TorbenResult<Vec<PluginSummary>> {
        let mut plugins = vec![
            bundled_plugin_summary(
                BUNDLED_NODE_PLUGIN_MANIFEST,
                BUNDLED_NODE_PLUGIN_ID,
                "Node.js",
            )?,
            bundled_plugin_summary(
                include_str!("../../../plugins/temurin/plugin.manifest.template.json"),
                BUNDLED_TEMURIN_PLUGIN_ID,
                "Eclipse Temurin",
            )?,
            bundled_plugin_summary(
                include_str!("../../../plugins/python/plugin.manifest.template.json"),
                BUNDLED_PYTHON_PLUGIN_ID,
                "Python",
            )?,
            bundled_plugin_summary(
                include_str!("../../../plugins/git/plugin.manifest.template.json"),
                BUNDLED_GIT_PLUGIN_ID,
                "Git",
            )?,
            bundled_plugin_summary(
                include_str!("../../../plugins/vscode/plugin.manifest.template.json"),
                BUNDLED_VSCODE_PLUGIN_ID,
                "Visual Studio Code",
            )?,
            bundled_plugin_summary(
                include_str!("../../../plugins/codex/plugin.manifest.template.json"),
                BUNDLED_CODEX_PLUGIN_ID,
                "Codex CLI",
            )?,
        ];
        for record in self.store.list_plugins()? {
            plugins.push(stored_plugin_summary(&record)?);
        }
        Ok(plugins)
    }

    pub async fn plugin_schema_pages(&self, plugin_id: &PluginId) -> TorbenResult<Vec<SchemaPage>> {
        let pages = if let Some(plugin) = self.bundled_plugin_for_plugin_id(plugin_id) {
            let mut session = plugin.connect().await?;
            let result = session.schema_pages(plugin_id).await;
            finish_plugin_session(result, session.shutdown().await)?
        } else {
            let mut session =
                schema_ui::InstalledSchemaSession::connect(&self.paths, &self.store, plugin_id)
                    .await?;
            let result = session.pages().await;
            finish_plugin_session(result, session.shutdown().await)?
        };
        schema_ui::validate_pages(&pages)?;
        Ok(pages)
    }

    pub async fn invoke_plugin_schema_action(
        &self,
        plugin_id: &PluginId,
        page_id: &str,
        section_id: &str,
        action_id: &str,
        values: BTreeMap<String, String>,
        confirmed: bool,
    ) -> TorbenResult<SchemaActionResult> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let params = SchemaActionParams {
            plugin_id: plugin_id.clone(),
            page_id: page_id.to_owned(),
            section_id: section_id.to_owned(),
            action_id: action_id.to_owned(),
            values,
        };
        if let Some(plugin) = self.bundled_plugin_for_plugin_id(plugin_id) {
            let mut session = plugin.connect().await?;
            let outcome = async {
                let pages = session.schema_pages(plugin_id).await?;
                schema_ui::validate_action_request(&pages, &params, confirmed)?;
                let result = session.schema_action(&params).await?;
                schema_ui::validate_action_result(plugin_id, page_id, &result)?;
                Ok(result)
            }
            .await;
            finish_plugin_session(outcome, session.shutdown().await)
        } else {
            let mut session =
                schema_ui::InstalledSchemaSession::connect(&self.paths, &self.store, plugin_id)
                    .await?;
            let outcome = async {
                let pages = session.pages().await?;
                schema_ui::validate_action_request(&pages, &params, confirmed)?;
                let result = session.action(&params).await?;
                schema_ui::validate_action_result(plugin_id, page_id, &result)?;
                Ok(result)
            }
            .await;
            finish_plugin_session(outcome, session.shutdown().await)
        }
    }

    pub fn official_plugin_registry_status(&self) -> TorbenResult<PluginRegistryStatus> {
        let cache_path = self.paths.official_plugin_registry_cache();
        let registry = if cache_path.is_file() {
            let key = self
                .official_registry_key
                .as_deref()
                .ok_or_else(official_registry_key_unavailable)?;
            let verifier = torben_plugin_host::RegistryVerifier::from_base64(key)?;
            plugin_registry::load(&cache_path, |bytes| verifier.verify_registry_bytes(bytes))?
        } else {
            None
        };
        Ok(plugin_registry::status(
            self.official_registry_key.is_some() && self.official_registry_url.is_some(),
            self.official_registry_url.as_deref(),
            &cache_path,
            registry.as_ref(),
        ))
    }

    pub async fn refresh_official_plugin_registry(&self) -> TorbenResult<PluginRegistryStatus> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let key = self
            .official_registry_key
            .as_deref()
            .ok_or_else(official_registry_key_unavailable)?;
        let source_url = self
            .official_registry_url
            .as_deref()
            .ok_or_else(official_registry_url_unavailable)?;
        let (url, client) = self.official_registry_network(source_url)?;
        let verifier = torben_plugin_host::RegistryVerifier::from_base64(key)?;
        let cache_path = self.paths.official_plugin_registry_cache();
        let registry = plugin_registry::refresh(&client, &url, &cache_path, |bytes| {
            verifier.verify_registry_bytes(bytes)
        })
        .await?;
        Ok(plugin_registry::status(
            true,
            Some(source_url),
            &cache_path,
            Some(&registry),
        ))
    }

    pub fn install_plugin(
        &self,
        manifest_path: &std::path::Path,
        developer_mode: bool,
    ) -> TorbenResult<PluginSummary> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        if !developer_mode {
            return Err(TorbenError::new(
                "developer_mode_required",
                "Direct manifest installation requires explicit developer mode.",
            )
            .with_remediation(
                "Use the official registry install command, or enable developer mode only for a publisher you trust.",
            ));
        }
        let validator = torben_plugin_host::PluginVerifier::developer_mode();
        let source_plugin = validator.verify(manifest_path)?;
        self.install_verified_plugin(manifest_path, source_plugin, PluginOrigin::Sideloaded)
    }

    pub fn install_official_plugin(
        &self,
        registry_path: &Path,
        plugin_id: &PluginId,
        version: Option<&ExactVersion>,
    ) -> TorbenResult<PluginSummary> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let key = self
            .official_registry_key
            .as_deref()
            .ok_or_else(official_registry_key_unavailable)?;
        let verified = torben_plugin_host::RegistryVerifier::from_base64(key)?.verify(
            registry_path,
            plugin_id,
            version,
        )?;
        self.install_verified_plugin(
            &verified.manifest_path,
            verified.plugin,
            PluginOrigin::OfficialRegistry,
        )
    }

    pub async fn install_official_plugin_from_registry(
        &self,
        plugin_id: &PluginId,
        version: Option<&ExactVersion>,
    ) -> TorbenResult<PluginSummary> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let key = self
            .official_registry_key
            .as_deref()
            .ok_or_else(official_registry_key_unavailable)?;
        let source_url = self
            .official_registry_url
            .as_deref()
            .ok_or_else(official_registry_url_unavailable)?;
        let (url, client) = self.official_registry_network(source_url)?;
        let registry_verifier = torben_plugin_host::RegistryVerifier::from_base64(key)?;
        let registry_path = self.paths.official_plugin_registry_cache();
        let registry = plugin_registry::refresh(&client, &url, &registry_path, |bytes| {
            registry_verifier.verify_registry_bytes(bytes)
        })
        .await?;
        let selection = registry_verifier.select_plugin(&registry, plugin_id, version)?;
        let mut journal = OperationJournal::start_plugin(
            &self.paths,
            Arc::clone(&self.store),
            &selection.entry.plugin_id,
            &selection.entry.version,
        )?;
        journal.record(
            OperationState::Running,
            "download",
            format!(
                "Downloading official plugin {} {}",
                selection.entry.plugin_id, selection.entry.version
            ),
            Some(0.1),
        )?;
        let cancellation = journal.cancellation_probe();
        let verified_package = match plugin_registry::download_package(
            &client,
            &url,
            &registry_path,
            &selection,
            &registry_verifier,
            journal.operation_id(),
            &cancellation,
        )
        .await
        {
            Ok(package) => package,
            Err(error) => {
                record_plugin_install_failure(&mut journal, &error)?;
                return Err(error);
            }
        };
        let prepared = match self.prepare_verified_plugin(
            verified_package.manifest_path.as_path(),
            verified_package.plugin,
            PluginOrigin::OfficialRegistry,
        ) {
            Ok(prepared) => prepared,
            Err(error) => {
                record_plugin_install_failure(&mut journal, &error)?;
                return Err(error);
            }
        };
        self.commit_prepared_plugin(prepared, &mut journal)
    }

    fn install_verified_plugin(
        &self,
        manifest_path: &Path,
        source_plugin: torben_plugin_host::VerifiedPlugin,
        origin: PluginOrigin,
    ) -> TorbenResult<PluginSummary> {
        let prepared = self.prepare_verified_plugin(manifest_path, source_plugin, origin)?;
        let mut journal = OperationJournal::start_plugin(
            &self.paths,
            Arc::clone(&self.store),
            &prepared.record.id,
            &prepared.record.version,
        )?;
        self.commit_prepared_plugin(prepared, &mut journal)
    }

    fn prepare_verified_plugin(
        &self,
        manifest_path: &Path,
        source_plugin: torben_plugin_host::VerifiedPlugin,
        origin: PluginOrigin,
    ) -> TorbenResult<PreparedPluginInstall> {
        if manifest_path.file_name().and_then(std::ffi::OsStr::to_str) != Some("plugin.json") {
            return Err(TorbenError::new(
                "plugin_manifest_name_invalid",
                "A plugin package manifest must be named plugin.json.",
            ));
        }
        if matches!(
            source_plugin.manifest.id.as_str(),
            BUNDLED_NODE_PLUGIN_ID
                | BUNDLED_TEMURIN_PLUGIN_ID
                | BUNDLED_PYTHON_PLUGIN_ID
                | BUNDLED_GIT_PLUGIN_ID
                | BUNDLED_VSCODE_PLUGIN_ID
                | BUNDLED_CODEX_PLUGIN_ID
        ) {
            return Err(TorbenError::new(
                "plugin_id_reserved",
                "This plugin identifier is reserved for a bundled Torben App plugin.",
            )
            .with_detail("pluginId", source_plugin.manifest.id.to_string()));
        }
        if let Some(existing) = self
            .store
            .list_plugins()?
            .into_iter()
            .find(|plugin| plugin.id == source_plugin.manifest.id)
        {
            return Err(TorbenError::new(
                "plugin_already_installed",
                "This plugin is already installed.",
            )
            .with_detail("pluginId", existing.id.to_string())
            .with_detail("installedVersion", existing.version.to_string())
            .with_remediation(
                "Disable the installed plugin and wait for the plugin upgrade workflow.",
            ));
        }
        let destination = self
            .paths
            .plugin_dir()
            .join(source_plugin.manifest.id.as_str())
            .join(source_plugin.manifest.version.to_string());
        if destination.exists() {
            return Err(TorbenError::new(
                "plugin_already_installed",
                "This plugin version is already installed.",
            )
            .with_detail("path", destination.display().to_string()));
        }
        let manifest_json = serde_json::to_string(&source_plugin.manifest).map_err(|error| {
            TorbenError::internal("Could not serialize the verified plugin manifest.")
                .with_detail("reason", error.to_string())
        })?;
        let summary = plugin_summary(&source_plugin.manifest, true, origin);
        let record = PluginRecord {
            id: source_plugin.manifest.id,
            version: source_plugin.manifest.version,
            enabled: true,
            manifest_json,
            origin,
        };
        Ok(PreparedPluginInstall {
            manifest_path: manifest_path.to_path_buf(),
            destination,
            record,
            summary,
        })
    }

    fn commit_prepared_plugin(
        &self,
        prepared: PreparedPluginInstall,
        journal: &mut OperationJournal,
    ) -> TorbenResult<PluginSummary> {
        execute_plugin_install_transaction(
            &self.paths,
            &self.store,
            &prepared.manifest_path,
            &prepared.destination,
            &prepared.record,
            journal,
        )?;
        Ok(prepared.summary)
    }

    pub fn set_plugin_enabled(&self, plugin_id: &PluginId, enabled: bool) -> TorbenResult<()> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        if matches!(
            plugin_id.as_str(),
            BUNDLED_NODE_PLUGIN_ID
                | BUNDLED_TEMURIN_PLUGIN_ID
                | BUNDLED_PYTHON_PLUGIN_ID
                | BUNDLED_GIT_PLUGIN_ID
                | BUNDLED_VSCODE_PLUGIN_ID
                | BUNDLED_CODEX_PLUGIN_ID
        ) {
            return Err(TorbenError::new(
                "plugin_built_in_immutable",
                "Bundled plugins cannot be enabled or disabled independently.",
            )
            .with_detail("pluginId", plugin_id.to_string()));
        }
        self.store.set_plugin_enabled(plugin_id, enabled)
    }

    fn ensure_supported_app(app_id: &AppId) -> TorbenResult<()> {
        if matches!(
            app_id.as_str(),
            "node" | "temurin" | "python" | "git" | "vscode" | "codex"
        ) {
            Ok(())
        } else {
            Err(TorbenError::new(
                "capability_not_available",
                "This application is registered for a later milestone.",
            )
            .with_detail("appId", app_id.to_string()))
        }
    }

    fn bundled_plugin(&self, app_id: &AppId) -> TorbenResult<&node_plugin::BundledPlugin> {
        match app_id.as_str() {
            "node" => Ok(&self.node_plugin),
            "temurin" => Ok(&self.temurin_plugin),
            "python" => Ok(&self.python_plugin),
            "git" => Ok(&self.git_plugin),
            "vscode" => Ok(&self.vscode_plugin),
            "codex" => Ok(&self.codex_plugin),
            _ => Err(TorbenError::new(
                "capability_not_available",
                "This application has no bundled provider plugin.",
            )
            .with_detail("appId", app_id.to_string())),
        }
    }

    fn bundled_plugin_for_plugin_id(
        &self,
        plugin_id: &PluginId,
    ) -> Option<&node_plugin::BundledPlugin> {
        match plugin_id.as_str() {
            BUNDLED_NODE_PLUGIN_ID => Some(&self.node_plugin),
            BUNDLED_TEMURIN_PLUGIN_ID => Some(&self.temurin_plugin),
            BUNDLED_PYTHON_PLUGIN_ID => Some(&self.python_plugin),
            BUNDLED_GIT_PLUGIN_ID => Some(&self.git_plugin),
            BUNDLED_VSCODE_PLUGIN_ID => Some(&self.vscode_plugin),
            BUNDLED_CODEX_PLUGIN_ID => Some(&self.codex_plugin),
            _ => None,
        }
    }

    fn official_registry_network(
        &self,
        source_url: &str,
    ) -> TorbenResult<(url::Url, reqwest::Client)> {
        #[cfg(not(test))]
        let _ = self;
        #[cfg(test)]
        if self.official_registry_fixture_mode {
            return Ok((
                plugin_registry::fixture_url(source_url)?,
                plugin_registry::fixture_client()?,
            ));
        }
        Ok((
            plugin_registry::official_url(source_url)?,
            plugin_registry::official_client()?,
        ))
    }

    async fn plugin_health_check(&self, record: &InstallRecord) -> TorbenResult<()> {
        let mut plugin = self.bundled_plugin(&record.app_id)?.connect().await?;
        let result = plugin.health_check(record).await;
        let shutdown = plugin.shutdown().await;
        result?;
        shutdown
    }

    async fn plugin_uninstall_plan(
        &self,
        operation_id: OperationId,
        record: &InstallRecord,
    ) -> TorbenResult<UninstallPlan> {
        let mut plugin = self.bundled_plugin(&record.app_id)?.connect().await?;
        let result = plugin.uninstall_plan(operation_id, record).await;
        let shutdown = plugin.shutdown().await;
        let plan = result?;
        shutdown?;
        Ok(plan)
    }

    async fn plugin_uninstall_plan_with_events(
        &self,
        record: &InstallRecord,
        journal: &mut OperationJournal,
    ) -> TorbenResult<UninstallPlan> {
        let mut plugin = self.bundled_plugin(&record.app_id)?.connect().await?;
        let operation_id = journal.operation_id();
        let result = plugin
            .uninstall_plan_with_events(operation_id, record, |event| {
                journal.record(
                    OperationState::Running,
                    format!("plugin.{}", event.phase),
                    event.message,
                    event.progress.map(|progress| 0.05 + progress * 0.14),
                )
            })
            .await;
        let shutdown = plugin.shutdown().await;
        let plan = result?;
        shutdown?;
        Ok(plan)
    }
}

#[cfg(feature = "test-fixtures")]
fn invalid_node_fixture_configuration(field: &str, reason: impl Into<String>) -> TorbenError {
    TorbenError::new(
        "test_fixture_configuration_invalid",
        "The Node.js CLI test fixture configuration is invalid.",
    )
    .with_detail("field", field)
    .with_detail("reason", reason.into())
}

fn acknowledge_cancellation_error(
    journal: &mut OperationJournal,
    error: &TorbenError,
) -> TorbenResult<()> {
    if error.code == "operation_cancelled" {
        journal.acknowledge_cancellation()?;
    }
    Ok(())
}

fn install_rollback_pending(error: &TorbenError, cleanup_error: &TorbenError) -> TorbenError {
    TorbenError::new(
        "install_rollback_pending",
        "Installation failed and filesystem rollback is incomplete.",
    )
    .with_detail("installErrorCode", &error.code)
    .with_detail("installError", &error.message)
    .with_detail("cleanupErrorCode", &cleanup_error.code)
    .with_detail("cleanupError", &cleanup_error.message)
    .with_remediation("Restart Torben App to resume recovery before retrying the installation.")
}

fn finish_plugin_session<T>(
    result: TorbenResult<T>,
    shutdown: TorbenResult<()>,
) -> TorbenResult<T> {
    match result {
        Err(error) => Err(error),
        Ok(value) => shutdown.map(|()| value),
    }
}

fn official_registry_key_unavailable() -> TorbenError {
    TorbenError::new(
        "official_registry_key_unavailable",
        "This Torben App build has no official plugin registry trust root.",
    )
    .with_remediation(
        "Use a release build configured with the official registry key, or use explicit developer-mode sideloading.",
    )
}

fn official_registry_url_unavailable() -> TorbenError {
    TorbenError::new(
        "official_registry_url_unavailable",
        "This Torben App build has no official plugin registry URL.",
    )
    .with_remediation(
        "Use a release build configured with both the official registry URL and trust root.",
    )
}

fn bundled_plugin_summary(
    manifest_json: &str,
    expected_id: &str,
    display_name: &str,
) -> TorbenResult<PluginSummary> {
    let manifest: PluginManifest = serde_json::from_str(manifest_json).map_err(|error| {
        TorbenError::internal("A bundled plugin manifest is invalid.")
            .with_detail("plugin", display_name)
            .with_detail("reason", error.to_string())
    })?;
    if manifest.id.as_str() != expected_id {
        return Err(
            TorbenError::internal("A bundled plugin manifest identifier is invalid.")
                .with_detail("plugin", display_name)
                .with_detail("pluginId", manifest.id.to_string()),
        );
    }
    Ok(plugin_summary(&manifest, true, PluginOrigin::BuiltIn))
}

fn stored_plugin_summary(record: &PluginRecord) -> TorbenResult<PluginSummary> {
    let manifest: PluginManifest =
        serde_json::from_str(&record.manifest_json).map_err(|error| {
            TorbenError::new(
                "plugin_manifest_state_invalid",
                "A stored plugin manifest is invalid.",
            )
            .with_detail("pluginId", record.id.to_string())
            .with_detail("reason", error.to_string())
            .with_remediation("Reinstall the plugin from a trusted package.")
        })?;
    if manifest.id != record.id || manifest.version != record.version {
        return Err(TorbenError::new(
            "plugin_manifest_state_mismatch",
            "A stored plugin record does not match its verified manifest.",
        )
        .with_detail("pluginId", record.id.to_string())
        .with_remediation("Reinstall the plugin from a trusted package."));
    }
    Ok(plugin_summary(&manifest, record.enabled, record.origin))
}

fn plugin_summary(manifest: &PluginManifest, enabled: bool, origin: PluginOrigin) -> PluginSummary {
    PluginSummary {
        id: manifest.id.clone(),
        display_name: manifest.display_name.clone(),
        version: manifest.version.clone(),
        enabled,
        origin,
        publisher: manifest.publisher.clone(),
        capabilities: manifest.capabilities.clone(),
        permissions: manifest.permissions.clone(),
    }
}

fn validate_uninstall_plan(record: &InstallRecord, plan: &UninstallPlan) -> TorbenResult<()> {
    if plan.app_id != record.app_id
        || plan.version != record.version
        || plan.source_id != record.source_id
        || plan.install_path != record.install_path
        || !plan.preserve_user_data
    {
        return Err(TorbenError::new(
            "plugin_uninstall_plan_invalid",
            "The plugin returned an unsafe or inconsistent uninstall plan.",
        )
        .with_detail("appId", plan.app_id.to_string())
        .with_detail("version", plan.version.to_string())
        .with_detail("sourceId", plan.source_id.to_string())
        .with_detail("installPath", &plan.install_path)
        .with_detail("preserveUserData", plan.preserve_user_data.to_string()));
    }
    Ok(())
}

fn validate_selected_installation(
    paths: &TorbenPaths,
    record: &InstallRecord,
) -> TorbenResult<PathBuf> {
    let expected = paths.app_version_dir(record.app_id.as_str(), &record.version.to_string());
    let actual = PathBuf::from(&record.install_path);
    if record.scope != InstallScope::Managed || actual != expected {
        return Err(TorbenError::new(
            "selection_state_invalid",
            "The selected installation is not a standard managed installation.",
        )
        .with_detail("appId", record.app_id.to_string())
        .with_detail("version", record.version.to_string())
        .with_detail("expectedPath", expected.display().to_string())
        .with_detail("actualPath", actual.display().to_string())
        .with_detail("scope", format!("{:?}", record.scope).to_ascii_lowercase())
        .with_remediation("Reinstall the version from the Torben application catalog."));
    }
    match actual.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(actual),
        Ok(_) => Err(TorbenError::new(
            "selection_state_invalid",
            "The selected managed installation path is not a plain directory.",
        )
        .with_detail("path", actual.display().to_string())
        .with_remediation("Reinstall the selected managed version before using its commands.")),
        Err(error) => Err(TorbenError::new(
            "selection_state_invalid",
            "The selected managed installation directory is unavailable.",
        )
        .with_detail("path", actual.display().to_string())
        .with_detail("reason", error.to_string())
        .with_remediation("Reinstall the selected managed version before using its commands.")),
    }
}

fn validate_selected_command_path(
    install_path: &Path,
    command_path: &Path,
) -> TorbenResult<PathBuf> {
    let canonical_install = std::fs::canonicalize(install_path).map_err(|error| {
        TorbenError::new(
            "selection_state_invalid",
            "The selected managed installation directory cannot be resolved.",
        )
        .with_detail("path", install_path.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    let canonical_command = std::fs::canonicalize(command_path).map_err(|error| {
        TorbenError::new(
            "managed_command_missing",
            "The selected managed command cannot be resolved.",
        )
        .with_detail("path", command_path.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    if !canonical_command.starts_with(&canonical_install) || canonical_command == canonical_install
    {
        return Err(TorbenError::new(
            "managed_command_outside_installation",
            "The selected command resolves outside its managed installation.",
        )
        .with_detail("installationPath", install_path.display().to_string())
        .with_detail("commandPath", command_path.display().to_string())
        .with_detail("resolvedPath", canonical_command.display().to_string())
        .with_remediation("Reinstall the selected managed version from its official archive."));
    }
    Ok(command_path.to_path_buf())
}

fn execute_uninstall_transaction(
    paths: &TorbenPaths,
    store: &StateStore,
    record: &InstallRecord,
    source: &Path,
    staged: &Path,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    if let Err(error) = validate_managed_uninstall_record(record, source)
        .and_then(|()| {
            validate_managed_uninstall_identity(
                paths,
                journal,
                &record.app_id,
                &record.version,
                source,
                staged,
            )
        })
        .and_then(|()| ensure_managed_uninstall_receipt_absent(paths, journal.operation_id()))
        .and_then(|()| validate_uninstall_stage_paths(source, staged))
    {
        journal.fail_and_rollback(&error)?;
        return Err(error);
    }
    if let Err(io_error) = std::fs::rename(source, staged) {
        let error = TorbenError::new(
            "uninstall_stage_failed",
            "Could not stage the installation for removal.",
        )
        .with_detail("reason", io_error.to_string());
        journal.fail_and_rollback(&error)?;
        return Err(error);
    }
    if let Err(receipt_error) = write_managed_uninstall_receipt(
        paths,
        journal,
        &record.app_id,
        &record.version,
        source,
        staged,
    ) {
        let pending = uninstall_rollback_pending(
            "The installation was staged, but its ownership receipt could not be persisted.",
            &receipt_error,
        );
        journal.fail(&pending)?;
        return Err(pending);
    }
    if let Err(error) = store.remove_installation(&record.app_id, &record.version) {
        match restore_managed_uninstall_staging(
            paths,
            journal,
            &record.app_id,
            &record.version,
            source,
            staged,
        ) {
            Ok(()) => {
                if let Err(receipt_error) =
                    remove_managed_uninstall_receipt_if_present(paths, journal.operation_id())
                {
                    let pending = uninstall_rollback_pending(
                        "Uninstall state failed and receipt cleanup is incomplete.",
                        &receipt_error,
                    );
                    journal.fail(&pending)?;
                    return Err(pending);
                }
                journal.fail_and_rollback(&error)?;
                return Err(error);
            }
            Err(restore_error) => {
                let pending = TorbenError::new(
                    "uninstall_rollback_pending",
                    "Uninstall state failed and the staged installation could not be restored.",
                )
                .with_detail("stateErrorCode", &error.code)
                .with_detail("stateError", &error.message)
                .with_detail("restoreErrorCode", &restore_error.code)
                .with_detail("restoreError", &restore_error.message)
                .with_remediation(
                    "Restart Torben App to resume recovery before retrying the uninstall.",
                );
                journal.fail(&pending)?;
                return Err(pending);
            }
        }
    }
    if let Err(cleanup_error) = remove_receipt_bound_uninstall_staging(
        paths,
        journal,
        &record.app_id,
        &record.version,
        source,
        staged,
    )
    .and_then(|()| remove_managed_uninstall_receipt_if_present(paths, journal.operation_id()))
    {
        let error = uninstall_cleanup_pending(&cleanup_error);
        journal.fail(&error)?;
        return Err(error);
    }
    journal.succeed("Uninstall committed")
}

fn install_shims_locked(paths: &TorbenPaths, shim_binary: &Path) -> TorbenResult<Vec<PathBuf>> {
    let destinations = validate_shim_install_inputs(paths, shim_binary)?;
    if shims_match_source(paths, shim_binary)? {
        return Ok(destinations);
    }

    let staging = paths
        .staging_dir()
        .join(format!("shims-{}", torben_contracts::OperationId::new()));
    let backup = staging.join("backup");
    std::fs::create_dir_all(&backup).map_err(|error| {
        TorbenError::new("shim_stage_failed", "Could not create shim staging.")
            .with_detail("path", staging.display().to_string())
            .with_detail("reason", error.to_string())
    })?;

    let result = stage_and_commit_shims(shim_binary, &destinations, &staging, &backup);
    if result.is_ok() {
        let _ = cleanup_shim_staging(paths);
    }
    result
}

fn validate_shim_install_inputs(
    paths: &TorbenPaths,
    shim_binary: &Path,
) -> TorbenResult<Vec<PathBuf>> {
    let source_metadata = std::fs::symlink_metadata(shim_binary).map_err(|error| {
        TorbenError::new("shim_binary_missing", "The shim binary does not exist.")
            .with_detail("path", shim_binary.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    if !source_metadata.is_file() || source_metadata.file_type().is_symlink() {
        return Err(TorbenError::new(
            "shim_binary_invalid",
            "The shim source must be a regular file.",
        )
        .with_detail("path", shim_binary.display().to_string()));
    }

    let destinations = shim_destinations(paths);
    for destination in &destinations {
        match std::fs::symlink_metadata(destination) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(TorbenError::new(
                    "shim_destination_conflict",
                    "A command shim destination is not a regular file.",
                )
                .with_detail("path", destination.display().to_string()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(TorbenError::new(
                    "shim_destination_inspect_failed",
                    "Could not inspect a command shim destination.",
                )
                .with_detail("path", destination.display().to_string())
                .with_detail("reason", error.to_string()));
            }
        }
    }

    Ok(destinations)
}

fn install_selection_shims_locked(
    paths: &TorbenPaths,
    shim_binary: &Path,
    journal: &OperationJournal,
) -> TorbenResult<Vec<PathBuf>> {
    let destinations = validate_shim_install_inputs(paths, shim_binary)?;
    if shims_match_source(paths, shim_binary)? {
        return Ok(destinations);
    }
    let staging = selection_shim_staging_path(paths, journal.operation_id());
    ensure_selection_shim_artifacts_absent(paths, journal.operation_id(), &staging)?;
    let backup = staging.join("backup");
    std::fs::create_dir_all(&backup).map_err(|error| {
        TorbenError::new(
            "shim_stage_failed",
            "Could not create selection shim staging.",
        )
        .with_detail("path", staging.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    let source_sha256 = match stage_shim_copies(shim_binary, &destinations, &staging) {
        Ok(hash) => hash,
        Err(error) => return Err(cleanup_failed_selection_shim_stage(&staging, error)),
    };
    if let Err(error) = write_selection_shim_receipt(
        paths,
        journal,
        &staging,
        &backup,
        &source_sha256,
        &destinations,
    ) {
        return Err(cleanup_failed_selection_shim_receipt(
            paths,
            journal.operation_id(),
            &staging,
            error,
        ));
    }
    match commit_staged_shims(&destinations, &staging, &backup) {
        Ok(installed) => match complete_selection_shim_transaction(paths, journal) {
            Ok(()) => Ok(installed),
            Err(cleanup_error) => Err(selection_shim_rollback_pending(
                &TorbenError::new(
                    "selection_shim_commit_incomplete",
                    "Command shims committed, but transaction cleanup is incomplete.",
                ),
                &cleanup_error,
            )),
        },
        Err(error) => match rollback_selection_shim_transaction(paths, journal) {
            Ok(()) => Err(error),
            Err(rollback_error) => Err(selection_shim_rollback_pending(&error, &rollback_error)),
        },
    }
}

fn stage_and_commit_shims(
    shim_binary: &Path,
    destinations: &[PathBuf],
    staging: &Path,
    backup: &Path,
) -> TorbenResult<Vec<PathBuf>> {
    stage_shim_copies(shim_binary, destinations, staging)?;
    commit_staged_shims(destinations, staging, backup)
}

fn stage_shim_copies(
    shim_binary: &Path,
    destinations: &[PathBuf],
    staging: &Path,
) -> TorbenResult<String> {
    let source_hash = sha256_path(shim_binary)?;
    for destination in destinations {
        let filename = destination.file_name().ok_or_else(|| {
            TorbenError::new(
                "shim_destination_invalid",
                "A command shim destination has no file name.",
            )
        })?;
        let staged_path = staging.join(filename);
        std::fs::copy(shim_binary, &staged_path).map_err(|error| {
            TorbenError::new("shim_copy_failed", "Could not stage a command shim.")
                .with_detail("path", staged_path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
        #[cfg(unix)]
        make_executable(&staged_path)?;
        if sha256_path(&staged_path)? != source_hash {
            return Err(TorbenError::new(
                "shim_copy_mismatch",
                "A staged command shim does not match its bundled source.",
            )
            .with_detail("path", staged_path.display().to_string()));
        }
    }
    Ok(hex::encode(source_hash))
}

fn commit_staged_shims(
    destinations: &[PathBuf],
    staging: &Path,
    backup: &Path,
) -> TorbenResult<Vec<PathBuf>> {
    let mut installed = Vec::new();
    let mut backups = Vec::new();
    for destination in destinations {
        let filename = destination.file_name().ok_or_else(|| {
            TorbenError::new(
                "shim_destination_invalid",
                "A command shim destination has no file name.",
            )
        })?;
        let staged_path = staging.join(filename);
        if destination.exists() {
            let backup_path = backup.join(filename);
            if let Err(error) = std::fs::rename(destination, &backup_path) {
                let rollback_complete = rollback_shims(&installed, &backups);
                return Err(TorbenError::new(
                    "shim_backup_failed",
                    "Could not stage an existing command shim for replacement.",
                )
                .with_detail("path", destination.display().to_string())
                .with_detail("reason", error.to_string())
                .with_detail("rollbackComplete", rollback_complete.to_string()));
            }
            backups.push((destination.clone(), backup_path));
        }
        if let Err(error) = std::fs::rename(staged_path, destination) {
            let rollback_complete = rollback_shims(&installed, &backups);
            return Err(TorbenError::new(
                "shim_commit_failed",
                "Could not commit all command shims.",
            )
            .with_detail("path", destination.display().to_string())
            .with_detail("reason", error.to_string())
            .with_detail("rollbackComplete", rollback_complete.to_string()));
        }
        installed.push(destination.clone());
    }
    Ok(installed)
}

fn rollback_shims(installed: &[PathBuf], backups: &[(PathBuf, PathBuf)]) -> bool {
    let mut complete = true;
    for destination in installed.iter().rev() {
        if destination.exists() && std::fs::remove_file(destination).is_err() {
            complete = false;
        }
    }
    for (destination, backup) in backups.iter().rev() {
        if backup.exists() && std::fs::rename(backup, destination).is_err() {
            complete = false;
        }
    }
    complete
}

fn selection_shim_staging_path(paths: &TorbenPaths, operation_id: OperationId) -> PathBuf {
    paths.staging_dir().join(format!("shims-{operation_id}"))
}

fn selection_shim_receipt_path(paths: &TorbenPaths, operation_id: OperationId) -> PathBuf {
    paths
        .operation_dir()
        .join(format!("{operation_id}.selection-shims.receipt"))
}

fn selection_shim_artifact_presence(
    paths: &TorbenPaths,
    operation_id: OperationId,
) -> TorbenResult<(bool, bool)> {
    let inspect = |path: &Path| match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(selection_shim_receipt_error(
            "Could not inspect a selection shim recovery artifact.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    };
    Ok((
        inspect(&selection_shim_staging_path(paths, operation_id))?,
        inspect(&selection_shim_receipt_path(paths, operation_id))?,
    ))
}

fn ensure_selection_shim_artifacts_absent(
    paths: &TorbenPaths,
    operation_id: OperationId,
    staging: &Path,
) -> TorbenResult<()> {
    for path in [
        staging.to_path_buf(),
        selection_shim_receipt_path(paths, operation_id),
    ] {
        match path.symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(selection_shim_receipt_error(
                    "A selection shim transaction artifact already exists.",
                )
                .with_detail("path", path.display().to_string()));
            }
            Err(error) => {
                return Err(selection_shim_receipt_error(
                    "Could not inspect a selection shim transaction artifact.",
                )
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string()));
            }
        }
    }
    Ok(())
}

fn selection_shim_receipt(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    source_sha256: String,
) -> TorbenResult<SelectionShimReceipt> {
    let app_id = journal.app_id().cloned().ok_or_else(|| {
        selection_shim_receipt_error("The selection journal has no application identifier.")
    })?;
    let version = journal.version().cloned().ok_or_else(|| {
        selection_shim_receipt_error("A clear-selection journal cannot own shim replacement.")
    })?;
    if journal.kind() != OperationKind::Select {
        return Err(selection_shim_receipt_error(
            "The shim receipt does not belong to a selection operation.",
        ));
    }
    let staging_path = selection_shim_staging_path(paths, journal.operation_id());
    Ok(SelectionShimReceipt {
        schema_version: SELECTION_SHIM_RECEIPT_SCHEMA_VERSION,
        operation_id: journal.operation_id(),
        app_id,
        version,
        backup_path: staging_path.join("backup"),
        staging_path,
        source_sha256,
        destinations: shim_destinations(paths),
    })
}

fn write_selection_shim_receipt(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    staging: &Path,
    backup: &Path,
    source_sha256: &str,
    destinations: &[PathBuf],
) -> TorbenResult<()> {
    let expected = selection_shim_receipt(paths, journal, source_sha256.to_owned())?;
    if expected.staging_path != staging
        || expected.backup_path != backup
        || expected.destinations != destinations
    {
        return Err(selection_shim_receipt_error(
            "The staged shim paths do not match the selection transaction.",
        ));
    }
    validate_sha256_text(source_sha256)?;
    ensure_plain_directory(staging, "selection shim staging")?;
    ensure_plain_directory(backup, "selection shim backup")?;
    for destination in destinations {
        let filename = destination
            .file_name()
            .ok_or_else(|| selection_shim_receipt_error("A shim destination has no file name."))?;
        let staged_path = staging.join(filename);
        ensure_regular_file_path(&staged_path, "staged selection shim")?;
        if hex::encode(sha256_path(&staged_path)?) != source_sha256 {
            return Err(selection_shim_receipt_error(
                "A staged selection shim does not match the receipt hash.",
            )
            .with_detail("path", staged_path.display().to_string()));
        }
    }
    let content = serde_json::to_vec(&expected).map_err(|error| {
        selection_shim_receipt_error("Could not serialize the selection shim receipt.")
            .with_detail("reason", error.to_string())
    })?;
    let path = selection_shim_receipt_path(paths, journal.operation_id());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            selection_shim_receipt_error("Could not create the selection shim receipt.")
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
    file.write_all(&content).map_err(|error| {
        selection_shim_receipt_error("Could not write the selection shim receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    file.sync_all().map_err(|error| {
        selection_shim_receipt_error("Could not sync the selection shim receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })
}

fn read_selection_shim_receipt(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    require_staging: bool,
) -> TorbenResult<SelectionShimReceipt> {
    let path = selection_shim_receipt_path(paths, journal.operation_id());
    let metadata = path.symlink_metadata().map_err(|error| {
        selection_shim_receipt_error("The selection shim receipt is unavailable.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > SELECTION_SHIM_RECEIPT_MAX_BYTES
    {
        return Err(selection_shim_receipt_error(
            "The selection shim receipt is not a bounded regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    let content = std::fs::read(&path).map_err(|error| {
        selection_shim_receipt_error("Could not read the selection shim receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let actual: SelectionShimReceipt = serde_json::from_slice(&content).map_err(|error| {
        selection_shim_receipt_error("The selection shim receipt is invalid.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    validate_sha256_text(&actual.source_sha256)?;
    let expected = selection_shim_receipt(paths, journal, actual.source_sha256.clone())?;
    if actual != expected {
        return Err(selection_shim_receipt_error(
            "The selection journal does not match its shim receipt.",
        ));
    }
    if require_staging {
        ensure_plain_directory(&actual.staging_path, "selection shim staging")?;
        ensure_plain_directory(&actual.backup_path, "selection shim backup")?;
    }
    Ok(actual)
}

fn validate_sha256_text(value: &str) -> TorbenResult<()> {
    if value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(selection_shim_receipt_error(
            "The selection shim receipt contains an invalid SHA-256 value.",
        ))
    }
}

fn ensure_plain_directory(path: &Path, description: &str) -> TorbenResult<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(selection_shim_receipt_error(
            "A selection shim transaction path is not a plain directory.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())),
        Err(error) => Err(selection_shim_receipt_error(
            "A selection shim transaction directory is unavailable.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn ensure_regular_file_path(path: &Path, description: &str) -> TorbenResult<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(selection_shim_receipt_error(
            "A selection shim transaction path is not a regular file.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())),
        Err(error) => Err(selection_shim_receipt_error(
            "A selection shim transaction file is unavailable.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn optional_regular_file(path: &Path, description: &str) -> TorbenResult<bool> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(selection_shim_receipt_error(
            "A selection shim recovery path is not a regular file.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(selection_shim_receipt_error(
            "Could not inspect a selection shim recovery path.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn complete_selection_shim_transaction(
    paths: &TorbenPaths,
    journal: &OperationJournal,
) -> TorbenResult<()> {
    let receipt = read_selection_shim_receipt(paths, journal, true)?;
    validate_committed_selection_shim_destinations(&receipt)?;
    remove_selection_shim_artifacts(paths, &receipt)
}

fn validate_committed_selection_shim_destinations(
    receipt: &SelectionShimReceipt,
) -> TorbenResult<()> {
    for destination in &receipt.destinations {
        ensure_regular_file_path(destination, "committed command shim")?;
        if hex::encode(sha256_path(destination)?) != receipt.source_sha256 {
            return Err(selection_shim_receipt_error(
                "A committed command shim does not match its transaction receipt.",
            )
            .with_detail("path", destination.display().to_string()));
        }
    }
    Ok(())
}

fn finish_selection_shim_receipt_only(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    selection_committed: bool,
) -> TorbenResult<()> {
    let receipt = read_selection_shim_receipt(paths, journal, false)?;
    if selection_committed {
        validate_committed_selection_shim_destinations(&receipt)?;
    }
    remove_selection_shim_receipt_if_present(paths, receipt.operation_id)
}

fn rollback_selection_shim_transaction(
    paths: &TorbenPaths,
    journal: &OperationJournal,
) -> TorbenResult<()> {
    let receipt = read_selection_shim_receipt(paths, journal, true)?;
    for destination in &receipt.destinations {
        rollback_selection_shim_destination(&receipt, destination)?;
    }
    remove_selection_shim_artifacts(paths, &receipt)
}

fn rollback_selection_shim_destination(
    receipt: &SelectionShimReceipt,
    destination: &Path,
) -> TorbenResult<()> {
    let filename = destination
        .file_name()
        .ok_or_else(|| selection_shim_receipt_error("A shim destination has no file name."))?;
    let staged = receipt.staging_path.join(filename);
    let backup = receipt.backup_path.join(filename);
    let staged_present = optional_regular_file(&staged, "staged selection shim")?;
    let backup_present = optional_regular_file(&backup, "backed-up command shim")?;
    let destination_present = optional_regular_file(destination, "command shim destination")?;

    if backup_present {
        if staged_present && destination_present {
            return Err(selection_shim_receipt_error(
                "A command shim destination conflicts with both staged and backup files.",
            )
            .with_detail("path", destination.display().to_string()));
        }
        if destination_present {
            ensure_transaction_shim_hash(destination, &receipt.source_sha256)?;
            std::fs::remove_file(destination).map_err(|error| {
                selection_shim_receipt_error("Could not remove a partially committed command shim.")
                    .with_detail("path", destination.display().to_string())
                    .with_detail("reason", error.to_string())
            })?;
        }
        std::fs::rename(&backup, destination).map_err(|error| {
            selection_shim_receipt_error("Could not restore a backed-up command shim.")
                .with_detail("path", destination.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
    } else if !staged_present && destination_present {
        ensure_transaction_shim_hash(destination, &receipt.source_sha256)?;
        std::fs::remove_file(destination).map_err(|error| {
            selection_shim_receipt_error("Could not remove a newly committed command shim.")
                .with_detail("path", destination.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
    }
    Ok(())
}

fn ensure_transaction_shim_hash(path: &Path, expected: &str) -> TorbenResult<()> {
    if hex::encode(sha256_path(path)?) == expected {
        Ok(())
    } else {
        Err(selection_shim_receipt_error(
            "A command shim changed after the selection transaction began.",
        )
        .with_detail("path", path.display().to_string()))
    }
}

fn remove_selection_shim_artifacts(
    paths: &TorbenPaths,
    receipt: &SelectionShimReceipt,
) -> TorbenResult<()> {
    std::fs::remove_dir_all(&receipt.staging_path).map_err(|error| {
        selection_shim_receipt_error("Could not remove selection shim staging.")
            .with_detail("path", receipt.staging_path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    remove_selection_shim_receipt_if_present(paths, receipt.operation_id)
}

fn remove_selection_shim_receipt_if_present(
    paths: &TorbenPaths,
    operation_id: OperationId,
) -> TorbenResult<()> {
    let path = selection_shim_receipt_path(paths, operation_id);
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(&path).map_err(|error| {
                selection_shim_receipt_error("Could not remove the selection shim receipt.")
                    .with_detail("path", path.display().to_string())
                    .with_detail("reason", error.to_string())
            })
        }
        Ok(_) => Err(selection_shim_receipt_error(
            "The selection shim receipt is not a regular file.",
        )
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(selection_shim_receipt_error(
            "Could not inspect the selection shim receipt.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn cleanup_failed_selection_shim_stage(staging: &Path, error: TorbenError) -> TorbenError {
    match staging.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            if std::fs::remove_dir_all(staging).is_ok() {
                error
            } else {
                selection_shim_rollback_pending(
                    &error,
                    &selection_shim_receipt_error(
                        "Could not remove incomplete selection shim staging.",
                    ),
                )
            }
        }
        Err(io_error) if io_error.kind() == std::io::ErrorKind::NotFound => error,
        _ => selection_shim_rollback_pending(
            &error,
            &selection_shim_receipt_error(
                "Incomplete selection shim staging is not a removable plain directory.",
            ),
        ),
    }
}

fn cleanup_failed_selection_shim_receipt(
    paths: &TorbenPaths,
    operation_id: OperationId,
    staging: &Path,
    error: TorbenError,
) -> TorbenError {
    let staging_cleanup = match staging.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(staging).map_err(|cleanup| cleanup.to_string())
        }
        Err(io_error) if io_error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(_) => Err("staging is not a plain directory".to_owned()),
        Err(io_error) => Err(io_error.to_string()),
    };
    let receipt_cleanup = remove_selection_shim_receipt_if_present(paths, operation_id);
    if staging_cleanup.is_ok() && receipt_cleanup.is_ok() {
        error
    } else {
        let cleanup = selection_shim_receipt_error(
            "Could not remove incomplete selection shim receipt artifacts.",
        )
        .with_detail(
            "stagingCleanup",
            staging_cleanup
                .err()
                .unwrap_or_else(|| "complete".to_owned()),
        )
        .with_detail(
            "receiptCleanup",
            receipt_cleanup
                .err()
                .map_or_else(|| "complete".to_owned(), |failure| failure.message),
        );
        selection_shim_rollback_pending(&error, &cleanup)
    }
}

fn selection_shim_rollback_pending(error: &TorbenError, rollback: &TorbenError) -> TorbenError {
    TorbenError::new(
        "selection_shim_rollback_pending",
        "Command shim replacement failed and rollback is incomplete.",
    )
    .with_detail("shimErrorCode", &error.code)
    .with_detail("shimError", &error.message)
    .with_detail("rollbackErrorCode", &rollback.code)
    .with_detail("rollbackError", &rollback.message)
    .with_remediation("Restart Torben App to resume selection shim recovery.")
}

fn selection_shim_receipt_error(message: &str) -> TorbenError {
    TorbenError::new("selection_shim_ownership_receipt_invalid", message)
        .with_remediation("Inspect the command shim staging and destinations before retrying.")
}

fn cleanup_shim_staging(paths: &TorbenPaths) -> TorbenResult<()> {
    for entry in std::fs::read_dir(paths.staging_dir()).map_err(shim_io_error)? {
        let entry = entry.map_err(shim_io_error)?;
        let path = entry.path();
        let is_shim_staging = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("shims-"));
        let is_regular_directory = std::fs::symlink_metadata(&path)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
        if is_shim_staging && is_regular_directory {
            std::fs::remove_dir_all(path).map_err(shim_io_error)?;
        }
    }
    Ok(())
}

fn shims_match_source(paths: &TorbenPaths, shim_binary: &Path) -> TorbenResult<bool> {
    let source_hash = sha256_path(shim_binary)?;
    for destination in shim_destinations(paths) {
        let metadata = match std::fs::symlink_metadata(&destination) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(shim_io_error(error)),
        };
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || sha256_path(&destination)? != source_hash
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn shim_destinations(paths: &TorbenPaths) -> Vec<PathBuf> {
    [
        "node", "npm", "npx", "java", "javac", "python", "python3", "pip", "pip3", "git", "code",
        "codex",
    ]
    .into_iter()
    .map(|command| {
        let filename = if cfg!(windows) {
            format!("{command}.exe")
        } else {
            command.to_owned()
        };
        paths.shim_dir().join(filename)
    })
    .collect()
}

fn sha256_path(path: &Path) -> TorbenResult<[u8; 32]> {
    let mut file = File::open(path).map_err(shim_io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(shim_io_error)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> TorbenResult<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)
        .map_err(shim_io_error)?
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).map_err(shim_io_error)?;
    Ok(())
}

fn shim_io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new("shim_io_failed", "Could not access command shim files.")
        .with_detail("reason", error.to_string())
}

fn validate_source_owner_matches_request(
    owner: &PackageInstallationRecord,
    request: &SourceExecutionRequest,
) -> TorbenResult<()> {
    let matches = owner.app_id == request.app_id
        && owner.app_version == request.app_version
        && owner.adapter == request.adapter
        && owner.coordinate == request.coordinate
        && owner.package_kind == request.package_kind
        && request
            .package_version
            .as_ref()
            .is_none_or(|version| version == &owner.package_version);
    if matches {
        Ok(())
    } else {
        Err(TorbenError::new(
            "package_source_owner_mismatch",
            "The uninstall request does not match Torben's immutable package source owner.",
        )
        .with_detail("ownedAdapter", owner.adapter.to_string())
        .with_detail("ownedCoordinate", owner.coordinate.to_string())
        .with_detail("ownedPackageKind", owner.package_kind.to_string())
        .with_detail("ownedPackageVersion", owner.package_version.to_string()))
    }
}

fn cleanup_plan_for_target(
    service: &source_adapters::SourceAdapterService,
    install: &SourceOperationPlan,
) -> TorbenResult<SourceOperationPlan> {
    let mut cleanup = service.plan(
        SourceAction::Uninstall,
        install.adapter,
        install.coordinate.clone(),
        install.package_kind,
        install.package_version.clone(),
    )?;
    if install.adapter == SourceAdapterKind::Dnf {
        let identity = install.execution_identity.clone().ok_or_else(|| {
            TorbenError::new(
                "source_plan_approval_required",
                "The DNF migration target has no locked execution identity.",
            )
        })?;
        if let Some(argument) = cleanup.preview_arguments.last_mut() {
            argument.clone_from(&identity);
        }
        if let Some(argument) = cleanup.execute_arguments.last_mut() {
            argument.clone_from(&identity);
        }
        cleanup.execution_identity = Some(identity);
        cleanup.warnings.push(
            "Compensation removes only the same full NEVRA approved for target installation."
                .to_owned(),
        );
    }
    Ok(cleanup)
}

fn source_migration_token(plan: &SourceMigrationPlan) -> TorbenResult<String> {
    let mut approved = plan.clone();
    approved.approval_token.clear();
    let encoded = serde_json::to_vec(&approved).map_err(|error| {
        TorbenError::internal("The source migration plan could not be serialized.")
            .with_detail("reason", error.to_string())
    })?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

fn managed_to_package_token(plan: &ManagedToPackageMigrationPlan) -> TorbenResult<String> {
    let mut approved = plan.clone();
    approved.approval_token.clear();
    let encoded = serde_json::to_vec(&approved).map_err(|error| {
        TorbenError::internal("The managed-to-package migration plan could not be serialized.")
            .with_detail("reason", error.to_string())
    })?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

fn package_to_managed_token(plan: &PackageToManagedMigrationPlan) -> TorbenResult<String> {
    let mut approved = plan.clone();
    approved.approval_token.clear();
    let encoded = serde_json::to_vec(&approved).map_err(|error| {
        TorbenError::internal("The package-to-managed migration plan could not be serialized.")
            .with_detail("reason", error.to_string())
    })?;
    Ok(hex::encode(Sha256::digest(encoded)))
}

fn validate_managed_to_package_approval(request: &SourceMigrationRequest) -> TorbenResult<()> {
    if !request.accept_system_changes {
        return Err(TorbenError::new(
            "source_migration_confirmation_required",
            "Managed-to-package migration requires explicit acceptance of system changes.",
        ));
    }
    if request.approved_plan_token.is_none() {
        return Err(TorbenError::new(
            "source_migration_plan_approval_required",
            "Managed-to-package migration requires a reviewed plan token.",
        ));
    }
    Ok(())
}

fn validate_package_to_managed_approval(
    request: &PackageToManagedMigrationRequest,
) -> TorbenResult<()> {
    if !request.accept_system_changes {
        return Err(TorbenError::new(
            "source_migration_confirmation_required",
            "Package-to-managed migration requires explicit acceptance of system changes.",
        ));
    }
    if request.approved_plan_token.is_none() {
        return Err(TorbenError::new(
            "source_migration_plan_approval_required",
            "Package-to-managed migration requires a reviewed plan token.",
        ));
    }
    Ok(())
}

fn validate_package_to_managed_installation(
    plan: &PackageToManagedMigrationPlan,
    record: &InstallRecord,
) -> TorbenResult<()> {
    if record.app_id != plan.app_id
        || record.version != plan.app_version
        || record.source_id != plan.install_managed.source_id
        || record.scope != InstallScope::Managed
        || record.install_path != plan.managed_target_path
    {
        return Err(TorbenError::new(
            "source_migration_managed_result_invalid",
            "The official managed installation result does not match the reviewed plan.",
        ));
    }
    Ok(())
}

async fn remove_package_for_managed_migration(
    plan: &PackageToManagedMigrationPlan,
    service: &source_adapters::SourceAdapterService,
) -> TorbenResult<()> {
    let removal = service.execute(&plan.uninstall_current).await;
    let after = service
        .inspect(
            plan.current_owner.adapter,
            plan.current_owner.coordinate.clone(),
            plan.current_owner.package_kind,
        )
        .await
        .map_err(|error| source_migration_reconciliation_error(&error))?;
    if !after.installed {
        return Ok(());
    }
    if after.installed_version.as_ref() != Some(&plan.current_owner.package_version) {
        return Err(TorbenError::new(
            "source_migration_reconciliation_required",
            "The package source changed to an unexpected version during migration.",
        ));
    }
    Err(removal.err().unwrap_or_else(|| {
        TorbenError::new(
            "source_reconciliation_failed",
            "The package source remains installed after the reviewed removal command.",
        )
    }))
}

async fn restore_package_after_managed_failure(
    plan: &PackageToManagedMigrationPlan,
    service: &source_adapters::SourceAdapterService,
) -> TorbenResult<()> {
    let before = service
        .inspect(
            plan.current_owner.adapter,
            plan.current_owner.coordinate.clone(),
            plan.current_owner.package_kind,
        )
        .await?;
    if !before.installed {
        service.execute(&plan.restore_current).await?;
    }
    let restored = service
        .inspect(
            plan.current_owner.adapter,
            plan.current_owner.coordinate.clone(),
            plan.current_owner.package_kind,
        )
        .await?;
    if !restored.installed
        || restored.installed_version.as_ref() != Some(&plan.current_owner.package_version)
    {
        return Err(TorbenError::new(
            "source_migration_restore_failed",
            "The previous package source was not restored to its exact owned version.",
        ));
    }
    service
        .health_check(
            plan.app_id.as_str(),
            &plan.app_version.to_string(),
            Path::new(&plan.current_owner.executable_path),
        )
        .await?;
    Ok(())
}

fn cleanup_package_to_managed_payload(
    paths: &TorbenPaths,
    plan: &PackageToManagedMigrationPlan,
    operation_id: OperationId,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    let managed_target = validate_package_to_managed_recovery_plan(paths, journal, plan)?;
    let staging = paths
        .staging_dir()
        .join(format!("install-{}-{operation_id}", plan.app_id));
    remove_managed_directory_if_exists(&staging, journal)?;
    let target_present = match managed_target.symlink_metadata() {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(recovery_error(
                journal,
                "operation_recovery_inspect_failed",
                "Could not inspect the managed migration target.",
            )
            .with_detail("path", managed_target.display().to_string())
            .with_detail("reason", error.to_string()));
        }
    };
    if target_present || package_to_managed_receipt_exists(paths, operation_id) {
        validate_package_to_managed_receipt(paths, operation_id, plan, &managed_target)?;
    }
    if target_present {
        remove_managed_directory_if_exists(&managed_target, journal)?;
    }
    remove_package_to_managed_receipt_if_present(paths, operation_id)
}

fn managed_install_receipt_path(paths: &TorbenPaths, operation_id: OperationId) -> PathBuf {
    paths
        .operation_dir()
        .join(format!("{operation_id}.managed-install.receipt"))
}

fn managed_install_receipt(
    operation_id: OperationId,
    app_id: &AppId,
    version: &ExactVersion,
    final_path: &Path,
) -> ManagedInstallReceipt {
    ManagedInstallReceipt {
        schema_version: MANAGED_INSTALL_RECEIPT_SCHEMA_VERSION,
        operation_id,
        app_id: app_id.clone(),
        version: version.clone(),
        final_path: final_path.to_path_buf(),
    }
}

fn validate_managed_install_record(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    record: &InstallRecord,
) -> TorbenResult<PathBuf> {
    let expected = paths.app_version_dir(record.app_id.as_str(), &record.version.to_string());
    if journal.kind() != OperationKind::Install
        || journal.app_id() != Some(&record.app_id)
        || journal.version() != Some(&record.version)
        || record.scope != InstallScope::Managed
        || Path::new(&record.install_path) != expected
    {
        return Err(TorbenError::new(
            "install_result_invalid",
            "The managed installation result does not match its transaction identity.",
        )
        .with_detail("expectedPath", expected.display().to_string())
        .with_detail("recordedPath", &record.install_path)
        .with_remediation("Inspect the managed target before retrying installation."));
    }
    Ok(expected)
}

fn write_managed_install_receipt(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    record: &InstallRecord,
) -> TorbenResult<()> {
    let final_path = validate_managed_install_record(paths, journal, record)?;
    ensure_recovery_directory(journal, &final_path, "prepared managed installation")?;
    let receipt = managed_install_receipt(
        journal.operation_id(),
        &record.app_id,
        &record.version,
        &final_path,
    );
    let content = serde_json::to_vec(&receipt).map_err(|error| {
        managed_install_receipt_error("Could not serialize the managed-install ownership receipt.")
            .with_detail("reason", error.to_string())
    })?;
    let path = managed_install_receipt_path(paths, journal.operation_id());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            managed_install_receipt_error("Could not create the managed-install ownership receipt.")
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
    file.write_all(&content).map_err(|error| {
        managed_install_receipt_error("Could not write the managed-install ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    file.sync_all().map_err(|error| {
        managed_install_receipt_error("Could not sync the managed-install ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })
}

fn validate_managed_install_receipt(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    app_id: &AppId,
    version: &ExactVersion,
    final_path: &Path,
) -> TorbenResult<()> {
    let path = managed_install_receipt_path(paths, journal.operation_id());
    let metadata = path.symlink_metadata().map_err(|error| {
        managed_install_receipt_error("The managed-install ownership receipt is unavailable.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MANAGED_INSTALL_RECEIPT_MAX_BYTES
    {
        return Err(managed_install_receipt_error(
            "The managed-install ownership receipt is not a bounded regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    let content = std::fs::read(&path).map_err(|error| {
        managed_install_receipt_error("Could not read the managed-install ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let actual: ManagedInstallReceipt = serde_json::from_slice(&content).map_err(|error| {
        managed_install_receipt_error("The managed-install ownership receipt is invalid.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let expected = managed_install_receipt(journal.operation_id(), app_id, version, final_path);
    if actual != expected {
        return Err(managed_install_receipt_error(
            "The install journal does not match its ownership receipt.",
        ));
    }
    Ok(())
}

fn managed_install_receipt_exists(paths: &TorbenPaths, operation_id: OperationId) -> bool {
    managed_install_receipt_path(paths, operation_id)
        .symlink_metadata()
        .is_ok()
}

fn remove_managed_install_receipt_if_present(
    paths: &TorbenPaths,
    operation_id: OperationId,
) -> TorbenResult<()> {
    let path = managed_install_receipt_path(paths, operation_id);
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(&path).map_err(|error| {
                managed_install_receipt_error(
                    "Could not remove the managed-install ownership receipt.",
                )
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
            })
        }
        Ok(_) => Err(managed_install_receipt_error(
            "The managed-install ownership receipt is not a regular file.",
        )
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(managed_install_receipt_error(
            "Could not inspect the managed-install ownership receipt.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn cleanup_managed_install_artifacts(
    paths: &TorbenPaths,
    app_id: &AppId,
    version: &ExactVersion,
    journal: &OperationJournal,
) -> TorbenResult<()> {
    if journal.kind() != OperationKind::Install
        || journal.app_id() != Some(app_id)
        || journal.version() != Some(version)
    {
        return Err(recovery_error(
            journal,
            "operation_recovery_invalid",
            "The install transaction identity changed before cleanup.",
        ));
    }
    let staging =
        paths
            .staging_dir()
            .join(format!("install-{}-{}", app_id, journal.operation_id()));
    remove_managed_directory_if_exists(&staging, journal)?;
    let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
    let final_present = match final_path.symlink_metadata() {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(recovery_error(
                journal,
                "operation_recovery_inspect_failed",
                "Could not inspect the managed installation target.",
            )
            .with_detail("path", final_path.display().to_string())
            .with_detail("reason", error.to_string()));
        }
    };
    if final_present || managed_install_receipt_exists(paths, journal.operation_id()) {
        validate_managed_install_receipt(paths, journal, app_id, version, &final_path)?;
    }
    if final_present {
        remove_managed_directory_if_exists(&final_path, journal)?;
    }
    remove_managed_install_receipt_if_present(paths, journal.operation_id())
}

fn managed_install_receipt_error(message: &str) -> TorbenError {
    TorbenError::new("install_ownership_receipt_invalid", message)
        .with_remediation("Inspect the managed target and operation journal before retrying.")
}

fn managed_uninstall_receipt_path(paths: &TorbenPaths, operation_id: OperationId) -> PathBuf {
    paths
        .operation_dir()
        .join(format!("{operation_id}.managed-uninstall.receipt"))
}

fn managed_uninstall_receipt(
    operation_id: OperationId,
    app_id: &AppId,
    version: &ExactVersion,
    source_path: &Path,
    staged_path: &Path,
) -> ManagedUninstallReceipt {
    ManagedUninstallReceipt {
        schema_version: MANAGED_UNINSTALL_RECEIPT_SCHEMA_VERSION,
        operation_id,
        app_id: app_id.clone(),
        version: version.clone(),
        source_path: source_path.to_path_buf(),
        staged_path: staged_path.to_path_buf(),
    }
}

fn validate_managed_uninstall_identity(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    app_id: &AppId,
    version: &ExactVersion,
    source_path: &Path,
    staged_path: &Path,
) -> TorbenResult<()> {
    let expected_source = paths.app_version_dir(app_id.as_str(), &version.to_string());
    let expected_staged =
        paths
            .staging_dir()
            .join(format!("uninstall-{}-{}", app_id, journal.operation_id()));
    if journal.kind() != OperationKind::Uninstall
        || journal.app_id() != Some(app_id)
        || journal.version() != Some(version)
        || source_path != expected_source
        || staged_path != expected_staged
    {
        return Err(managed_uninstall_receipt_error(
            "The uninstall paths do not match the transaction identity.",
        )
        .with_detail("expectedSourcePath", expected_source.display().to_string())
        .with_detail("actualSourcePath", source_path.display().to_string())
        .with_detail("expectedStagedPath", expected_staged.display().to_string())
        .with_detail("actualStagedPath", staged_path.display().to_string()));
    }
    Ok(())
}

fn validate_managed_uninstall_record(
    record: &InstallRecord,
    source_path: &Path,
) -> TorbenResult<()> {
    if record.scope == InstallScope::Managed && Path::new(&record.install_path) == source_path {
        return Ok(());
    }
    Err(managed_uninstall_receipt_error(
        "The uninstall record is not a standard managed installation.",
    )
    .with_detail("recordedPath", &record.install_path)
    .with_detail("sourcePath", source_path.display().to_string()))
}

fn ensure_managed_uninstall_receipt_absent(
    paths: &TorbenPaths,
    operation_id: OperationId,
) -> TorbenResult<()> {
    let path = managed_uninstall_receipt_path(paths, operation_id);
    match path.symlink_metadata() {
        Ok(_) => Err(managed_uninstall_receipt_error(
            "An uninstall ownership receipt already exists before staging.",
        )
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(managed_uninstall_receipt_error(
            "Could not inspect the uninstall ownership receipt path.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn validate_uninstall_stage_paths(source_path: &Path, staged_path: &Path) -> TorbenResult<()> {
    match source_path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(TorbenError::new(
                "uninstall_stage_failed",
                "The managed installation is not a plain directory.",
            )
            .with_detail("path", source_path.display().to_string()));
        }
        Err(error) => {
            return Err(TorbenError::new(
                "uninstall_stage_failed",
                "Could not inspect the managed installation before staging.",
            )
            .with_detail("path", source_path.display().to_string())
            .with_detail("reason", error.to_string()));
        }
    }
    match staged_path.symlink_metadata() {
        Ok(_) => Err(TorbenError::new(
            "uninstall_stage_failed",
            "The uninstall staging path already exists.",
        )
        .with_detail("path", staged_path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TorbenError::new(
            "uninstall_stage_failed",
            "Could not inspect the uninstall staging path.",
        )
        .with_detail("path", staged_path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn write_managed_uninstall_receipt(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    app_id: &AppId,
    version: &ExactVersion,
    source_path: &Path,
    staged_path: &Path,
) -> TorbenResult<()> {
    validate_managed_uninstall_identity(paths, journal, app_id, version, source_path, staged_path)?;
    match staged_path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(managed_uninstall_receipt_error(
                "The staged uninstall payload is not a plain directory.",
            )
            .with_detail("path", staged_path.display().to_string()));
        }
        Err(error) => {
            return Err(managed_uninstall_receipt_error(
                "Could not inspect the staged uninstall payload.",
            )
            .with_detail("path", staged_path.display().to_string())
            .with_detail("reason", error.to_string()));
        }
    }
    let receipt = managed_uninstall_receipt(
        journal.operation_id(),
        app_id,
        version,
        source_path,
        staged_path,
    );
    let content = serde_json::to_vec(&receipt).map_err(|error| {
        managed_uninstall_receipt_error("Could not serialize the uninstall ownership receipt.")
            .with_detail("reason", error.to_string())
    })?;
    let path = managed_uninstall_receipt_path(paths, journal.operation_id());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            managed_uninstall_receipt_error("Could not create the uninstall ownership receipt.")
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
    file.write_all(&content).map_err(|error| {
        managed_uninstall_receipt_error("Could not write the uninstall ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    file.sync_all().map_err(|error| {
        managed_uninstall_receipt_error("Could not sync the uninstall ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })
}

fn validate_managed_uninstall_receipt(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    app_id: &AppId,
    version: &ExactVersion,
    source_path: &Path,
    staged_path: &Path,
) -> TorbenResult<()> {
    validate_managed_uninstall_identity(paths, journal, app_id, version, source_path, staged_path)?;
    let path = managed_uninstall_receipt_path(paths, journal.operation_id());
    let metadata = path.symlink_metadata().map_err(|error| {
        managed_uninstall_receipt_error("The uninstall ownership receipt is unavailable.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MANAGED_UNINSTALL_RECEIPT_MAX_BYTES
    {
        return Err(managed_uninstall_receipt_error(
            "The uninstall ownership receipt is not a bounded regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    let content = std::fs::read(&path).map_err(|error| {
        managed_uninstall_receipt_error("Could not read the uninstall ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let actual: ManagedUninstallReceipt = serde_json::from_slice(&content).map_err(|error| {
        managed_uninstall_receipt_error("The uninstall ownership receipt is invalid.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let expected = managed_uninstall_receipt(
        journal.operation_id(),
        app_id,
        version,
        source_path,
        staged_path,
    );
    if actual != expected {
        return Err(managed_uninstall_receipt_error(
            "The uninstall journal does not match its ownership receipt.",
        ));
    }
    Ok(())
}

fn managed_uninstall_receipt_presence(
    paths: &TorbenPaths,
    operation_id: OperationId,
) -> TorbenResult<bool> {
    let path = managed_uninstall_receipt_path(paths, operation_id);
    match path.symlink_metadata() {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(managed_uninstall_receipt_error(
            "Could not inspect the uninstall ownership receipt.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn remove_managed_uninstall_receipt_if_present(
    paths: &TorbenPaths,
    operation_id: OperationId,
) -> TorbenResult<()> {
    let path = managed_uninstall_receipt_path(paths, operation_id);
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(&path).map_err(|error| {
                managed_uninstall_receipt_error("Could not remove the uninstall ownership receipt.")
                    .with_detail("path", path.display().to_string())
                    .with_detail("reason", error.to_string())
            })
        }
        Ok(_) => Err(managed_uninstall_receipt_error(
            "The uninstall ownership receipt is not a regular file.",
        )
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(managed_uninstall_receipt_error(
            "Could not inspect the uninstall ownership receipt.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn restore_managed_uninstall_staging(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    app_id: &AppId,
    version: &ExactVersion,
    source_path: &Path,
    staged_path: &Path,
) -> TorbenResult<()> {
    validate_managed_uninstall_receipt(paths, journal, app_id, version, source_path, staged_path)?;
    match source_path.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(managed_uninstall_receipt_error(
                "The managed uninstall target already exists during restoration.",
            )
            .with_detail("path", source_path.display().to_string()));
        }
        Err(error) => {
            return Err(managed_uninstall_receipt_error(
                "Could not inspect the managed uninstall target during restoration.",
            )
            .with_detail("path", source_path.display().to_string())
            .with_detail("reason", error.to_string()));
        }
    }
    match staged_path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(managed_uninstall_receipt_error(
                "The staged uninstall payload is not a plain directory.",
            )
            .with_detail("path", staged_path.display().to_string()));
        }
        Err(error) => {
            return Err(managed_uninstall_receipt_error(
                "Could not inspect the staged uninstall payload during restoration.",
            )
            .with_detail("path", staged_path.display().to_string())
            .with_detail("reason", error.to_string()));
        }
    }
    std::fs::rename(staged_path, source_path).map_err(|error| {
        managed_uninstall_receipt_error(
            "Could not restore the receipt-bound staged uninstall payload.",
        )
        .with_detail("reason", error.to_string())
    })
}

fn remove_receipt_bound_uninstall_staging(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    app_id: &AppId,
    version: &ExactVersion,
    source_path: &Path,
    staged_path: &Path,
) -> TorbenResult<()> {
    validate_managed_uninstall_receipt(paths, journal, app_id, version, source_path, staged_path)?;
    match staged_path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(staged_path).map_err(|error| {
                managed_uninstall_receipt_error(
                    "Could not remove the receipt-bound staged uninstall payload.",
                )
                .with_detail("path", staged_path.display().to_string())
                .with_detail("reason", error.to_string())
            })
        }
        Ok(_) => Err(managed_uninstall_receipt_error(
            "The staged uninstall payload is not a plain directory.",
        )
        .with_detail("path", staged_path.display().to_string())),
        Err(error) => Err(managed_uninstall_receipt_error(
            "Could not inspect the staged uninstall payload before cleanup.",
        )
        .with_detail("path", staged_path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn uninstall_rollback_pending(message: &str, cause: &TorbenError) -> TorbenError {
    TorbenError::new("uninstall_rollback_pending", message)
        .with_detail("causeCode", &cause.code)
        .with_detail("cause", &cause.message)
        .with_remediation(
            "Inspect the receipt-bound staging directory before restarting Torben App recovery.",
        )
}

fn uninstall_cleanup_pending(cause: &TorbenError) -> TorbenError {
    TorbenError::new(
        "uninstall_cleanup_pending",
        "Uninstall state committed, but receipt-bound staged cleanup is incomplete.",
    )
    .with_detail("causeCode", &cause.code)
    .with_detail("cause", &cause.message)
    .with_remediation(
        "Restart Torben App to resume cleanup; do not manually restore the staged directory.",
    )
}

fn managed_uninstall_receipt_error(message: &str) -> TorbenError {
    TorbenError::new("uninstall_ownership_receipt_invalid", message)
        .with_remediation("Inspect the managed and staged paths before retrying the uninstall.")
}

fn package_to_managed_receipt_path(paths: &TorbenPaths, operation_id: OperationId) -> PathBuf {
    paths
        .operation_dir()
        .join(format!("{operation_id}.package-to-managed.receipt"))
}

fn package_to_managed_receipt(
    operation_id: OperationId,
    plan: &PackageToManagedMigrationPlan,
    managed_target: &Path,
) -> PackageToManagedReceipt {
    PackageToManagedReceipt {
        schema_version: SOURCE_MIGRATION_RECEIPT_SCHEMA_VERSION,
        operation_id,
        app_id: plan.app_id.clone(),
        app_version: plan.app_version.clone(),
        managed_target_path: managed_target.to_path_buf(),
        approval_token: plan.approval_token.clone(),
    }
}

fn write_package_to_managed_receipt(
    paths: &TorbenPaths,
    plan: &PackageToManagedMigrationPlan,
    journal: &OperationJournal,
) -> TorbenResult<()> {
    let managed_target = validate_package_to_managed_recovery_plan(paths, journal, plan)?;
    ensure_recovery_directory(
        journal,
        &managed_target,
        "prepared managed migration target",
    )?;
    let receipt = package_to_managed_receipt(journal.operation_id(), plan, &managed_target);
    let content = serde_json::to_vec(&receipt).map_err(|error| {
        package_to_managed_receipt_error(
            "Could not serialize the package-to-managed ownership receipt.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let path = package_to_managed_receipt_path(paths, journal.operation_id());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            package_to_managed_receipt_error(
                "Could not create the package-to-managed ownership receipt.",
            )
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
    file.write_all(&content).map_err(|error| {
        package_to_managed_receipt_error(
            "Could not write the package-to-managed ownership receipt.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    file.sync_all().map_err(|error| {
        package_to_managed_receipt_error("Could not sync the package-to-managed ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })
}

fn validate_package_to_managed_receipt(
    paths: &TorbenPaths,
    operation_id: OperationId,
    plan: &PackageToManagedMigrationPlan,
    managed_target: &Path,
) -> TorbenResult<()> {
    let path = package_to_managed_receipt_path(paths, operation_id);
    let metadata = path.symlink_metadata().map_err(|error| {
        package_to_managed_receipt_error("The package-to-managed ownership receipt is unavailable.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > SOURCE_MIGRATION_RECEIPT_MAX_BYTES
    {
        return Err(package_to_managed_receipt_error(
            "The package-to-managed ownership receipt is not a bounded regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    let content = std::fs::read(&path).map_err(|error| {
        package_to_managed_receipt_error("Could not read the package-to-managed ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let actual: PackageToManagedReceipt = serde_json::from_slice(&content).map_err(|error| {
        package_to_managed_receipt_error("The package-to-managed ownership receipt is invalid.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let expected = package_to_managed_receipt(operation_id, plan, managed_target);
    if actual != expected {
        return Err(package_to_managed_receipt_error(
            "The source migration journal does not match its ownership receipt.",
        ));
    }
    Ok(())
}

fn package_to_managed_receipt_exists(paths: &TorbenPaths, operation_id: OperationId) -> bool {
    package_to_managed_receipt_path(paths, operation_id)
        .symlink_metadata()
        .is_ok()
}

fn remove_package_to_managed_receipt_if_present(
    paths: &TorbenPaths,
    operation_id: OperationId,
) -> TorbenResult<()> {
    let path = package_to_managed_receipt_path(paths, operation_id);
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(&path).map_err(|error| {
                package_to_managed_receipt_error(
                    "Could not remove the package-to-managed ownership receipt.",
                )
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
            })
        }
        Ok(_) => Err(package_to_managed_receipt_error(
            "The package-to-managed ownership receipt is not a regular file.",
        )
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(package_to_managed_receipt_error(
            "Could not inspect the package-to-managed ownership receipt.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn package_to_managed_receipt_error(message: &str) -> TorbenError {
    TorbenError::new("source_migration_recovery_receipt_invalid", message)
        .with_remediation("Inspect the managed library and operation journal before retrying.")
}

fn validate_package_to_managed_recovery_plan(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    plan: &PackageToManagedMigrationPlan,
) -> TorbenResult<PathBuf> {
    let expected = paths.app_version_dir(plan.app_id.as_str(), &plan.app_version.to_string());
    let journal_matches =
        journal.app_id() == Some(&plan.app_id) && journal.version() == Some(&plan.app_version);
    let plan_matches = plan.current_owner.app_id == plan.app_id
        && plan.current_owner.app_version == plan.app_version
        && plan.install_managed.app_id == plan.app_id
        && plan.install_managed.version == plan.app_version
        && Path::new(&plan.managed_target_path) == expected;
    if !journal_matches || !plan_matches {
        return Err(recovery_error(
            journal,
            "source_migration_recovery_path_invalid",
            "The interrupted source migration does not describe the standard managed target.",
        )
        .with_detail("expectedPath", expected.display().to_string())
        .with_detail("recordedPath", &plan.managed_target_path));
    }
    Ok(expected)
}

fn validate_managed_to_package_recovery_plan(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    plan: &ManagedToPackageMigrationPlan,
) -> TorbenResult<PathBuf> {
    let expected = paths.app_version_dir(plan.app_id.as_str(), &plan.app_version.to_string());
    let current = &plan.current_installation;
    let journal_matches =
        journal.app_id() == Some(&plan.app_id) && journal.version() == Some(&plan.app_version);
    let plan_matches = current.app_id == plan.app_id
        && current.version == plan.app_version
        && current.scope == InstallScope::Managed
        && Path::new(&current.install_path) == expected
        && plan.uninstall_current.app_id == plan.app_id
        && plan.uninstall_current.version == plan.app_version
        && plan.uninstall_current.source_id == current.source_id
        && Path::new(&plan.uninstall_current.install_path) == expected;
    if !journal_matches || !plan_matches {
        return Err(recovery_error(
            journal,
            "source_migration_recovery_path_invalid",
            "The interrupted source migration does not describe the standard managed source.",
        )
        .with_detail("expectedPath", expected.display().to_string())
        .with_detail("recordedPath", &current.install_path));
    }
    Ok(expected)
}

fn package_to_managed_package_was_untouched(journal: &OperationJournal) -> bool {
    !matches!(
        journal.latest_phase(),
        Some("remove_package" | "state_commit" | "compensate")
    )
}

fn source_migration_cleanup_error(cause: &TorbenError, cleanup: &TorbenError) -> TorbenError {
    TorbenError::new(
        "source_migration_reconciliation_required",
        "Source migration failed and managed payload cleanup is incomplete.",
    )
    .with_detail("causeCode", &cause.code)
    .with_detail("cleanupCode", &cleanup.code)
    .with_remediation("Inspect the managed library before retrying the migration.")
}

fn source_migration_backup(paths: &TorbenPaths, operation_id: OperationId) -> PathBuf {
    paths
        .staging_dir()
        .join(format!("source-migrate-{operation_id}"))
        .join("managed-backup")
}

fn stage_managed_source(
    plan: &ManagedToPackageMigrationPlan,
    backup: &Path,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    let source = Path::new(&plan.current_installation.install_path);
    if source.to_string_lossy() != plan.uninstall_current.install_path {
        return Err(TorbenError::new(
            "plugin_uninstall_plan_invalid",
            "The managed migration uninstall plan changed its installation path.",
        ));
    }
    let parent = backup.parent().ok_or_else(|| {
        TorbenError::internal("The source migration backup has no parent directory.")
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        TorbenError::new(
            "source_migration_stage_failed",
            "Could not create managed migration staging.",
        )
        .with_detail("reason", error.to_string())
    })?;
    journal.record(
        OperationState::Running,
        "stage_current",
        "Staging the managed installation for reversible removal",
        Some(0.28),
    )?;
    std::fs::rename(source, backup).map_err(|error| {
        TorbenError::new(
            "source_migration_stage_failed",
            "Could not stage the managed installation for source migration.",
        )
        .with_detail("reason", error.to_string())
    })
}

fn restore_managed_source(record: &InstallRecord, backup: &Path) -> TorbenResult<()> {
    let destination = Path::new(&record.install_path);
    let backup_is_plain_directory = backup
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink());
    if destination.exists() || !backup_is_plain_directory {
        return Err(TorbenError::new(
            "source_migration_restore_failed",
            "The managed migration backup cannot be restored safely.",
        ));
    }
    std::fs::rename(backup, destination).map_err(|error| {
        TorbenError::new(
            "source_migration_restore_failed",
            "Could not restore the managed installation backup.",
        )
        .with_detail("reason", error.to_string())
    })?;
    if let Some(parent) = backup.parent() {
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

fn remove_source_migration_backup(backup: &Path) -> TorbenResult<()> {
    let Some(parent) = backup.parent() else {
        return Err(TorbenError::internal(
            "The source migration backup has no parent directory.",
        ));
    };
    match parent.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(parent).map_err(|error| {
                TorbenError::new(
                    "source_migration_cleanup_pending",
                    "Source ownership committed, but the managed backup cleanup is incomplete.",
                )
                .with_detail("reason", error.to_string())
                .with_remediation("Restart Torben App to resume cleanup.")
            })?;
        }
        Ok(_) => {
            return Err(TorbenError::new(
                "source_migration_cleanup_pending",
                "The managed migration backup path is not a plain directory.",
            )
            .with_detail("path", parent.display().to_string())
            .with_remediation("Inspect the migration staging path before retrying."));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(TorbenError::new(
                "source_migration_cleanup_pending",
                "Could not inspect the managed migration backup path.",
            )
            .with_detail("path", parent.display().to_string())
            .with_detail("reason", error.to_string())
            .with_remediation("Inspect the migration staging path before retrying."));
        }
    }
    Ok(())
}

async fn cleanup_migration_package_target(
    cleanup: &SourceOperationPlan,
    service: &source_adapters::SourceAdapterService,
) -> TorbenResult<()> {
    let before = service
        .inspect(
            cleanup.adapter,
            cleanup.coordinate.clone(),
            cleanup.package_kind,
        )
        .await?;
    if !before.installed {
        return Ok(());
    }
    let execution = service.execute(cleanup).await;
    let after = service
        .inspect(
            cleanup.adapter,
            cleanup.coordinate.clone(),
            cleanup.package_kind,
        )
        .await?;
    if after.installed {
        return Err(execution.err().unwrap_or_else(|| {
            TorbenError::new(
                "source_migration_cleanup_failed",
                "The package migration target remains installed.",
            )
        }));
    }
    Ok(())
}

fn package_request_from_managed_plan(
    plan: &ManagedToPackageMigrationPlan,
) -> SourceExecutionRequest {
    SourceExecutionRequest {
        app_id: plan.app_id.clone(),
        app_version: plan.app_version.clone(),
        action: SourceAction::Install,
        adapter: plan.install_target.adapter,
        coordinate: plan.install_target.coordinate.clone(),
        package_kind: plan.install_target.package_kind,
        package_version: plan.install_target.package_version.clone(),
        executable_path: Some(plan.target_executable_path.clone()),
        approved_execution_identity: plan.install_target.execution_identity.clone(),
        accept_system_changes: true,
    }
}

fn source_migration_reconciliation_error(error: &TorbenError) -> TorbenError {
    TorbenError::new(
        "source_migration_reconciliation_required",
        "Torben could not verify package-manager state during source migration.",
    )
    .with_detail("causeCode", &error.code)
    .with_detail("cause", &error.message)
    .with_remediation("Inspect both package sources manually before retrying the migration.")
}

fn source_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or_else(
            |_| "0".to_owned(),
            |duration| duration.as_secs().to_string(),
        )
}

fn recover_interrupted_operations(paths: &TorbenPaths, store: Arc<StateStore>) -> TorbenResult<()> {
    for mut journal in OperationJournal::interrupted(paths, Arc::clone(&store))? {
        match journal.kind() {
            OperationKind::Install => recover_install(paths, &store, &mut journal)?,
            OperationKind::Uninstall => recover_uninstall(paths, &store, &mut journal)?,
            OperationKind::Select => recover_selection(paths, &store, &mut journal)?,
            OperationKind::PluginInstall => {
                recover_plugin_install(paths, &store, &mut journal)?;
            }
            OperationKind::Migrate => {
                library_migration::recover(paths, &store, &mut journal)?;
            }
            OperationKind::SourceInstall | OperationKind::SourceUninstall => {
                recover_source_operation(&store, &mut journal)?;
            }
            OperationKind::SourceMigrate => {
                recover_source_migration(paths, &store, &mut journal)?;
            }
        }
    }
    Ok(())
}

fn recover_source_migration(
    paths: &TorbenPaths,
    store: &StateStore,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    let subject = journal.source_migration().cloned().ok_or_else(|| {
        recovery_error(
            journal,
            "operation_recovery_invalid",
            "The interrupted source migration has no reviewed plan.",
        )
    })?;
    match subject {
        SourceMigrationSubject::PackageToPackage(plan) => {
            recover_package_source_migration(store, journal, &plan)
        }
        SourceMigrationSubject::ManagedToPackage(plan) => {
            recover_managed_to_package_migration(paths, store, journal, &plan)
        }
        SourceMigrationSubject::PackageToManaged(plan) => {
            recover_package_to_managed_migration(paths, store, journal, &plan)
        }
    }
}

fn recover_package_source_migration(
    store: &StateStore,
    journal: &mut OperationJournal,
    plan: &SourceMigrationPlan,
) -> TorbenResult<()> {
    let ownership = store.package_installation(&plan.app_id, &plan.app_version)?;
    if ownership.as_ref().is_some_and(|record| {
        record.adapter == plan.install_target.adapter
            && record.coordinate == plan.install_target.coordinate
            && record.package_kind == plan.install_target.package_kind
            && plan
                .install_target
                .package_version
                .as_ref()
                .is_none_or(|version| version == &record.package_version)
    }) {
        return journal.succeed("Recovered source migration after the atomic ownership commit");
    }
    if ownership.is_some() {
        store.remove_package_installation(&plan.app_id, &plan.app_version)?;
    }
    let error = recovery_error(
        journal,
        "source_migration_reconciliation_required",
        "The interrupted source migration requires explicit package-manager reconciliation.",
    )
    .with_detail("approvalToken", &plan.approval_token)
    .with_remediation(
        "Inspect both reviewed package sources before retrying; Torben did not infer a new owner.",
    );
    journal.fail_reconciliation_required(&error)
}

fn recover_managed_to_package_migration(
    paths: &TorbenPaths,
    store: &StateStore,
    journal: &mut OperationJournal,
    plan: &ManagedToPackageMigrationPlan,
) -> TorbenResult<()> {
    let destination = validate_managed_to_package_recovery_plan(paths, journal, plan)?;
    let backup = source_migration_backup(paths, journal.operation_id());
    let ownership = store.package_installation(&plan.app_id, &plan.app_version)?;
    if ownership.as_ref().is_some_and(|record| {
        record.adapter == plan.install_target.adapter
            && record.coordinate == plan.install_target.coordinate
            && record.package_kind == plan.install_target.package_kind
    }) {
        remove_source_migration_backup(&backup)?;
        return journal
            .succeed("Recovered managed-to-package migration after the atomic ownership commit");
    }
    let current = store.get_installation(&plan.app_id, &plan.app_version)?;
    let managed_matches = current.as_ref().is_some_and(|record| {
        record.scope == InstallScope::Managed
            && record.source_id == plan.current_installation.source_id
            && record.install_path == plan.current_installation.install_path
    });
    if managed_matches {
        if backup.exists() && !destination.exists() {
            restore_managed_source(&plan.current_installation, &backup)?;
        } else if backup.exists() && destination.exists() {
            return Err(recovery_error(
                journal,
                "source_migration_restore_failed",
                "Both the managed source and migration backup exist during recovery.",
            ));
        }
        let error = recovery_error(
            journal,
            "source_migration_reconciliation_required",
            "The managed source was restored, but the package target requires inspection.",
        )
        .with_remediation(
            "The managed installation remains owned. Treat any target package as external until inspected.",
        );
        return journal.fail_reconciliation_required(&error);
    }
    Err(recovery_error(
        journal,
        "source_migration_reconciliation_required",
        "Interrupted managed-to-package migration has no verifiable owner.",
    ))
}

fn recover_package_to_managed_migration(
    paths: &TorbenPaths,
    store: &StateStore,
    journal: &mut OperationJournal,
    plan: &PackageToManagedMigrationPlan,
) -> TorbenResult<()> {
    let managed_path = validate_package_to_managed_recovery_plan(paths, journal, plan)?;
    let current = store.get_installation(&plan.app_id, &plan.app_version)?;
    let managed_matches = current.as_ref().is_some_and(|record| {
        record.scope == InstallScope::Managed
            && record.source_id == plan.install_managed.source_id
            && Path::new(&record.install_path) == managed_path
    });
    let package_owner = store.package_installation(&plan.app_id, &plan.app_version)?;
    if managed_matches && package_owner.is_none() {
        ensure_recovery_directory(journal, &managed_path, "committed managed migration target")?;
        remove_package_to_managed_receipt_if_present(paths, journal.operation_id())?;
        return journal
            .succeed("Recovered package-to-managed migration after the atomic ownership commit");
    }
    if current
        .as_ref()
        .is_some_and(|record| record.scope == InstallScope::Managed)
    {
        return Err(recovery_error(
            journal,
            "source_migration_recovery_inconsistent",
            "The managed installation state does not match the interrupted source migration.",
        ));
    }
    let staging = paths.staging_dir().join(format!(
        "install-{}-{}",
        plan.app_id,
        journal.operation_id()
    ));
    remove_managed_directory_if_exists(&staging, journal)?;
    if managed_path.exists() {
        validate_package_to_managed_receipt(paths, journal.operation_id(), plan, &managed_path)?;
        remove_managed_directory_if_exists(&managed_path, journal)?;
    }
    remove_package_to_managed_receipt_if_present(paths, journal.operation_id())?;
    let package_owner_matches = package_owner.as_ref() == Some(&plan.current_owner);
    if package_owner_matches && package_to_managed_package_was_untouched(journal) {
        return journal.recover_rollback(
            "Interrupted package-to-managed migration stopped before package removal",
        );
    }
    if package_owner_matches {
        store.remove_package_installation(&plan.app_id, &plan.app_version)?;
    }
    let error = recovery_error(
        journal,
        "source_migration_reconciliation_required",
        "Interrupted package-to-managed migration requires package-manager reconciliation.",
    )
    .with_remediation(
        "The incomplete managed payload was removed. Inspect the package source before retrying.",
    );
    journal.fail_reconciliation_required(&error)
}

fn recover_source_operation(
    store: &StateStore,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    let app_id = required_recovery_app_id(journal)?.clone();
    let version = required_recovery_version(journal)?.clone();
    let source = journal.source().ok_or_else(|| {
        recovery_error(
            journal,
            "operation_recovery_invalid",
            "The interrupted source operation has no package subject.",
        )
    })?;
    let ownership = store.package_installation(&app_id, &version)?;
    match journal.kind() {
        OperationKind::SourceInstall
            if ownership.as_ref().is_some_and(|record| {
                record.owned_by_torben
                    && record.adapter == source.adapter
                    && record.coordinate == source.coordinate
                    && record.package_kind == source.package_kind
            }) =>
        {
            journal.succeed("Recovered package installation after the atomic ownership commit")
        }
        OperationKind::SourceUninstall if ownership.is_none() => {
            journal.succeed("Recovered package uninstall after the ownership removal commit")
        }
        OperationKind::SourceInstall | OperationKind::SourceUninstall => {
            let error = recovery_error(
                journal,
                "source_operation_reconciliation_required",
                "The interrupted package-manager operation requires explicit reconciliation.",
            )
            .with_detail("adapter", source.adapter.to_string())
            .with_detail("coordinate", source.coordinate.to_string())
            .with_remediation(
                "Inspect the package manager state before retrying; Torben did not infer or change ownership.",
            );
            journal.fail_reconciliation_required(&error)
        }
        _ => Err(recovery_error(
            journal,
            "operation_recovery_invalid",
            "The source recovery handler received another operation kind.",
        )),
    }
}

fn recover_install(
    paths: &TorbenPaths,
    store: &StateStore,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    if journal.version().is_none() {
        let app_id = required_recovery_app_id(journal)?.clone();
        let staged =
            paths
                .staging_dir()
                .join(format!("install-{}-{}", app_id, journal.operation_id()));
        remove_managed_directory_if_exists(&staged, journal)?;
        return journal.recover_rollback(
            "Interrupted installation had not resolved an exact version or mutated managed state",
        );
    }
    let app_id = required_recovery_app_id(journal)?.clone();
    let version = required_recovery_version(journal)?.clone();
    let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
    let staged = paths
        .staging_dir()
        .join(format!("install-{}-{}", app_id, journal.operation_id()));
    remove_interrupted_download_partials(paths, &app_id, &version, journal)?;
    let installation = store.get_installation(&app_id, &version)?;

    if let Some(record) = installation {
        ensure_record_path(journal, &record, &final_path)?;
        if record.scope != InstallScope::Managed {
            return Err(recovery_error(
                journal,
                "operation_recovery_inconsistent",
                "The install journal resolved to a non-managed ownership record.",
            ));
        }
        ensure_recovery_directory(journal, &final_path, "committed managed installation")?;
        remove_managed_directory_if_exists(&staged, journal)?;
        remove_managed_install_receipt_if_present(paths, journal.operation_id())?;
        journal.succeed("Recovered committed installation after interrupted shutdown")?;
        return Ok(());
    }

    cleanup_managed_install_artifacts(paths, &app_id, &version, journal)?;
    journal.recover_rollback("Interrupted installation had not committed state")
}

fn recover_uninstall(
    paths: &TorbenPaths,
    store: &StateStore,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    let app_id = required_recovery_app_id(journal)?.clone();
    let version = required_recovery_version(journal)?.clone();
    let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
    let staged =
        paths
            .staging_dir()
            .join(format!("uninstall-{}-{}", app_id, journal.operation_id()));
    validate_managed_uninstall_identity(paths, journal, &app_id, &version, &final_path, &staged)?;
    let final_present =
        inspect_uninstall_recovery_directory(journal, &final_path, "managed uninstall source")?;
    let staged_present =
        inspect_uninstall_recovery_directory(journal, &staged, "staged uninstall payload")?;
    let receipt_present = managed_uninstall_receipt_presence(paths, journal.operation_id())?;
    let installation = store.get_installation(&app_id, &version)?;

    if let Some(record) = installation {
        ensure_record_path(journal, &record, &final_path)?;
        if record.scope != InstallScope::Managed {
            return Err(recovery_error(
                journal,
                "operation_recovery_inconsistent",
                "The uninstall journal resolved to a non-managed ownership record.",
            ));
        }
        return recover_uncommitted_uninstall(
            paths,
            journal,
            &app_id,
            &version,
            &final_path,
            &staged,
            (final_present, staged_present, receipt_present),
        );
    }

    recover_committed_uninstall(
        paths,
        journal,
        &app_id,
        &version,
        &final_path,
        &staged,
        (final_present, staged_present, receipt_present),
    )
}

fn recover_uncommitted_uninstall(
    paths: &TorbenPaths,
    journal: &mut OperationJournal,
    app_id: &AppId,
    version: &ExactVersion,
    final_path: &Path,
    staged: &Path,
    presence: (bool, bool, bool),
) -> TorbenResult<()> {
    let (final_present, staged_present, receipt_present) = presence;
    match (final_present, staged_present) {
        (false, true) => {
            validate_managed_uninstall_receipt(
                paths, journal, app_id, version, final_path, staged,
            )?;
            if let Some(parent) = final_path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    recovery_error(
                        journal,
                        "operation_recovery_restore_failed",
                        "Could not prepare the managed directory for startup recovery.",
                    )
                    .with_detail("reason", error.to_string())
                })?;
            }
            restore_managed_uninstall_staging(paths, journal, app_id, version, final_path, staged)?;
        }
        (true, false) => {
            if receipt_present {
                validate_managed_uninstall_receipt(
                    paths, journal, app_id, version, final_path, staged,
                )?;
            }
        }
        (true, true) => {
            return Err(recovery_error(
                journal,
                "operation_recovery_path_conflict",
                "Both the managed and staged uninstall directories exist.",
            )
            .with_detail("managedPath", final_path.display().to_string())
            .with_detail("stagedPath", staged.display().to_string()));
        }
        (false, false) => {
            return Err(recovery_error(
                journal,
                "operation_recovery_inconsistent",
                "The installation remains in state but neither managed nor staged data exists.",
            ));
        }
    }
    remove_managed_uninstall_receipt_if_present(paths, journal.operation_id())?;
    journal.recover_rollback("Interrupted uninstall had not committed state")
}

fn recover_committed_uninstall(
    paths: &TorbenPaths,
    journal: &mut OperationJournal,
    app_id: &AppId,
    version: &ExactVersion,
    final_path: &Path,
    staged: &Path,
    presence: (bool, bool, bool),
) -> TorbenResult<()> {
    let (final_present, staged_present, receipt_present) = presence;
    if final_present {
        return Err(recovery_error(
            journal,
            "operation_recovery_path_conflict",
            "An untracked managed directory remains after the uninstall committed state.",
        )
        .with_detail("path", final_path.display().to_string()));
    }
    if staged_present {
        remove_receipt_bound_uninstall_staging(
            paths, journal, app_id, version, final_path, staged,
        )?;
    } else if receipt_present {
        validate_managed_uninstall_receipt(paths, journal, app_id, version, final_path, staged)?;
    }
    remove_managed_uninstall_receipt_if_present(paths, journal.operation_id())?;
    journal.succeed("Recovered committed uninstall after interrupted shutdown")
}

fn recover_selection(
    paths: &TorbenPaths,
    store: &StateStore,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    let app_id = required_recovery_app_id(journal)?.clone();
    let selected = store.selected_version(&app_id)?;
    let (shim_staging_present, shim_receipt_present) =
        selection_shim_artifact_presence(paths, journal.operation_id())?;
    if shim_staging_present && !shim_receipt_present {
        return Err(selection_shim_receipt_error(
            "Selection shim recovery requires both staging and its matching receipt.",
        ));
    }
    if shim_staging_present {
        if selected.as_ref() == journal.version() {
            complete_selection_shim_transaction(paths, journal)?;
        } else {
            rollback_selection_shim_transaction(paths, journal)?;
        }
    } else if shim_receipt_present {
        finish_selection_shim_receipt_only(paths, journal, selected.as_ref() == journal.version())?;
    }
    if selected.as_ref() != journal.version() {
        return journal
            .recover_rollback("Interrupted selection had not committed the requested state");
    }
    if let Some(version) = journal.version() {
        let record = store.get_installation(&app_id, version)?.ok_or_else(|| {
            recovery_error(
                journal,
                "operation_recovery_inconsistent",
                "The committed selection points to a missing installation.",
            )
        })?;
        validate_selected_installation(paths, &record).map_err(|error| {
            recovery_error(
                journal,
                "operation_recovery_inconsistent",
                "The committed selection is not a valid managed installation.",
            )
            .with_detail("selectionErrorCode", error.code)
            .with_detail("selectionError", error.message)
        })?;
    }
    journal.succeed("Recovered committed selection after interrupted shutdown")
}

fn recover_plugin_install(
    paths: &TorbenPaths,
    store: &StateStore,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    let plugin_id = required_recovery_plugin_id(journal)?.clone();
    let version = required_recovery_version(journal)?.clone();
    let final_path = paths
        .plugin_dir()
        .join(plugin_id.as_str())
        .join(version.to_string());
    let staged =
        paths
            .staging_dir()
            .join(format!("plugin-{}-{}", plugin_id, journal.operation_id()));

    if let Some(record) = store.get_plugin(&plugin_id)? {
        if record.version != version {
            return Err(recovery_error(
                journal,
                "operation_recovery_inconsistent",
                "The committed plugin version does not match the interrupted operation.",
            )
            .with_detail("committedVersion", record.version.to_string())
            .with_detail("operationVersion", version.to_string()));
        }
        stored_plugin_summary(&record).map_err(|error| {
            recovery_error(
                journal,
                "operation_recovery_inconsistent",
                "The committed plugin manifest is invalid.",
            )
            .with_detail("manifestErrorCode", error.code)
            .with_detail("manifestError", error.message)
        })?;
        ensure_recovery_directory(journal, &final_path, "committed plugin")?;
        remove_managed_directory_if_exists(&staged, journal)?;
        remove_plugin_install_receipt_if_present(paths, journal.operation_id())?;
        journal.succeed("Recovered committed plugin installation after interrupted shutdown")?;
        return Ok(());
    }

    cleanup_plugin_install_artifacts(paths, &plugin_id, &version, journal)?;
    journal.recover_rollback("Interrupted plugin installation had not committed state")
}

fn required_recovery_version(journal: &OperationJournal) -> TorbenResult<&ExactVersion> {
    journal.version().ok_or_else(|| {
        recovery_error(
            journal,
            "operation_recovery_invalid",
            "The interrupted operation journal has no exact version.",
        )
    })
}

fn required_recovery_app_id(journal: &OperationJournal) -> TorbenResult<&AppId> {
    journal.app_id().ok_or_else(|| {
        recovery_error(
            journal,
            "operation_recovery_invalid",
            "The interrupted operation journal has no application identifier.",
        )
    })
}

fn required_recovery_plugin_id(journal: &OperationJournal) -> TorbenResult<&PluginId> {
    journal.plugin_id().ok_or_else(|| {
        recovery_error(
            journal,
            "operation_recovery_invalid",
            "The interrupted operation journal has no plugin identifier.",
        )
    })
}

fn ensure_recovery_directory(
    journal: &OperationJournal,
    path: &std::path::Path,
    description: &str,
) -> TorbenResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(recovery_error(
            journal,
            "operation_recovery_path_conflict",
            "A recovered managed path is not a plain directory.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(recovery_error(
            journal,
            "operation_recovery_inconsistent",
            "A committed managed directory is missing during startup recovery.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())),
        Err(error) => Err(recovery_error(
            journal,
            "operation_recovery_inspect_failed",
            "Could not inspect a managed directory during startup recovery.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn inspect_uninstall_recovery_directory(
    journal: &OperationJournal,
    path: &Path,
    description: &str,
) -> TorbenResult<bool> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(true),
        Ok(_) => Err(recovery_error(
            journal,
            "operation_recovery_path_conflict",
            "An uninstall recovery path is not a plain directory.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(recovery_error(
            journal,
            "operation_recovery_inspect_failed",
            "Could not inspect an uninstall recovery path.",
        )
        .with_detail("description", description)
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn ensure_record_path(
    journal: &OperationJournal,
    record: &InstallRecord,
    expected: &std::path::Path,
) -> TorbenResult<()> {
    let actual = PathBuf::from(&record.install_path);
    if actual == expected {
        Ok(())
    } else {
        Err(recovery_error(
            journal,
            "operation_recovery_inconsistent",
            "The stored installation path is not the standard managed path.",
        )
        .with_detail("expectedPath", expected.display().to_string())
        .with_detail("actualPath", actual.display().to_string()))
    }
}

fn remove_managed_directory_if_exists(
    path: &std::path::Path,
    journal: &OperationJournal,
) -> TorbenResult<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            std::fs::remove_dir_all(path).map_err(|error| {
                recovery_error(
                    journal,
                    "operation_recovery_cleanup_failed",
                    "Could not clean an interrupted operation directory.",
                )
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
            })
        }
        Ok(_) => Err(recovery_error(
            journal,
            "operation_recovery_path_conflict",
            "An interrupted operation path is not a managed directory.",
        )
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(recovery_error(
            journal,
            "operation_recovery_inspect_failed",
            "Could not inspect an interrupted operation path.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn remove_interrupted_download_partials(
    paths: &TorbenPaths,
    app_id: &AppId,
    version: &ExactVersion,
    journal: &OperationJournal,
) -> TorbenResult<()> {
    let directory = paths.download_dir(app_id.as_str(), &version.to_string());
    match std::fs::symlink_metadata(&directory) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(recovery_error(
                journal,
                "operation_recovery_path_conflict",
                "The interrupted download cache path is not a managed directory.",
            )
            .with_detail("path", directory.display().to_string()));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(recovery_error(
                journal,
                "operation_recovery_inspect_failed",
                "Could not inspect the interrupted download cache.",
            )
            .with_detail("path", directory.display().to_string())
            .with_detail("reason", error.to_string()));
        }
    }

    for entry in std::fs::read_dir(&directory).map_err(|error| {
        recovery_error(
            journal,
            "operation_recovery_inspect_failed",
            "Could not inspect the interrupted download cache.",
        )
        .with_detail("path", directory.display().to_string())
        .with_detail("reason", error.to_string())
    })? {
        let entry = entry.map_err(|error| {
            recovery_error(
                journal,
                "operation_recovery_inspect_failed",
                "Could not inspect an interrupted download cache entry.",
            )
            .with_detail("path", directory.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("partial") {
            continue;
        }
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            recovery_error(
                journal,
                "operation_recovery_inspect_failed",
                "Could not inspect an interrupted partial download.",
            )
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(recovery_error(
                journal,
                "operation_recovery_path_conflict",
                "An interrupted partial download is not a regular managed cache file.",
            )
            .with_detail("path", path.display().to_string()));
        }
        std::fs::remove_file(&path).map_err(|error| {
            recovery_error(
                journal,
                "operation_recovery_cleanup_failed",
                "Could not remove an interrupted partial download.",
            )
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
    }
    Ok(())
}

fn recovery_error(
    journal: &OperationJournal,
    code: &'static str,
    message: &'static str,
) -> TorbenError {
    let mut error = TorbenError::new(code, message)
        .with_detail("operationId", journal.operation_id().to_string())
        .with_detail("kind", format!("{:?}", journal.kind()).to_ascii_lowercase());
    if let Some(app_id) = journal.app_id() {
        error = error.with_detail("appId", app_id.to_string());
    }
    if let Some(plugin_id) = journal.plugin_id() {
        error = error.with_detail("pluginId", plugin_id.to_string());
    }
    error
}

fn copy_plugin_directory(
    source: &std::path::Path,
    destination: &std::path::Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<()> {
    cancellation.check()?;
    std::fs::create_dir_all(destination).map_err(|error| {
        TorbenError::new("plugin_stage_failed", "Could not create plugin staging.")
            .with_detail("reason", error.to_string())
    })?;
    for entry in walkdir::WalkDir::new(source).follow_links(false) {
        cancellation.check()?;
        let entry = entry.map_err(|error| {
            TorbenError::new(
                "plugin_copy_failed",
                "Could not inspect the plugin package.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let relative = entry.path().strip_prefix(source).map_err(|error| {
            TorbenError::new(
                "plugin_copy_failed",
                "Could not resolve a plugin package path.",
            )
            .with_detail("reason", error.to_string())
        })?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = destination.join(relative);
        if entry.file_type().is_symlink() {
            return Err(TorbenError::new(
                "plugin_symlink_rejected",
                "Sideloaded plugin packages may not contain symbolic links.",
            ));
        }
        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|error| TorbenError::new("plugin_copy_failed", error.to_string()))?;
        } else if entry.file_type().is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|error| TorbenError::new("plugin_copy_failed", error.to_string()))?;
            }
            std::fs::copy(entry.path(), &target).map_err(|error| {
                TorbenError::new(
                    "plugin_copy_failed",
                    "Could not copy a plugin package file.",
                )
                .with_detail("reason", error.to_string())
            })?;
        }
    }
    Ok(())
}

fn stage_plugin_package(
    manifest_path: &std::path::Path,
    expected_manifest_json: &str,
    staging: &std::path::Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<()> {
    let source_root = manifest_path.parent().ok_or_else(|| {
        TorbenError::new(
            "plugin_manifest_path_invalid",
            "The plugin manifest has no parent directory.",
        )
    })?;
    let manifest_name = manifest_path.file_name().ok_or_else(|| {
        TorbenError::new(
            "plugin_manifest_path_invalid",
            "The plugin manifest has no file name.",
        )
    })?;
    if let Err(error) = copy_plugin_directory(source_root, staging, cancellation) {
        return Err(cleanup_failed_plugin_stage(staging, error));
    }
    // Official trust was established before staging. This pass verifies the copied target and
    // compares the complete manifest below; it must not infer origin from a signature field.
    let staged_plugin = match torben_plugin_host::PluginVerifier::developer_mode()
        .verify(&staging.join(manifest_name))
    {
        Ok(plugin) => plugin,
        Err(error) => return Err(cleanup_failed_plugin_stage(staging, error)),
    };
    if let Err(error) = cancellation.check() {
        return Err(cleanup_failed_plugin_stage(staging, error));
    }
    let staged_manifest_json = match serde_json::to_string(&staged_plugin.manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            let failure = TorbenError::internal("Could not serialize the staged plugin manifest.")
                .with_detail("reason", error.to_string());
            return Err(cleanup_failed_plugin_stage(staging, failure));
        }
    };
    if staged_manifest_json != expected_manifest_json {
        let error = TorbenError::new(
            "plugin_package_changed",
            "The plugin package changed while it was being staged.",
        )
        .with_remediation("Retry with a stable plugin package from a trusted publisher.");
        return Err(cleanup_failed_plugin_stage(staging, error));
    }
    Ok(())
}

fn plugin_install_receipt_path(paths: &TorbenPaths, operation_id: OperationId) -> PathBuf {
    paths
        .operation_dir()
        .join(format!("{operation_id}.plugin-install.receipt"))
}

fn plugin_install_receipt(
    operation_id: OperationId,
    plugin_id: &PluginId,
    version: &ExactVersion,
    final_path: &Path,
) -> PluginInstallReceipt {
    PluginInstallReceipt {
        schema_version: PLUGIN_INSTALL_RECEIPT_SCHEMA_VERSION,
        operation_id,
        plugin_id: plugin_id.clone(),
        version: version.clone(),
        final_path: final_path.to_path_buf(),
    }
}

fn validate_plugin_install_identity(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    plugin_id: &PluginId,
    version: &ExactVersion,
    destination: &Path,
) -> TorbenResult<PathBuf> {
    let expected = paths
        .plugin_dir()
        .join(plugin_id.as_str())
        .join(version.to_string());
    if journal.kind() != OperationKind::PluginInstall
        || journal.plugin_id() != Some(plugin_id)
        || journal.version() != Some(version)
        || destination != expected
    {
        return Err(TorbenError::new(
            "plugin_install_ownership_receipt_invalid",
            "The plugin installation result does not match its transaction identity.",
        )
        .with_detail("expectedPath", expected.display().to_string())
        .with_detail("recordedPath", destination.display().to_string())
        .with_remediation("Inspect the plugin target before retrying installation."));
    }
    Ok(expected)
}

fn write_plugin_install_receipt(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    record: &PluginRecord,
    destination: &Path,
) -> TorbenResult<()> {
    let final_path =
        validate_plugin_install_identity(paths, journal, &record.id, &record.version, destination)?;
    ensure_recovery_directory(journal, &final_path, "prepared plugin installation")?;
    let receipt = plugin_install_receipt(
        journal.operation_id(),
        &record.id,
        &record.version,
        &final_path,
    );
    let content = serde_json::to_vec(&receipt).map_err(|error| {
        plugin_install_receipt_error("Could not serialize the plugin-install ownership receipt.")
            .with_detail("reason", error.to_string())
    })?;
    let path = plugin_install_receipt_path(paths, journal.operation_id());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            plugin_install_receipt_error("Could not create the plugin-install ownership receipt.")
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
    file.write_all(&content).map_err(|error| {
        plugin_install_receipt_error("Could not write the plugin-install ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    file.sync_all().map_err(|error| {
        plugin_install_receipt_error("Could not sync the plugin-install ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })
}

fn validate_plugin_install_receipt(
    paths: &TorbenPaths,
    journal: &OperationJournal,
    plugin_id: &PluginId,
    version: &ExactVersion,
    final_path: &Path,
) -> TorbenResult<()> {
    let path = plugin_install_receipt_path(paths, journal.operation_id());
    let metadata = path.symlink_metadata().map_err(|error| {
        plugin_install_receipt_error("The plugin-install ownership receipt is unavailable.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > PLUGIN_INSTALL_RECEIPT_MAX_BYTES
    {
        return Err(plugin_install_receipt_error(
            "The plugin-install ownership receipt is not a bounded regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    let content = std::fs::read(&path).map_err(|error| {
        plugin_install_receipt_error("Could not read the plugin-install ownership receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let actual: PluginInstallReceipt = serde_json::from_slice(&content).map_err(|error| {
        plugin_install_receipt_error("The plugin-install ownership receipt is invalid.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    let expected = plugin_install_receipt(journal.operation_id(), plugin_id, version, final_path);
    if actual != expected {
        return Err(plugin_install_receipt_error(
            "The plugin-install journal does not match its ownership receipt.",
        ));
    }
    Ok(())
}

fn plugin_install_receipt_exists(paths: &TorbenPaths, operation_id: OperationId) -> bool {
    plugin_install_receipt_path(paths, operation_id)
        .symlink_metadata()
        .is_ok()
}

fn remove_plugin_install_receipt_if_present(
    paths: &TorbenPaths,
    operation_id: OperationId,
) -> TorbenResult<()> {
    let path = plugin_install_receipt_path(paths, operation_id);
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(&path).map_err(|error| {
                plugin_install_receipt_error(
                    "Could not remove the plugin-install ownership receipt.",
                )
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
            })
        }
        Ok(_) => Err(plugin_install_receipt_error(
            "The plugin-install ownership receipt is not a regular file.",
        )
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(plugin_install_receipt_error(
            "Could not inspect the plugin-install ownership receipt.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())),
    }
}

fn cleanup_plugin_install_artifacts(
    paths: &TorbenPaths,
    plugin_id: &PluginId,
    version: &ExactVersion,
    journal: &OperationJournal,
) -> TorbenResult<()> {
    let final_path = validate_plugin_install_identity(
        paths,
        journal,
        plugin_id,
        version,
        &paths
            .plugin_dir()
            .join(plugin_id.as_str())
            .join(version.to_string()),
    )?;
    let staging =
        paths
            .staging_dir()
            .join(format!("plugin-{}-{}", plugin_id, journal.operation_id()));
    remove_managed_directory_if_exists(&staging, journal)?;
    let final_present = match final_path.symlink_metadata() {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(recovery_error(
                journal,
                "operation_recovery_inspect_failed",
                "Could not inspect the plugin installation target.",
            )
            .with_detail("path", final_path.display().to_string())
            .with_detail("reason", error.to_string()));
        }
    };
    if final_present || plugin_install_receipt_exists(paths, journal.operation_id()) {
        validate_plugin_install_receipt(paths, journal, plugin_id, version, &final_path)?;
    }
    if final_present {
        remove_managed_directory_if_exists(&final_path, journal)?;
    }
    remove_plugin_install_receipt_if_present(paths, journal.operation_id())
}

fn plugin_install_receipt_error(message: &str) -> TorbenError {
    TorbenError::new("plugin_install_ownership_receipt_invalid", message)
        .with_remediation("Inspect the plugin target and operation journal before retrying.")
}

fn execute_plugin_install_transaction(
    paths: &TorbenPaths,
    store: &StateStore,
    manifest_path: &std::path::Path,
    destination: &std::path::Path,
    record: &PluginRecord,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    let cancellation = journal.cancellation_probe();
    if let Err(error) = cancellation.check() {
        record_plugin_install_failure(journal, &error)?;
        return Err(error);
    }
    journal.record(
        OperationState::Running,
        "stage",
        format!("Staging plugin {} {}", record.id, record.version),
        Some(0.35),
    )?;
    let staging =
        paths
            .staging_dir()
            .join(format!("plugin-{}-{}", record.id, journal.operation_id()));
    if let Err(error) = stage_plugin_package(
        manifest_path,
        &record.manifest_json,
        &staging,
        &cancellation,
    ) {
        record_plugin_install_failure(journal, &error)?;
        return Err(error);
    }
    journal.record(
        OperationState::Running,
        "commit_files",
        "Committing the verified plugin package",
        Some(0.65),
    )?;
    if let Some(parent) = destination.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        let failure = cleanup_failed_plugin_stage(
            &staging,
            TorbenError::new("plugin_directory_create_failed", error.to_string()),
        );
        record_plugin_install_failure(journal, &failure)?;
        return Err(failure);
    }
    if let Err(error) = std::fs::rename(&staging, destination) {
        let failure = cleanup_failed_plugin_stage(
            &staging,
            TorbenError::new(
                "plugin_commit_failed",
                "Could not commit the plugin installation.",
            )
            .with_detail("reason", error.to_string()),
        );
        record_plugin_install_failure(journal, &failure)?;
        return Err(failure);
    }
    if let Err(error) = write_plugin_install_receipt(paths, journal, record, destination) {
        record_plugin_install_failure(journal, &error)?;
        return Err(error);
    }
    if let Err(error) = cancellation.check() {
        let failure = cleanup_failed_plugin_commit(paths, record, journal, error);
        record_plugin_install_failure(journal, &failure)?;
        return Err(failure);
    }
    journal.record(
        OperationState::Running,
        "commit_state",
        "Committing plugin state",
        Some(0.85),
    )?;
    if let Err(error) = cancellation.check() {
        let failure = cleanup_failed_plugin_commit(paths, record, journal, error);
        record_plugin_install_failure(journal, &failure)?;
        return Err(failure);
    }
    if let Err(error) = store.upsert_plugin(record) {
        let failure = cleanup_failed_plugin_commit(paths, record, journal, error);
        record_plugin_install_failure(journal, &failure)?;
        return Err(failure);
    }
    remove_plugin_install_receipt_if_present(paths, journal.operation_id())?;
    journal.succeed(format!("Installed plugin {} {}", record.id, record.version))
}

fn cleanup_failed_plugin_stage(staging: &std::path::Path, error: TorbenError) -> TorbenError {
    match std::fs::remove_dir_all(staging) {
        Ok(()) => error,
        Err(cleanup_error) if cleanup_error.kind() == std::io::ErrorKind::NotFound => error,
        Err(cleanup_error) => TorbenError::new(
            "plugin_stage_cleanup_failed",
            "Plugin staging failed and the partial staging directory could not be removed.",
        )
        .with_detail("pluginErrorCode", error.code)
        .with_detail("pluginError", error.message)
        .with_detail("cleanupError", cleanup_error.to_string())
        .with_detail("path", staging.display().to_string())
        .with_remediation("Remove the partial staging directory before retrying installation."),
    }
}

fn cleanup_failed_plugin_commit(
    paths: &TorbenPaths,
    record: &PluginRecord,
    journal: &OperationJournal,
    error: TorbenError,
) -> TorbenError {
    match cleanup_plugin_install_artifacts(paths, &record.id, &record.version, journal) {
        Ok(()) => error,
        Err(cleanup_error) => TorbenError::new(
            "plugin_install_rollback_pending",
            "Plugin state could not be committed and filesystem rollback is incomplete.",
        )
        .with_detail("pluginId", record.id.to_string())
        .with_detail("stateErrorCode", error.code)
        .with_detail("stateError", error.message)
        .with_detail("cleanupErrorCode", cleanup_error.code)
        .with_detail("cleanupError", cleanup_error.message)
        .with_remediation("Restart Torben App to resume plugin installation recovery."),
    }
}

fn record_plugin_install_failure(
    journal: &mut OperationJournal,
    error: &TorbenError,
) -> TorbenResult<()> {
    if error.code == "operation_cancelled"
        || error
            .details
            .get("pluginErrorCode")
            .is_some_and(|code| code == "operation_cancelled")
        || error
            .details
            .get("stateErrorCode")
            .is_some_and(|code| code == "operation_cancelled")
    {
        journal.acknowledge_cancellation()?;
    }
    if matches!(
        error.code.as_str(),
        "plugin_stage_cleanup_failed"
            | "plugin_install_rollback_pending"
            | "plugin_install_ownership_receipt_invalid"
    ) {
        journal.fail(error)
    } else {
        journal.fail_and_rollback(error)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    pub id: String,
    pub healthy: bool,
    pub message: String,
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::TcpListener,
        path::{Path, PathBuf},
        str::FromStr,
        sync::{Arc, Mutex},
        thread,
    };

    use sha2::{Digest, Sha256};
    use tempfile::tempdir;
    use torben_contracts::{
        AppId, ExactVersion, InstallRecord, InstallScope, OperationId, OperationKind,
        OperationState, PackageCoordinate, PackageToManagedMigrationPlan, PluginId,
        ShellIntegrationState, ShellIntegrationStatus, SourceAction, SourceAdapterAvailability,
        SourceAdapterKind, SourceExecutionOutcome, SourceExecutionRequest, SourceId,
        SourceMigrationRequest, SourceOperationPlan, SourcePackageKind, SourcePackageVersion,
        TorbenError, TorbenResult,
        plugin::{
            InstallPlan, PluginCapability, PluginManifest, PluginOrigin, PluginPermissions,
            PluginTarget, UninstallPlan,
        },
    };

    use crate::bundled_shim::BundledShim;
    use crate::node_plugin::BundledPlugin;
    use crate::operation::OperationJournal;
    use crate::shell_integration::ShellIntegrationBackend;
    use crate::source_adapters::{
        AdapterCommands, CommandFuture, CommandOutput, SourceAdapterService, SourceCommandRunner,
    };

    use super::{
        PluginRecord, StateStore, TorbenCore, TorbenPaths, commit_staged_shims,
        execute_plugin_install_transaction, execute_uninstall_transaction,
        install_selection_shims_locked, install_shims_locked, package_request_from_managed_plan,
        package_to_managed_token, shell_integration_is_healthy, shim_destinations,
        source_adapter_is_healthy, source_migration_backup, stage_and_commit_shims,
        stage_managed_source, stage_shim_copies, validate_uninstall_plan,
        write_managed_install_receipt, write_managed_uninstall_receipt,
        write_package_to_managed_receipt, write_plugin_install_receipt,
        write_selection_shim_receipt,
    };

    fn node_identity() -> (AppId, ExactVersion) {
        (
            AppId::new("node").unwrap(),
            ExactVersion::from_str("24.19.0").unwrap(),
        )
    }

    fn install_record(
        paths: &TorbenPaths,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> InstallRecord {
        InstallRecord {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: SourceId::new("node.official").unwrap(),
            scope: InstallScope::Managed,
            install_path: paths
                .app_version_dir(app_id.as_str(), &version.to_string())
                .display()
                .to_string(),
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        }
    }

    #[derive(Clone)]
    struct FixtureSourceRunner {
        primary: PathBuf,
        query: PathBuf,
        health: PathBuf,
        state: Arc<Mutex<Option<String>>>,
        execute_success: bool,
        mutate_on_execute: bool,
        health_output: String,
        executions: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl SourceCommandRunner for FixtureSourceRunner {
        fn run(
            &self,
            executable: PathBuf,
            arguments: Vec<String>,
            _environment: BTreeMap<String, String>,
        ) -> CommandFuture {
            let this = self.clone();
            Box::pin(async move {
                if executable == this.query {
                    let state = this.state.lock().unwrap().clone();
                    return Ok(state.map_or(
                        CommandOutput {
                            success: false,
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                        },
                        |version| CommandOutput {
                            success: true,
                            stdout: format!("ii \t{version}\tamd64\n").into_bytes(),
                            stderr: Vec::new(),
                        },
                    ));
                }
                if executable == this.primary {
                    this.executions.lock().unwrap().push(arguments.clone());
                    if this.mutate_on_execute {
                        if arguments.iter().any(|argument| argument == "install") {
                            let version = arguments
                                .iter()
                                .position(|argument| argument == "install")
                                .and_then(|index| arguments.get(index + 1))
                                .and_then(|argument| argument.split_once('='))
                                .map(|(_, value)| value)
                                .ok_or_else(|| {
                                    TorbenError::internal("Fixture install version missing.")
                                })?;
                            *this.state.lock().unwrap() = Some(version.to_owned());
                        } else if arguments.iter().any(|argument| argument == "remove") {
                            *this.state.lock().unwrap() = None;
                        }
                    }
                    return Ok(CommandOutput {
                        success: this.execute_success,
                        stdout: Vec::new(),
                        stderr: if this.execute_success {
                            Vec::new()
                        } else {
                            b"fixture execution failed".to_vec()
                        },
                    });
                }
                if executable.file_name() == this.health.file_name() {
                    return Ok(CommandOutput {
                        success: true,
                        stdout: this.health_output.into_bytes(),
                        stderr: Vec::new(),
                    });
                }
                Err(TorbenError::internal("Unexpected fixture command."))
            })
        }
    }

    struct SourceFixture {
        service: SourceAdapterService,
        state: Arc<Mutex<Option<String>>>,
        executions: Arc<Mutex<Vec<Vec<String>>>>,
        health: PathBuf,
    }

    type MigrationExecutions = Arc<Mutex<Vec<(String, Vec<String>)>>>;

    #[derive(Clone)]
    struct MigrationFixtureRunner {
        apt_primary: PathBuf,
        apt_query: PathBuf,
        dnf_primary: PathBuf,
        rpm_query: PathBuf,
        old_health: PathBuf,
        target_health: PathBuf,
        old_state: Arc<Mutex<Option<String>>>,
        target_state: Arc<Mutex<Option<String>>>,
        target_install_success: bool,
        restore_success: bool,
        executions: MigrationExecutions,
    }

    impl SourceCommandRunner for MigrationFixtureRunner {
        fn run(
            &self,
            executable: PathBuf,
            arguments: Vec<String>,
            _environment: BTreeMap<String, String>,
        ) -> CommandFuture {
            let this = self.clone();
            Box::pin(async move {
                if executable == this.apt_query {
                    return Ok(this.old_state.lock().unwrap().clone().map_or(
                        CommandOutput {
                            success: false,
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                        },
                        |version| CommandOutput {
                            success: true,
                            stdout: format!("ii \t{version}\tamd64\n").into_bytes(),
                            stderr: Vec::new(),
                        },
                    ));
                }
                if executable == this.rpm_query {
                    return Ok(this.target_state.lock().unwrap().clone().map_or(
                        CommandOutput {
                            success: false,
                            stdout: Vec::new(),
                            stderr: Vec::new(),
                        },
                        |version| CommandOutput {
                            success: true,
                            stdout: format!("code\t{version}\tx86_64\n").into_bytes(),
                            stderr: Vec::new(),
                        },
                    ));
                }
                if executable == this.dnf_primary {
                    if arguments.iter().any(|argument| argument == "repoquery") {
                        return Ok(CommandOutput {
                            success: true,
                            stdout: b"code\t0\t1.134.0\t1.fc42\tx86_64\n".to_vec(),
                            stderr: Vec::new(),
                        });
                    }
                    this.executions
                        .lock()
                        .unwrap()
                        .push(("dnf".to_owned(), arguments.clone()));
                    if arguments.iter().any(|argument| argument == "install") {
                        *this.target_state.lock().unwrap() = Some("1.134.0-1.fc42".to_owned());
                        return Ok(CommandOutput {
                            success: this.target_install_success,
                            stdout: Vec::new(),
                            stderr: if this.target_install_success {
                                Vec::new()
                            } else {
                                b"fixture target failure".to_vec()
                            },
                        });
                    }
                    if arguments.iter().any(|argument| argument == "remove") {
                        *this.target_state.lock().unwrap() = None;
                    }
                    return Ok(CommandOutput {
                        success: true,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    });
                }
                if executable == this.apt_primary {
                    this.executions
                        .lock()
                        .unwrap()
                        .push(("apt".to_owned(), arguments.clone()));
                    if arguments.iter().any(|argument| argument == "remove") {
                        *this.old_state.lock().unwrap() = None;
                    } else if arguments.iter().any(|argument| argument == "install") {
                        if this.restore_success {
                            *this.old_state.lock().unwrap() = Some("1.134.0".to_owned());
                        } else {
                            return Ok(CommandOutput {
                                success: false,
                                stdout: Vec::new(),
                                stderr: b"fixture restore failure".to_vec(),
                            });
                        }
                    }
                    return Ok(CommandOutput {
                        success: true,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    });
                }
                if executable.file_name() == this.old_health.file_name()
                    || executable.file_name() == this.target_health.file_name()
                {
                    return Ok(CommandOutput {
                        success: true,
                        stdout: b"code 1.134.0\n".to_vec(),
                        stderr: Vec::new(),
                    });
                }
                Err(TorbenError::internal(
                    "Unexpected source migration fixture command.",
                ))
            })
        }
    }

    struct MigrationFixture {
        service: SourceAdapterService,
        old_state: Arc<Mutex<Option<String>>>,
        target_state: Arc<Mutex<Option<String>>>,
        executions: MigrationExecutions,
        old_health: PathBuf,
        target_health: PathBuf,
    }

    #[derive(Clone)]
    struct DnfFixtureRunner {
        primary: PathBuf,
        query: PathBuf,
        health: PathBuf,
        state: Arc<Mutex<Option<String>>>,
        executions: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl SourceCommandRunner for DnfFixtureRunner {
        fn run(
            &self,
            executable: PathBuf,
            arguments: Vec<String>,
            _environment: BTreeMap<String, String>,
        ) -> CommandFuture {
            let this = self.clone();
            Box::pin(async move {
                if executable == this.query {
                    let state = this.state.lock().unwrap().clone();
                    return Ok(state.map_or_else(
                        || CommandOutput {
                            success: false,
                            stdout: Vec::new(),
                            stderr: b"not installed".to_vec(),
                        },
                        |version| CommandOutput {
                            success: true,
                            stdout: format!("code\t{version}\tx86_64\n").into_bytes(),
                            stderr: Vec::new(),
                        },
                    ));
                }
                if executable == this.primary {
                    if arguments.iter().any(|argument| argument == "repoquery") {
                        return Ok(CommandOutput {
                            success: true,
                            stdout: b"code\t0\t1.134.0\t1.fc42\tx86_64\n".to_vec(),
                            stderr: Vec::new(),
                        });
                    }
                    this.executions.lock().unwrap().push(arguments.clone());
                    if arguments.iter().any(|argument| argument == "install") {
                        *this.state.lock().unwrap() = Some("1.134.0-1.fc42".to_owned());
                    } else if arguments.iter().any(|argument| argument == "remove") {
                        *this.state.lock().unwrap() = None;
                    }
                    return Ok(CommandOutput {
                        success: true,
                        stdout: Vec::new(),
                        stderr: Vec::new(),
                    });
                }
                if executable.file_name() == this.health.file_name() {
                    return Ok(CommandOutput {
                        success: true,
                        stdout: b"code 1.134.0\n".to_vec(),
                        stderr: Vec::new(),
                    });
                }
                Err(TorbenError::internal("Unexpected DNF fixture command."))
            })
        }
    }

    fn source_fixture(
        root: &Path,
        initial_version: Option<&str>,
        execute_success: bool,
        mutate_on_execute: bool,
        health_version: &str,
    ) -> SourceFixture {
        let primary = root.join("apt-get-fixture");
        let query = root.join("dpkg-query-fixture");
        let health = root.join(if cfg!(windows) { "code.exe" } else { "code" });
        std::fs::write(&health, b"fixture").unwrap();
        let state = Arc::new(Mutex::new(initial_version.map(str::to_owned)));
        let executions = Arc::new(Mutex::new(Vec::new()));
        let runner = FixtureSourceRunner {
            primary: primary.clone(),
            query: query.clone(),
            health: health.clone(),
            state: Arc::clone(&state),
            execute_success,
            mutate_on_execute,
            health_output: format!("code {health_version}\n"),
            executions: Arc::clone(&executions),
        };
        SourceFixture {
            service: SourceAdapterService::for_test(
                SourceAdapterKind::Apt,
                primary,
                Some(query),
                Arc::new(runner),
            ),
            state,
            executions,
            health,
        }
    }

    fn source_request(action: SourceAction, executable: Option<&Path>) -> SourceExecutionRequest {
        SourceExecutionRequest {
            app_id: AppId::new("vscode").unwrap(),
            app_version: ExactVersion::from_str("1.134.0").unwrap(),
            action,
            adapter: SourceAdapterKind::Apt,
            coordinate: PackageCoordinate::new("code").unwrap(),
            package_kind: SourcePackageKind::Native,
            package_version: Some(SourcePackageVersion::new("1.134.0").unwrap()),
            executable_path: executable.map(|path| path.display().to_string()),
            approved_execution_identity: None,
            accept_system_changes: true,
        }
    }

    fn dnf_source_fixture(root: &Path) -> SourceFixture {
        let primary = root.join("dnf-fixture");
        let query = root.join("rpm-fixture");
        let health = root.join(if cfg!(windows) { "code.exe" } else { "code" });
        std::fs::write(&health, b"fixture").unwrap();
        let state = Arc::new(Mutex::new(None));
        let executions = Arc::new(Mutex::new(Vec::new()));
        let runner = DnfFixtureRunner {
            primary: primary.clone(),
            query: query.clone(),
            health: health.clone(),
            state: Arc::clone(&state),
            executions: Arc::clone(&executions),
        };
        SourceFixture {
            service: SourceAdapterService::for_test(
                SourceAdapterKind::Dnf,
                primary,
                Some(query),
                Arc::new(runner),
            ),
            state,
            executions,
            health,
        }
    }

    fn dnf_source_request(
        action: SourceAction,
        executable: Option<&Path>,
    ) -> SourceExecutionRequest {
        let mut request = source_request(action, executable);
        request.adapter = SourceAdapterKind::Dnf;
        request.package_version = Some(SourcePackageVersion::new("1.134.0-1.fc42").unwrap());
        request.approved_execution_identity = Some("code-1.134.0-1.fc42.x86_64".to_owned());
        request
    }

    fn migration_fixture(root: &Path, target_install_success: bool) -> MigrationFixture {
        migration_fixture_with_restore(root, target_install_success, true)
    }

    fn migration_fixture_with_restore(
        root: &Path,
        target_install_success: bool,
        restore_success: bool,
    ) -> MigrationFixture {
        let apt_primary = root.join("apt-get-migration-fixture");
        let apt_query = root.join("dpkg-query-migration-fixture");
        let dnf_primary = root.join("dnf-migration-fixture");
        let rpm_query = root.join("rpm-migration-fixture");
        let old_health = root
            .join("old")
            .join(if cfg!(windows) { "code.exe" } else { "code" });
        let target_health =
            root.join("target")
                .join(if cfg!(windows) { "code.exe" } else { "code" });
        std::fs::create_dir_all(old_health.parent().unwrap()).unwrap();
        std::fs::create_dir_all(target_health.parent().unwrap()).unwrap();
        std::fs::write(&old_health, b"fixture").unwrap();
        std::fs::write(&target_health, b"fixture").unwrap();
        let old_state = Arc::new(Mutex::new(Some("1.134.0".to_owned())));
        let target_state = Arc::new(Mutex::new(None));
        let executions = Arc::new(Mutex::new(Vec::new()));
        let runner = Arc::new(MigrationFixtureRunner {
            apt_primary: apt_primary.clone(),
            apt_query: apt_query.clone(),
            dnf_primary: dnf_primary.clone(),
            rpm_query: rpm_query.clone(),
            old_health: old_health.clone(),
            target_health: target_health.clone(),
            old_state: Arc::clone(&old_state),
            target_state: Arc::clone(&target_state),
            target_install_success,
            restore_success,
            executions: Arc::clone(&executions),
        });
        let service = SourceAdapterService::for_tests(
            BTreeMap::from([
                (
                    SourceAdapterKind::Apt,
                    AdapterCommands {
                        primary: apt_primary,
                        query: Some(apt_query),
                    },
                ),
                (
                    SourceAdapterKind::Dnf,
                    AdapterCommands {
                        primary: dnf_primary,
                        query: Some(rpm_query),
                    },
                ),
            ]),
            runner,
        );
        MigrationFixture {
            service,
            old_state,
            target_state,
            executions,
            old_health,
            target_health,
        }
    }

    fn seed_package_owner(core: &TorbenCore, fixture: &MigrationFixture) {
        let app_id = AppId::new("vscode").unwrap();
        let version = ExactVersion::from_str("1.134.0").unwrap();
        let source_id = SourceId::new("source.apt").unwrap();
        let installed_at = "fixture".to_owned();
        let installation = InstallRecord {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: source_id.clone(),
            scope: InstallScope::PackageManager,
            install_path: fixture.old_health.parent().unwrap().display().to_string(),
            installed_at: installed_at.clone(),
            health: "healthy".to_owned(),
        };
        let package = torben_contracts::PackageInstallationRecord {
            app_id,
            app_version: version,
            source_id,
            adapter: SourceAdapterKind::Apt,
            coordinate: PackageCoordinate::new("code-old").unwrap(),
            package_kind: SourcePackageKind::Native,
            package_version: SourcePackageVersion::new("1.134.0").unwrap(),
            architecture: "amd64".to_owned(),
            executable_path: fixture.old_health.display().to_string(),
            owned_by_torben: true,
            installed_at,
            health: "healthy".to_owned(),
        };
        core.store
            .commit_package_installation(&installation, &package)
            .unwrap();
    }

    fn migration_request(fixture: &MigrationFixture) -> SourceMigrationRequest {
        SourceMigrationRequest {
            app_id: AppId::new("vscode").unwrap(),
            app_version: ExactVersion::from_str("1.134.0").unwrap(),
            target_adapter: SourceAdapterKind::Dnf,
            target_coordinate: PackageCoordinate::new("code").unwrap(),
            target_package_kind: SourcePackageKind::Native,
            target_package_version: Some(SourcePackageVersion::new("1.134.0-1.fc42").unwrap()),
            target_executable_path: fixture.target_health.display().to_string(),
            approved_plan_token: None,
            accept_system_changes: false,
        }
    }

    fn seed_managed_vscode(core: &TorbenCore) -> InstallRecord {
        let app_id = AppId::new("vscode").unwrap();
        let version = ExactVersion::from_str("1.134.0").unwrap();
        let install_path = core
            .paths
            .app_version_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&install_path).unwrap();
        std::fs::write(install_path.join("managed-fixture"), b"managed").unwrap();
        let record = InstallRecord {
            app_id,
            version,
            source_id: SourceId::new("vscode.official").unwrap(),
            scope: InstallScope::Managed,
            install_path: install_path.display().to_string(),
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        };
        core.store.add_installation(&record).unwrap();
        record
    }

    async fn package_to_managed_plan(
        core: &TorbenCore,
        fixture: &MigrationFixture,
    ) -> PackageToManagedMigrationPlan {
        let current_owner = core.package_installations().unwrap().remove(0);
        let current_state = fixture
            .service
            .inspect(
                current_owner.adapter,
                current_owner.coordinate.clone(),
                current_owner.package_kind,
            )
            .await
            .unwrap();
        let uninstall_current = fixture
            .service
            .reviewed_plan(
                SourceAction::Uninstall,
                current_owner.adapter,
                current_owner.coordinate.clone(),
                current_owner.package_kind,
                Some(current_owner.package_version.clone()),
            )
            .await
            .unwrap();
        let restore_current = fixture
            .service
            .reviewed_plan(
                SourceAction::Install,
                current_owner.adapter,
                current_owner.coordinate.clone(),
                current_owner.package_kind,
                Some(current_owner.package_version.clone()),
            )
            .await
            .unwrap();
        let managed_target = core.paths.app_version_dir(
            current_owner.app_id.as_str(),
            &current_owner.app_version.to_string(),
        );
        let mut plan = PackageToManagedMigrationPlan {
            app_id: current_owner.app_id.clone(),
            app_version: current_owner.app_version.clone(),
            current_owner,
            current_state,
            uninstall_current,
            restore_current,
            install_managed: InstallPlan {
                app_id: AppId::new("vscode").unwrap(),
                version: ExactVersion::from_str("1.134.0").unwrap(),
                source_id: SourceId::new("vscode.official").unwrap(),
                steps: Vec::new(),
                metadata: BTreeMap::new(),
            },
            managed_target_path: managed_target.display().to_string(),
            approval_token: String::new(),
            warnings: Vec::new(),
        };
        plan.approval_token = package_to_managed_token(&plan).unwrap();
        plan
    }

    fn start_interrupted(
        paths: &TorbenPaths,
        kind: OperationKind,
        app_id: &AppId,
        version: Option<&ExactVersion>,
    ) -> OperationId {
        let journal =
            OperationJournal::start(paths, operation_store(paths), kind, app_id, version).unwrap();
        journal.operation_id()
    }

    fn operation_store(paths: &TorbenPaths) -> Arc<StateStore> {
        Arc::new(StateStore::open(paths.state_database()).unwrap())
    }

    struct FakeShellIntegration {
        state: Mutex<ShellIntegrationState>,
    }

    impl FakeShellIntegration {
        const fn new(state: ShellIntegrationState) -> Self {
            Self {
                state: Mutex::new(state),
            }
        }

        fn result(&self, shim_path: &Path) -> ShellIntegrationStatus {
            ShellIntegrationStatus {
                state: *self.state.lock().unwrap(),
                shim_path: shim_path.display().to_string(),
                targets: vec!["fixture".to_owned()],
                new_terminal_required: false,
            }
        }
    }

    impl ShellIntegrationBackend for FakeShellIntegration {
        fn status(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
            Ok(self.result(shim_path))
        }

        fn enable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
            *self.state.lock().unwrap() = ShellIntegrationState::Managed;
            Ok(self.result(shim_path))
        }

        fn disable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
            *self.state.lock().unwrap() = ShellIntegrationState::Disabled;
            Ok(self.result(shim_path))
        }
    }

    fn operation_states(core: &TorbenCore, operation_id: OperationId) -> Vec<OperationState> {
        let mut events = core
            .operation_events()
            .unwrap()
            .into_iter()
            .filter(|event| event.operation_id == operation_id)
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.sequence);
        events.into_iter().map(|event| event.state).collect()
    }

    fn operation_states_from_store(
        store: &StateStore,
        operation_id: OperationId,
    ) -> Vec<OperationState> {
        let mut events = OperationJournal::list(store)
            .unwrap()
            .into_iter()
            .filter(|event| event.operation_id == operation_id)
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.sequence);
        events.into_iter().map(|event| event.state).collect()
    }

    fn prepare_selection_shim_transaction(
        paths: &TorbenPaths,
        journal: &OperationJournal,
        source: &Path,
    ) -> (Vec<PathBuf>, PathBuf, PathBuf) {
        let destinations = shim_destinations(paths);
        let staging = paths
            .staging_dir()
            .join(format!("shims-{}", journal.operation_id()));
        let backup = staging.join("backup");
        std::fs::create_dir_all(&backup).unwrap();
        let source_sha256 = stage_shim_copies(source, &destinations, &staging).unwrap();
        write_selection_shim_receipt(
            paths,
            journal,
            &staging,
            &backup,
            &source_sha256,
            &destinations,
        )
        .unwrap();
        (destinations, staging, backup)
    }

    fn seed_installation(core: &TorbenCore) -> (AppId, ExactVersion) {
        let (app_id, version) = node_identity();
        let record = install_record(&core.paths, &app_id, &version);
        core.store.add_installation(&record).unwrap();
        std::fs::create_dir_all(&record.install_path).unwrap();
        (app_id, version)
    }

    fn sideloaded_plugin_manifest() -> PluginManifest {
        PluginManifest {
            id: PluginId::new("dev.example.fixture").unwrap(),
            display_name: "Fixture plugin".to_owned(),
            version: ExactVersion::from_str("1.2.3").unwrap(),
            protocol_version: torben_contracts::plugin::PLUGIN_PROTOCOL_VERSION,
            minimum_host_version: ExactVersion::from_str("0.1.0").unwrap(),
            publisher: "Example Publisher".to_owned(),
            capabilities: vec![PluginCapability::SchemaUi],
            permissions: PluginPermissions {
                network_domains: vec!["example.invalid".to_owned()],
                filesystem_roots: vec!["managed_app_library".to_owned()],
                external_commands: vec!["fixture".to_owned()],
                package_managers: vec![],
            },
            targets: vec![],
            signature: None,
            revoked: false,
        }
    }

    fn seed_sideloaded_plugin(core: &TorbenCore, manifest_json: String) {
        core.store
            .upsert_plugin(&sideloaded_plugin_record(manifest_json))
            .unwrap();
    }

    fn sideloaded_plugin_record(manifest_json: String) -> PluginRecord {
        let manifest = sideloaded_plugin_manifest();
        PluginRecord {
            id: manifest.id,
            version: manifest.version,
            enabled: true,
            manifest_json,
            origin: PluginOrigin::Sideloaded,
        }
    }

    #[test]
    fn lists_plugins_with_their_persisted_origin() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let manifest = sideloaded_plugin_manifest();
        seed_sideloaded_plugin(&core, serde_json::to_string(&manifest).unwrap());
        let mut official_manifest = sideloaded_plugin_manifest();
        official_manifest.id = PluginId::new("app.example.official").unwrap();
        official_manifest.display_name = "Official fixture".to_owned();
        core.store
            .upsert_plugin(&PluginRecord {
                id: official_manifest.id.clone(),
                version: official_manifest.version.clone(),
                enabled: true,
                manifest_json: serde_json::to_string(&official_manifest).unwrap(),
                origin: PluginOrigin::OfficialRegistry,
            })
            .unwrap();

        let plugins = core.plugins().unwrap();

        assert_eq!(plugins.len(), 8);
        assert_eq!(plugins[0].origin, PluginOrigin::BuiltIn);
        assert_eq!(plugins[0].id.as_str(), "app.torben.plugin.node");
        assert_eq!(plugins[1].origin, PluginOrigin::BuiltIn);
        assert_eq!(plugins[1].id.as_str(), "app.torben.plugin.temurin");
        assert_eq!(plugins[2].origin, PluginOrigin::BuiltIn);
        assert_eq!(plugins[2].id.as_str(), "app.torben.plugin.python");
        assert_eq!(plugins[3].origin, PluginOrigin::BuiltIn);
        assert_eq!(plugins[3].id.as_str(), "app.torben.plugin.git");
        assert_eq!(plugins[4].origin, PluginOrigin::BuiltIn);
        assert_eq!(plugins[4].id.as_str(), "app.torben.plugin.vscode");
        assert_eq!(plugins[5].origin, PluginOrigin::BuiltIn);
        assert_eq!(plugins[5].id.as_str(), "app.torben.plugin.codex");
        assert_eq!(plugins[6].origin, PluginOrigin::OfficialRegistry);
        assert_eq!(plugins[6].display_name, "Official fixture");
        assert_eq!(plugins[7].origin, PluginOrigin::Sideloaded);
        assert_eq!(plugins[7].display_name, "Fixture plugin");
        assert_eq!(plugins[7].permissions.network_domains, ["example.invalid"]);
    }

    #[test]
    fn managed_auto_update_preferences_are_idempotent_and_sorted() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let node = AppId::new("node").unwrap();
        let codex = AppId::new("codex").unwrap();

        core.set_managed_auto_update(&node, true).unwrap();
        core.set_managed_auto_update(&codex, true).unwrap();
        core.set_managed_auto_update(&node, true).unwrap();
        assert_eq!(
            core.user_settings()
                .unwrap()
                .updates
                .automatically_update_apps,
            [codex.clone(), node.clone()]
        );

        core.set_managed_auto_update(&node, false).unwrap();
        assert_eq!(
            core.user_settings()
                .unwrap()
                .updates
                .automatically_update_apps,
            [codex]
        );
    }

    #[tokio::test]
    async fn managed_update_does_not_replace_a_concurrent_selection() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let (app_id, selected) = seed_installation(&core);
        core.store.set_selection(&app_id, &selected).unwrap();
        let previously_observed = ExactVersion::from_str("22.22.3").unwrap();
        let available = ExactVersion::from_str("24.20.1").unwrap();

        assert!(
            !core
                .select_if_current(&app_id, &previously_observed, &available)
                .await
                .unwrap()
        );
        assert_eq!(core.selected_version(&app_id).unwrap(), Some(selected));
    }

    #[test]
    fn direct_manifest_install_requires_explicit_developer_mode() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();

        let error = core
            .install_plugin(&root.path().join("plugin.json"), false)
            .unwrap_err();

        assert_eq!(error.code, "developer_mode_required");
    }

    #[test]
    fn official_install_requires_a_build_time_registry_key() {
        let root = tempdir().unwrap();
        let mut core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        core.official_registry_key = None;

        let error = core
            .install_official_plugin(
                &root.path().join("registry.json"),
                &PluginId::new("app.example.official").unwrap(),
                None,
            )
            .unwrap_err();

        assert_eq!(error.code, "official_registry_key_unavailable");
    }

    #[test]
    fn registry_status_reports_an_unconfigured_uncached_build() {
        let root = tempdir().unwrap();
        let mut core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        core.official_registry_key = None;
        core.official_registry_url = None;

        let status = core.official_plugin_registry_status().unwrap();

        assert!(!status.configured);
        assert_eq!(status.sequence, None);
        assert_eq!(status.generated_at, None);
        assert_eq!(status.source_url, None);
    }

    #[tokio::test]
    async fn signed_network_registry_installs_through_the_shared_plugin_transaction() {
        const ROOT_KEY: &str = "6kpsY+KcUgq+9VB7Ey7F+ZVHdq6+vnuSQh7qaRRG0iw=";
        const REGISTRY: &[u8] = include_bytes!("../tests/fixtures/plugin-registry/registry.json");
        const MANIFEST: &[u8] = include_bytes!(
            "../tests/fixtures/plugin-registry/packages/online-fixture/1.2.3/plugin.json"
        );
        const EXECUTABLE: &[u8] = include_bytes!("../tests/fixtures/plugin-registry/plugin.bin");

        let executable_path = if cfg!(windows) {
            format!("bin/windows-{}/plugin.exe", std::env::consts::ARCH)
        } else {
            format!(
                "bin/{}-{}/plugin",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        };
        let routes = BTreeMap::from([
            ("/registry.json".to_owned(), REGISTRY.to_vec()),
            (
                "/packages/online-fixture/1.2.3/plugin.json".to_owned(),
                MANIFEST.to_vec(),
            ),
            (
                format!("/packages/online-fixture/1.2.3/{executable_path}"),
                EXECUTABLE.to_vec(),
            ),
        ]);
        let (registry_url, server) = serve_http_routes(routes);
        let root = tempdir().unwrap();
        let mut core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        core.official_registry_key = Some(ROOT_KEY.to_owned());
        core.official_registry_url = Some(registry_url);
        core.official_registry_fixture_mode = true;
        let plugin_id = PluginId::new("app.example.online-fixture").unwrap();

        let installed = core
            .install_official_plugin_from_registry(&plugin_id, None)
            .await
            .unwrap();
        server.join().unwrap();

        assert_eq!(installed.id, plugin_id);
        assert_eq!(installed.origin, PluginOrigin::OfficialRegistry);
        assert_eq!(
            core.official_plugin_registry_status().unwrap().sequence,
            Some(1)
        );
        assert!(
            core.paths
                .plugin_dir()
                .join("app.example.online-fixture/1.2.3")
                .join(executable_path)
                .is_file()
        );
        assert!(core.operation_events().unwrap().iter().any(|event| {
            event.state == OperationState::Succeeded
                && event.message == "Installed plugin app.example.online-fixture 1.2.3"
        }));
    }

    #[test]
    fn bundled_plugin_cannot_be_disabled() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let plugin_id = PluginId::new("app.torben.plugin.node").unwrap();

        let error = core.set_plugin_enabled(&plugin_id, false).unwrap_err();

        assert_eq!(error.code, "plugin_built_in_immutable");
        assert!(core.plugins().unwrap()[0].enabled);
    }

    #[tokio::test]
    async fn bundled_node_plugin_exposes_validated_schema_pages() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let plugin_id = PluginId::new("app.torben.plugin.node").unwrap();

        let pages = core.plugin_schema_pages(&plugin_id).await.unwrap();

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, "node");
        assert_eq!(pages[0].sections[0].id, "trust");
        assert_eq!(pages[0].sections[0].fields.len(), 3);
    }

    #[tokio::test]
    async fn bundled_temurin_plugin_exposes_validated_schema_pages() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let plugin_id = PluginId::new("app.torben.plugin.temurin").unwrap();

        let pages = core.plugin_schema_pages(&plugin_id).await.unwrap();

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, "temurin");
        assert_eq!(pages[0].sections[0].fields.len(), 3);
    }

    #[tokio::test]
    async fn bundled_python_plugin_exposes_validated_schema_pages() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let plugin_id = PluginId::new("app.torben.plugin.python").unwrap();

        let pages = core.plugin_schema_pages(&plugin_id).await.unwrap();

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, "python");
        assert_eq!(pages[0].sections[0].fields.len(), 4);
    }

    #[tokio::test]
    async fn bundled_git_plugin_exposes_validated_schema_pages() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let plugin_id = PluginId::new("app.torben.plugin.git").unwrap();

        let pages = core.plugin_schema_pages(&plugin_id).await.unwrap();

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, "git");
        assert_eq!(pages[0].sections[0].fields.len(), 4);
    }

    #[tokio::test]
    async fn bundled_vscode_plugin_exposes_validated_schema_pages() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let plugin_id = PluginId::new("app.torben.plugin.vscode").unwrap();

        let pages = core.plugin_schema_pages(&plugin_id).await.unwrap();

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, "vscode");
        assert_eq!(pages[0].sections[0].fields.len(), 4);
    }

    #[tokio::test]
    async fn bundled_codex_plugin_exposes_validated_schema_pages() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let plugin_id = PluginId::new("app.torben.plugin.codex").unwrap();

        let pages = core.plugin_schema_pages(&plugin_id).await.unwrap();

        assert_eq!(pages.len(), 1);
        assert_eq!(pages[0].id, "codex");
        assert_eq!(pages[0].sections[0].fields.len(), 4);
    }

    #[tokio::test]
    async fn installed_plugin_schema_action_round_trips_through_json_rpc() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let plugin_id = PluginId::new("dev.example.schema").unwrap();
        let version = ExactVersion::from_str("1.0.0").unwrap();
        let package = core
            .paths
            .plugin_dir()
            .join(plugin_id.as_str())
            .join(version.to_string());
        std::fs::create_dir_all(&package).unwrap();
        let executable_name = if cfg!(windows) {
            "schema-fixture.cmd"
        } else {
            "schema-fixture"
        };
        let executable = package.join(executable_name);
        std::fs::write(&executable, schema_fixture_script()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&executable, permissions).unwrap();
        }
        let manifest = PluginManifest {
            id: plugin_id.clone(),
            display_name: "Schema fixture".to_owned(),
            version: version.clone(),
            protocol_version: torben_contracts::plugin::PLUGIN_PROTOCOL_VERSION,
            minimum_host_version: ExactVersion::from_str("0.1.0").unwrap(),
            publisher: "Fixture".to_owned(),
            capabilities: vec![PluginCapability::SchemaUi],
            permissions: PluginPermissions::default(),
            targets: vec![PluginTarget {
                target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                executable: executable_name.to_owned(),
                sha256: hex::encode(Sha256::digest(std::fs::read(&executable).unwrap())),
            }],
            signature: None,
            revoked: false,
        };
        let manifest_json = serde_json::to_string(&manifest).unwrap();
        std::fs::write(
            package.join("plugin.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        core.store
            .upsert_plugin(&PluginRecord {
                id: plugin_id.clone(),
                version,
                enabled: true,
                manifest_json,
                origin: PluginOrigin::Sideloaded,
            })
            .unwrap();

        let pages = core.plugin_schema_pages(&plugin_id).await.unwrap();
        let result = core
            .invoke_plugin_schema_action(
                &plugin_id,
                "settings",
                "general",
                "apply",
                BTreeMap::from([("mode".to_owned(), "fast".to_owned())]),
                false,
            )
            .await
            .unwrap();

        assert_eq!(pages[0].id, "settings");
        assert_eq!(result.message.as_deref(), Some("Applied"));
        assert_eq!(
            result.page.sections[0].fields[0].value.as_deref(),
            Some("fast")
        );
    }

    #[test]
    fn invalid_stored_plugin_manifest_returns_a_structured_error() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        seed_sideloaded_plugin(&core, "{}".to_owned());

        let error = core.plugins().unwrap_err();

        assert_eq!(error.code, "plugin_manifest_state_invalid");
        assert_eq!(
            error.details.get("pluginId").map(String::as_str),
            Some("dev.example.fixture")
        );
    }

    #[test]
    fn developer_mode_rechecks_staging_and_does_not_infer_official_origin() {
        let root = tempdir().unwrap();
        let package = root.path().join("plugin-package");
        std::fs::create_dir_all(&package).unwrap();
        let executable = package.join("fixture-plugin.bin");
        std::fs::write(&executable, b"trusted fixture executable").unwrap();
        let mut manifest = sideloaded_plugin_manifest();
        manifest.signature = Some("publisher-controlled-signature-field".to_owned());
        manifest.targets.push(PluginTarget {
            target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            executable: "fixture-plugin.bin".to_owned(),
            sha256: hex::encode(Sha256::digest(b"trusted fixture executable")),
        });
        let manifest_path = package.join("plugin.json");
        std::fs::write(&manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();

        let installed = core.install_plugin(&manifest_path, true).unwrap();

        assert_eq!(installed.origin, PluginOrigin::Sideloaded);
        assert_eq!(installed.display_name, "Fixture plugin");
        assert!(
            core.paths
                .plugin_dir()
                .join("dev.example.fixture")
                .join("1.2.3")
                .join("fixture-plugin.bin")
                .is_file()
        );
        assert_eq!(core.plugins().unwrap().len(), 7);

        let duplicate = core.install_plugin(&manifest_path, true).unwrap_err();
        assert_eq!(duplicate.code, "plugin_already_installed");
        assert_eq!(
            duplicate
                .details
                .get("installedVersion")
                .map(String::as_str),
            Some("1.2.3")
        );
        assert!(core.operation_events().unwrap().iter().any(|event| {
            event.state == OperationState::Succeeded
                && event.message == "Installed plugin dev.example.fixture 1.2.3"
        }));
    }

    #[test]
    fn cancelled_plugin_install_uses_the_shared_rollback_state_sequence() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = operation_store(&paths);
        let manifest = sideloaded_plugin_manifest();
        let record = sideloaded_plugin_record(serde_json::to_string(&manifest).unwrap());
        let mut journal =
            OperationJournal::start_plugin(&paths, Arc::clone(&store), &record.id, &record.version)
                .unwrap();
        let operation_id = journal.operation_id();
        OperationJournal::request_cancellation(&paths, &store, operation_id).unwrap();
        let destination = paths
            .plugin_dir()
            .join(record.id.as_str())
            .join(record.version.to_string());

        let error = execute_plugin_install_transaction(
            &paths,
            &store,
            root.path().join("unused-manifest.json").as_path(),
            &destination,
            &record,
            &mut journal,
        )
        .unwrap_err();

        assert_eq!(error.code, "operation_cancelled");
        assert!(!destination.exists());
        let mut events = OperationJournal::list(&store)
            .unwrap()
            .into_iter()
            .filter(|event| event.operation_id == operation_id)
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.sequence);
        let states = events
            .into_iter()
            .map(|event| event.state)
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            [
                OperationState::Running,
                OperationState::Cancelling,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[test]
    fn startup_rolls_back_an_uncommitted_plugin_installation() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let manifest = sideloaded_plugin_manifest();
        let journal = OperationJournal::start_plugin(
            &paths,
            operation_store(&paths),
            &manifest.id,
            &manifest.version,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let staged = paths
            .staging_dir()
            .join(format!("plugin-{}-{operation_id}", manifest.id));
        let installed = paths
            .plugin_dir()
            .join(manifest.id.as_str())
            .join(manifest.version.to_string());
        std::fs::create_dir_all(&staged).unwrap();

        let core = TorbenCore::open(paths).unwrap();

        assert!(!staged.exists());
        assert!(!installed.exists());
        assert!(core.store.get_plugin(&manifest.id).unwrap().is_none());
        assert_eq!(
            operation_states(&core, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[test]
    fn startup_removes_a_receipt_bound_plugin_without_state() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let manifest = sideloaded_plugin_manifest();
        let journal = OperationJournal::start_plugin(
            &paths,
            operation_store(&paths),
            &manifest.id,
            &manifest.version,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let installed = paths
            .plugin_dir()
            .join(manifest.id.as_str())
            .join(manifest.version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        let record = sideloaded_plugin_record(serde_json::to_string(&manifest).unwrap());
        write_plugin_install_receipt(&paths, &journal, &record, &installed).unwrap();
        drop(journal);

        let core = TorbenCore::open(paths).unwrap();

        assert!(!installed.exists());
        assert!(core.store.get_plugin(&manifest.id).unwrap().is_none());
        assert_eq!(
            operation_states(&core, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[test]
    fn startup_preserves_an_unowned_plugin_target_without_a_receipt() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let manifest = sideloaded_plugin_manifest();
        let journal = OperationJournal::start_plugin(
            &paths,
            operation_store(&paths),
            &manifest.id,
            &manifest.version,
        )
        .unwrap();
        drop(journal);
        let installed = paths
            .plugin_dir()
            .join(manifest.id.as_str())
            .join(manifest.version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("must-not-delete"), b"unowned").unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "plugin_install_ownership_receipt_invalid");
        assert!(installed.join("must-not-delete").is_file());
    }

    #[test]
    fn startup_preserves_a_plugin_target_when_its_receipt_mismatches() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let manifest = sideloaded_plugin_manifest();
        let journal = OperationJournal::start_plugin(
            &paths,
            operation_store(&paths),
            &manifest.id,
            &manifest.version,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let installed = paths
            .plugin_dir()
            .join(manifest.id.as_str())
            .join(manifest.version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("must-not-delete"), b"plugin").unwrap();
        let record = sideloaded_plugin_record(serde_json::to_string(&manifest).unwrap());
        write_plugin_install_receipt(&paths, &journal, &record, &installed).unwrap();
        drop(journal);
        let receipt_path = paths
            .operation_dir()
            .join(format!("{operation_id}.plugin-install.receipt"));
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        receipt["finalPath"] = serde_json::json!(root.path().join("tampered"));
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "plugin_install_ownership_receipt_invalid");
        assert!(installed.join("must-not-delete").is_file());
    }

    #[test]
    fn startup_completes_a_committed_plugin_installation() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let manifest = sideloaded_plugin_manifest();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .upsert_plugin(&sideloaded_plugin_record(
                serde_json::to_string(&manifest).unwrap(),
            ))
            .unwrap();
        drop(store);
        let installed = paths
            .plugin_dir()
            .join(manifest.id.as_str())
            .join(manifest.version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        let journal = OperationJournal::start_plugin(
            &paths,
            operation_store(&paths),
            &manifest.id,
            &manifest.version,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let staged = paths
            .staging_dir()
            .join(format!("plugin-{}-{operation_id}", manifest.id));
        std::fs::create_dir_all(&staged).unwrap();

        let core = TorbenCore::open(paths).unwrap();

        assert!(installed.is_dir());
        assert!(!staged.exists());
        assert!(core.store.get_plugin(&manifest.id).unwrap().is_some());
        assert_eq!(
            operation_states(&core, operation_id),
            [OperationState::Running, OperationState::Succeeded]
        );
    }

    #[test]
    fn startup_rejects_committed_plugin_state_without_its_directory() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let manifest = sideloaded_plugin_manifest();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .upsert_plugin(&sideloaded_plugin_record(
                serde_json::to_string(&manifest).unwrap(),
            ))
            .unwrap();
        drop(store);
        OperationJournal::start_plugin(
            &paths,
            operation_store(&paths),
            &manifest.id,
            &manifest.version,
        )
        .unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_inconsistent");
        assert_eq!(
            error.details.get("pluginId").map(String::as_str),
            Some("dev.example.fixture")
        );
    }

    #[test]
    fn startup_rejects_a_committed_plugin_path_conflict() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let manifest = sideloaded_plugin_manifest();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .upsert_plugin(&sideloaded_plugin_record(
                serde_json::to_string(&manifest).unwrap(),
            ))
            .unwrap();
        drop(store);
        let installed = paths
            .plugin_dir()
            .join(manifest.id.as_str())
            .join(manifest.version.to_string());
        std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
        std::fs::write(&installed, b"not a managed directory").unwrap();
        OperationJournal::start_plugin(
            &paths,
            operation_store(&paths),
            &manifest.id,
            &manifest.version,
        )
        .unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_path_conflict");
        assert!(installed.is_file());
    }

    #[test]
    fn installs_and_updates_all_command_shims() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let source = root.path().join("torben-shim-fixture");
        std::fs::write(&source, b"shim-v1").unwrap();

        let installed = install_shims_locked(&paths, &source).unwrap();

        assert_eq!(installed.len(), 12);
        for destination in shim_destinations(&paths) {
            assert_eq!(std::fs::read(destination).unwrap(), b"shim-v1");
        }

        std::fs::write(&source, b"shim-v2").unwrap();
        install_shims_locked(&paths, &source).unwrap();

        for destination in shim_destinations(&paths) {
            assert_eq!(std::fs::read(destination).unwrap(), b"shim-v2");
        }
        assert!(
            std::fs::read_dir(paths.staging_dir())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("shims-"))
        );
    }

    #[test]
    fn selection_shim_transaction_cleans_its_receipt_after_commit() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let source = root.path().join("torben-shim-fixture");
        std::fs::write(&source, b"selection-shim").unwrap();
        let (app_id, version) = node_identity();
        let journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Select,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();

        let installed = install_selection_shims_locked(&paths, &source, &journal).unwrap();

        assert_eq!(installed, shim_destinations(&paths));
        for destination in installed {
            assert_eq!(std::fs::read(destination).unwrap(), b"selection-shim");
        }
        assert!(
            !paths
                .staging_dir()
                .join(format!("shims-{operation_id}"))
                .exists()
        );
        assert!(
            !paths
                .operation_dir()
                .join(format!("{operation_id}.selection-shims.receipt"))
                .exists()
        );
    }

    #[test]
    fn startup_rolls_back_a_receipt_bound_partial_selection_shim_commit() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let source = root.path().join("torben-shim-fixture");
        std::fs::write(&source, b"new-shim").unwrap();
        let (app_id, version) = node_identity();
        let journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Select,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        for destination in shim_destinations(&paths) {
            std::fs::write(destination, b"old-shim").unwrap();
        }
        let (destinations, staging, backup) =
            prepare_selection_shim_transaction(&paths, &journal, &source);
        let first_name = destinations[0].file_name().unwrap();
        std::fs::rename(&destinations[0], backup.join(first_name)).unwrap();
        std::fs::rename(staging.join(first_name), &destinations[0]).unwrap();
        drop(journal);

        let core = TorbenCore::open(paths.clone()).unwrap();

        for destination in destinations {
            assert_eq!(std::fs::read(destination).unwrap(), b"old-shim");
        }
        assert!(!staging.exists());
        assert!(
            !paths
                .operation_dir()
                .join(format!("{operation_id}.selection-shims.receipt"))
                .exists()
        );
        assert_eq!(
            operation_states(&core, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[test]
    fn startup_finishes_receipt_bound_shim_cleanup_after_selection_commit() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let source = root.path().join("torben-shim-fixture");
        std::fs::write(&source, b"new-shim").unwrap();
        let (app_id, version) = node_identity();
        let store = operation_store(&paths);
        let record = install_record(&paths, &app_id, &version);
        store.add_installation(&record).unwrap();
        std::fs::create_dir_all(&record.install_path).unwrap();
        let journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Select,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        for destination in shim_destinations(&paths) {
            std::fs::write(destination, b"old-shim").unwrap();
        }
        let (destinations, staging, backup) =
            prepare_selection_shim_transaction(&paths, &journal, &source);
        commit_staged_shims(&destinations, &staging, &backup).unwrap();
        store.set_selection(&app_id, &version).unwrap();
        drop(journal);
        drop(store);

        let core = TorbenCore::open(paths.clone()).unwrap();

        for destination in destinations {
            assert_eq!(std::fs::read(destination).unwrap(), b"new-shim");
        }
        assert!(!staging.exists());
        assert!(
            !paths
                .operation_dir()
                .join(format!("{operation_id}.selection-shims.receipt"))
                .exists()
        );
        assert_eq!(
            operation_states(&core, operation_id),
            [OperationState::Running, OperationState::Succeeded]
        );
    }

    #[test]
    fn startup_removes_a_residual_shim_receipt_after_staging_cleanup() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let source = root.path().join("torben-shim-fixture");
        std::fs::write(&source, b"new-shim").unwrap();
        let (app_id, version) = node_identity();
        let store = operation_store(&paths);
        let record = install_record(&paths, &app_id, &version);
        store.add_installation(&record).unwrap();
        std::fs::create_dir_all(&record.install_path).unwrap();
        let journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Select,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let (destinations, staging, backup) =
            prepare_selection_shim_transaction(&paths, &journal, &source);
        commit_staged_shims(&destinations, &staging, &backup).unwrap();
        store.set_selection(&app_id, &version).unwrap();
        std::fs::remove_dir_all(&staging).unwrap();
        drop(journal);
        drop(store);

        let core = TorbenCore::open(paths.clone()).unwrap();

        for destination in destinations {
            assert_eq!(std::fs::read(destination).unwrap(), b"new-shim");
        }
        assert!(
            !paths
                .operation_dir()
                .join(format!("{operation_id}.selection-shims.receipt"))
                .exists()
        );
        assert_eq!(
            operation_states(&core, operation_id),
            [OperationState::Running, OperationState::Succeeded]
        );
    }

    #[test]
    fn startup_closes_a_residual_shim_receipt_after_selection_rollback() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let source = root.path().join("torben-shim-fixture");
        std::fs::write(&source, b"new-shim").unwrap();
        let (app_id, version) = node_identity();
        let journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Select,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        for destination in shim_destinations(&paths) {
            std::fs::write(destination, b"old-shim").unwrap();
        }
        let (destinations, staging, _) =
            prepare_selection_shim_transaction(&paths, &journal, &source);
        std::fs::remove_dir_all(&staging).unwrap();
        drop(journal);

        let core = TorbenCore::open(paths.clone()).unwrap();

        for destination in destinations {
            assert_eq!(std::fs::read(destination).unwrap(), b"old-shim");
        }
        assert!(
            !paths
                .operation_dir()
                .join(format!("{operation_id}.selection-shims.receipt"))
                .exists()
        );
        assert_eq!(
            operation_states(&core, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[test]
    fn startup_preserves_selection_shim_staging_without_a_receipt() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let operation_id =
            start_interrupted(&paths, OperationKind::Select, &app_id, Some(&version));
        let staging = paths.staging_dir().join(format!("shims-{operation_id}"));
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("must-not-delete"), b"unowned").unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "selection_shim_ownership_receipt_invalid");
        assert!(staging.join("must-not-delete").is_file());
    }

    #[test]
    fn startup_preserves_selection_shim_staging_when_its_receipt_mismatches() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let source = root.path().join("torben-shim-fixture");
        std::fs::write(&source, b"new-shim").unwrap();
        let (app_id, version) = node_identity();
        let journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Select,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let (_, staging, _) = prepare_selection_shim_transaction(&paths, &journal, &source);
        drop(journal);
        let receipt_path = paths
            .operation_dir()
            .join(format!("{operation_id}.selection-shims.receipt"));
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        receipt["stagingPath"] = serde_json::json!(root.path().join("tampered"));
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "selection_shim_ownership_receipt_invalid");
        assert!(staging.is_dir());
    }

    #[test]
    fn refuses_a_non_file_shim_destination_before_mutation() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let source = root.path().join("torben-shim-fixture");
        std::fs::write(&source, b"shim").unwrap();
        let destinations = shim_destinations(&paths);
        std::fs::create_dir_all(&destinations[0]).unwrap();

        let error = install_shims_locked(&paths, &source).unwrap_err();

        assert_eq!(error.code, "shim_destination_conflict");
        assert!(destinations[0].is_dir());
        assert!(!destinations[1].exists());
        assert!(!destinations[2].exists());
    }

    #[test]
    fn doctor_detects_outdated_command_shims() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        let source = root.path().join("torben-shim-fixture");
        std::fs::write(&source, b"shim").unwrap();
        let mut core = TorbenCore::open(paths.clone()).unwrap();
        core.bundled_shim = BundledShim::from_executable(source.clone());
        let (app_id, version) = node_identity();
        let record = install_record(&paths, &app_id, &version);
        std::fs::create_dir_all(&record.install_path).unwrap();
        core.store.add_installation(&record).unwrap();
        core.store.set_selection(&app_id, &version).unwrap();
        core.install_shims(&source).unwrap();

        let healthy = core
            .doctor()
            .unwrap()
            .into_iter()
            .find(|check| check.id == "terminal_shims")
            .unwrap();
        assert!(healthy.healthy);

        std::fs::write(&shim_destinations(&paths)[1], b"outdated").unwrap();
        let outdated = core
            .doctor()
            .unwrap()
            .into_iter()
            .find(|check| check.id == "terminal_shims")
            .unwrap();
        assert!(!outdated.healthy);
        assert!(outdated.message.contains("missing or outdated"));
    }

    #[test]
    fn doctor_reports_the_local_diagnostic_log() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        let core = TorbenCore::open(paths.clone()).unwrap();

        let check = core
            .doctor()
            .unwrap()
            .into_iter()
            .find(|check| check.id == "diagnostic_log")
            .unwrap();

        assert!(check.healthy);
        assert_eq!(
            check.message,
            paths.log_dir().join("torben.jsonl").display().to_string()
        );
        assert!(paths.log_dir().join("torben.jsonl").is_file());
    }

    #[test]
    fn shell_integration_actions_are_idempotent_and_update_doctor() {
        let root = tempdir().unwrap();
        let mut core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        core.shell_integration =
            Arc::new(FakeShellIntegration::new(ShellIntegrationState::Disabled));

        let initial_checks = core.doctor().unwrap();
        let initial_shell = initial_checks
            .iter()
            .find(|check| check.id == "shell_integration")
            .unwrap();
        let initial_shims = initial_checks
            .iter()
            .find(|check| check.id == "terminal_shims")
            .unwrap();
        assert!(initial_shell.healthy);
        assert!(initial_shims.healthy);
        assert!(initial_shims.message.contains("not required"));

        let enabled = core.enable_shell_integration().unwrap();
        let enabled_again = core.enable_shell_integration().unwrap();
        let check = core
            .doctor()
            .unwrap()
            .into_iter()
            .find(|check| check.id == "shell_integration")
            .unwrap();

        assert_eq!(enabled.state, ShellIntegrationState::Managed);
        assert!(enabled.new_terminal_required);
        assert!(!enabled_again.new_terminal_required);
        assert!(check.healthy);

        let disabled = core.disable_shell_integration().unwrap();
        let disabled_check = core
            .doctor()
            .unwrap()
            .into_iter()
            .find(|check| check.id == "shell_integration")
            .unwrap();
        assert_eq!(disabled.state, ShellIntegrationState::Disabled);
        assert!(disabled.new_terminal_required);
        assert!(disabled_check.healthy);
    }

    #[test]
    fn doctor_distinguishes_optional_configuration_from_broken_configuration() {
        assert!(shell_integration_is_healthy(
            ShellIntegrationState::Disabled
        ));
        assert!(shell_integration_is_healthy(ShellIntegrationState::Managed));
        assert!(shell_integration_is_healthy(
            ShellIntegrationState::External
        ));
        assert!(!shell_integration_is_healthy(
            ShellIntegrationState::Outdated
        ));
        assert!(source_adapter_is_healthy(
            SourceAdapterAvailability::Available
        ));
        assert!(source_adapter_is_healthy(
            SourceAdapterAvailability::Missing
        ));
        assert!(!source_adapter_is_healthy(
            SourceAdapterAvailability::Unsupported
        ));
    }

    #[test]
    fn shim_commit_failure_restores_replaced_files() {
        let root = tempdir().unwrap();
        let source = root.path().join("torben-shim-fixture");
        let staging = root.path().join("staging");
        let backup = staging.join("backup");
        let final_directory = root.path().join("final");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::create_dir_all(&final_directory).unwrap();
        std::fs::write(&source, b"new-shim").unwrap();
        let destinations = [
            final_directory.join("node"),
            root.path().join("missing-parent").join("npm"),
            final_directory.join("npx"),
        ];
        std::fs::write(&destinations[0], b"old-shim").unwrap();

        let error = stage_and_commit_shims(&source, &destinations, &staging, &backup).unwrap_err();

        assert_eq!(error.code, "shim_commit_failed");
        assert_eq!(
            error.details.get("rollbackComplete").map(String::as_str),
            Some("true")
        );
        assert_eq!(std::fs::read(&destinations[0]).unwrap(), b"old-shim");
        assert!(!destinations[1].exists());
        assert!(!destinations[2].exists());
    }

    #[test]
    fn validates_every_uninstall_plan_owner_field() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        let (app_id, version) = node_identity();
        let record = install_record(&paths, &app_id, &version);
        let mut plan = UninstallPlan {
            app_id: record.app_id.clone(),
            version: record.version.clone(),
            source_id: record.source_id.clone(),
            install_path: record.install_path.clone(),
            preserve_user_data: true,
        };
        assert!(validate_uninstall_plan(&record, &plan).is_ok());

        plan.preserve_user_data = false;
        assert_eq!(
            validate_uninstall_plan(&record, &plan).unwrap_err().code,
            "plugin_uninstall_plan_invalid"
        );
    }

    #[test]
    fn managed_uninstall_commits_only_after_receipt_bound_cleanup() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = operation_store(&paths);
        let record = install_record(&paths, &app_id, &version);
        store.add_installation(&record).unwrap();
        let source = PathBuf::from(&record.install_path);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("managed-payload"), b"fixture").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Uninstall,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let staged = paths
            .staging_dir()
            .join(format!("uninstall-{app_id}-{operation_id}"));

        execute_uninstall_transaction(&paths, &store, &record, &source, &staged, &mut journal)
            .unwrap();

        assert!(store.get_installation(&app_id, &version).unwrap().is_none());
        assert!(!source.exists());
        assert!(!staged.exists());
        assert!(
            !paths
                .operation_dir()
                .join(format!("{operation_id}.managed-uninstall.receipt"))
                .exists()
        );
        assert_eq!(
            operation_states_from_store(&store, operation_id),
            [OperationState::Running, OperationState::Succeeded]
        );
    }

    #[test]
    fn managed_uninstall_restores_a_receipt_bound_stage_when_state_commit_fails() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = operation_store(&paths);
        let record = install_record(&paths, &app_id, &version);
        store.add_installation(&record).unwrap();
        rusqlite::Connection::open(paths.state_database())
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fixture_uninstall_failure
                 BEFORE DELETE ON installations
                 BEGIN
                   SELECT RAISE(FAIL, 'fixture uninstall failure');
                 END;",
            )
            .unwrap();
        let source = PathBuf::from(&record.install_path);
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("managed-payload"), b"fixture").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Uninstall,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let staged = paths
            .staging_dir()
            .join(format!("uninstall-{app_id}-{operation_id}"));

        let error =
            execute_uninstall_transaction(&paths, &store, &record, &source, &staged, &mut journal)
                .unwrap_err();

        assert_ne!(error.code, "uninstall_rollback_pending");
        assert!(source.join("managed-payload").is_file());
        assert!(!staged.exists());
        assert!(
            !paths
                .operation_dir()
                .join(format!("{operation_id}.managed-uninstall.receipt"))
                .exists()
        );
        assert!(store.get_installation(&app_id, &version).unwrap().is_some());
        assert_eq!(
            operation_states_from_store(&store, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[tokio::test]
    async fn uninstall_rejects_a_path_outside_the_managed_library() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        let mut core = TorbenCore::open(paths.clone()).unwrap();
        core.node_plugin =
            BundledPlugin::node_from_executable(root.path().join("missing-node-plugin"));
        let (app_id, version) = node_identity();
        let outside = root.path().join("outside-installation");
        std::fs::create_dir_all(&outside).unwrap();
        let mut record = install_record(&paths, &app_id, &version);
        record.install_path = outside.display().to_string();
        core.store.add_installation(&record).unwrap();

        let error = core.uninstall(&app_id, &version).await.unwrap_err();

        assert_eq!(error.code, "managed_install_path_invalid");
        assert!(outside.is_dir());
        assert!(
            core.store
                .get_installation(&app_id, &version)
                .unwrap()
                .is_some()
        );
        assert!(
            core.operation_events()
                .unwrap()
                .iter()
                .any(|event| event.state == OperationState::RolledBack)
        );
    }

    #[tokio::test]
    async fn selection_rejects_package_manager_installations() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        let core = TorbenCore::open(paths.clone()).unwrap();
        let (app_id, version) = node_identity();
        let mut record = install_record(&paths, &app_id, &version);
        record.scope = InstallScope::PackageManager;
        record.source_id = SourceId::new("source.winget").unwrap();
        record.install_path = root.path().join("node.exe").display().to_string();
        core.store.add_installation(&record).unwrap();

        let error = core.select(&app_id, &version).await.unwrap_err();

        assert_eq!(error.code, "installation_not_selectable");
        assert!(core.store.selected_version(&app_id).unwrap().is_none());
        assert!(core.operation_events().unwrap().is_empty());
    }

    #[tokio::test]
    async fn selection_rejects_a_managed_record_outside_the_standard_library() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let (app_id, version) = node_identity();
        let outside = root.path().join("outside-installation");
        std::fs::create_dir_all(&outside).unwrap();
        let mut record = install_record(&paths, &app_id, &version);
        record.install_path = outside.display().to_string();
        core.store.add_installation(&record).unwrap();

        let error = core.select(&app_id, &version).await.unwrap_err();

        assert_eq!(error.code, "selection_state_invalid");
        assert!(core.selected_version(&app_id).unwrap().is_none());
        assert!(core.operation_events().unwrap().is_empty());
        assert!(outside.is_dir());
    }

    #[test]
    fn shim_resolution_rejects_a_package_manager_selection() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        let core = TorbenCore::open(paths.clone()).unwrap();
        let (app_id, version) = node_identity();
        let mut record = install_record(&paths, &app_id, &version);
        record.scope = InstallScope::PackageManager;
        record.source_id = SourceId::new("source.winget").unwrap();
        core.store.add_installation(&record).unwrap();
        core.store.set_selection(&app_id, &version).unwrap();

        let error = core.executable_for(&app_id, "node").unwrap_err();

        assert_eq!(error.code, "selection_state_invalid");
        let selection_check = core
            .doctor()
            .unwrap()
            .into_iter()
            .find(|check| check.id == "selection.node")
            .unwrap();
        assert!(!selection_check.healthy);
        assert!(selection_check.message.contains("selection_state_invalid"));
    }

    #[cfg(unix)]
    #[test]
    fn shim_resolution_rejects_a_command_link_outside_the_managed_installation() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        let core = TorbenCore::open(paths.clone()).unwrap();
        let (app_id, version) = node_identity();
        let record = install_record(&paths, &app_id, &version);
        core.store.add_installation(&record).unwrap();
        core.store.set_selection(&app_id, &version).unwrap();
        let install_path = PathBuf::from(&record.install_path);
        std::fs::create_dir_all(install_path.join("bin")).unwrap();
        let outside = root.path().join("outside-node");
        std::fs::write(&outside, b"external").unwrap();
        symlink(&outside, install_path.join("bin/node")).unwrap();

        let error = core.executable_for(&app_id, "node").unwrap_err();

        assert_eq!(error.code, "managed_command_outside_installation");
        assert_eq!(std::fs::read(outside).unwrap(), b"external");
    }

    #[tokio::test]
    async fn ordinary_uninstall_rejects_package_manager_installations() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        let core = TorbenCore::open(paths.clone()).unwrap();
        let (app_id, version) = node_identity();
        let mut record = install_record(&paths, &app_id, &version);
        record.scope = InstallScope::PackageManager;
        record.source_id = SourceId::new("source.winget").unwrap();
        record.install_path = root.path().join("node.exe").display().to_string();
        core.store.add_installation(&record).unwrap();

        let error = core.uninstall(&app_id, &version).await.unwrap_err();

        assert_eq!(error.code, "package_manager_uninstall_required");
        assert!(
            core.store
                .get_installation(&app_id, &version)
                .unwrap()
                .is_some()
        );
        assert!(core.operation_events().unwrap().is_empty());
    }

    #[test]
    fn startup_rolls_back_an_uncommitted_installation() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = operation_store(&paths);
        let journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let staged = paths
            .staging_dir()
            .join(format!("install-{app_id}-{operation_id}"));
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        let download_directory = paths.download_dir(app_id.as_str(), &version.to_string());
        let partial = download_directory.join("node.zip.partial");
        let completed_cache = download_directory.join("SHASUMS256.txt");
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::create_dir_all(&download_directory).unwrap();
        std::fs::write(&partial, b"interrupted").unwrap();
        std::fs::write(&completed_cache, b"trusted cache entry").unwrap();
        write_managed_install_receipt(&paths, &journal, &install_record(&paths, &app_id, &version))
            .unwrap();
        drop(journal);
        drop(store);

        let core = TorbenCore::open(paths).unwrap();

        assert!(!staged.exists());
        assert!(!installed.exists());
        assert!(!partial.exists());
        assert_eq!(
            std::fs::read(completed_cache).unwrap(),
            b"trusted cache entry"
        );
        assert_eq!(
            operation_states(&core, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[test]
    fn startup_preserves_an_unowned_install_target_without_a_receipt() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let operation_id =
            start_interrupted(&paths, OperationKind::Install, &app_id, Some(&version));
        let staged = paths
            .staging_dir()
            .join(format!("install-{app_id}-{operation_id}"));
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("must-not-delete"), b"unowned").unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "install_ownership_receipt_invalid");
        assert!(!staged.exists());
        assert!(installed.join("must-not-delete").is_file());
    }

    #[test]
    fn startup_preserves_an_install_target_when_its_receipt_mismatches() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = operation_store(&paths);
        let journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("must-not-delete"), b"managed").unwrap();
        write_managed_install_receipt(&paths, &journal, &install_record(&paths, &app_id, &version))
            .unwrap();
        drop(journal);
        drop(store);
        let receipt_path = paths
            .operation_dir()
            .join(format!("{operation_id}.managed-install.receipt"));
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        receipt["finalPath"] = serde_json::json!(root.path().join("tampered"));
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "install_ownership_receipt_invalid");
        assert!(installed.join("must-not-delete").is_file());
    }

    #[test]
    fn startup_recovery_rejects_a_non_file_partial_download() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        start_interrupted(&paths, OperationKind::Install, &app_id, Some(&version));
        let conflict = paths
            .download_dir(app_id.as_str(), &version.to_string())
            .join("archive.partial");
        std::fs::create_dir_all(&conflict).unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_path_conflict");
        assert!(conflict.is_dir());
    }

    #[test]
    fn startup_recovers_a_cancelled_installation_before_version_resolution() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, unrelated_version) = node_identity();
        let store = operation_store(&paths);
        let journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            None,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        OperationJournal::request_cancellation(&paths, &store, operation_id).unwrap();
        drop(journal);
        drop(store);
        let staged = paths
            .staging_dir()
            .join(format!("install-{app_id}-{operation_id}"));
        let unrelated_install =
            paths.app_version_dir(app_id.as_str(), &unrelated_version.to_string());
        let cancellation_marker = paths
            .operation_dir()
            .join(format!("{operation_id}.json.cancel"));
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::create_dir_all(&unrelated_install).unwrap();

        let core = TorbenCore::open(paths).unwrap();

        assert!(!staged.exists());
        assert!(unrelated_install.is_dir());
        assert!(!cancellation_marker.exists());
        assert_eq!(
            operation_states(&core, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[test]
    fn startup_completes_a_committed_installation_journal() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        drop(store);
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        let operation_id =
            start_interrupted(&paths, OperationKind::Install, &app_id, Some(&version));
        let staged = paths
            .staging_dir()
            .join(format!("install-{app_id}-{operation_id}"));
        std::fs::create_dir_all(&staged).unwrap();

        let core = TorbenCore::open(paths).unwrap();

        assert!(installed.is_dir());
        assert!(!staged.exists());
        assert!(
            core.store
                .get_installation(&app_id, &version)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            operation_states(&core, operation_id),
            [OperationState::Running, OperationState::Succeeded]
        );
    }

    #[test]
    fn startup_refuses_a_committed_installation_with_missing_files() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        drop(store);
        start_interrupted(&paths, OperationKind::Install, &app_id, Some(&version));

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_inconsistent");
    }

    #[test]
    fn startup_restores_an_uncommitted_uninstall() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        drop(store);
        let mut journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Uninstall,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        journal
            .fail(&TorbenError::new(
                "uninstall_rollback_pending",
                "Fixture restore is pending.",
            ))
            .unwrap();
        let staged = paths
            .staging_dir()
            .join(format!("uninstall-{app_id}-{operation_id}"));
        std::fs::create_dir_all(&staged).unwrap();
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        write_managed_uninstall_receipt(&paths, &journal, &app_id, &version, &installed, &staged)
            .unwrap();
        drop(journal);

        let core = TorbenCore::open(paths).unwrap();

        assert!(installed.is_dir());
        assert!(!staged.exists());
        assert!(
            core.store
                .get_installation(&app_id, &version)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            operation_states(&core, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[test]
    fn startup_resumes_a_failed_committed_uninstall_cleanup() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let mut journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Uninstall,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        journal
            .fail(&TorbenError::new(
                "uninstall_cleanup_pending",
                "Fixture cleanup is pending.",
            ))
            .unwrap();
        let staged = paths
            .staging_dir()
            .join(format!("uninstall-{app_id}-{operation_id}"));
        std::fs::create_dir_all(&staged).unwrap();
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        write_managed_uninstall_receipt(&paths, &journal, &app_id, &version, &installed, &staged)
            .unwrap();
        drop(journal);

        let core = TorbenCore::open(paths).unwrap();

        assert!(!staged.exists());
        assert_eq!(
            operation_states(&core, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::Succeeded
            ]
        );
    }

    #[test]
    fn startup_preserves_an_unowned_uninstall_stage_without_a_receipt() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        drop(store);
        let operation_id =
            start_interrupted(&paths, OperationKind::Uninstall, &app_id, Some(&version));
        let staged = paths
            .staging_dir()
            .join(format!("uninstall-{app_id}-{operation_id}"));
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("must-not-move"), b"unowned").unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "uninstall_ownership_receipt_invalid");
        assert!(staged.join("must-not-move").is_file());
    }

    #[test]
    fn startup_preserves_an_uninstall_stage_when_its_receipt_mismatches() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        drop(store);
        let journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Uninstall,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let staged = paths
            .staging_dir()
            .join(format!("uninstall-{app_id}-{operation_id}"));
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&staged).unwrap();
        std::fs::write(staged.join("must-not-move"), b"managed").unwrap();
        write_managed_uninstall_receipt(&paths, &journal, &app_id, &version, &installed, &staged)
            .unwrap();
        drop(journal);
        let receipt_path = paths
            .operation_dir()
            .join(format!("{operation_id}.managed-uninstall.receipt"));
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
        receipt["sourcePath"] = serde_json::json!(root.path().join("tampered"));
        std::fs::write(&receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "uninstall_ownership_receipt_invalid");
        assert!(staged.join("must-not-move").is_file());
    }

    #[test]
    fn startup_preserves_a_non_directory_uninstall_stage() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        drop(store);
        let operation_id =
            start_interrupted(&paths, OperationKind::Uninstall, &app_id, Some(&version));
        let staged = paths
            .staging_dir()
            .join(format!("uninstall-{app_id}-{operation_id}"));
        std::fs::write(&staged, b"must-not-delete").unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_path_conflict");
        assert_eq!(std::fs::read(staged).unwrap(), b"must-not-delete");
    }

    #[cfg(unix)]
    #[test]
    fn startup_preserves_a_linked_uninstall_stage() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        drop(store);
        let operation_id =
            start_interrupted(&paths, OperationKind::Uninstall, &app_id, Some(&version));
        let staged = paths
            .staging_dir()
            .join(format!("uninstall-{app_id}-{operation_id}"));
        let external = root.path().join("must-not-touch");
        std::fs::create_dir_all(&external).unwrap();
        std::fs::write(external.join("payload"), b"external").unwrap();
        symlink(&external, &staged).unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_path_conflict");
        assert!(staged.symlink_metadata().unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read(external.join("payload")).unwrap(),
            b"external"
        );
    }

    #[test]
    fn startup_safely_rolls_back_an_uninstall_interrupted_before_staging() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        drop(store);
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("managed-payload"), b"fixture").unwrap();
        let operation_id =
            start_interrupted(&paths, OperationKind::Uninstall, &app_id, Some(&version));

        let core = TorbenCore::open(paths).unwrap();

        assert!(installed.join("managed-payload").is_file());
        assert_eq!(
            operation_states(&core, operation_id),
            [
                OperationState::Running,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
    }

    #[test]
    fn startup_rejects_a_package_owner_in_managed_uninstall_recovery() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let mut record = install_record(&paths, &app_id, &version);
        record.scope = InstallScope::PackageManager;
        record.source_id = SourceId::new("source.winget").unwrap();
        let store = StateStore::open(paths.state_database()).unwrap();
        store.add_installation(&record).unwrap();
        drop(store);
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("must-not-touch"), b"package").unwrap();
        start_interrupted(&paths, OperationKind::Uninstall, &app_id, Some(&version));

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_inconsistent");
        assert!(installed.join("must-not-touch").is_file());
    }

    #[test]
    fn startup_preserves_an_untracked_final_directory_after_uninstall_state_commit() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        start_interrupted(&paths, OperationKind::Uninstall, &app_id, Some(&version));
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("must-not-delete"), b"untracked").unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_path_conflict");
        assert!(installed.join("must-not-delete").is_file());
    }

    #[test]
    fn startup_recognizes_committed_select_and_clear_operations() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        store.set_selection(&app_id, &version).unwrap();
        std::fs::create_dir_all(paths.app_version_dir(app_id.as_str(), &version.to_string()))
            .unwrap();
        let select_id = start_interrupted(&paths, OperationKind::Select, &app_id, Some(&version));
        drop(store);

        let core = TorbenCore::open(paths.clone()).unwrap();
        assert_eq!(
            operation_states(&core, select_id),
            [OperationState::Running, OperationState::Succeeded]
        );
        drop(core);

        let clear_id = start_interrupted(&paths, OperationKind::Select, &app_id, None);
        let store = StateStore::open(paths.state_database()).unwrap();
        store.clear_selection(&app_id).unwrap();
        drop(store);

        let core = TorbenCore::open(paths).unwrap();
        assert_eq!(
            operation_states(&core, clear_id),
            [OperationState::Running, OperationState::Succeeded]
        );
    }

    #[test]
    fn startup_rejects_a_committed_package_manager_selection() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let mut record = install_record(&paths, &app_id, &version);
        record.scope = InstallScope::PackageManager;
        record.source_id = SourceId::new("source.winget").unwrap();
        let store = StateStore::open(paths.state_database()).unwrap();
        store.add_installation(&record).unwrap();
        store.set_selection(&app_id, &version).unwrap();
        drop(store);
        let installed = paths.app_version_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&installed).unwrap();
        std::fs::write(installed.join("must-not-touch"), b"package").unwrap();
        start_interrupted(&paths, OperationKind::Select, &app_id, Some(&version));

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_inconsistent");
        assert!(installed.join("must-not-touch").is_file());
    }

    #[test]
    fn startup_rejects_a_committed_selection_with_missing_managed_files() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let store = StateStore::open(paths.state_database()).unwrap();
        store
            .add_installation(&install_record(&paths, &app_id, &version))
            .unwrap();
        store.set_selection(&app_id, &version).unwrap();
        drop(store);
        start_interrupted(&paths, OperationKind::Select, &app_id, Some(&version));

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "operation_recovery_inconsistent");
    }

    #[test]
    fn startup_does_not_recover_a_terminal_journal_twice() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let (app_id, version) = node_identity();
        let mut journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Install,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        journal.succeed("fixture committed").unwrap();
        drop(journal);

        let core = TorbenCore::open(paths.clone()).unwrap();
        assert_eq!(operation_states(&core, operation_id).len(), 2);
        drop(core);
        let core = TorbenCore::open(paths).unwrap();
        assert_eq!(operation_states(&core, operation_id).len(), 2);
    }

    #[tokio::test]
    async fn failed_plugin_health_check_rolls_back_selection_state() {
        let root = tempdir().unwrap();
        let mut core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        core.node_plugin =
            BundledPlugin::node_from_executable(root.path().join("missing-node-plugin"));
        let (app_id, version) = seed_installation(&core);

        let error = core.select(&app_id, &version).await.unwrap_err();

        assert_eq!(error.code, "bundled_plugin_missing");
        assert_eq!(core.selected_version(&app_id).unwrap(), None);
        let events = core.operation_events().unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.state == OperationState::Failed)
        );
        assert!(
            events
                .iter()
                .any(|event| event.state == OperationState::RolledBack)
        );
    }

    #[test]
    fn clearing_selection_commits_a_durable_operation() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().to_path_buf())).unwrap();
        let (app_id, version) = seed_installation(&core);
        core.store.set_selection(&app_id, &version).unwrap();

        core.clear_selection(&app_id).unwrap();

        assert_eq!(core.selected_version(&app_id).unwrap(), None);
        assert!(
            core.operation_events()
                .unwrap()
                .iter()
                .any(|event| event.state == OperationState::Succeeded)
        );
    }

    #[tokio::test]
    async fn package_source_install_and_uninstall_commit_only_after_reconciliation() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths).unwrap();
        let fixture = source_fixture(root.path(), None, true, true, "1.134.0");

        let installed = core
            .execute_source_operation_with_service(
                source_request(SourceAction::Install, Some(&fixture.health)),
                &fixture.service,
            )
            .await
            .unwrap();

        assert_eq!(
            installed.outcome,
            SourceExecutionOutcome::OwnershipCommitted
        );
        assert_eq!(fixture.state.lock().unwrap().as_deref(), Some("1.134.0"));
        let ownership = core.package_installations().unwrap();
        assert_eq!(ownership.len(), 1);
        assert!(ownership[0].owned_by_torben);
        assert_eq!(ownership[0].coordinate.as_str(), "code");
        assert_eq!(
            core.store
                .get_installation(
                    &AppId::new("vscode").unwrap(),
                    &ExactVersion::from_str("1.134.0").unwrap(),
                )
                .unwrap()
                .unwrap()
                .scope,
            InstallScope::PackageManager
        );
        assert_eq!(
            operation_states(&core, installed.operation_id).last(),
            Some(&OperationState::Succeeded)
        );

        let removed = core
            .execute_source_operation_with_service(
                source_request(SourceAction::Uninstall, None),
                &fixture.service,
            )
            .await
            .unwrap();

        assert_eq!(removed.outcome, SourceExecutionOutcome::OwnershipRemoved);
        assert!(fixture.state.lock().unwrap().is_none());
        assert!(core.package_installations().unwrap().is_empty());
        assert_eq!(fixture.executions.lock().unwrap().len(), 2);
        assert_eq!(
            operation_states(&core, removed.operation_id).last(),
            Some(&OperationState::Succeeded)
        );
    }

    #[tokio::test]
    async fn dnf_install_and_uninstall_execute_only_the_locked_nevra() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = dnf_source_fixture(root.path());

        let installed = core
            .execute_source_operation_with_service(
                dnf_source_request(SourceAction::Install, Some(&fixture.health)),
                &fixture.service,
            )
            .await
            .unwrap();
        assert_eq!(
            installed.outcome,
            SourceExecutionOutcome::OwnershipCommitted
        );
        assert_eq!(
            installed.plan.execution_identity.as_deref(),
            Some("code-1.134.0-1.fc42.x86_64")
        );
        assert_eq!(fixture.executions.lock().unwrap().len(), 1);
        assert_eq!(
            fixture.executions.lock().unwrap()[0]
                .last()
                .map(String::as_str),
            installed.plan.execution_identity.as_deref()
        );

        let removed = core
            .execute_source_operation_with_service(
                dnf_source_request(SourceAction::Uninstall, None),
                &fixture.service,
            )
            .await
            .unwrap();
        assert_eq!(removed.outcome, SourceExecutionOutcome::OwnershipRemoved);
        assert_eq!(fixture.executions.lock().unwrap().len(), 2);
        assert_eq!(
            fixture.executions.lock().unwrap()[1]
                .last()
                .map(String::as_str),
            removed.plan.execution_identity.as_deref()
        );
        assert!(core.package_installations().unwrap().is_empty());
    }

    #[tokio::test]
    async fn package_source_migration_replaces_owner_only_after_target_health_check() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let mut request = migration_request(&fixture);
        let plan = core
            .plan_source_migration_with_service(request.clone(), &fixture.service)
            .await
            .unwrap();
        assert_eq!(
            plan.install_target.execution_identity.as_deref(),
            Some("code-1.134.0-1.fc42.x86_64")
        );
        assert_eq!(
            plan.cleanup_target.execution_identity,
            plan.install_target.execution_identity
        );
        request.approved_plan_token = Some(plan.approval_token.clone());
        request.accept_system_changes = true;

        let result = core
            .execute_source_migration_with_service(request, &fixture.service)
            .await
            .unwrap();

        assert_eq!(result.installation.adapter, SourceAdapterKind::Dnf);
        assert_eq!(result.installation.coordinate.as_str(), "code");
        assert!(fixture.old_state.lock().unwrap().is_none());
        assert_eq!(
            fixture.target_state.lock().unwrap().as_deref(),
            Some("1.134.0-1.fc42")
        );
        assert_eq!(
            core.package_installations().unwrap()[0].adapter,
            SourceAdapterKind::Dnf
        );
        assert_eq!(
            operation_states(&core, result.operation_id).last(),
            Some(&OperationState::Succeeded)
        );
    }

    #[tokio::test]
    async fn package_source_migration_failure_cleans_target_and_restores_current_owner() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = migration_fixture(root.path(), false);
        seed_package_owner(&core, &fixture);
        let mut request = migration_request(&fixture);
        let plan = core
            .plan_source_migration_with_service(request.clone(), &fixture.service)
            .await
            .unwrap();
        request.approved_plan_token = Some(plan.approval_token);
        request.accept_system_changes = true;

        let error = core
            .execute_source_migration_with_service(request, &fixture.service)
            .await
            .unwrap_err();

        assert_eq!(error.code, "source_execution_failed");
        assert_eq!(
            error.details.get("sourceRestored"),
            Some(&"true".to_owned())
        );
        assert_eq!(
            fixture.old_state.lock().unwrap().as_deref(),
            Some("1.134.0")
        );
        assert!(fixture.target_state.lock().unwrap().is_none());
        assert_eq!(
            core.package_installations().unwrap()[0].adapter,
            SourceAdapterKind::Apt
        );
        let executions = fixture.executions.lock().unwrap();
        assert_eq!(
            executions
                .iter()
                .map(|(adapter, _)| adapter.as_str())
                .collect::<Vec<_>>(),
            vec!["apt", "dnf", "dnf", "apt"]
        );
    }

    #[tokio::test]
    async fn package_source_migration_failed_compensation_drops_unverified_ownership() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = migration_fixture_with_restore(root.path(), false, false);
        seed_package_owner(&core, &fixture);
        let mut request = migration_request(&fixture);
        let plan = core
            .plan_source_migration_with_service(request.clone(), &fixture.service)
            .await
            .unwrap();
        request.approved_plan_token = Some(plan.approval_token);
        request.accept_system_changes = true;

        let error = core
            .execute_source_migration_with_service(request, &fixture.service)
            .await
            .unwrap_err();

        assert_eq!(error.code, "source_migration_reconciliation_required");
        assert!(fixture.old_state.lock().unwrap().is_none());
        assert!(fixture.target_state.lock().unwrap().is_none());
        assert!(core.package_installations().unwrap().is_empty());
    }

    #[tokio::test]
    async fn package_source_migration_requires_the_latest_reviewed_plan_token() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let mut request = migration_request(&fixture);
        request.approved_plan_token = Some("stale".to_owned());
        request.accept_system_changes = true;

        let error = core
            .execute_source_migration_with_service(request, &fixture.service)
            .await
            .unwrap_err();

        assert_eq!(error.code, "source_migration_plan_approval_required");
        assert!(fixture.executions.lock().unwrap().is_empty());
        assert_eq!(
            core.package_installations().unwrap()[0].adapter,
            SourceAdapterKind::Apt
        );
    }

    #[tokio::test]
    async fn managed_to_package_migration_stages_files_and_atomically_replaces_ownership() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = migration_fixture(root.path(), true);
        let managed = seed_managed_vscode(&core);
        let mut request = migration_request(&fixture);
        let plan = core
            .plan_managed_to_package_migration_with_service(request.clone(), &fixture.service)
            .await
            .unwrap();
        assert_eq!(plan.current_installation, managed);
        request.approved_plan_token = Some(plan.approval_token.clone());
        request.accept_system_changes = true;

        let result = core
            .execute_managed_to_package_migration_with_service(request, &fixture.service)
            .await
            .unwrap();

        assert!(!Path::new(&managed.install_path).exists());
        assert_eq!(result.installation.adapter, SourceAdapterKind::Dnf);
        assert_eq!(
            core.store
                .get_installation(&managed.app_id, &managed.version)
                .unwrap()
                .unwrap()
                .scope,
            InstallScope::PackageManager
        );
        assert_eq!(core.package_installations().unwrap().len(), 1);
        assert!(!source_migration_backup(&core.paths, result.operation_id).exists());
    }

    #[tokio::test]
    async fn managed_to_package_plan_rejects_the_selected_managed_version() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = migration_fixture(root.path(), true);
        let managed = seed_managed_vscode(&core);
        core.store
            .set_selection(&managed.app_id, &managed.version)
            .unwrap();

        let error = core
            .plan_managed_to_package_migration_with_service(
                migration_request(&fixture),
                &fixture.service,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "version_is_selected");
        assert!(fixture.executions.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn managed_to_package_failed_target_drops_ownership_when_restore_health_is_unverified() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = migration_fixture(root.path(), false);
        let managed = seed_managed_vscode(&core);
        let mut request = migration_request(&fixture);
        let plan = core
            .plan_managed_to_package_migration_with_service(request.clone(), &fixture.service)
            .await
            .unwrap();
        request.approved_plan_token = Some(plan.approval_token);
        request.accept_system_changes = true;

        let error = core
            .execute_managed_to_package_migration_with_service(request, &fixture.service)
            .await
            .unwrap_err();

        assert_eq!(error.code, "source_migration_reconciliation_required");
        assert!(Path::new(&managed.install_path).is_dir());
        assert!(
            core.store
                .get_installation(&managed.app_id, &managed.version)
                .unwrap()
                .is_none()
        );
        assert!(fixture.target_state.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn interrupted_managed_to_package_migration_restores_the_managed_directory() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        let managed = seed_managed_vscode(&core);
        let plan = core
            .plan_managed_to_package_migration_with_service(
                migration_request(&fixture),
                &fixture.service,
            )
            .await
            .unwrap();
        let mut journal = OperationJournal::start_managed_to_package_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let backup = source_migration_backup(&paths, operation_id);
        stage_managed_source(&plan, &backup, &mut journal).unwrap();
        assert!(!Path::new(&managed.install_path).exists());
        drop(journal);
        drop(core);

        let reopened = TorbenCore::open(paths).unwrap();

        assert!(Path::new(&managed.install_path).is_dir());
        assert_eq!(
            reopened
                .store
                .get_installation(&managed.app_id, &managed.version)
                .unwrap()
                .unwrap()
                .scope,
            InstallScope::Managed
        );
        assert_eq!(
            operation_states(&reopened, operation_id).last(),
            Some(&OperationState::Failed)
        );
    }

    #[tokio::test]
    async fn interrupted_managed_to_package_migration_cleans_backup_after_ownership_commit() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        let managed = seed_managed_vscode(&core);
        let plan = core
            .plan_managed_to_package_migration_with_service(
                migration_request(&fixture),
                &fixture.service,
            )
            .await
            .unwrap();
        let mut journal = OperationJournal::start_managed_to_package_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let backup = source_migration_backup(&paths, operation_id);
        stage_managed_source(&plan, &backup, &mut journal).unwrap();
        let request = package_request_from_managed_plan(&plan);
        let package_version = plan.install_target.package_version.clone().unwrap();
        let mut target_state = plan.target_state.clone();
        target_state.installed = true;
        target_state.installed_version = Some(package_version.clone());
        target_state.architecture = Some("x86_64".to_owned());
        target_state.manager_owned = true;
        let (installation, package) = TorbenCore::source_installation_records(
            &request,
            package_version,
            &fixture.target_health,
            &target_state,
        )
        .unwrap();
        core.store
            .replace_managed_with_package(&managed, &installation, &package)
            .unwrap();
        assert!(backup.is_dir());
        drop(journal);
        drop(core);

        let reopened = TorbenCore::open(paths).unwrap();

        assert!(!backup.exists());
        assert_eq!(
            reopened
                .store
                .get_installation(&managed.app_id, &managed.version)
                .unwrap()
                .unwrap()
                .scope,
            InstallScope::PackageManager
        );
        assert_eq!(
            operation_states(&reopened, operation_id).last(),
            Some(&OperationState::Succeeded)
        );
    }

    #[tokio::test]
    async fn managed_to_package_recovery_rejects_a_tampered_managed_source_path() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        let managed = seed_managed_vscode(&core);
        let plan = core
            .plan_managed_to_package_migration_with_service(
                migration_request(&fixture),
                &fixture.service,
            )
            .await
            .unwrap();
        let mut journal = OperationJournal::start_managed_to_package_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let backup = source_migration_backup(&paths, operation_id);
        stage_managed_source(&plan, &backup, &mut journal).unwrap();
        let protected = root.path().join("must-not-restore-here");
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::write(protected.join("protected.bin"), b"protected").unwrap();
        drop(journal);
        drop(core);

        let journal_path = paths.operation_dir().join(format!("{operation_id}.json"));
        let mut content: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        content["sourceMigration"]["currentInstallation"]["installPath"] =
            serde_json::json!(protected.display().to_string());
        content["sourceMigration"]["uninstallCurrent"]["installPath"] =
            serde_json::json!(protected.display().to_string());
        std::fs::write(&journal_path, serde_json::to_vec_pretty(&content).unwrap()).unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "source_migration_recovery_path_invalid");
        assert!(protected.join("protected.bin").is_file());
        assert!(backup.is_dir());
        assert!(!Path::new(&managed.install_path).exists());
    }

    #[tokio::test]
    async fn interrupted_package_to_managed_migration_keeps_committed_managed_ownership() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let plan = package_to_managed_plan(&core, &fixture).await;
        let journal = OperationJournal::start_package_to_managed_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        std::fs::create_dir_all(&plan.managed_target_path).unwrap();
        let managed = InstallRecord {
            app_id: plan.app_id.clone(),
            version: plan.app_version.clone(),
            source_id: plan.install_managed.source_id.clone(),
            scope: InstallScope::Managed,
            install_path: plan.managed_target_path.clone(),
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        };
        core.store
            .replace_package_with_managed(&plan.current_owner, &managed)
            .unwrap();
        drop(journal);
        drop(core);

        let reopened = TorbenCore::open(paths).unwrap();

        assert!(Path::new(&plan.managed_target_path).is_dir());
        assert_eq!(
            reopened
                .store
                .get_installation(&plan.app_id, &plan.app_version)
                .unwrap(),
            Some(managed)
        );
        assert_eq!(
            operation_states(&reopened, operation_id).last(),
            Some(&OperationState::Succeeded)
        );
    }

    #[tokio::test]
    async fn package_to_managed_migration_removes_package_and_atomically_commits_managed_owner() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let plan = package_to_managed_plan(&core, &fixture).await;
        let mut journal = OperationJournal::start_package_to_managed_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        std::fs::create_dir_all(&plan.managed_target_path).unwrap();
        let managed = InstallRecord {
            app_id: plan.app_id.clone(),
            version: plan.app_version.clone(),
            source_id: plan.install_managed.source_id.clone(),
            scope: InstallScope::Managed,
            install_path: plan.managed_target_path.clone(),
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        };
        write_package_to_managed_receipt(&paths, &plan, &journal).unwrap();

        let result = core
            .commit_package_to_managed_payload(
                plan.clone(),
                &fixture.service,
                &mut journal,
                managed.clone(),
            )
            .await
            .unwrap();

        assert_eq!(result.installation, managed);
        assert!(fixture.old_state.lock().unwrap().is_none());
        assert!(core.package_installations().unwrap().is_empty());
        assert_eq!(
            core.store
                .get_installation(&plan.app_id, &plan.app_version)
                .unwrap(),
            Some(managed)
        );
        assert_eq!(
            operation_states(&core, journal.operation_id()).last(),
            Some(&OperationState::Succeeded)
        );
    }

    #[tokio::test]
    async fn interrupted_package_to_managed_migration_removes_payload_after_package_removal_begins()
    {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let plan = package_to_managed_plan(&core, &fixture).await;
        let mut journal = OperationJournal::start_package_to_managed_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        std::fs::create_dir_all(&plan.managed_target_path).unwrap();
        std::fs::write(
            Path::new(&plan.managed_target_path).join("managed-fixture"),
            b"managed",
        )
        .unwrap();
        write_package_to_managed_receipt(&paths, &plan, &journal).unwrap();
        journal
            .record(
                OperationState::Running,
                "remove_package",
                "Removing the reviewed package-manager source",
                Some(0.72),
            )
            .unwrap();
        drop(journal);
        drop(core);

        let reopened = TorbenCore::open(paths).unwrap();

        assert!(!Path::new(&plan.managed_target_path).exists());
        assert!(
            reopened
                .store
                .get_installation(&plan.app_id, &plan.app_version)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            operation_states(&reopened, operation_id).last(),
            Some(&OperationState::Failed)
        );
    }

    #[tokio::test]
    async fn package_to_managed_recovery_preserves_a_target_without_an_ownership_receipt() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let plan = package_to_managed_plan(&core, &fixture).await;
        let journal = OperationJournal::start_package_to_managed_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        let target = PathBuf::from(&plan.managed_target_path);
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("must-not-delete"), b"unowned").unwrap();
        drop(journal);
        drop(core);

        let error = TorbenCore::open(paths.clone()).err().unwrap();

        assert_eq!(error.code, "source_migration_recovery_receipt_invalid");
        assert!(target.join("must-not-delete").is_file());
        let store = StateStore::open(paths.state_database()).unwrap();
        assert_eq!(
            store
                .package_installation(&plan.app_id, &plan.app_version)
                .unwrap(),
            Some(plan.current_owner)
        );
    }

    #[tokio::test]
    async fn package_to_managed_recovery_rejects_a_tampered_target_before_deleting_any_directory() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let plan = package_to_managed_plan(&core, &fixture).await;
        let journal = OperationJournal::start_package_to_managed_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let managed_target = PathBuf::from(&plan.managed_target_path);
        std::fs::create_dir_all(&managed_target).unwrap();
        std::fs::write(managed_target.join("managed.bin"), b"managed").unwrap();
        write_package_to_managed_receipt(&paths, &plan, &journal).unwrap();
        let staging = paths
            .staging_dir()
            .join(format!("install-{}-{operation_id}", plan.app_id));
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("staged.bin"), b"staged").unwrap();
        let protected = root.path().join("must-not-delete");
        std::fs::create_dir_all(&protected).unwrap();
        std::fs::write(protected.join("protected.bin"), b"protected").unwrap();
        drop(journal);
        drop(core);

        let journal_path = paths.operation_dir().join(format!("{operation_id}.json"));
        let mut content: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        content["sourceMigration"]["managedTargetPath"] =
            serde_json::json!(protected.display().to_string());
        std::fs::write(&journal_path, serde_json::to_vec_pretty(&content).unwrap()).unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "source_migration_recovery_path_invalid");
        assert!(protected.join("protected.bin").is_file());
        assert!(managed_target.join("managed.bin").is_file());
        assert!(staging.join("staged.bin").is_file());
    }

    #[tokio::test]
    async fn package_to_managed_recovery_preserves_the_target_when_the_receipt_mismatches() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let plan = package_to_managed_plan(&core, &fixture).await;
        let journal = OperationJournal::start_package_to_managed_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let managed_target = PathBuf::from(&plan.managed_target_path);
        std::fs::create_dir_all(&managed_target).unwrap();
        std::fs::write(managed_target.join("managed.bin"), b"managed").unwrap();
        write_package_to_managed_receipt(&paths, &plan, &journal).unwrap();
        drop(journal);
        drop(core);

        let journal_path = paths.operation_dir().join(format!("{operation_id}.json"));
        let mut content: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        content["sourceMigration"]["approvalToken"] = serde_json::json!("tampered");
        std::fs::write(&journal_path, serde_json::to_vec_pretty(&content).unwrap()).unwrap();

        let error = TorbenCore::open(paths).err().unwrap();

        assert_eq!(error.code, "source_migration_recovery_receipt_invalid");
        assert!(managed_target.join("managed.bin").is_file());
    }

    #[tokio::test]
    async fn package_to_managed_recovery_rolls_back_before_package_removal() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let plan = package_to_managed_plan(&core, &fixture).await;
        let journal = OperationJournal::start_package_to_managed_migration(
            &paths,
            Arc::clone(&core.store),
            &plan,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        drop(journal);
        drop(core);

        let reopened = TorbenCore::open(paths).unwrap();

        assert_eq!(
            reopened
                .store
                .package_installation(&plan.app_id, &plan.app_version)
                .unwrap(),
            Some(plan.current_owner)
        );
        assert_eq!(
            operation_states(&reopened, operation_id).last(),
            Some(&OperationState::RolledBack)
        );
    }

    #[tokio::test]
    async fn package_source_install_refuses_to_take_over_an_external_package() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = source_fixture(root.path(), Some("1.134.0"), true, true, "1.134.0");

        let error = core
            .execute_source_operation_with_service(
                source_request(SourceAction::Install, Some(&fixture.health)),
                &fixture.service,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "source_external_installation_present");
        assert!(fixture.executions.lock().unwrap().is_empty());
        assert!(core.package_installations().unwrap().is_empty());
    }

    #[tokio::test]
    async fn package_source_execution_requires_explicit_system_change_acceptance() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = source_fixture(root.path(), None, true, true, "1.134.0");
        let mut request = source_request(SourceAction::Install, Some(&fixture.health));
        request.accept_system_changes = false;

        let error = core
            .execute_source_operation_with_service(request, &fixture.service)
            .await
            .unwrap_err();

        assert_eq!(error.code, "source_operation_confirmation_required");
        assert!(fixture.executions.lock().unwrap().is_empty());
        assert!(core.operation_events().unwrap().is_empty());
    }

    #[test]
    fn resolved_source_plan_requires_the_same_reviewed_identity() {
        let mut request = source_request(SourceAction::Install, None);
        request.adapter = SourceAdapterKind::Dnf;
        let plan = SourceOperationPlan {
            action: SourceAction::Install,
            adapter: SourceAdapterKind::Dnf,
            source_id: SourceId::new("source.dnf").unwrap(),
            coordinate: request.coordinate.clone(),
            package_kind: SourcePackageKind::Native,
            package_version: request.package_version.clone(),
            executable: "dnf".to_owned(),
            preview_arguments: vec!["--assumeno".to_owned()],
            execute_arguments: vec!["code-1.134.0-1.fc42.x86_64".to_owned()],
            execution_identity: Some("code-1.134.0-1.fc42.x86_64".to_owned()),
            environment: BTreeMap::new(),
            requires_elevation: true,
            exact_version_guaranteed: true,
            mutates_system: true,
            warnings: Vec::new(),
        };

        let error = TorbenCore::validate_source_plan_approval(&request, &plan).unwrap_err();
        assert_eq!(error.code, "source_plan_approval_required");

        request.approved_execution_identity = plan.execution_identity.clone();
        TorbenCore::validate_source_plan_approval(&request, &plan).unwrap();
    }

    #[tokio::test]
    async fn package_source_uninstall_refuses_version_drift() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = source_fixture(root.path(), None, true, true, "1.134.0");
        core.execute_source_operation_with_service(
            source_request(SourceAction::Install, Some(&fixture.health)),
            &fixture.service,
        )
        .await
        .unwrap();
        *fixture.state.lock().unwrap() = Some("1.135.0".to_owned());
        let execution_count = fixture.executions.lock().unwrap().len();

        let error = core
            .execute_source_operation_with_service(
                source_request(SourceAction::Uninstall, None),
                &fixture.service,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "source_package_state_drifted");
        assert_eq!(fixture.executions.lock().unwrap().len(), execution_count);
        assert_eq!(core.package_installations().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn package_source_failure_is_reconciled_without_claiming_ownership() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = source_fixture(root.path(), None, false, false, "1.134.0");

        let error = core
            .execute_source_operation_with_service(
                source_request(SourceAction::Install, Some(&fixture.health)),
                &fixture.service,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "source_execution_failed");
        assert!(fixture.state.lock().unwrap().is_none());
        assert!(core.package_installations().unwrap().is_empty());
        assert!(
            core.operation_events()
                .unwrap()
                .iter()
                .any(|event| event.state == OperationState::Failed)
        );
        drop(core);
        assert!(TorbenCore::open(paths).is_ok());
    }

    #[tokio::test]
    async fn package_source_health_failure_leaves_the_new_package_external() {
        let root = tempdir().unwrap();
        let core = TorbenCore::open(TorbenPaths::for_test(root.path().join("workspace"))).unwrap();
        let fixture = source_fixture(root.path(), None, true, true, "9.9.9");

        let error = core
            .execute_source_operation_with_service(
                source_request(SourceAction::Install, Some(&fixture.health)),
                &fixture.service,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "source_health_check_version_mismatch");
        assert_eq!(fixture.state.lock().unwrap().as_deref(), Some("1.134.0"));
        assert!(core.package_installations().unwrap().is_empty());
        assert!(
            core.operation_events()
                .unwrap()
                .iter()
                .any(|event| event.state == OperationState::Failed)
        );
    }

    #[test]
    fn interrupted_package_source_operation_does_not_block_startup_or_change_ownership() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let health = root
            .path()
            .join(if cfg!(windows) { "code.exe" } else { "code" });
        let request = source_request(SourceAction::Install, Some(&health));
        let operation_id = OperationJournal::start_source(&paths, Arc::clone(&store), &request)
            .unwrap()
            .operation_id();
        drop(store);

        let core = TorbenCore::open(paths).unwrap();

        assert!(core.package_installations().unwrap().is_empty());
        assert_eq!(
            operation_states(&core, operation_id).last(),
            Some(&OperationState::Failed)
        );
    }

    #[test]
    fn interrupted_package_source_operation_recovers_an_atomic_ownership_commit() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let health = root
            .path()
            .join(if cfg!(windows) { "code.exe" } else { "code" });
        let request = source_request(SourceAction::Install, Some(&health));
        let operation_id = OperationJournal::start_source(&paths, Arc::clone(&store), &request)
            .unwrap()
            .operation_id();
        let source_id = SourceId::new("source.apt").unwrap();
        let installation = InstallRecord {
            app_id: request.app_id.clone(),
            version: request.app_version.clone(),
            source_id: source_id.clone(),
            scope: InstallScope::PackageManager,
            install_path: root.path().display().to_string(),
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        };
        let package = torben_contracts::PackageInstallationRecord {
            app_id: request.app_id.clone(),
            app_version: request.app_version.clone(),
            source_id,
            adapter: request.adapter,
            coordinate: request.coordinate.clone(),
            package_kind: request.package_kind,
            package_version: request.package_version.clone().unwrap(),
            architecture: "fixture".to_owned(),
            executable_path: health.display().to_string(),
            owned_by_torben: true,
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        };
        store
            .commit_package_installation(&installation, &package)
            .unwrap();
        drop(store);

        let core = TorbenCore::open(paths).unwrap();

        assert_eq!(core.package_installations().unwrap().len(), 1);
        assert_eq!(
            operation_states(&core, operation_id).last(),
            Some(&OperationState::Succeeded)
        );
    }

    #[tokio::test]
    async fn interrupted_source_migration_drops_unverified_ownership_and_requires_reconciliation() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let core = TorbenCore::open(paths.clone()).unwrap();
        let fixture = migration_fixture(root.path(), true);
        seed_package_owner(&core, &fixture);
        let plan = core
            .plan_source_migration_with_service(migration_request(&fixture), &fixture.service)
            .await
            .unwrap();
        let operation_id =
            OperationJournal::start_source_migration(&paths, Arc::clone(&core.store), &plan)
                .unwrap()
                .operation_id();
        drop(core);

        let reopened = TorbenCore::open(paths).unwrap();

        assert!(reopened.package_installations().unwrap().is_empty());
        assert_eq!(
            operation_states(&reopened, operation_id).last(),
            Some(&OperationState::Failed)
        );
    }

    fn serve_http_routes(
        mut routes: BTreeMap<String, Vec<u8>>,
    ) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            while !routes.is_empty() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap();
                let body = routes.remove(path).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (format!("http://{address}/registry.json"), handle)
    }

    fn schema_fixture_script() -> String {
        let initialize = r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"pluginId":"dev.example.schema","pluginVersion":"1.0.0","applications":[]}}"#;
        let pages = r#"{"jsonrpc":"2.0","id":2,"result":{"pluginId":"dev.example.schema","pages":[{"id":"settings","title":"Settings","description":"Fixture settings","sections":[{"id":"general","title":"General","description":null,"fields":[{"id":"mode","label":"Mode","description":null,"kind":"select","value":"safe","placeholder":null,"options":[{"value":"safe","label":"Safe"},{"value":"fast","label":"Fast"}],"readOnly":false}],"actions":[{"id":"apply","label":"Apply","description":null,"kind":"primary","enabled":true,"confirmation":null}]}]}]}}"#;
        let action = r#"{"jsonrpc":"2.0","id":3,"result":{"pluginId":"dev.example.schema","page":{"id":"settings","title":"Settings","description":"Fixture settings","sections":[{"id":"general","title":"General","description":null,"fields":[{"id":"mode","label":"Mode","description":null,"kind":"select","value":"fast","placeholder":null,"options":[{"value":"safe","label":"Safe"},{"value":"fast","label":"Fast"}],"readOnly":false}],"actions":[{"id":"apply","label":"Apply","description":null,"kind":"primary","enabled":true,"confirmation":null}]}]},"message":"Applied"}}"#;
        if cfg!(windows) {
            format!(
                "@echo off\r\nset /p request=\r\necho {initialize}\r\nset /p request=\r\necho {pages}\r\nset /p request=\r\necho {action}\r\nset /p request=\r\n"
            )
        } else {
            format!(
                "#!/bin/sh\nIFS= read -r request\nprintf '%s\\n' '{initialize}'\nIFS= read -r request\nprintf '%s\\n' '{pages}'\nIFS= read -r request\nprintf '%s\\n' '{action}'\nIFS= read -r request\n"
            )
        }
    }
}
