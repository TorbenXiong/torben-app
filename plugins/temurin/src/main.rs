#![allow(clippy::needless_pass_by_value)]

use std::{collections::BTreeMap, str::FromStr};

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use torben_contracts::{
    AppId, ApplicationDescriptor, ExactVersion, InstallSource, PluginId, SourceId, TorbenError,
    plugin::{
        ExternalDiscoverParams, ExternalDiscoverResult, HealthCheckParams, HealthCheckResult,
        InitializeParams, InitializeResult, InstallPlan, InstallPlanParams, InstallStep,
        JsonRpcError, JsonRpcRequest, JsonRpcResponse, PLUGIN_PROTOCOL_VERSION,
        ResolveVersionParams, ResolveVersionResult, SchemaField, SchemaFieldKind, SchemaPage,
        SchemaPageListParams, SchemaPageListResult, SchemaSection, UninstallPlan,
        UninstallPlanParams, VersionListParams, VersionListResult, method,
    },
};
use torben_core::TemurinProvider;

const PLUGIN_ID: &str = "app.torben.plugin.temurin";
const APP_ID: &str = "temurin";
const SOURCE_ID: &str = "temurin.official";
const ADOPTIUM_RELEASE_FINGERPRINT: &str = "3B04D753C9050D9A5D343F39843C48A565F8F04B";

#[tokio::main]
async fn main() {
    if let Err(error) = serve().await {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}

async fn serve() -> Result<(), TorbenError> {
    let provider = TemurinProvider::official()?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut output = tokio::io::stdout();
    while let Some(line) = lines.next_line().await.map_err(io_error)? {
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => handle(&provider, request).await,
            Err(error) => JsonRpcResponse {
                jsonrpc: "2.0".to_owned(),
                id: 0,
                result: None,
                error: Some(JsonRpcError {
                    code: -32700,
                    message: "Parse error".to_owned(),
                    data: Some(
                        TorbenError::new(
                            "plugin_request_invalid",
                            "The JSON-RPC request is malformed.",
                        )
                        .with_detail("reason", error.to_string()),
                    ),
                }),
            },
        };
        let mut bytes = serde_json::to_vec(&response).map_err(serialize_error)?;
        bytes.push(b'\n');
        output.write_all(&bytes).await.map_err(io_error)?;
        output.flush().await.map_err(io_error)?;
    }
    Ok(())
}

async fn handle(provider: &TemurinProvider, request: JsonRpcRequest) -> JsonRpcResponse {
    let id = request.id;
    match dispatch(provider, &request.method, request.params).await {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0".to_owned(),
            id,
            result: Some(result),
            error: None,
        },
        Err(error) => JsonRpcResponse {
            jsonrpc: "2.0".to_owned(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code: -32000,
                message: error.message.clone(),
                data: Some(error),
            }),
        },
    }
}

