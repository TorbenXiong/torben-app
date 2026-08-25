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
use torben_core::{PythonInstallKind, PythonProvider};

const PLUGIN_ID: &str = "app.torben.plugin.python";
const APP_ID: &str = "python";
const SOURCE_ID: &str = "python.official";

#[tokio::main]
async fn main() {
    if let Err(error) = serve().await {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}

async fn serve() -> Result<(), TorbenError> {
    let provider = PythonProvider::official()?;
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

async fn handle(provider: &PythonProvider, request: JsonRpcRequest) -> JsonRpcResponse {
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
    provider: &PythonProvider,
    method_name: &str,
    params: Value,
) -> Result<Value, TorbenError> {
    match method_name {
        method::INITIALIZE => {
            let params: InitializeParams = parse(params)?;
            if params.protocol_version != PLUGIN_PROTOCOL_VERSION {
                return Err(TorbenError::new(
                    "plugin_protocol_incompatible",
                    "The host protocol is incompatible with the Python plugin.",
                ));
            }
            let plugin_version = ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?;
            if params.host_version < plugin_version || params.target != current_target() {
                return Err(TorbenError::new(
                    "plugin_host_incompatible",
                    "The Python plugin host version or target is incompatible.",
                ));
            }
            value(InitializeResult {
                protocol_version: PLUGIN_PROTOCOL_VERSION,
                plugin_id: PluginId::new(PLUGIN_ID)?,
                plugin_version,
                applications: vec![python_descriptor()?],
            })
        }
        method::APP_DESCRIBE => value(python_descriptor()?),
        method::VERSIONS_LIST => {
            let params: VersionListParams = parse(params)?;
            ensure_python(&params.app_id)?;
            value(VersionListResult {
                versions: provider.list_versions().await?,
            })
        }
        method::VERSION_RESOLVE => {
            let params: ResolveVersionParams = parse(params)?;
            ensure_python(&params.app_id)?;
            value(ResolveVersionResult {
                requested: params.requested.clone(),
                resolved: provider.resolve_version(&params.requested).await?,
            })
        }
        method::INSTALL_PLAN => {
            let params: InstallPlanParams = parse(params)?;
            ensure_python(&params.app_id)?;
            if params.source_id != SourceId::new(SOURCE_ID)? || params.target != current_target() {
                return Err(TorbenError::new(
                    "plugin_install_request_invalid",
                    "The Python install request changed source ownership or target.",
                ));
            }
            let distribution = provider.distribution(&params.version).await?;
            let mut steps = Vec::new();
            let install_method = match distribution.kind {
                PythonInstallKind::WindowsManager { tag } => {
                    steps.push(InstallStep::InstallWithPythonManager { tag });
                    "python_manager"
                }
                PythonInstallKind::SourceArchive(source) => {
                    steps.extend([
                        InstallStep::Download {
                            url: source.archive_url.to_string(),
                            destination_name: source.archive_name.clone(),
                        },
                        InstallStep::VerifySha256 {
                            archive_name: source.archive_name.clone(),
                            expected: source.sha256,
                        },
                        InstallStep::VerifySigstoreBundle {
                            archive_name: source.archive_name.clone(),
                            bundle_url: source.sigstore_bundle_url.to_string(),
                            certificate_identity: source.sigstore_identity,
                            oidc_issuer: source.sigstore_oidc_issuer,
                        },
                        InstallStep::ExtractArchive {
                            archive_name: source.archive_name.clone(),
                            strip_components: 0,
                        },
                        InstallStep::BuildPythonSource {
                            archive_name: source.archive_name,
                            configure_arguments: vec![
                                "--with-ensurepip=install".to_owned(),
                                "--disable-test-modules".to_owned(),
                            ],
                        },
                    ]);
                    "source_build"
                }
            };
            steps.extend([
                InstallStep::HealthCheck {
                    executable: "python".to_owned(),
                    arguments: vec!["--version".to_owned()],
                    expected_output: params.version.to_string(),
                },
                InstallStep::CreateShims {
                    commands: vec![
                        "python".to_owned(),
                        "python3".to_owned(),
                        "pip".to_owned(),
                        "pip3".to_owned(),
                    ],
                },
            ]);
            value(InstallPlan {
                app_id: params.app_id,
                version: params.version,
                source_id: params.source_id,
                steps,
                metadata: BTreeMap::from([
                    ("target".to_owned(), current_target()),
                    ("installMethod".to_owned(), install_method.to_owned()),
                ]),
            })
        }
        method::HEALTH_CHECK => {
            let params: HealthCheckParams = parse(params)?;
            ensure_python(&params.app_id)?;
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
                message: "CPython and pip health checks passed.".to_owned(),
            })
        }
        method::EXTERNAL_DISCOVER => {
            let params: ExternalDiscoverParams = parse(params)?;
            ensure_python(&params.app_id)?;
            value(ExternalDiscoverResult {
                installations: provider
                    .discover_external(std::path::Path::new(&params.managed_root))
                    .await?,
            })
        }
        method::UNINSTALL_PLAN => {
            let params: UninstallPlanParams = parse(params)?;
            ensure_python(&params.app_id)?;
            if params.source_id != SourceId::new(SOURCE_ID)? {
                return Err(TorbenError::new(
                    "source_owner_mismatch",
                    "The Python uninstall source owner is invalid.",
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
                pages: vec![python_schema_page()],
            })
        }
        _ => Err(TorbenError::new(
            "plugin_method_not_found",
            "The JSON-RPC method is not supported.",
        )
        .with_detail("method", method_name)),
    }
}

fn python_descriptor() -> Result<ApplicationDescriptor, TorbenError> {
    Ok(ApplicationDescriptor {
        id: AppId::new(APP_ID)?,
        display_name: "Python".to_owned(),
        summary: "Official CPython runtimes with managed versions and pip.".to_owned(),
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
            display_name: "Official Python distribution".to_owned(),
            managed: true,
        }],
    })
}

fn python_schema_page() -> SchemaPage {
    let install_method = if cfg!(windows) {
        "Official Python Install Manager target extraction"
    } else {
        "Verified CPython source build with managed prefix"
    };
    SchemaPage {
        id: "python".to_owned(),
        title: "Python provider".to_owned(),
        description: Some(
            "Official stable CPython metadata, managed runtimes, pip, and terminal commands."
                .to_owned(),
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
                    "python.org official release catalog",
                ),
                status_field("install", "Install method", install_method),
                status_field(
                    "integrity",
                    "Integrity",
                    if cfg!(windows) {
                        "Python Install Manager signed catalog and target extraction"
                    } else {
                        "Release-manager Sigstore identity + SHA-256"
                    },
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

fn ensure_python(app_id: &AppId) -> Result<(), TorbenError> {
    if app_id.as_str() == APP_ID {
        Ok(())
    } else {
        Err(TorbenError::new(
            "plugin_application_mismatch",
            "The Python plugin received a request for another application.",
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
            "The Python plugin request parameters are invalid.",
        )
        .with_detail("reason", error.to_string())
    })
}

fn value<T: Serialize>(value: T) -> Result<Value, TorbenError> {
    serde_json::to_value(value).map_err(serialize_error)
}

fn serialize_error(error: serde_json::Error) -> TorbenError {
    TorbenError::internal("The Python plugin could not serialize a response.")
        .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "plugin_io_failed",
        "The Python plugin stdio operation failed.",
    )
    .with_detail("reason", error.to_string())
}
