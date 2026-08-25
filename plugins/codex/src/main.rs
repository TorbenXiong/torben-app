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
use torben_core::CodexProvider;

const PLUGIN_ID: &str = "app.torben.plugin.codex";
const APP_ID: &str = "codex";
const SOURCE_ID: &str = "codex.official";

#[tokio::main]
async fn main() {
    if let Err(error) = serve().await {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}

async fn serve() -> Result<(), TorbenError> {
    let provider = CodexProvider::official()?;
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

async fn handle(provider: &CodexProvider, request: JsonRpcRequest) -> JsonRpcResponse {
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
    provider: &CodexProvider,
    method_name: &str,
    params: Value,
) -> Result<Value, TorbenError> {
    match method_name {
        method::INITIALIZE => {
            let params: InitializeParams = parse(params)?;
            if params.protocol_version != PLUGIN_PROTOCOL_VERSION {
                return Err(TorbenError::new(
                    "plugin_protocol_incompatible",
                    "The host protocol is incompatible with the Codex plugin.",
                ));
            }
            let plugin_version = ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?;
            if params.host_version < plugin_version || params.target != current_target() {
                return Err(TorbenError::new(
                    "plugin_host_incompatible",
                    "The Codex plugin host version or target is incompatible.",
                ));
            }
            value(InitializeResult {
                protocol_version: PLUGIN_PROTOCOL_VERSION,
                plugin_id: PluginId::new(PLUGIN_ID)?,
                plugin_version,
                applications: vec![codex_descriptor()?],
            })
        }
        method::APP_DESCRIBE => value(codex_descriptor()?),
        method::VERSIONS_LIST => {
            let params: VersionListParams = parse(params)?;
            ensure_codex(&params.app_id)?;
            value(VersionListResult {
                versions: provider.list_versions().await?,
            })
        }
        method::VERSION_RESOLVE => {
            let params: ResolveVersionParams = parse(params)?;
            ensure_codex(&params.app_id)?;
            value(ResolveVersionResult {
                requested: params.requested.clone(),
                resolved: provider.resolve_version(&params.requested).await?,
            })
        }
        method::INSTALL_PLAN => {
            let params: InstallPlanParams = parse(params)?;
            ensure_codex(&params.app_id)?;
            if params.source_id != SourceId::new(SOURCE_ID)? || params.target != current_target() {
                return Err(TorbenError::new(
                    "plugin_install_request_invalid",
                    "The Codex install request changed source ownership or target.",
                ));
            }
            let distribution = provider.distribution(&params.version).await?;
            let mut steps = vec![
                InstallStep::Download {
                    url: distribution.archive_url.to_string(),
                    destination_name: distribution.archive_name.clone(),
                },
                InstallStep::VerifySha256 {
                    archive_name: distribution.archive_name.clone(),
                    expected: distribution.archive_checksum,
                },
            ];
            if let Some(sigstore) = distribution.sigstore {
                steps.push(InstallStep::VerifySigstoreBundle {
                    archive_name: distribution.binary_name.clone(),
                    bundle_url: sigstore.url.to_string(),
                    certificate_identity: sigstore.identity,
                    oidc_issuer: sigstore.issuer,
                });
            }
            steps.extend([
                InstallStep::ExtractArchive {
                    archive_name: distribution.archive_name,
                    strip_components: 0,
                },
                InstallStep::HealthCheck {
                    executable: "codex".to_owned(),
                    arguments: vec!["--version".to_owned()],
                    expected_output: params.version.to_string(),
                },
                InstallStep::CreateShims {
                    commands: vec!["codex".to_owned()],
                },
            ]);
            value(InstallPlan {
                app_id: params.app_id,
                version: params.version,
                source_id: params.source_id,
                steps,
                metadata: BTreeMap::from([
                    ("target".to_owned(), distribution.target),
                    ("releaseTag".to_owned(), distribution.tag),
                ]),
            })
        }
        method::HEALTH_CHECK => {
            let params: HealthCheckParams = parse(params)?;
            ensure_codex(&params.app_id)?;
            let record = torben_contracts::InstallRecord {
                app_id: params.app_id,
                version: params.version.clone(),
                source_id: SourceId::new(SOURCE_ID)?,
                scope: torben_contracts::InstallScope::Managed,
                install_path: params.install_path,
                installed_at: String::new(),
                health: String::new(),
            };
            provider.health_check(&record).await?;
            value(HealthCheckResult {
                healthy: true,
                actual_version: Some(params.version),
                message: "Codex version health check passed with isolated state.".to_owned(),
            })
        }
        method::EXTERNAL_DISCOVER => {
            let params: ExternalDiscoverParams = parse(params)?;
            ensure_codex(&params.app_id)?;
            value(ExternalDiscoverResult {
                installations: provider
                    .discover_external(std::path::Path::new(&params.managed_root))
                    .await?,
            })
        }
        method::UNINSTALL_PLAN => {
            let params: UninstallPlanParams = parse(params)?;
            ensure_codex(&params.app_id)?;
            if params.source_id != SourceId::new(SOURCE_ID)? {
                return Err(TorbenError::new(
                    "source_owner_mismatch",
                    "The Codex uninstall source owner is invalid.",
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
                pages: vec![codex_schema_page()],
            })
        }
        _ => Err(TorbenError::new(
            "plugin_method_not_found",
            "The JSON-RPC method is not supported.",
        )
        .with_detail("method", method_name)),
    }
}

fn codex_descriptor() -> Result<ApplicationDescriptor, TorbenError> {
    Ok(ApplicationDescriptor {
        id: AppId::new(APP_ID)?,
        display_name: "Codex CLI".to_owned(),
        summary: "OpenAI's official coding agent command-line client.".to_owned(),
        categories: vec!["ai".to_owned(), "development".to_owned()],
        capabilities: vec![
            "versions".to_owned(),
            "install".to_owned(),
            "select".to_owned(),
            "uninstall".to_owned(),
            "external-detection".to_owned(),
        ],
        sources: vec![InstallSource {
            id: SourceId::new(SOURCE_ID)?,
            display_name: "OpenAI Codex native release".to_owned(),
            managed: true,
        }],
    })
}

fn codex_schema_page() -> SchemaPage {
    SchemaPage {
        id: "codex".to_owned(),
        title: "Codex CLI provider".to_owned(),
        description: Some(
            "Official native Codex releases, exact versions, and the managed codex command."
                .to_owned(),
        ),
        sections: vec![SchemaSection {
            id: "trust".to_owned(),
            title: Some("Supply-chain and identity boundary".to_owned()),
            description: Some(
                "Torben manages executable versions only and never opens Codex authentication or configuration data."
                    .to_owned(),
            ),
            fields: vec![
                status_field("source", "Release source", "openai/codex stable native release"),
                status_field(
                    "integrity",
                    "Integrity",
                    if cfg!(target_os = "linux") {
                        "GitHub SHA-256 + Fulcio/Rekor Sigstore"
                    } else {
                        "GitHub release asset SHA-256"
                    },
                ),
                status_field(
                    "identity",
                    "Authentication",
                    "External: CODEX_HOME, auth.json, keyring, and login state",
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

fn status_field(id: &str, label: &str, field_value: &str) -> SchemaField {
    SchemaField {
        id: id.to_owned(),
        label: label.to_owned(),
        description: None,
        kind: SchemaFieldKind::Status,
        value: Some(field_value.to_owned()),
        placeholder: None,
        options: Vec::new(),
        read_only: true,
        required: false,
    }
}

fn ensure_codex(app_id: &AppId) -> Result<(), TorbenError> {
    if app_id.as_str() == APP_ID {
        Ok(())
    } else {
        Err(TorbenError::new(
            "plugin_application_mismatch",
            "The Codex plugin received a request for another application.",
        ))
    }
}

fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, TorbenError> {
    serde_json::from_value(value).map_err(|error| {
        TorbenError::new(
            "plugin_params_invalid",
            "The Codex plugin request parameters are invalid.",
        )
        .with_detail("reason", error.to_string())
    })
}

fn value<T: Serialize>(value: T) -> Result<Value, TorbenError> {
    serde_json::to_value(value).map_err(serialize_error)
}

fn serialize_error(error: serde_json::Error) -> TorbenError {
    TorbenError::new(
        "plugin_response_invalid",
        "The Codex plugin could not serialize its response.",
    )
    .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new("plugin_io_failed", "The Codex plugin stdio channel failed.")
        .with_detail("reason", error.to_string())
}