#[allow(clippy::too_many_lines)]
async fn dispatch(
    provider: &TemurinProvider,
    method_name: &str,
    params: Value,
) -> Result<Value, TorbenError> {
    match method_name {
        method::INITIALIZE => {
            let params: InitializeParams = parse(params)?;
            if params.protocol_version != PLUGIN_PROTOCOL_VERSION {
                return Err(TorbenError::new(
                    "plugin_protocol_incompatible",
                    "The host protocol is incompatible with the Eclipse Temurin plugin.",
                ));
            }
            let plugin_version = ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?;
            if params.host_version < plugin_version {
                return Err(TorbenError::new(
                    "plugin_host_version_incompatible",
                    "The Eclipse Temurin plugin requires a newer host.",
                ));
            }
            if params.target != current_target() {
                return Err(TorbenError::new(
                    "plugin_target_mismatch",
                    "The Eclipse Temurin plugin target does not match the host request.",
                ));
            }
            value(InitializeResult {
                protocol_version: PLUGIN_PROTOCOL_VERSION,
                plugin_id: PluginId::new(PLUGIN_ID)?,
                plugin_version,
                applications: vec![temurin_descriptor()?],
            })
        }
        method::APP_DESCRIBE => value(temurin_descriptor()?),
        method::VERSIONS_LIST => {
            let params: VersionListParams = parse(params)?;
            ensure_temurin(&params.app_id)?;
            value(VersionListResult {
                versions: provider.list_versions().await?,
            })
        }
        method::VERSION_RESOLVE => {
            let params: ResolveVersionParams = parse(params)?;
            ensure_temurin(&params.app_id)?;
            value(ResolveVersionResult {
                requested: params.requested.clone(),
                resolved: provider.resolve_version(&params.requested).await?,
            })
        }
        method::INSTALL_PLAN => {
            let params: InstallPlanParams = parse(params)?;
            ensure_temurin(&params.app_id)?;
            let source_id = SourceId::new(SOURCE_ID)?;
            if params.source_id != source_id || params.target != current_target() {
                return Err(TorbenError::new(
                    "plugin_install_request_invalid",
                    "The Eclipse Temurin install request changed source ownership or target.",
                ));
            }
            let distribution = provider.distribution(&params.version).await?;
            let expected_output = java_version_core(&params.version);
            value(InstallPlan {
                app_id: params.app_id,
                version: params.version,
                source_id,
                steps: vec![
                    InstallStep::Download {
                        url: distribution.archive_url.to_string(),
                        destination_name: distribution.archive_name.clone(),
                    },
                    InstallStep::VerifySha256 {
                        archive_name: distribution.archive_name.clone(),
                        expected: distribution.checksum,
                    },
                    InstallStep::VerifyDetachedSignature {
                        archive_name: distribution.archive_name.clone(),
                        signature_url: distribution.signature_url.to_string(),
                        public_key_url: distribution.public_key_url.to_string(),
                        trusted_fingerprint: ADOPTIUM_RELEASE_FINGERPRINT.to_owned(),
                    },
                    InstallStep::ExtractArchive {
                        archive_name: distribution.archive_name,
                        strip_components: 0,
                    },
                    InstallStep::HealthCheck {
                        executable: "java".to_owned(),
                        arguments: vec!["-version".to_owned()],
                        expected_output,
                    },
                    InstallStep::CreateShims {
                        commands: vec!["java".to_owned(), "javac".to_owned()],
                    },
                ],
                metadata: BTreeMap::from([
                    ("target".to_owned(), api_target()?),
                    ("feature".to_owned(), distribution.feature.to_string()),
                ]),
            })
        }
        method::HEALTH_CHECK => {
            let params: HealthCheckParams = parse(params)?;
            ensure_temurin(&params.app_id)?;
            let record = torben_contracts::InstallRecord {
                app_id: params.app_id,
                version: params.version.clone(),
                source_id: SourceId::new(SOURCE_ID)?,
                scope: torben_contracts::InstallScope::Managed,
                install_path: params.install_path,
                installed_at: String::new(),
                health: String::new(),
            };
            provider.health_check(&record)?;
            value(HealthCheckResult {
                healthy: true,
                actual_version: Some(params.version),
                message: "Eclipse Temurin java and javac health checks passed.".to_owned(),
            })
        }
        method::EXTERNAL_DISCOVER => {
            let params: ExternalDiscoverParams = parse(params)?;
            ensure_temurin(&params.app_id)?;
            value(ExternalDiscoverResult {
                installations: provider
                    .discover_external(std::path::Path::new(&params.managed_root))
                    .await?,
            })
        }
        method::UNINSTALL_PLAN => {
            let params: UninstallPlanParams = parse(params)?;
            ensure_temurin(&params.app_id)?;
            if params.source_id != SourceId::new(SOURCE_ID)? {
                return Err(TorbenError::new(
                    "source_owner_mismatch",
                    "The Eclipse Temurin uninstall source owner is invalid.",
                ));
            }
            value(UninstallPlan {
                app_id: params.app_id,
                version: params.version,
                source_id: params.source_id,
                install_path: params.install_path,
                preserve_user_data: true,
            })
        }
        method::SCHEMA_PAGES => {
            let params: SchemaPageListParams = parse(params)?;
            let plugin_id = PluginId::new(PLUGIN_ID)?;
            if params.plugin_id != plugin_id {
                return Err(TorbenError::new(
                    "plugin_identity_mismatch",
                    "The schema request targets a different plugin.",
                ));
            }
            value(SchemaPageListResult {
                plugin_id,
                pages: vec![temurin_schema_page()],
            })
        }
        _ => Err(TorbenError::new(
            "plugin_method_not_found",
            "The JSON-RPC method is not supported.",
        )
        .with_detail("method", method_name)),
    }
}

