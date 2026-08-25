use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AppId, ApplicationDescriptor, ExactVersion, OperationId, PluginId, SourceId, TorbenError,
    VersionDescriptor,
};

pub const PLUGIN_PROTOCOL_VERSION: u32 = 1;

pub mod method {
    pub const INITIALIZE: &str = "initialize";
    pub const APP_DESCRIBE: &str = "app.describe";
    pub const VERSIONS_LIST: &str = "versions.list";
    pub const VERSION_RESOLVE: &str = "version.resolve";
    pub const EXTERNAL_DISCOVER: &str = "external.discover";
    pub const INSTALL_PLAN: &str = "install.plan";
    pub const HEALTH_CHECK: &str = "health.check";
    pub const UNINSTALL_PLAN: &str = "uninstall.plan";
    pub const SCHEMA_PAGES: &str = "schema.pages";
    pub const SCHEMA_ACTION: &str = "schema.action";
    pub const OPERATION_EVENT: &str = "operation.event";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginCapability {
    VersionDiscovery,
    ExternalDiscovery,
    ManagedInstall,
    GlobalSelection,
    ManagedUninstall,
    SchemaUi,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PluginPermissions {
    #[serde(default)]
    pub network_domains: Vec<String>,
    #[serde(default)]
    pub filesystem_roots: Vec<String>,
    #[serde(default)]
    pub external_commands: Vec<String>,
    #[serde(default)]
    pub package_managers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginTarget {
    pub target: String,
    pub executable: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: PluginId,
    pub display_name: String,
    pub version: ExactVersion,
    pub protocol_version: u32,
    pub minimum_host_version: ExactVersion,
    pub publisher: String,
    pub capabilities: Vec<PluginCapability>,
    pub permissions: PluginPermissions,
    pub targets: Vec<PluginTarget>,
    pub signature: Option<String>,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginOrigin {
    BuiltIn,
    OfficialRegistry,
    Sideloaded,
}

pub const PLUGIN_REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRegistry {
    pub schema_version: u32,
    pub sequence: u64,
    pub generated_at: String,
    pub minimum_host_version: ExactVersion,
    pub publishers: Vec<PluginRegistryPublisher>,
    pub entries: Vec<PluginRegistryEntry>,
    pub signature: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRegistryStatus {
    pub configured: bool,
    pub source_url: Option<String>,
    pub cache_path: String,
    pub sequence: Option<u64>,
    pub generated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRegistryPublisher {
    pub id: String,
    pub display_name: String,
    pub public_key: String,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRegistryEntry {
    pub plugin_id: PluginId,
    pub version: ExactVersion,
    pub publisher_id: String,
    pub manifest_path: String,
    pub manifest_sha256: String,
    #[serde(default)]
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginSummary {
    pub id: PluginId,
    pub display_name: String,
    pub version: ExactVersion,
    pub enabled: bool,
    pub origin: PluginOrigin,
    pub publisher: String,
    pub capabilities: Vec<PluginCapability>,
    pub permissions: PluginPermissions,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            id,
            method: method.into(),
            params,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<TorbenError>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginOperationEvent {
    pub operation_id: OperationId,
    pub phase: String,
    pub message: String,
    pub progress: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub protocol_version: u32,
    pub host_version: ExactVersion,
    pub target: String,
    pub locale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResult {
    pub protocol_version: u32,
    pub plugin_id: PluginId,
    pub plugin_version: ExactVersion,
    pub applications: Vec<ApplicationDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionListParams {
    pub app_id: AppId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionListResult {
    pub versions: Vec<VersionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveVersionParams {
    pub app_id: AppId,
    pub requested: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveVersionResult {
    pub requested: String,
    pub resolved: ExactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDiscoverParams {
    pub app_id: AppId,
    pub managed_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalDiscoverResult {
    pub installations: Vec<crate::InstallRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlanParams {
    pub operation_id: OperationId,
    pub app_id: AppId,
    pub version: ExactVersion,
    pub source_id: SourceId,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstallStep {
    Download {
        url: String,
        destination_name: String,
    },
    VerifySha256Manifest {
        manifest_url: String,
        signature_url: Option<String>,
        archive_name: String,
    },
    VerifySha256 {
        archive_name: String,
        expected: String,
    },
    VerifyDetachedSignature {
        archive_name: String,
        signature_url: String,
        public_key_url: String,
        trusted_fingerprint: String,
    },
    VerifySigstoreBundle {
        archive_name: String,
        bundle_url: String,
        certificate_identity: String,
        oidc_issuer: String,
    },
    VerifyGitReleaseSignature {
        archive_name: String,
        signature_url: String,
        trusted_fingerprint: String,
    },
    ExtractArchive {
        archive_name: String,
        strip_components: usize,
    },
    InstallWithPythonManager {
        tag: String,
    },
    BuildPythonSource {
        archive_name: String,
        configure_arguments: Vec<String>,
    },
    BuildGitSource {
        archive_name: String,
        make_arguments: Vec<String>,
    },
    HealthCheck {
        executable: String,
        arguments: Vec<String>,
        expected_output: String,
    },
    CreateShims {
        commands: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallPlan {
    pub app_id: AppId,
    pub version: ExactVersion,
    pub source_id: SourceId,
    pub steps: Vec<InstallStep>,
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckParams {
    pub app_id: AppId,
    pub version: ExactVersion,
    pub install_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheckResult {
    pub healthy: bool,
    pub actual_version: Option<ExactVersion>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallPlanParams {
    pub operation_id: OperationId,
    pub app_id: AppId,
    pub version: ExactVersion,
    pub source_id: SourceId,
    pub install_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallPlan {
    pub app_id: AppId,
    pub version: ExactVersion,
    pub source_id: SourceId,
    pub install_path: String,
    pub preserve_user_data: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaPage {
    pub id: String,
    pub title: String,
    pub description: Option<String>,
    pub sections: Vec<SchemaSection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaSection {
    pub id: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub fields: Vec<SchemaField>,
    pub actions: Vec<SchemaAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaFieldKind {
    Text,
    Boolean,
    Select,
    Status,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaField {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub kind: SchemaFieldKind,
    pub value: Option<String>,
    pub placeholder: Option<String>,
    #[serde(default)]
    pub options: Vec<SchemaOption>,
    pub read_only: bool,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaActionKind {
    Primary,
    Secondary,
    Destructive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaAction {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub kind: SchemaActionKind,
    pub enabled: bool,
    pub confirmation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaPageListResult {
    pub plugin_id: PluginId,
    pub pages: Vec<SchemaPage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaPageListParams {
    pub plugin_id: PluginId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaActionParams {
    pub plugin_id: PluginId,
    pub page_id: String,
    pub section_id: String,
    pub action_id: String,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaActionResult {
    pub plugin_id: PluginId,
    pub page: SchemaPage,
    pub message: Option<String>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{JsonRpcRequest, PluginRegistryStatus, SchemaActionKind, SchemaFieldKind};

    #[test]
    fn request_uses_json_rpc_two() {
        let request = JsonRpcRequest::new(7, "initialize", json!({ "protocolVersion": 1 }));
        let value = serde_json::to_value(request).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
    }

    #[test]
    fn registry_status_uses_stable_nullable_wire_fields() {
        let value = serde_json::to_value(PluginRegistryStatus {
            configured: false,
            source_url: None,
            cache_path: "cache/registry.json".to_owned(),
            sequence: None,
            generated_at: None,
        })
        .unwrap();

        assert_eq!(value["configured"], false);
        assert_eq!(value["sourceUrl"], serde_json::Value::Null);
        assert_eq!(value["sequence"], serde_json::Value::Null);
    }

    #[test]
    fn schema_kinds_use_stable_wire_values() {
        assert_eq!(
            serde_json::to_value(SchemaFieldKind::Boolean).unwrap(),
            "boolean"
        );
        assert_eq!(
            serde_json::to_value(SchemaActionKind::Destructive).unwrap(),
            "destructive"
        );
    }
}
