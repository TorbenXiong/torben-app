#![allow(clippy::needless_pass_by_value)]

use std::{collections::BTreeMap, path::PathBuf, str::FromStr, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use minisign_verify::PublicKey;
use serde::Serialize;
use tauri::State;
use torben_contracts::{
    AppId, ApplicationDescriptor, ExactVersion, InstallRecord, ManagedLibraryMigrationResult,
    ManagedLibraryStatus, ManagedToPackageMigrationPlan, ManagedToPackageMigrationResult,
    ManagedUpdateCheck, ManagedUpdateResult, OperationEvent, OperationId, PackageCoordinate,
    PackageInstallationRecord, PackageToManagedMigrationPlan, PackageToManagedMigrationRequest,
    PackageToManagedMigrationResult, PluginId, SelectionRecord, ShellIntegrationStatus,
    SourceAction, SourceAdapterKind, SourceAdapterStatus, SourceExecutionRequest,
    SourceExecutionResult, SourceMigrationPlan, SourceMigrationRequest, SourceMigrationResult,
    SourceOperationPlan, SourcePackageKind, SourcePackageVersion, TorbenError, UserSettings,
    VersionDescriptor,
    plugin::{PluginRegistryStatus, PluginSummary, SchemaActionResult, SchemaPage},
};
use torben_core::{DoctorCheck, TorbenCore};

const UPDATER_ENDPOINT: &str =
    "https://github.com/TorbenXiong/torben-app/releases/latest/download/latest.json";
const UPDATER_PUBLIC_KEY: Option<&str> = option_env!("TORBEN_UPDATER_PUBLIC_KEY");

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DesktopUpdaterConfiguration {
    configured: bool,
    current_version: &'static str,
    endpoint: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardSnapshot {
    applications: Vec<ApplicationDescriptor>,
    installed: Vec<InstallRecord>,
    selected: Vec<SelectionRecord>,
    external: Vec<InstallRecord>,
    warnings: Vec<DashboardWarning>,
    operations: Vec<OperationEvent>,
    plugins: Vec<PluginSummary>,
    plugin_registry: PluginRegistryStatus,
    doctor: Vec<DoctorCheck>,
    source_adapters: Vec<SourceAdapterStatus>,
    package_installations: Vec<PackageInstallationRecord>,
    updater: DesktopUpdaterConfiguration,
    settings: UserSettings,
    shell_integration: ShellIntegrationStatus,
    managed_library: ManagedLibraryStatus,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DashboardWarning {
    app_id: AppId,
    code: String,
    message: String,
    details: BTreeMap<String, String>,
    remediation: Option<String>,
}

impl DashboardWarning {
    fn from_external_discovery(app_id: AppId, error: TorbenError) -> Self {
        Self {
            app_id,
            code: error.code,
            message: error.message,
            details: error.details,
            remediation: error.remediation,
        }
    }
}

#[tauri::command]
async fn dashboard_snapshot(
    core: State<'_, Arc<TorbenCore>>,
) -> Result<DashboardSnapshot, TorbenError> {
    let core = Arc::clone(core.inner());
    let applications = core.applications()?;
    let (external, warnings) = collect_external_installations(&core, &applications).await;
    let source_adapters = core.source_adapter_statuses().await?;
    let settings = core.user_settings()?;
    Ok(DashboardSnapshot {
        applications,
        installed: core.installed()?,
        selected: core.selections()?,
        external,
        warnings,
        operations: core.operation_events()?,
        plugins: core.plugins()?,
        plugin_registry: core.official_plugin_registry_status()?,
        doctor: core.doctor()?,
        source_adapters,
        package_installations: core.package_installations()?,
        updater: desktop_updater_configuration(),
        settings,
        shell_integration: core.shell_integration_status()?,
        managed_library: core.managed_library_status()?,
    })
}

async fn collect_external_installations(
    core: &Arc<TorbenCore>,
    applications: &[ApplicationDescriptor],
) -> (Vec<InstallRecord>, Vec<DashboardWarning>) {
    let discoveries = applications
        .iter()
        .map(|application| {
            let app_id = application.id.clone();
            let core = Arc::clone(core);
            let task_app_id = app_id.clone();
            let task = tauri::async_runtime::spawn(async move {
                core.external_installations(&task_app_id).await
            });
            (app_id, task)
        })
        .collect::<Vec<_>>();
    let mut external = Vec::new();
    let mut warnings = Vec::new();
    for (app_id, discovery) in discoveries {
        let result = external_discovery_task_result(discovery.await);
        merge_external_discovery(&app_id, result, &mut external, &mut warnings);
    }
    (external, warnings)
}

fn external_discovery_task_result(
    result: tauri::Result<Result<Vec<InstallRecord>, TorbenError>>,
) -> Result<Vec<InstallRecord>, TorbenError> {
    match result {
        Ok(result) => result,
        Err(error) => Err(TorbenError::new(
            "external_discovery_task_failed",
            "The external installation discovery task stopped unexpectedly.",
        )
        .with_detail("reason", error.to_string())
        .with_remediation("Inspect the application provider and retry discovery.")),
    }
}

fn merge_external_discovery(
    app_id: &AppId,
    result: Result<Vec<InstallRecord>, TorbenError>,
    external: &mut Vec<InstallRecord>,
    warnings: &mut Vec<DashboardWarning>,
) {
    match result {
        Ok(records) => external.extend(records),
        Err(error) => warnings.push(DashboardWarning::from_external_discovery(
            app_id.clone(),
            error,
        )),
    }
}

fn desktop_updater_configuration() -> DesktopUpdaterConfiguration {
    DesktopUpdaterConfiguration {
        configured: UPDATER_PUBLIC_KEY.is_some(),
        current_version: env!("CARGO_PKG_VERSION"),
        endpoint: UPDATER_ENDPOINT,
    }
}

fn validate_updater_public_key(value: Option<&str>) -> Result<Option<String>, TorbenError> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.trim();
    if value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(updater_public_key_error());
    }
    let decoded = BASE64_STANDARD
        .decode(value)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok());
    if decoded
        .as_deref()
        .and_then(|key| PublicKey::decode(key).ok())
        .is_none()
    {
        return Err(updater_public_key_error());
    }
    Ok(Some(value.to_owned()))
}

