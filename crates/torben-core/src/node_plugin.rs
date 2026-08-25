use std::{
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use torben_contracts::{
    AppId, ExactVersion, InstallRecord, OperationId, PluginId, SourceId, TorbenError, TorbenResult,
    VersionDescriptor,
    plugin::{
        ExternalDiscoverParams, ExternalDiscoverResult, HealthCheckParams, HealthCheckResult,
        InitializeParams, InitializeResult, InstallPlan, InstallPlanParams,
        PLUGIN_PROTOCOL_VERSION, PluginCapability, PluginManifest, PluginOperationEvent,
        ResolveVersionParams, ResolveVersionResult, SchemaActionParams, SchemaActionResult,
        SchemaPage, SchemaPageListParams, SchemaPageListResult, UninstallPlan, UninstallPlanParams,
        VersionListParams, VersionListResult, method,
    },
};
use torben_plugin_host::PluginClient;

const PLUGIN_CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub(crate) struct BundledPlugin {
    plugin_id: PluginId,
    app_id: AppId,
    source_id: SourceId,
    capabilities: Vec<PluginCapability>,
    candidates: Vec<PathBuf>,
}

impl BundledPlugin {
    pub(crate) fn node() -> TorbenResult<Self> {
        Self::discover(
            "torben-plugin-node",
            "app.torben.plugin.node",
            "node",
            "node.official",
            include_str!("../../../plugins/node/plugin.manifest.template.json"),
        )
    }

    pub(crate) fn temurin() -> TorbenResult<Self> {
        Self::discover(
            "torben-plugin-temurin",
            "app.torben.plugin.temurin",
            "temurin",
            "temurin.official",
            include_str!("../../../plugins/temurin/plugin.manifest.template.json"),
        )
    }

    pub(crate) fn python() -> TorbenResult<Self> {
        Self::discover(
            "torben-plugin-python",
            "app.torben.plugin.python",
            "python",
            "python.official",
            include_str!("../../../plugins/python/plugin.manifest.template.json"),
        )
    }

    pub(crate) fn git() -> TorbenResult<Self> {
        Self::discover(
            "torben-plugin-git",
            "app.torben.plugin.git",
            "git",
            "git.official",
            include_str!("../../../plugins/git/plugin.manifest.template.json"),
        )
    }

    pub(crate) fn vscode() -> TorbenResult<Self> {
        Self::discover(
            "torben-plugin-vscode",
            "app.torben.plugin.vscode",
            "vscode",
            "vscode.official",
            include_str!("../../../plugins/vscode/plugin.manifest.template.json"),
        )
    }

    pub(crate) fn codex() -> TorbenResult<Self> {
        Self::discover(
            "torben-plugin-codex",
            "app.torben.plugin.codex",
            "codex",
            "codex.official",
            include_str!("../../../plugins/codex/plugin.manifest.template.json"),
        )
    }

    fn discover(
        binary_name: &str,
        plugin_id: &str,
        app_id: &str,
        source_id: &str,
        manifest_json: &str,
    ) -> TorbenResult<Self> {
        let expected_plugin_id = PluginId::new(plugin_id)?;
        let manifest: PluginManifest = serde_json::from_str(manifest_json).map_err(|error| {
            TorbenError::new(
                "bundled_plugin_manifest_invalid",
                "A bundled plugin manifest is not valid JSON.",
            )
            .with_detail("pluginId", plugin_id)
            .with_detail("reason", error.to_string())
        })?;
        if manifest.id != expected_plugin_id || manifest.protocol_version != PLUGIN_PROTOCOL_VERSION
        {
            return Err(TorbenError::new(
                "bundled_plugin_manifest_invalid",
                "A bundled plugin manifest identity or protocol version is invalid.",
            )
            .with_detail("pluginId", plugin_id));
        }
        let current_executable = std::env::current_exe().map_err(|error| {
            TorbenError::new(
                "host_executable_unavailable",
                "Could not locate the Torben App executable.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let executable_directory = current_executable.parent().ok_or_else(|| {
            TorbenError::new(
                "host_executable_unavailable",
                "The Torben App executable has no parent directory.",
            )
        })?;
        let filename = format!("{binary_name}{}", std::env::consts::EXE_SUFFIX);
        let mut candidates = vec![
            executable_directory.join(&filename),
            executable_directory.join("plugins").join(&filename),
        ];
        if executable_directory.ends_with("deps")
            && let Some(target_directory) = executable_directory.parent()
        {
            candidates.push(target_directory.join(&filename));
        }
        #[cfg(target_os = "macos")]
        if let Some(contents_directory) = executable_directory.parent() {
            candidates.push(
                contents_directory
                    .join("Resources")
                    .join("plugins")
                    .join(&filename),
            );
        }
        Ok(Self {
            plugin_id: expected_plugin_id,
            app_id: AppId::new(app_id)?,
            source_id: SourceId::new(source_id)?,
            capabilities: manifest.capabilities,
            candidates,
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub(crate) fn node_from_executable(executable: PathBuf) -> Self {
        Self::from_executable(
            executable,
            PluginId::new("app.torben.plugin.node").unwrap(),
            AppId::new("node").unwrap(),
            SourceId::new("node.official").unwrap(),
        )
    }

    #[cfg(test)]
    pub(crate) fn temurin_from_executable(executable: PathBuf) -> Self {
        Self::from_executable(
            executable,
            PluginId::new("app.torben.plugin.temurin").unwrap(),
            AppId::new("temurin").unwrap(),
            SourceId::new("temurin.official").unwrap(),
        )
    }

    #[cfg(all(test, windows))]
    pub(crate) fn python_from_executable(executable: PathBuf) -> Self {
        Self::from_executable(
            executable,
            PluginId::new("app.torben.plugin.python").unwrap(),
            AppId::new("python").unwrap(),
            SourceId::new("python.official").unwrap(),
        )
    }

    #[cfg(all(test, windows))]
    pub(crate) fn git_from_executable(executable: PathBuf) -> Self {
        Self::from_executable(
            executable,
            PluginId::new("app.torben.plugin.git").unwrap(),
            AppId::new("git").unwrap(),
            SourceId::new("git.official").unwrap(),
        )
    }

    #[cfg(all(test, windows))]
    pub(crate) fn vscode_from_executable(executable: PathBuf) -> Self {
        Self::from_executable(
            executable,
            PluginId::new("app.torben.plugin.vscode").unwrap(),
            AppId::new("vscode").unwrap(),
            SourceId::new("vscode.official").unwrap(),
        )
    }

    #[cfg(all(test, windows))]
    pub(crate) fn codex_from_executable(executable: PathBuf) -> Self {
        Self::from_executable(
            executable,
            PluginId::new("app.torben.plugin.codex").unwrap(),
            AppId::new("codex").unwrap(),
            SourceId::new("codex.official").unwrap(),
        )
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn from_executable(
        executable: PathBuf,
        plugin_id: PluginId,
        app_id: AppId,
        source_id: SourceId,
    ) -> Self {
        Self {
            plugin_id,
            app_id,
            source_id,
            capabilities: vec![
                PluginCapability::VersionDiscovery,
                PluginCapability::ExternalDiscovery,
                PluginCapability::ManagedInstall,
                PluginCapability::GlobalSelection,
                PluginCapability::ManagedUninstall,
                PluginCapability::SchemaUi,
            ],
            candidates: vec![executable],
        }
    }

    pub(crate) async fn connect(&self) -> TorbenResult<BundledPluginSession> {
        let executable = self.executable().ok_or_else(|| {
            TorbenError::new(
                "bundled_plugin_missing",
                "The bundled Node.js plugin executable is missing.",
            )
            .with_detail(
                "searchedPaths",
                self.candidates
                    .iter()
                    .map(|candidate| candidate.display().to_string())
                    .collect::<Vec<_>>()
                    .join(";"),
            )
            .with_remediation("Reinstall Torben App or rebuild the complete workspace.")
        })?;
        let mut client = PluginClient::spawn_bundled_scoped(
            executable,
            &self.capabilities,
            PLUGIN_CALL_TIMEOUT,
        )?;
        let host_version = ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?;
        let initialized: InitializeResult = client
            .call(
                method::INITIALIZE,
                &InitializeParams {
                    protocol_version: PLUGIN_PROTOCOL_VERSION,
                    host_version: host_version.clone(),
                    target: current_target(),
                    locale: "en-US".to_owned(),
                },
            )
            .await?;
        let exposes_application = initialized
            .applications
            .iter()
            .any(|application| application.id == self.app_id);
        if initialized.protocol_version != PLUGIN_PROTOCOL_VERSION
            || initialized.plugin_id != self.plugin_id
            || initialized.plugin_version != host_version
            || !exposes_application
        {
            return Err(TorbenError::new(
                "bundled_plugin_identity_mismatch",
                "The bundled Node.js plugin identity does not match the host package.",
            )
            .with_detail("pluginId", initialized.plugin_id.to_string())
            .with_detail("pluginVersion", initialized.plugin_version.to_string())
            .with_detail("protocolVersion", initialized.protocol_version.to_string()));
        }
        Ok(BundledPluginSession {
            client,
            source_id: self.source_id.clone(),
        })
    }

    pub(crate) fn diagnostic(&self) -> (bool, String) {
        self.executable().map_or_else(
            || {
                (
                    false,
                    self.candidates
                        .iter()
                        .map(|candidate| candidate.display().to_string())
                        .collect::<Vec<_>>()
                        .join(";"),
                )
            },
            |executable| (true, executable.display().to_string()),
        )
    }

    fn executable(&self) -> Option<&Path> {
        self.candidates
            .iter()
            .find(|candidate| candidate.is_file())
            .map(PathBuf::as_path)
    }
}

pub(crate) struct BundledPluginSession {
    client: PluginClient,
    source_id: SourceId,
}

impl BundledPluginSession {
    pub(crate) async fn versions(
        &mut self,
        app_id: &AppId,
    ) -> TorbenResult<Vec<VersionDescriptor>> {
        let result: VersionListResult = self
            .client
            .call(
                method::VERSIONS_LIST,
                &VersionListParams {
                    app_id: app_id.clone(),
                },
            )
            .await?;
        Ok(result.versions)
    }

    pub(crate) async fn resolve_version(
        &mut self,
        app_id: &AppId,
        requested: &str,
    ) -> TorbenResult<ExactVersion> {
        let result: ResolveVersionResult = self
            .client
            .call(
                method::VERSION_RESOLVE,
                &ResolveVersionParams {
                    app_id: app_id.clone(),
                    requested: requested.to_owned(),
                },
            )
            .await?;
        if result.requested != requested {
            return Err(TorbenError::new(
                "plugin_response_mismatch",
                "The plugin resolved a different version request.",
            )
            .with_detail("requested", requested)
            .with_detail("pluginRequested", result.requested));
        }
        Ok(result.resolved)
    }

    pub(crate) async fn external_installations(
        &mut self,
        app_id: &AppId,
        managed_root: &Path,
    ) -> TorbenResult<Vec<InstallRecord>> {
        let result: ExternalDiscoverResult = self
            .client
            .call(
                method::EXTERNAL_DISCOVER,
                &ExternalDiscoverParams {
                    app_id: app_id.clone(),
                    managed_root: managed_root.display().to_string(),
                },
            )
            .await?;
        if result
            .installations
            .iter()
            .any(|record| &record.app_id != app_id)
        {
            return Err(TorbenError::new(
                "plugin_response_mismatch",
                "The plugin returned an installation for a different application.",
            ));
        }
        Ok(result.installations)
    }

    pub(crate) async fn install_plan(
        &mut self,
        operation_id: OperationId,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<InstallPlan> {
        self.client
            .call(
                method::INSTALL_PLAN,
                &InstallPlanParams {
                    operation_id,
                    app_id: app_id.clone(),
                    version: version.clone(),
                    source_id: self.source_id.clone(),
                    target: current_target(),
                },
            )
            .await
    }

    pub(crate) async fn install_plan_with_events<F>(
        &mut self,
        operation_id: OperationId,
        app_id: &AppId,
        version: &ExactVersion,
        on_event: F,
    ) -> TorbenResult<InstallPlan>
    where
        F: FnMut(PluginOperationEvent) -> TorbenResult<()> + Send,
    {
        self.client
            .call_with_operation_events(
                method::INSTALL_PLAN,
                &InstallPlanParams {
                    operation_id,
                    app_id: app_id.clone(),
                    version: version.clone(),
                    source_id: self.source_id.clone(),
                    target: current_target(),
                },
                operation_id,
                on_event,
            )
            .await
    }

    pub(crate) async fn health_check(&mut self, record: &InstallRecord) -> TorbenResult<()> {
        let result: HealthCheckResult = self
            .client
            .call(
                method::HEALTH_CHECK,
                &HealthCheckParams {
                    app_id: record.app_id.clone(),
                    version: record.version.clone(),
                    install_path: record.install_path.clone(),
                },
            )
            .await?;
        if !result.healthy || result.actual_version.as_ref() != Some(&record.version) {
            return Err(TorbenError::new(
                "plugin_health_check_invalid",
                "The plugin did not confirm the exact managed version.",
            )
            .with_detail("appId", record.app_id.to_string())
            .with_detail("expectedVersion", record.version.to_string())
            .with_detail(
                "actualVersion",
                result
                    .actual_version
                    .map_or_else(|| "none".to_owned(), |version| version.to_string()),
            )
            .with_detail("pluginMessage", result.message));
        }
        Ok(())
    }

    pub(crate) async fn uninstall_plan(
        &mut self,
        operation_id: OperationId,
        record: &InstallRecord,
    ) -> TorbenResult<UninstallPlan> {
        self.client
            .call(
                method::UNINSTALL_PLAN,
                &UninstallPlanParams {
                    operation_id,
                    app_id: record.app_id.clone(),
                    version: record.version.clone(),
                    source_id: record.source_id.clone(),
                    install_path: record.install_path.clone(),
                },
            )
            .await
    }

    pub(crate) async fn uninstall_plan_with_events<F>(
        &mut self,
        operation_id: OperationId,
        record: &InstallRecord,
        on_event: F,
    ) -> TorbenResult<UninstallPlan>
    where
        F: FnMut(PluginOperationEvent) -> TorbenResult<()> + Send,
    {
        self.client
            .call_with_operation_events(
                method::UNINSTALL_PLAN,
                &UninstallPlanParams {
                    operation_id,
                    app_id: record.app_id.clone(),
                    version: record.version.clone(),
                    source_id: record.source_id.clone(),
                    install_path: record.install_path.clone(),
                },
                operation_id,
                on_event,
            )
            .await
    }

    pub(crate) async fn schema_pages(
        &mut self,
        plugin_id: &PluginId,
    ) -> TorbenResult<Vec<SchemaPage>> {
        let result: SchemaPageListResult = self
            .client
            .call(
                method::SCHEMA_PAGES,
                &SchemaPageListParams {
                    plugin_id: plugin_id.clone(),
                },
            )
            .await?;
        if &result.plugin_id != plugin_id {
            return Err(TorbenError::new(
                "plugin_response_mismatch",
                "The plugin returned schema pages for a different plugin.",
            ));
        }
        Ok(result.pages)
    }

    pub(crate) async fn schema_action(
        &mut self,
        params: &SchemaActionParams,
    ) -> TorbenResult<SchemaActionResult> {
        self.client.call(method::SCHEMA_ACTION, params).await
    }

    pub(crate) async fn shutdown(self) -> TorbenResult<()> {
        self.client.shutdown().await
    }
}

pub(crate) fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use torben_contracts::plugin::PluginCapability;

    use super::BundledPlugin;

    #[test]
    fn bundled_manifests_declare_every_invoked_capability() {
        let plugins = [
            BundledPlugin::node().unwrap(),
            BundledPlugin::temurin().unwrap(),
            BundledPlugin::python().unwrap(),
            BundledPlugin::git().unwrap(),
            BundledPlugin::vscode().unwrap(),
            BundledPlugin::codex().unwrap(),
        ];
        let required = [
            PluginCapability::VersionDiscovery,
            PluginCapability::ExternalDiscovery,
            PluginCapability::ManagedInstall,
            PluginCapability::GlobalSelection,
            PluginCapability::ManagedUninstall,
            PluginCapability::SchemaUi,
        ];

        for plugin in plugins {
            for capability in &required {
                assert!(
                    plugin.capabilities.contains(capability),
                    "{} is missing {capability:?}",
                    plugin.plugin_id
                );
            }
        }
    }

    #[tokio::test]
    async fn missing_bundled_plugin_fails_without_starting_a_process() {
        let plugin =
            BundledPlugin::node_from_executable(PathBuf::from("definitely-missing-plugin"));
        let Err(error) = plugin.connect().await else {
            panic!("missing plugin unexpectedly started");
        };
        assert_eq!(error.code, "bundled_plugin_missing");
    }
}
