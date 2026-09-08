#![allow(clippy::needless_pass_by_value)]

mod scheduled_tasks;

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
#[cfg(all(windows, not(debug_assertions)))]
use torben_core::WINDOWS_DATA_ROOT_POINTER_FILE;
use torben_core::{DoctorCheck, TorbenCore};

const UPDATER_ENDPOINT: &str =
    "https://github.com/TorbenXiong/torben-app/releases/latest/download/latest.json";
const UPDATER_PUBLIC_KEY: Option<&str> = option_env!("TORBEN_UPDATER_PUBLIC_KEY");

#[cfg(all(windows, not(debug_assertions)))]
const EMBEDDED_TEMURIN_PLUGIN: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../target/release/torben-plugin-temurin.exe"
));
#[cfg(all(windows, not(debug_assertions)))]
const EMBEDDED_TORBEN_SHIM: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../target/release/torben-shim.exe"
));
#[cfg(any(not(windows), debug_assertions))]
const EMBEDDED_TEMURIN_PLUGIN: &[u8] = &[];
#[cfg(any(not(windows), debug_assertions))]
const EMBEDDED_TORBEN_SHIM: &[u8] = &[];

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
        .filter(|application| {
            application
                .capabilities
                .iter()
                .any(|capability| capability == "external-detection")
        })
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
    let app_id = AppId::new(app_id)?;
    if app_id.as_str() == "temurin" {
        return Ok(core.cached_versions(&app_id)?.unwrap_or_default());
    }
    core.versions(&app_id).await
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
async fn install_bundled_temurin_plugin(
    core: State<'_, Arc<TorbenCore>>,
    app: tauri::AppHandle,
) -> Result<PluginSummary, TorbenError> {
    let core = Arc::clone(core.inner());
    let install_core = Arc::clone(&core);
    let summary = tauri::async_runtime::spawn_blocking(move || {
        install_core.install_bundled_temurin(EMBEDDED_TEMURIN_PLUGIN, EMBEDDED_TORBEN_SHIM)
    })
    .await
    .map_err(|error| {
        TorbenError::internal(
            "The bundled Temurin plugin installation task could not be completed.",
        )
        .with_detail("reason", error.to_string())
    })??;
    scheduled_tasks::refresh_after_user_action(core, app, AppId::new("temurin")?);
    Ok(summary)
}