fn updater_public_key_error() -> TorbenError {
    TorbenError::new(
            "updater_public_key_invalid",
            "The compiled updater public key is not a valid Base64-encoded minisign public key.",
        )
        .with_remediation(
            "Build without TORBEN_UPDATER_PUBLIC_KEY for a development artifact, or provide only the reviewed minisign public key.",
        )
}

#[tauri::command]
async fn list_versions(
    core: State<'_, Arc<TorbenCore>>,
    app_id: String,
) -> Result<Vec<VersionDescriptor>, TorbenError> {
    list_versions_for_core(core.inner(), app_id).await
}

async fn list_versions_for_core(
    core: &TorbenCore,
    app_id: String,
) -> Result<Vec<VersionDescriptor>, TorbenError> {
    core.versions(&AppId::new(app_id)?).await
}

#[tauri::command]
async fn install_app(
    core: State<'_, Arc<TorbenCore>>,
    app_id: String,
    version: String,
) -> Result<InstallRecord, TorbenError> {
    install_app_for_core(core.inner(), app_id, version).await
}

async fn install_app_for_core(
    core: &TorbenCore,
    app_id: String,
    version: String,
) -> Result<InstallRecord, TorbenError> {
    core.install(&AppId::new(app_id)?, &version).await
}

#[tauri::command]
async fn select_version(
    core: State<'_, Arc<TorbenCore>>,
    app_id: String,
    version: String,
) -> Result<(), TorbenError> {
    select_version_for_core(core.inner(), app_id, version).await
}

async fn select_version_for_core(
    core: &TorbenCore,
    app_id: String,
    version: String,
) -> Result<(), TorbenError> {
    core.select(&AppId::new(app_id)?, &ExactVersion::from_str(&version)?)
        .await
}

#[tauri::command]
fn clear_selection(core: State<'_, Arc<TorbenCore>>, app_id: String) -> Result<(), TorbenError> {
    clear_selection_for_core(core.inner(), app_id)
}

fn clear_selection_for_core(core: &TorbenCore, app_id: String) -> Result<(), TorbenError> {
    core.clear_selection(&AppId::new(app_id)?)
}