fn temurin_descriptor() -> Result<ApplicationDescriptor, TorbenError> {
    Ok(ApplicationDescriptor {
        id: AppId::new(APP_ID)?,
        display_name: "Eclipse Temurin".to_owned(),
        summary: "Cross-platform Eclipse Temurin HotSpot JDK LTS releases from Adoptium."
            .to_owned(),
        categories: vec!["runtime".to_owned(), "development".to_owned()],
        capabilities: vec![
            "versions".to_owned(),
            "install".to_owned(),
            "select".to_owned(),
            "uninstall".to_owned(),
            "external-detection".to_owned(),
        ],
        sources: vec![InstallSource {
            id: SourceId::new(SOURCE_ID)?,
            display_name: "Eclipse Temurin official archive".to_owned(),
            managed: true,
        }],
    })
}

fn temurin_schema_page() -> SchemaPage {
    SchemaPage {
        id: "temurin".to_owned(),
        title: "Eclipse Temurin provider".to_owned(),
        description: Some(
            "Official Adoptium LTS metadata, signed archives, and managed JDK commands.".to_owned(),
        ),
        sections: vec![SchemaSection {
            id: "trust".to_owned(),
            title: Some("Supply-chain status".to_owned()),
            description: Some(
                "Values are declared by the bundled plugin and rendered by Torben App.".to_owned(),
            ),
            fields: vec![
                status_field(
                    "source",
                    "Release source",
                    "Adoptium v3 Eclipse Temurin GA catalog",
                ),
                status_field(
                    "integrity",
                    "Integrity",
                    "Pinned OpenPGP signer + SHA-256 archive verification",
                ),
                SchemaField {
                    id: "target".to_owned(),
                    label: "Host target".to_owned(),
                    description: None,
                    kind: SchemaFieldKind::Text,
                    value: Some(current_target()),
                    placeholder: None,
                    options: Vec::new(),
                    read_only: true,
                    required: false,
                },
            ],
            actions: Vec::new(),
        }],
    }
}

fn status_field(id: &str, label: &str, value: &str) -> SchemaField {
    SchemaField {
        id: id.to_owned(),
        label: label.to_owned(),
        description: None,
        kind: SchemaFieldKind::Status,
        value: Some(value.to_owned()),
        placeholder: None,
        options: Vec::new(),
        read_only: true,
        required: false,
    }
}

fn ensure_temurin(app_id: &AppId) -> Result<(), TorbenError> {
    if app_id.as_str() == APP_ID {
        Ok(())
    } else {
        Err(TorbenError::new(
            "plugin_application_mismatch",
            "The Eclipse Temurin plugin received a request for another application.",
        ))
    }
}

fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn api_target() -> Result<String, TorbenError> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "mac",
        other => return Err(platform_error("os", other)),
    };
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        other => return Err(platform_error("architecture", other)),
    };
    Ok(format!("{os}-{architecture}"))
}

fn java_version_core(version: &ExactVersion) -> String {
    if version.as_semver().major == 8 {
        format!("1.8.0_{}", version.as_semver().patch)
    } else {
        format!(
            "{}.{}.{}",
            version.as_semver().major,
            version.as_semver().minor,
            version.as_semver().patch
        )
    }
}

fn platform_error(field: &str, value: &str) -> TorbenError {
    TorbenError::new(
        "platform_not_supported",
        "Eclipse Temurin is not supported on this platform.",
    )
    .with_detail(field, value)
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, TorbenError> {
    serde_json::from_value(value).map_err(|error| {
        TorbenError::new(
            "plugin_params_invalid",
            "The Eclipse Temurin plugin request parameters are invalid.",
        )
        .with_detail("reason", error.to_string())
    })
}

fn value<T: Serialize>(value: T) -> Result<Value, TorbenError> {
    serde_json::to_value(value).map_err(serialize_error)
}

fn serialize_error(error: serde_json::Error) -> TorbenError {
    TorbenError::internal("The Eclipse Temurin plugin could not serialize a response.")
        .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "plugin_io_failed",
        "The Eclipse Temurin plugin stdio operation failed.",
    )
    .with_detail("reason", error.to_string())
}
