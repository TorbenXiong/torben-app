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
use torben_core::NodeProvider;

#[tokio::main]
async fn main() {
    if let Err(error) = serve().await {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}

async fn serve() -> Result<(), TorbenError> {
    let provider = NodeProvider::official()?;
    let input = tokio::io::stdin();
    let mut lines = BufReader::new(input).lines();
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
        let mut bytes = serde_json::to_vec(&response).map_err(json_error)?;
        bytes.push(b'\n');
        output.write_all(&bytes).await.map_err(io_error)?;
        output.flush().await.map_err(io_error)?;
    }
    Ok(())
}

async fn handle(provider: &NodeProvider, request: JsonRpcRequest) -> JsonRpcResponse {
    let id = request.id;
    let result = dispatch(provider, &request.method, request.params).await;
    match result {
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
    provider: &NodeProvider,
    method_name: &str,
    params: Value,
) -> Result<Value, TorbenError> {
    match method_name {
        method::INITIALIZE => {
            let params: InitializeParams = parse(params)?;
            if params.protocol_version != PLUGIN_PROTOCOL_VERSION {
                return Err(TorbenError::new(
                    "plugin_protocol_incompatible",
                    "The host protocol is incompatible with the Node.js plugin.",
                ));
            }
            let minimum_host_version = ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?;
            if params.host_version < minimum_host_version {
                return Err(TorbenError::new(
                    "plugin_host_version_incompatible",
                    "The Node.js plugin requires a newer Torben App host.",
                )
                .with_detail("minimumHostVersion", minimum_host_version.to_string())
                .with_detail("hostVersion", params.host_version.to_string()));
            }
            let expected_target = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
            if params.target != expected_target {
                return Err(TorbenError::new(
                    "plugin_target_mismatch",
                    "The Node.js plugin target does not match the host request.",
                )
                .with_detail("pluginTarget", expected_target)
                .with_detail("hostTarget", params.target));
            }
            value(InitializeResult {
                protocol_version: PLUGIN_PROTOCOL_VERSION,
                plugin_id: PluginId::new("app.torben.plugin.node")?,
                plugin_version: ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?,
                applications: vec![node_descriptor()?],
            })
        }
        method::APP_DESCRIBE => value(node_descriptor()?),
        method::VERSIONS_LIST => {
            let params: VersionListParams = parse(params)?;
            ensure_node(&params.app_id)?;
            value(VersionListResult {
                versions: provider.list_versions().await?,
            })
        }
        method::VERSION_RESOLVE => {
            let params: ResolveVersionParams = parse(params)?;
            ensure_node(&params.app_id)?;
            value(ResolveVersionResult {
                requested: params.requested.clone(),
                resolved: provider.resolve_version(&params.requested).await?,
            })
        }
        method::INSTALL_PLAN => {
            let params: InstallPlanParams = parse(params)?;
            ensure_node(&params.app_id)?;
            let distribution = provider.distribution(&params.version)?;
            value(InstallPlan {
                app_id: params.app_id,
                version: params.version.clone(),
                source_id: params.source_id,
                steps: vec![
                    InstallStep::Download {
                        url: distribution.archive_url.to_string(),
                        destination_name: distribution.archive_name.clone(),
                    },
                    InstallStep::VerifySha256Manifest {
                        manifest_url: distribution.checksums_url.to_string(),
                        signature_url: Some(distribution.signature_url.to_string()),
                        archive_name: distribution.archive_name.clone(),
                    },
                    InstallStep::ExtractArchive {
                        archive_name: distribution.archive_name,
                        strip_components: 0,
                    },
                    InstallStep::HealthCheck {
                        executable: "node".to_owned(),
                        arguments: vec!["--version".to_owned()],
                        expected_output: format!("v{}", params.version),
                    },
                    InstallStep::CreateShims {
                        commands: vec!["node".to_owned(), "npm".to_owned(), "npx".to_owned()],
                    },
                ],
                metadata: BTreeMap::from([("target".to_owned(), params.target)]),
            })
        }
        method::HEALTH_CHECK => {
            let params: HealthCheckParams = parse(params)?;
            ensure_node(&params.app_id)?;
            let record = torben_contracts::InstallRecord {
                app_id: params.app_id,
                version: params.version.clone(),
                source_id: SourceId::new("node.official")?,
                scope: torben_contracts::InstallScope::Managed,
                install_path: params.install_path,
                installed_at: String::new(),
                health: String::new(),
            };
            provider.health_check(&record)?;
            value(HealthCheckResult {
                healthy: true,
                actual_version: Some(params.version),
                message: "Node.js, npm, and npx health checks passed.".to_owned(),
            })
        }
        method::EXTERNAL_DISCOVER => {
            let params: ExternalDiscoverParams = parse(params)?;
            ensure_node(&params.app_id)?;
            value(ExternalDiscoverResult {
                installations: provider
                    .discover_external(std::path::Path::new(&params.managed_root))
                    .await?,
            })
        }
        method::UNINSTALL_PLAN => {
            let params: UninstallPlanParams = parse(params)?;
            ensure_node(&params.app_id)?;
            if params.source_id != SourceId::new("node.official")? {
                return Err(TorbenError::new(
                    "source_owner_mismatch",
                    "The Node.js uninstall source owner is invalid.",
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
            let plugin_id = PluginId::new("app.torben.plugin.node")?;
            if params.plugin_id != plugin_id {
                return Err(TorbenError::new(
                    "plugin_identity_mismatch",
                    "The schema request targets a different plugin.",
                ));
            }
            value(SchemaPageListResult {
                plugin_id,
                pages: vec![node_schema_page()],
            })
        }
        _ => Err(TorbenError::new(
            "plugin_method_not_found",
            "The JSON-RPC method is not supported.",
        )
        .with_detail("method", method_name)),
    }
}

fn node_schema_page() -> SchemaPage {
    SchemaPage {
        id: "node".to_owned(),
        title: "Node.js provider".to_owned(),
        description: Some(
            "Official Node.js metadata, signed checksums, managed versions, and terminal commands."
                .to_owned(),
        ),
        sections: vec![SchemaSection {
            id: "trust".to_owned(),
            title: Some("Supply-chain status".to_owned()),
            description: Some(
                "These values are declared by the bundled plugin and rendered by Torben App."
                    .to_owned(),
            ),
            fields: vec![
                SchemaField {
                    id: "source".to_owned(),
                    label: "Release source".to_owned(),
                    description: None,
                    kind: SchemaFieldKind::Status,
                    value: Some("Official nodejs.org release metadata".to_owned()),
                    placeholder: None,
                    options: Vec::new(),
                    read_only: true,
                    required: false,
                },
                SchemaField {
                    id: "integrity".to_owned(),
                    label: "Integrity".to_owned(),
                    description: None,
                    kind: SchemaFieldKind::Status,
                    value: Some("OpenPGP manifest + SHA-256 archive verification".to_owned()),
                    placeholder: None,
                    options: Vec::new(),
                    read_only: true,
                    required: false,
                },
                SchemaField {
                    id: "target".to_owned(),
                    label: "Host target".to_owned(),
                    description: None,
                    kind: SchemaFieldKind::Text,
                    value: Some(format!(
                        "{}-{}",
                        std::env::consts::OS,
                        std::env::consts::ARCH
                    )),
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

fn node_descriptor() -> Result<ApplicationDescriptor, TorbenError> {
    Ok(ApplicationDescriptor {
        id: AppId::new("node")?,
        display_name: "Node.js".to_owned(),
        summary: "JavaScript runtime with managed LTS and Current releases.".to_owned(),
        categories: vec!["runtime".to_owned(), "development".to_owned()],
        capabilities: vec![
            "versions".to_owned(),
            "install".to_owned(),
            "select".to_owned(),
            "uninstall".to_owned(),
            "external-detection".to_owned(),
        ],
        sources: vec![InstallSource {
            id: SourceId::new("node.official")?,
            display_name: "Official archive".to_owned(),
            managed: true,
        }],
    })
}

fn ensure_node(app_id: &AppId) -> Result<(), TorbenError> {
    if app_id.as_str() == "node" {
        Ok(())
    } else {
        Err(TorbenError::new(
            "app_not_supported",
            "The Node.js plugin only supports the 'node' application.",
        ))
    }
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, TorbenError> {
    serde_json::from_value(params).map_err(json_error)
}

fn value<T: Serialize>(value: T) -> Result<Value, TorbenError> {
    serde_json::to_value(value).map_err(json_error)
}

fn json_error(error: serde_json::Error) -> TorbenError {
    TorbenError::new(
        "plugin_json_error",
        "The Node.js plugin could not process JSON.",
    )
    .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "plugin_io_error",
        "The Node.js plugin stdio channel failed.",
    )
    .with_detail("reason", error.to_string())
}