#[tauri::command]
async fn uninstall_app(
    core: State<'_, Arc<TorbenCore>>,
    app_id: String,
    version: String,
) -> Result<(), TorbenError> {
    uninstall_app_for_core(core.inner(), app_id, version).await
}

async fn uninstall_app_for_core(
    core: &TorbenCore,
    app_id: String,
    version: String,
) -> Result<(), TorbenError> {
    core.uninstall(&AppId::new(app_id)?, &ExactVersion::from_str(&version)?)
        .await
}

#[tauri::command]
async fn check_managed_updates(
    core: State<'_, Arc<TorbenCore>>,
    app_id: Option<String>,
) -> Result<ManagedUpdateCheck, TorbenError> {
    let app_id = app_id.map(AppId::new).transpose()?;
    let core = Arc::clone(core.inner());
    core.managed_update_check(app_id.as_ref()).await
}

#[tauri::command]
async fn apply_managed_update(
    core: State<'_, Arc<TorbenCore>>,
    app_id: String,
    installed_version: String,
    available_version: String,
) -> Result<ManagedUpdateResult, TorbenError> {
    let core = Arc::clone(core.inner());
    core.apply_managed_update(
        &AppId::new(app_id)?,
        &ExactVersion::from_str(&installed_version)?,
        &ExactVersion::from_str(&available_version)?,
    )
    .await
}

#[tauri::command]
fn set_managed_auto_update(
    core: State<'_, Arc<TorbenCore>>,
    app_id: String,
    enabled: bool,
) -> Result<UserSettings, TorbenError> {
    core.set_managed_auto_update(&AppId::new(app_id)?, enabled)?;
    core.user_settings()
}

#[tauri::command]
fn run_doctor(core: State<'_, Arc<TorbenCore>>) -> Result<Vec<DoctorCheck>, TorbenError> {
    core.doctor()
}

#[tauri::command]
async fn plan_source_operation(
    core: State<'_, Arc<TorbenCore>>,
    action: String,
    adapter: String,
    package: String,
    package_kind: String,
    package_version: Option<String>,
) -> Result<SourceOperationPlan, TorbenError> {
    core.plan_source_operation(
        SourceAction::from_str(&action)?,
        SourceAdapterKind::from_str(&adapter)?,
        PackageCoordinate::from_str(&package)?,
        SourcePackageKind::from_str(&package_kind)?,
        package_version
            .as_deref()
            .map(SourcePackageVersion::from_str)
            .transpose()?,
    )
    .await
}

#[tauri::command]
async fn execute_source_operation(
    core: State<'_, Arc<TorbenCore>>,
    request: SourceExecutionRequest,
) -> Result<SourceExecutionResult, TorbenError> {
    let core = Arc::clone(core.inner());
    core.execute_source_operation(request).await
}

#[tauri::command]
async fn plan_source_migration(
    core: State<'_, Arc<TorbenCore>>,
    request: SourceMigrationRequest,
) -> Result<SourceMigrationPlan, TorbenError> {
    let core = Arc::clone(core.inner());
    core.plan_source_migration(request).await
}

#[tauri::command]
async fn execute_source_migration(
    core: State<'_, Arc<TorbenCore>>,
    request: SourceMigrationRequest,
) -> Result<SourceMigrationResult, TorbenError> {
    let core = Arc::clone(core.inner());
    core.execute_source_migration(request).await
}

#[tauri::command]
async fn plan_managed_to_package_migration(
    core: State<'_, Arc<TorbenCore>>,
    request: SourceMigrationRequest,
) -> Result<ManagedToPackageMigrationPlan, TorbenError> {
    let core = Arc::clone(core.inner());
    core.plan_managed_to_package_migration(request).await
}

#[tauri::command]
async fn execute_managed_to_package_migration(
    core: State<'_, Arc<TorbenCore>>,
    request: SourceMigrationRequest,
) -> Result<ManagedToPackageMigrationResult, TorbenError> {
    let core = Arc::clone(core.inner());
    core.execute_managed_to_package_migration(request).await
}

#[tauri::command]
async fn plan_package_to_managed_migration(
    core: State<'_, Arc<TorbenCore>>,
    request: PackageToManagedMigrationRequest,
) -> Result<PackageToManagedMigrationPlan, TorbenError> {
    let core = Arc::clone(core.inner());
    core.plan_package_to_managed_migration(request).await
}