#[tauri::command]
async fn uninstall_bundled_temurin_plugin(
    core: State<'_, Arc<TorbenCore>>,
) -> Result<(), TorbenError> {
    let core = Arc::clone(core.inner());
    tauri::async_runtime::spawn_blocking(move || core.uninstall_bundled_temurin())
        .await
        .map_err(|error| {
            TorbenError::internal(
                "The bundled Temurin plugin uninstall task could not be completed.",
            )
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
    #[cfg(all(windows, not(debug_assertions)))]
    schedule_relocated_source_cleanup();

    let core = match startup_core() {
        Ok(Some(core)) => Arc::new(core),
        Ok(None) => return,
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

    let mut context = tauri::generate_context!();
    let windows = take_startup_windows(context.config_mut());
    let webview_data = core.paths().data_dir().join("webview");
    let scheduled_core = Arc::clone(&core);
    let builder = tauri::Builder::default()
        .setup(move |app| {
            for window in &windows {
                tauri::WebviewWindowBuilder::from_config(app, window)?
                    .data_directory(webview_data.clone())
                    .build()?;
            }
            scheduled_tasks::start(Arc::clone(&scheduled_core), app.handle().clone());
            Ok(())
        })
        .plugin(
            tauri_plugin_log::Builder::new()
                .targets([
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Stdout),
                    tauri_plugin_log::Target::new(tauri_plugin_log::TargetKind::Folder {
                        path: core.paths().log_dir().to_path_buf(),
                        file_name: Some("desktop".to_owned()),
                    }),
                ])
                .build(),
        )
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_os::init())
        .plugin(tauri_plugin_process::init())
        .plugin(updater.build());
    configure_core_commands(builder, core)
        .run(context)
        .expect("Torben App runtime failed");
}

fn startup_core() -> Result<Option<TorbenCore>, TorbenError> {
    #[cfg(all(windows, not(debug_assertions)))]
    {
        remove_redundant_windows_data_root_pointer()?;
        let paths = torben_core::TorbenPaths::discover()?;
        if paths.state_database().is_file() || paths.data_dir().is_dir() {
            return TorbenCore::open(paths).map(Some);
        }
        let executable = std::env::current_exe().map_err(|error| {
            TorbenError::new(
                "host_executable_unavailable",
                "Could not locate the Torben App executable.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let application_directory = executable.parent().ok_or_else(|| {
            TorbenError::new(
                "host_executable_unavailable",
                "The Torben App executable has no parent directory.",
            )
        })?;
        let default_base = default_windows_application_directory(application_directory);
        let Some(selection) = prompt_for_windows_data_root(&default_base)? else {
            return Ok(None);
        };
        prepare_windows_application(&executable, &selection)
    }

    #[cfg(any(not(windows), debug_assertions))]
    TorbenCore::open_default().map(Some)
}

#[cfg(all(windows, not(debug_assertions)))]
fn remove_redundant_windows_data_root_pointer() -> Result<(), TorbenError> {
    let executable = std::env::current_exe().map_err(|error| {
        TorbenError::new(
            "host_executable_unavailable",
            "Could not locate the Torben App executable.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let application_directory = executable.parent().ok_or_else(|| {
        TorbenError::new(
            "host_executable_unavailable",
            "The Torben App executable has no parent directory.",
        )
    })?;
    let pointer = application_directory.join(WINDOWS_DATA_ROOT_POINTER_FILE);
    let metadata = match std::fs::symlink_metadata(&pointer) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Ok(()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 16 * 1024 {
        return Ok(());
    }
    let Ok(selected) = std::fs::read_to_string(&pointer) else {
        return Ok(());
    };
    let selected = PathBuf::from(selected.trim());
    let sibling_data = application_directory.join("userData");
    if selected.is_absolute() && same_windows_path(&selected, &sibling_data) {
        std::fs::remove_file(&pointer).map_err(|error| {
            TorbenError::new(
                "data_root_pointer_remove_failed",
                "Could not remove the obsolete Torben App data-directory pointer.",
            )
            .with_detail("path", pointer.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
    }
    Ok(())
}

#[cfg(all(windows, not(debug_assertions)))]
fn default_windows_application_directory(application_directory: &std::path::Path) -> PathBuf {
    for drive in b'D'..=b'Z' {
        let root = PathBuf::from(format!("{}:\\", char::from(drive)));
        if root.is_dir() {
            return root.join("TorbenApp");
        }
    }
    application_directory.to_path_buf()
}

#[cfg(all(windows, not(debug_assertions)))]
fn prepare_windows_application(
    executable: &std::path::Path,
    application_directory: &std::path::Path,
) -> Result<Option<TorbenCore>, TorbenError> {
    if !application_directory.is_absolute() {
        return Err(TorbenError::new(
            "data_root_prompt_invalid",
            "The selected Torben App base directory must be absolute.",
        ));
    }
    std::fs::create_dir_all(application_directory).map_err(|error| {
        TorbenError::new(
            "application_directory_create_failed",
            "Could not create the Torben App base directory.",
        )
        .with_detail("path", application_directory.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    let data_directory = application_directory.join("userData");
    std::fs::create_dir_all(&data_directory).map_err(|error| {
        TorbenError::new(
            "data_directory_create_failed",
            "Could not create the Torben App data directory.",
        )
        .with_detail("path", data_directory.display().to_string())
        .with_detail("reason", error.to_string())
    })?;

    let target_executable = application_directory.join("TorbenApp.exe");
    if !same_windows_path(executable, &target_executable) {
        replace_windows_executable(executable, &target_executable)?;
        std::process::Command::new(&target_executable)
            .arg("--torben-relocated-source")
            .arg(executable)
            .spawn()
            .map_err(|error| {
                TorbenError::new(
                    "application_relaunch_failed",
                    "The relocated Torben App could not be started.",
                )
                .with_detail("path", target_executable.display().to_string())
                .with_detail("reason", error.to_string())
            })?;
        return Ok(None);
    }

    TorbenCore::open(torben_core::TorbenPaths::discover()?).map(Some)
}

#[cfg(all(windows, not(debug_assertions)))]
fn schedule_relocated_source_cleanup() {
    let mut arguments = std::env::args_os();
    let mut source = None;
    while let Some(argument) = arguments.next() {
        if argument == "--torben-relocated-source" {
            source = arguments.next().map(PathBuf::from);
            break;
        }
    }
    let Some(source) = source else {
        return;
    };
    let Ok(executable) = std::env::current_exe() else {
        return;
    };
    if same_windows_path(&source, &executable) || !identical_regular_files(&source, &executable) {
        return;
    }
    std::thread::spawn(move || {
        for _ in 0..100 {
            match std::fs::remove_file(&source) {
                Ok(()) => return,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(100)),
            }
        }
    });
}

#[cfg(all(windows, not(debug_assertions)))]
fn identical_regular_files(left: &std::path::Path, right: &std::path::Path) -> bool {
    let metadata = |path: &std::path::Path| std::fs::symlink_metadata(path).ok();
    let (Some(left_metadata), Some(right_metadata)) = (metadata(left), metadata(right)) else {
        return false;
    };
    if left_metadata.file_type().is_symlink()
        || right_metadata.file_type().is_symlink()
        || !left_metadata.is_file()
        || !right_metadata.is_file()
        || left_metadata.len() != right_metadata.len()
    {
        return false;
    }
    let (Ok(mut left_file), Ok(mut right_file)) =
        (std::fs::File::open(left), std::fs::File::open(right))
    else {
        return false;
    };
    let mut left_buffer = vec![0_u8; 64 * 1024];
    let mut right_buffer = vec![0_u8; 64 * 1024];
    loop {
        let Ok(left_read) = std::io::Read::read(&mut left_file, &mut left_buffer) else {
            return false;
        };
        let Ok(right_read) = std::io::Read::read(&mut right_file, &mut right_buffer) else {
            return false;
        };
        if left_read != right_read || left_buffer[..left_read] != right_buffer[..right_read] {
            return false;
        }
        if left_read == 0 {
            return true;
        }
    }
}

#[cfg(all(windows, not(debug_assertions)))]
fn same_windows_path(left: &std::path::Path, right: &std::path::Path) -> bool {
    let normalize = |path: &std::path::Path| {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .replace('/', "\\")
            .to_ascii_lowercase()
    };
    normalize(left) == normalize(right)
}

#[cfg(all(windows, not(debug_assertions)))]
fn replace_windows_executable(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<(), TorbenError> {
    let metadata = std::fs::symlink_metadata(source).map_err(|error| {
        TorbenError::new(
            "host_executable_unavailable",
            "Could not inspect the current Torben App executable.",
        )
        .with_detail("reason", error.to_string())
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TorbenError::new(
            "host_executable_unavailable",
            "The current Torben App executable is not a regular file.",
        ));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(destination)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(TorbenError::new(
            "application_target_invalid",
            "The target TorbenApp.exe is not a regular file.",
        )
        .with_detail("path", destination.display().to_string()));
    }
    let staged = destination.with_extension("exe.next");
    let previous = destination.with_extension("exe.previous");
    remove_regular_windows_file(&staged)?;
    remove_regular_windows_file(&previous)?;
    std::fs::copy(source, &staged).map_err(|error| {
        TorbenError::new(
            "application_copy_failed",
            "Could not copy TorbenApp.exe to the selected base directory.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let had_destination = destination.exists();
    if had_destination {
        std::fs::rename(destination, &previous).map_err(|error| {
            let _ = std::fs::remove_file(&staged);
            TorbenError::new(
                "application_replace_failed",
                "Could not stage the existing TorbenApp.exe for replacement.",
            )
            .with_detail("reason", error.to_string())
        })?;
    }
    if let Err(error) = std::fs::rename(&staged, destination) {
        if had_destination {
            let _ = std::fs::rename(&previous, destination);
        }
        let _ = std::fs::remove_file(&staged);
        return Err(TorbenError::new(
            "application_replace_failed",
            "Could not activate the TorbenApp.exe replacement.",
        )
        .with_detail("reason", error.to_string()));
    }
    remove_regular_windows_file(&previous)
}

#[cfg(all(windows, not(debug_assertions)))]
fn remove_regular_windows_file(path: &std::path::Path) -> Result<(), TorbenError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(TorbenError::new(
                "application_target_invalid",
                "A Torben App replacement path is not a regular file.",
            )
            .with_detail("path", path.display().to_string()))
        }
        Ok(_) => std::fs::remove_file(path).map_err(|error| {
            TorbenError::new(
                "application_replace_failed",
                "Could not remove a staged Torben App executable.",
            )
            .with_detail("reason", error.to_string())
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(TorbenError::new(
            "application_replace_failed",
            "Could not inspect a staged Torben App executable.",
        )
        .with_detail("reason", error.to_string())),
    }
}

#[cfg(all(windows, not(debug_assertions)))]
#[allow(clippy::too_many_lines)]
fn prompt_for_windows_data_root(
    application_directory: &std::path::Path,
) -> Result<Option<PathBuf>, TorbenError> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const SCRIPT: &str = r"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()
$defaultBase = $env:TORBEN_FIRST_RUN_APPLICATION_DIR
if ([string]::IsNullOrWhiteSpace($defaultBase)) { exit 3 }
$dataPath = Join-Path -Path $defaultBase -ChildPath 'userData'
$isChinese = [System.Globalization.CultureInfo]::CurrentUICulture.Name.StartsWith('zh')
if ($isChinese) {
  $windowTitle = 'Torben App · 初次设置'
  $heading = '确认存储位置'
  $baseLabelText = '基准目录（将存放 TorbenApp.exe）'
  $dataLabelText = '实际数据目录'
  $hint = '插件、缓存和日志均保存在 userData 中；替换 TorbenApp.exe 不会影响这些数据。'
  $useLabel = '使用此位置'
  $chooseLabel = '更改基准目录'
  $cancelLabel = '取消'
  $folderTitle = '选择 Torben App 数据目录的基准位置'
} else {
  $windowTitle = 'Torben App · First setup'
  $heading = 'Confirm storage location'
  $baseLabelText = 'Base directory (where TorbenApp.exe will be stored)'
  $dataLabelText = 'Actual data directory'
  $hint = 'Plugins, caches, and logs stay in userData when TorbenApp.exe is replaced.'
  $useLabel = 'Use this location'
  $chooseLabel = 'Change base directory'
  $cancelLabel = 'Cancel'
  $folderTitle = 'Select the base location for Torben App data'
}

$form = [System.Windows.Forms.Form]::new()
$form.Text = $windowTitle
$form.ClientSize = [System.Drawing.Size]::new(620, 340)
$form.FormBorderStyle = [System.Windows.Forms.FormBorderStyle]::FixedDialog
$form.StartPosition = [System.Windows.Forms.FormStartPosition]::CenterScreen
$form.AutoScaleMode = [System.Windows.Forms.AutoScaleMode]::Dpi
$form.MaximizeBox = $false
$form.MinimizeBox = $false
$form.TopMost = $true

$headingLabel = [System.Windows.Forms.Label]::new()
$headingLabel.Location = [System.Drawing.Point]::new(24, 22)
$headingLabel.Size = [System.Drawing.Size]::new(572, 30)
$headingLabel.Font = [System.Drawing.Font]::new('Segoe UI', 12, [System.Drawing.FontStyle]::Bold)
$headingLabel.Text = $heading

$baseLabel = [System.Windows.Forms.Label]::new()
$baseLabel.Location = [System.Drawing.Point]::new(24, 63)
$baseLabel.Size = [System.Drawing.Size]::new(572, 22)
$baseLabel.Font = [System.Drawing.Font]::new('Segoe UI', 9)
$baseLabel.Text = $baseLabelText

$baseBox = [System.Windows.Forms.TextBox]::new()
$baseBox.Location = [System.Drawing.Point]::new(24, 88)
$baseBox.Size = [System.Drawing.Size]::new(572, 32)
$baseBox.Font = [System.Drawing.Font]::new('Segoe UI', 10)
$baseBox.ReadOnly = $true
$baseBox.BackColor = [System.Drawing.SystemColors]::Window
$baseBox.Text = $defaultBase

$dataLabel = [System.Windows.Forms.Label]::new()
$dataLabel.Location = [System.Drawing.Point]::new(24, 140)
$dataLabel.Size = [System.Drawing.Size]::new(572, 22)
$dataLabel.Font = [System.Drawing.Font]::new('Segoe UI', 9)
$dataLabel.Text = $dataLabelText

$dataBox = [System.Windows.Forms.TextBox]::new()
$dataBox.Location = [System.Drawing.Point]::new(24, 165)
$dataBox.Size = [System.Drawing.Size]::new(572, 32)
$dataBox.Font = [System.Drawing.Font]::new('Segoe UI', 10)
$dataBox.ReadOnly = $true
$dataBox.BackColor = [System.Drawing.SystemColors]::Window
$dataBox.Text = $dataPath

$hintLabel = [System.Windows.Forms.Label]::new()
$hintLabel.Location = [System.Drawing.Point]::new(24, 218)
$hintLabel.Size = [System.Drawing.Size]::new(572, 38)
$hintLabel.Font = [System.Drawing.Font]::new('Segoe UI', 9)
$hintLabel.ForeColor = [System.Drawing.SystemColors]::GrayText
$hintLabel.Text = $hint

$useButton = [System.Windows.Forms.Button]::new()
$useButton.Location = [System.Drawing.Point]::new(86, 282)
$useButton.Size = [System.Drawing.Size]::new(160, 36)
$useButton.Text = $useLabel
$useButton.Add_Click({ $form.DialogResult = [System.Windows.Forms.DialogResult]::OK; $form.Close() })

$chooseButton = [System.Windows.Forms.Button]::new()
$chooseButton.Location = [System.Drawing.Point]::new(256, 282)
$chooseButton.Size = [System.Drawing.Size]::new(160, 36)
$chooseButton.Text = $chooseLabel
$chooseButton.Add_Click({ $form.DialogResult = [System.Windows.Forms.DialogResult]::Retry; $form.Close() })

$cancelButton = [System.Windows.Forms.Button]::new()
$cancelButton.Location = [System.Drawing.Point]::new(426, 282)
$cancelButton.Size = [System.Drawing.Size]::new(170, 36)
$cancelButton.Text = $cancelLabel
$cancelButton.Add_Click({ $form.DialogResult = [System.Windows.Forms.DialogResult]::Cancel; $form.Close() })

$form.AcceptButton = $useButton
$form.CancelButton = $cancelButton
$form.Controls.AddRange(@(
  $headingLabel,
  $baseLabel,
  $baseBox,
  $dataLabel,
  $dataBox,
  $hintLabel,
  $useButton,
  $chooseButton,
  $cancelButton
))
$answer = $form.ShowDialog()
$form.Dispose()
if ($answer -eq [System.Windows.Forms.DialogResult]::OK) {
  [Console]::Out.Write('DEFAULT')
  exit 0
}
if ($answer -eq [System.Windows.Forms.DialogResult]::Cancel) { exit 2 }

$dialog = New-Object System.Windows.Forms.FolderBrowserDialog
$dialog.Description = $folderTitle
$dialog.ShowNewFolderButton = $true
$dialog.SelectedPath = $defaultBase
if (Test-Path -LiteralPath $defaultBase -PathType Container) {
  $dialog.SelectedPath = $defaultBase
}
if ($dialog.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) {
  [Console]::Out.Write($dialog.SelectedPath)
  $dialog.Dispose()
  exit 0
}
$dialog.Dispose()
exit 2
";
    let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
        TorbenError::new(
            "data_root_prompt_unavailable",
            "Windows did not provide its system directory for the first-run data prompt.",
        )
    })?;
    let powershell =
        PathBuf::from(system_root).join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = std::process::Command::new(&powershell)
        .args(["-NoLogo", "-NoProfile", "-STA", "-Command", SCRIPT])
        .env("TORBEN_FIRST_RUN_APPLICATION_DIR", application_directory)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|error| {
            TorbenError::new(
                "data_root_prompt_unavailable",
                "Could not open the first-run data-directory prompt.",
            )
            .with_detail("reason", error.to_string())
        })?;
    if output.status.code() == Some(2) {
        return Ok(None);
    }
    if !output.status.success() {
        return Err(TorbenError::new(
            "data_root_prompt_failed",
            "The first-run data-directory prompt failed.",
        )
        .with_detail("exitCode", output.status.code().unwrap_or(-1).to_string()));
    }
    let selected = String::from_utf8(output.stdout).map_err(|error| {
        TorbenError::new(
            "data_root_prompt_invalid",
            "The first-run data-directory prompt returned invalid text.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let selected = selected.trim();
    let selected_base = if selected == "DEFAULT" {
        application_directory.to_path_buf()
    } else {
        PathBuf::from(selected)
    };
    if selected_base.as_os_str().is_empty() || !selected_base.is_absolute() {
        return Err(TorbenError::new(
            "data_root_prompt_invalid",
            "The selected Torben App data-directory base must be an absolute path.",
        ));
    }
    Ok(Some(selected_base))
}

fn take_startup_windows(config: &mut tauri::Config) -> Vec<tauri::utils::config::WindowConfig> {
    // Config dataDirectory accepts relative paths only. Build windows explicitly so the
    // absolute Core path takes effect before WebView2 can create an AppData profile.
    config
        .app
        .windows
        .iter_mut()
        .filter_map(|window| {
            if !window.create {
                return None;
            }
            let startup = window.clone();
            window.create = false;
            Some(startup)
        })
        .collect()
}

fn configure_core_commands(
    builder: tauri::Builder<tauri::Wry>,
    core: Arc<TorbenCore>,
) -> tauri::Builder<tauri::Wry> {
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
            install_bundled_temurin_plugin,
            uninstall_bundled_temurin_plugin,
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
        external_discovery_task_result, merge_external_discovery, take_startup_windows,
        validate_updater_public_key,
    };

    #[test]
    fn startup_windows_are_created_once_with_their_original_settings() {
        let mut config: tauri::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let expected = config.app.windows[0].clone();
        let mut deferred = expected.clone();
        deferred.label = "deferred".to_owned();
        deferred.create = false;
        config.app.windows.push(deferred);

        let windows = take_startup_windows(&mut config);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0], expected);
        assert!(config.app.windows.iter().all(|window| !window.create));
        assert!(take_startup_windows(&mut config).is_empty());
    }

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