#[tauri::command]
async fn execute_package_to_managed_migration(
    core: State<'_, Arc<TorbenCore>>,
    request: PackageToManagedMigrationRequest,
) -> Result<PackageToManagedMigrationResult, TorbenError> {
    let core = Arc::clone(core.inner());
    core.execute_package_to_managed_migration(request).await
}

#[tauri::command]
fn list_operations(core: State<'_, Arc<TorbenCore>>) -> Result<Vec<OperationEvent>, TorbenError> {
    list_operations_for_core(core.inner())
}

fn list_operations_for_core(core: &TorbenCore) -> Result<Vec<OperationEvent>, TorbenError> {
    core.operation_events()
}

#[tauri::command]
fn official_plugin_registry_status(
    core: State<'_, Arc<TorbenCore>>,
) -> Result<PluginRegistryStatus, TorbenError> {
    core.official_plugin_registry_status()
}

#[tauri::command]
async fn refresh_official_plugin_registry(
    core: State<'_, Arc<TorbenCore>>,
) -> Result<PluginRegistryStatus, TorbenError> {
    core.refresh_official_plugin_registry().await
}

#[tauri::command]
async fn install_plugin(
    core: State<'_, Arc<TorbenCore>>,
    manifest_path: PathBuf,
    developer_mode: bool,
) -> Result<PluginSummary, TorbenError> {
    let core = Arc::clone(core.inner());
    tauri::async_runtime::spawn_blocking(move || {
        core.install_plugin(&manifest_path, developer_mode)
    })
    .await
    .map_err(|error| {
        TorbenError::internal("The plugin installation task could not be completed.")
            .with_detail("reason", error.to_string())
    })?
}

#[tauri::command]
async fn install_official_plugin(
    core: State<'_, Arc<TorbenCore>>,
    registry_path: PathBuf,
    plugin_id: String,
    version: Option<String>,
) -> Result<PluginSummary, TorbenError> {
    let plugin_id = PluginId::new(plugin_id)?;
    let version = version.as_deref().map(ExactVersion::from_str).transpose()?;
    let core = Arc::clone(core.inner());
    tauri::async_runtime::spawn_blocking(move || {
        core.install_official_plugin(&registry_path, &plugin_id, version.as_ref())
    })
    .await
    .map_err(|error| {
        TorbenError::internal("The official plugin installation task could not be completed.")
            .with_detail("reason", error.to_string())
    })?
}

#[tauri::command]
async fn install_official_plugin_from_registry(
    core: State<'_, Arc<TorbenCore>>,
    plugin_id: String,
    version: Option<String>,
) -> Result<PluginSummary, TorbenError> {
    let plugin_id = PluginId::new(plugin_id)?;
    let version = version.as_deref().map(ExactVersion::from_str).transpose()?;
    core.install_official_plugin_from_registry(&plugin_id, version.as_ref())
        .await
}

#[tauri::command]
fn set_plugin_enabled(
    core: State<'_, Arc<TorbenCore>>,
    plugin_id: String,
    enabled: bool,
) -> Result<(), TorbenError> {
    core.set_plugin_enabled(&torben_contracts::PluginId::new(plugin_id)?, enabled)
}

#[tauri::command]
async fn plugin_schema_pages(
    core: State<'_, Arc<TorbenCore>>,
    plugin_id: String,
) -> Result<Vec<SchemaPage>, TorbenError> {
    core.plugin_schema_pages(&PluginId::new(plugin_id)?).await
}

#[tauri::command]
async fn invoke_plugin_schema_action(
    core: State<'_, Arc<TorbenCore>>,
    plugin_id: String,
    page_id: String,
    section_id: String,
    action_id: String,
    values: BTreeMap<String, String>,
    confirmed: bool,
) -> Result<SchemaActionResult, TorbenError> {
    core.invoke_plugin_schema_action(
        &PluginId::new(plugin_id)?,
        &page_id,
        &section_id,
        &action_id,
        values,
        confirmed,
    )
    .await
}

#[tauri::command]
fn cancel_operation(
    core: State<'_, Arc<TorbenCore>>,
    operation_id: String,
) -> Result<(), TorbenError> {
    core.cancel_operation(OperationId::from_str(&operation_id)?)
}

#[tauri::command]
fn update_settings(
    core: State<'_, Arc<TorbenCore>>,
    settings: UserSettings,
) -> Result<(), TorbenError> {
    core.update_user_settings(&settings)
}

#[tauri::command]
fn set_shell_integration(
    core: State<'_, Arc<TorbenCore>>,
    enabled: bool,
) -> Result<ShellIntegrationStatus, TorbenError> {
    if enabled {
        core.enable_shell_integration()
    } else {
        core.disable_shell_integration()
    }
}

#[tauri::command]
async fn migrate_managed_library(
    core: State<'_, Arc<TorbenCore>>,
    target_path: PathBuf,
) -> Result<ManagedLibraryMigrationResult, TorbenError> {
    let core = Arc::clone(core.inner());
    tauri::async_runtime::spawn_blocking(move || core.migrate_managed_library(&target_path))
        .await
        .map_err(|error| {
            TorbenError::internal("The managed library migration task could not be completed.")
                .with_detail("reason", error.to_string())
        })?
}

/// Starts the Torben App desktop runtime.
///
/// # Panics
///
/// Panics if Tauri cannot initialize or run its platform event loop.
pub fn run() {
    let core = match TorbenCore::open_default() {
        Ok(core) => Arc::new(core),
        Err(error) => {
            eprintln!(
                "Torben App startup failed [{}]: {}",
                error.code, error.message
            );
            return;
        }
    };
    let updater_public_key = match validate_updater_public_key(UPDATER_PUBLIC_KEY) {
        Ok(value) => value,
        Err(error) => {
            eprintln!(
                "Torben App startup failed [{}]: {}",
                error.code, error.message
            );
            return;
        }
    };
    let mut updater = tauri_plugin_updater::Builder::new();
    if let Some(public_key) = updater_public_key {
        updater = updater.pubkey(public_key);
    }

    let builder = tauri::Builder::default()
        .plugin(tauri_plugin_log::Builder::new().build())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(updater.build());
    configure_core_commands(builder, core)
        .run(tauri::generate_context!())
        .expect("Torben App runtime failed");
}

fn configure_core_commands<R: tauri::Runtime>(
    builder: tauri::Builder<R>,
    core: Arc<TorbenCore>,
) -> tauri::Builder<R> {
    builder
        .manage(core)
        .invoke_handler(tauri::generate_handler![
            dashboard_snapshot,
            list_versions,
            install_app,
            select_version,
            clear_selection,
            uninstall_app,
            check_managed_updates,
            apply_managed_update,
            set_managed_auto_update,
            plan_source_operation,
            execute_source_operation,
            plan_source_migration,
            execute_source_migration,
            plan_managed_to_package_migration,
            execute_managed_to_package_migration,
            plan_package_to_managed_migration,
            execute_package_to_managed_migration,
            run_doctor,
            list_operations,
            official_plugin_registry_status,
            refresh_official_plugin_registry,
            install_plugin,
            install_official_plugin,
            install_official_plugin_from_registry,
            set_plugin_enabled,
            plugin_schema_pages,
            invoke_plugin_schema_action,
            cancel_operation,
            update_settings,
            set_shell_integration,
            migrate_managed_library,
        ])
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
    use torben_contracts::{
        AppId, ExactVersion, InstallRecord, InstallScope, SourceId, TorbenError,
    };

    use super::{
        external_discovery_task_result, merge_external_discovery, validate_updater_public_key,
    };

    #[test]
    fn updater_key_is_optional_but_never_accepts_private_or_control_input() {
        assert_eq!(validate_updater_public_key(None).unwrap(), None);
        let public_key = BASE64_STANDARD.encode(
            "untrusted comment: minisign public key E7620F1842B4E81F\n\
             RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3",
        );
        assert!(
            validate_updater_public_key(Some(&public_key))
                .unwrap()
                .is_some()
        );
        assert!(validate_updater_public_key(Some("private key fixture")).is_err());
        assert!(validate_updater_public_key(Some("public\0key")).is_err());
    }

    #[test]
    fn external_discovery_failure_becomes_a_warning_without_dropping_other_records() {
        let node = AppId::new("node").unwrap();
        let git = AppId::new("git").unwrap();
        let mut external = Vec::new();
        let mut warnings = Vec::new();

        merge_external_discovery(
            &node,
            Err(TorbenError::new(
                "plugin_response_malformed",
                "The Node.js plugin returned malformed data.",
            )
            .with_detail("method", "external.discover")
            .with_remediation("Inspect the Node.js plugin and retry discovery.")),
            &mut external,
            &mut warnings,
        );
        merge_external_discovery(
            &git,
            Ok(vec![InstallRecord {
                app_id: git.clone(),
                version: ExactVersion::from_str("2.55.0").unwrap(),
                source_id: SourceId::new("git.external").unwrap(),
                scope: InstallScope::External,
                install_path: "C:/Program Files/Git/cmd/git.exe".to_owned(),
                installed_at: "2026-08-25T00:00:00Z".to_owned(),
                health: "healthy".to_owned(),
            }]),
            &mut external,
            &mut warnings,
        );

        assert_eq!(external.len(), 1);
        assert_eq!(external[0].app_id, git);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].app_id, node);
        assert_eq!(warnings[0].code, "plugin_response_malformed");
        assert_eq!(warnings[0].details["method"], "external.discover");
        assert!(warnings[0].remediation.is_some());
        let serialized = serde_json::to_value(&warnings[0]).unwrap();
        assert_eq!(serialized["appId"], "node");
        assert_eq!(serialized["code"], "plugin_response_malformed");
    }

    #[test]
    fn stopped_external_discovery_task_becomes_a_structured_error() {
        let task = tauri::async_runtime::spawn(std::future::pending::<
            Result<Vec<InstallRecord>, TorbenError>,
        >());
        task.abort();
        let joined = tauri::async_runtime::block_on(task);
        let error = external_discovery_task_result(joined).unwrap_err();

        assert_eq!(error.code, "external_discovery_task_failed");
        assert!(error.details.contains_key("reason"));
        assert!(error.remediation.is_some());
    }

    #[cfg(feature = "test-fixtures")]
    mod node_commands {
        use std::{
            collections::BTreeMap,
            io::{Read as _, Write as _},
            net::TcpListener,
            path::{Path, PathBuf},
            process::Command,
            str::FromStr,
            sync::atomic::{AtomicU64, Ordering},
            thread,
            time::{SystemTime, UNIX_EPOCH},
        };

        use serde_json::json;
        use torben_contracts::{ExactVersion, plugin::PLUGIN_PROTOCOL_VERSION};
        use torben_core::{
            NodeFixtureConfiguration, NodeProvider, TorbenCore, TorbenPaths, test_fixtures,
        };

        use super::super::{
            clear_selection_for_core, install_app_for_core, list_operations_for_core,
            list_versions_for_core, select_version_for_core, uninstall_app_for_core,
        };

        const VERSION: &str = "24.19.0";
        const SIGNATURE: &[u8] = b"torben-real-desktop-node-fixture-signature-v1";
        static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

        struct IsolatedRoot {
            path: PathBuf,
        }

        impl IsolatedRoot {
            fn new() -> Self {
                let nonce = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
                let timestamp = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time follows the Unix epoch")
                    .as_nanos();
                let path = std::env::temp_dir().join(format!(
                    "torben-desktop-node-command-{}-{timestamp}-{nonce}",
                    std::process::id()
                ));
                std::fs::create_dir(&path).expect("create isolated desktop fixture root");
                Self { path }
            }
        }

        impl Drop for IsolatedRoot {
            fn drop(&mut self) {
                let owned = self
                    .path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("torben-desktop-node-command-"));
                if owned {
                    let _ = std::fs::remove_dir_all(&self.path);
                }
            }
        }

        #[test]
        #[allow(clippy::too_many_lines)]
        fn desktop_commands_complete_the_managed_node_lifecycle() {
            tauri::async_runtime::block_on(async {
                let root = IsolatedRoot::new();
                let version = ExactVersion::from_str(VERSION).expect("valid fixture version");
                let distribution = NodeProvider::official()
                    .expect("create official provider without network access")
                    .distribution(&version)
                    .expect("resolve the current target distribution");
                let fixture_node = compile_rust_executable(
                    &root.path,
                    "fixture-node",
                    &format!("fn main() {{ println!(\"v{VERSION}\"); }}\n"),
                );
                let archive = test_fixtures::build_node_archive(
                    &distribution,
                    &std::fs::read(&fixture_node).expect("read fixture Node.js executable"),
                )
                .expect("build fixture Node.js archive");
                let manifest = format!(
                    "{}  {}\n",
                    test_fixtures::sha256_hex(&archive),
                    distribution.archive_name
                );
                let version_prefix = format!("/dist/v{VERSION}");
                let routes = BTreeMap::from([
                    (
                        format!("{version_prefix}/SHASUMS256.txt"),
                        manifest.into_bytes(),
                    ),
                    (
                        format!("{version_prefix}/SHASUMS256.txt.sig"),
                        SIGNATURE.to_vec(),
                    ),
                    (
                        format!("{version_prefix}/{}", distribution.archive_name),
                        archive,
                    ),
                ]);
                let (base_url, server) = fixture_server(routes);
                let paths = TorbenPaths::for_test(root.path.join("data"));
                let install_path = paths.app_version_dir("node", VERSION);
                let plugin = compile_fixture_plugin(
                    &root.path,
                    &base_url,
                    &distribution.archive_name,
                    &install_path,
                );
                let core = TorbenCore::open_node_fixture(
                    paths,
                    NodeFixtureConfiguration {
                        base_url,
                        checksum_signature: SIGNATURE.to_vec(),
                        plugin_executable: plugin,
                        shim_executable: fixture_node,
                    },
                )
                .expect("open the desktop Node.js fixture Core");

                let versions = list_versions_for_core(&core, "node".to_owned())
                    .await
                    .expect("list Node.js versions through the desktop command boundary");
                assert_eq!(versions[0].version, version);

                let installed = install_app_for_core(&core, "node".to_owned(), "lts".to_owned())
                    .await
                    .expect("install Node.js through the desktop command boundary");
                server.join().expect("finish the fixture HTTP server");
                assert_eq!(installed.app_id.as_str(), "node");
                assert_eq!(installed.version, version);
                assert_eq!(installed.health, "healthy");
                assert!(install_path.is_dir());

                select_version_for_core(&core, "node".to_owned(), VERSION.to_owned())
                    .await
                    .expect("select Node.js through the desktop command boundary");
                assert_eq!(
                    core.selections().expect("read selection")[0].version,
                    version
                );

                clear_selection_for_core(&core, "node".to_owned())
                    .expect("clear Node.js selection through the desktop command boundary");
                assert!(
                    core.selections()
                        .expect("read cleared selections")
                        .is_empty()
                );

                uninstall_app_for_core(&core, "node".to_owned(), VERSION.to_owned())
                    .await
                    .expect("uninstall Node.js through the desktop command boundary");
                assert!(core.installed().expect("read installations").is_empty());
                assert!(!install_path.exists());

                let events = list_operations_for_core(&core)
                    .expect("list operation events through the desktop command boundary");
                let terminal_successes = events
                    .iter()
                    .filter(|event| event.state == torben_contracts::OperationState::Succeeded)
                    .count();
                assert_eq!(terminal_successes, 4);
            });
        }

        fn compile_rust_executable(directory: &Path, name: &str, source: &str) -> PathBuf {
            let source_path = directory.join(format!("{name}.rs"));
            let executable = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&source_path, source).expect("write Rust fixture source");
            let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
            let output = Command::new(rustc)
                .arg(&source_path)
                .arg("-o")
                .arg(&executable)
                .output()
                .expect("run rustc for fixture executable");
            assert!(
                output.status.success(),
                "fixture rustc failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            executable
        }

        #[allow(clippy::too_many_lines)]
        fn compile_fixture_plugin(
            directory: &Path,
            base_url: &str,
            archive_name: &str,
            install_path: &Path,
        ) -> PathBuf {
            let initialize = json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {
                    "protocolVersion": PLUGIN_PROTOCOL_VERSION,
                    "pluginId": "app.torben.plugin.node",
                    "pluginVersion": env!("CARGO_PKG_VERSION"),
                    "applications": [{
                        "id": "node",
                        "displayName": "Node.js",
                        "summary": "real desktop command lifecycle fixture",
                        "categories": ["runtime"],
                        "capabilities": ["versions", "install", "select", "uninstall"],
                        "sources": [{
                            "id": "node.official",
                            "displayName": "Official Node.js archive",
                            "managed": true
                        }]
                    }]
                }
            })
            .to_string();
            let versions = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": { "versions": [{
                    "version": VERSION,
                    "ltsName": "Krypton",
                    "releasedAt": "2026-08-03",
                    "recommended": true
                }] }
            })
            .to_string();
            let resolved = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": { "requested": "lts", "resolved": VERSION }
            })
            .to_string();
            let version_url = format!("{base_url}v{VERSION}/");
            let plan = json!({
                "jsonrpc": "2.0",
                "id": 3,
                "result": {
                    "appId": "node",
                    "version": VERSION,
                    "sourceId": "node.official",
                    "steps": [
                        {
                            "type": "download",
                            "url": format!("{version_url}{archive_name}"),
                            "destination_name": archive_name
                        },
                        {
                            "type": "verify_sha256_manifest",
                            "manifest_url": format!("{version_url}SHASUMS256.txt"),
                            "signature_url": format!("{version_url}SHASUMS256.txt.sig"),
                            "archive_name": archive_name
                        },
                        {
                            "type": "extract_archive",
                            "archive_name": archive_name,
                            "strip_components": 0
                        },
                        {
                            "type": "health_check",
                            "executable": "node",
                            "arguments": ["--version"],
                            "expected_output": format!("v{VERSION}")
                        },
                        { "type": "create_shims", "commands": ["node", "npm", "npx"] }
                    ],
                    "metadata": { "target": test_fixtures::node_plugin_target() }
                }
            })
            .to_string();
            let health = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "healthy": true,
                    "actualVersion": VERSION,
                    "message": "healthy"
                }
            })
            .to_string();
            let uninstall = json!({
                "jsonrpc": "2.0",
                "id": 2,
                "result": {
                    "appId": "node",
                    "version": VERSION,
                    "sourceId": "node.official",
                    "installPath": install_path.display().to_string(),
                    "preserveUserData": true
                }
            })
            .to_string();
            let source = format!(
                r#"use std::io::{{BufRead, Write}};
fn main() {{
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {{
        let line = line.expect("read request");
        let response = if line.contains("initialize") {{ Some({initialize:?}) }}
            else if line.contains("versions.list") {{ Some({versions:?}) }}
            else if line.contains("version.resolve") {{ Some({resolved:?}) }}
            else if line.contains("uninstall.plan") {{ Some({uninstall:?}) }}
            else if line.contains("install.plan") {{ Some({plan:?}) }}
            else if line.contains("health.check") {{ Some({health:?}) }}
            else if line.contains("shutdown") {{ None }}
            else {{ std::process::exit(2) }};
        if let Some(response) = response {{
            writeln!(stdout, "{{}}", response).expect("write response");
            stdout.flush().expect("flush response");
        }} else {{ break; }}
    }}
}}
"#
            );
            compile_rust_executable(directory, "fixture-plugin", &source)
        }

        fn fixture_server(routes: BTreeMap<String, Vec<u8>>) -> (String, thread::JoinHandle<()>) {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture HTTP server");
            let address = listener.local_addr().expect("read fixture HTTP address");
            let expected_requests = routes.len();
            let server = thread::spawn(move || {
                for _ in 0..expected_requests {
                    let (mut stream, _) = listener.accept().expect("accept fixture HTTP request");
                    let mut request = [0_u8; 4096];
                    let read = stream
                        .read(&mut request)
                        .expect("read fixture HTTP request");
                    let request = String::from_utf8_lossy(&request[..read]);
                    let path = request
                        .lines()
                        .next()
                        .and_then(|line| line.split_whitespace().nth(1))
                        .expect("fixture HTTP request path");
                    let (status, body) = routes.get(path).map_or_else(
                        || ("404 Not Found", b"not found".as_slice()),
                        |body| ("200 OK", body.as_slice()),
                    );
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream
                        .write_all(response.as_bytes())
                        .expect("write fixture HTTP headers");
                    stream.write_all(body).expect("write fixture HTTP body");
                    stream.flush().expect("flush fixture HTTP response");
                }
            });
            (format!("http://{address}/dist/"), server)
        }
    }
}
