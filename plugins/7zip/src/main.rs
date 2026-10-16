#![allow(clippy::needless_pass_by_value)]

use std::str::FromStr;

use serde::Serialize;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use torben_contracts::{
    AppId, ApplicationDescriptor, ExactVersion, InstallRecord, InstallScope, InstallSource,
    PluginId, SourceId, TorbenError,
    plugin::{
        ExternalDiscoverParams, ExternalDiscoverResult, HealthCheckParams, HealthCheckResult,
        InitializeParams, InitializeResult, InstallPlanParams, JsonRpcError, JsonRpcRequest,
        JsonRpcResponse, PLUGIN_PROTOCOL_VERSION, ResolveVersionParams, ResolveVersionResult,
        SchemaField, SchemaFieldKind, SchemaPage, SchemaPageListParams, SchemaPageListResult,
        SchemaSection, UninstallPlan, UninstallPlanParams, VersionListParams, VersionListResult,
        method,
    },
};
use torben_core::SevenZipProvider;

const PLUGIN_ID: &str = "app.torben.plugin.7zip";
const APP_ID: &str = "7zip";
const SOURCE_ID: &str = "7zip.official";

#[tokio::main]
async fn main() {
    if let Err(error) = serve().await {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}

async fn serve() -> Result<(), TorbenError> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut output = tokio::io::stdout();
    while let Some(line) = lines.next_line().await.map_err(io_error)? {
        let response = match serde_json::from_str::<JsonRpcRequest>(&line) {
            Ok(request) => handle(request),
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

fn handle(request: JsonRpcRequest) -> JsonRpcResponse {
    let id = request.id;
    match dispatch(&request.method, request.params) {
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
fn dispatch(name: &str, params: Value) -> Result<Value, TorbenError> {
    match name {
        method::INITIALIZE => {
            let params: InitializeParams = parse(params)?;
            if params.protocol_version != PLUGIN_PROTOCOL_VERSION
                || params.target != current_target()
            {
                return Err(TorbenError::new(
                    "plugin_host_incompatible",
                    "The 7-Zip plugin target is incompatible.",
                ));
            }
            value(InitializeResult {
                protocol_version: PLUGIN_PROTOCOL_VERSION,
                plugin_id: PluginId::new(PLUGIN_ID)?,
                plugin_version: ExactVersion::from_str(env!("CARGO_PKG_VERSION"))
                    .map_err(version_error)?,
                applications: vec![descriptor()?],
            })
        }
        method::APP_DESCRIBE => value(descriptor()?),
        method::VERSIONS_LIST => {
            let params: VersionListParams = parse(params)?;
            ensure_app(&params.app_id)?;
            let provider = SevenZipProvider::official()?;
            value(VersionListResult {
                versions: provider.list_versions()?,
            })
        }
        method::VERSION_RESOLVE => {
            let params: ResolveVersionParams = parse(params)?;
            ensure_app(&params.app_id)?;
            let provider = SevenZipProvider::official()?;
            value(ResolveVersionResult {
                requested: params.requested.clone(),
                resolved: provider.resolve_version(&params.requested)?,
            })
        }
        method::INSTALL_PLAN => {
            let params: InstallPlanParams = parse(params)?;
            ensure_app(&params.app_id)?;
            let provider = SevenZipProvider::official()?;
            value(provider.install_plan(
                &params.app_id,
                &params.version,
                &params.source_id,
                &params.target,
            )?)
        }
        method::HEALTH_CHECK => {
            let params: HealthCheckParams = parse(params)?;
            ensure_app(&params.app_id)?;
            let provider = SevenZipProvider::official()?;
            provider.health_check(&InstallRecord {
                app_id: params.app_id.clone(),
                version: params.version.clone(),
                source_id: SourceId::new(SOURCE_ID)?,
                scope: InstallScope::Managed,
                install_path: params.install_path,
                installed_at: String::new(),
                health: String::new(),
            })?;
            value(HealthCheckResult {
                healthy: true,
                actual_version: Some(params.version),
                message: "7z health check passed.".to_owned(),
            })
        }
        method::EXTERNAL_DISCOVER => {
            let params: ExternalDiscoverParams = parse(params)?;
            ensure_app(&params.app_id)?;
            let provider = SevenZipProvider::official()?;
            value(ExternalDiscoverResult {
                installations: provider
                    .discover_external(std::path::Path::new(&params.managed_root))?,
            })
        }
        method::UNINSTALL_PLAN => {
            let params: UninstallPlanParams = parse(params)?;
            ensure_app(&params.app_id)?;
            if params.source_id != SourceId::new(SOURCE_ID)? {
                return Err(TorbenError::new(
                    "source_owner_mismatch",
                    "The 7-Zip uninstall source owner is invalid.",
                ));
            }
            value(UninstallPlan {
                app_id: params.app_id,
                version: params.version,
                source_id: params.source_id,
                install_path: params.install_path,
                preserve_user_data: false,
            })
        }
        method::SCHEMA_PAGES => {
            let params: SchemaPageListParams = parse(params)?;
            if params.plugin_id != PluginId::new(PLUGIN_ID)? {
                return Err(TorbenError::new(
                    "plugin_identity_mismatch",
                    "The schema request targets another plugin.",
                ));
            }
            value(SchemaPageListResult {
                plugin_id: PluginId::new(PLUGIN_ID)?,
                pages: vec![SchemaPage {
                    id: APP_ID.to_owned(),
                    title: "7-Zip".to_owned(),
                    description: Some("Windows x64 7z command-line archiver.".to_owned()),
                    sections: vec![SchemaSection {
                        id: "trust".to_owned(),
                        title: Some("Supply-chain status".to_owned()),
                        description: None,
                        fields: vec![SchemaField {
                            id: "sha256".to_owned(),
                            label: "SHA-256".to_owned(),
                            description: None,
                            kind: SchemaFieldKind::Text,
                            value: Some(
                                SevenZipProvider::official()?
                                    .distribution(&version()?)?
                                    .checksum,
                            ),
                            placeholder: None,
                            options: Vec::new(),
                            read_only: true,
                            required: false,
                        }],
                        actions: Vec::new(),
                    }],
                }],
            })
        }
        _ => Err(TorbenError::new(
            "plugin_method_not_found",
            "The 7-Zip plugin method is not supported.",
        )),
    }
}

fn descriptor() -> Result<ApplicationDescriptor, TorbenError> {
    Ok(ApplicationDescriptor {
        id: AppId::new(APP_ID)?,
        display_name: "7-Zip".to_owned(),
        summary: "The official 7z command-line archiver for Windows x64.".to_owned(),
        categories: vec!["tool".to_owned(), "development".to_owned()],
        capabilities: vec![
            "versions".to_owned(),
            "install".to_owned(),
            "select".to_owned(),
            "uninstall".to_owned(),
            "external-detection".to_owned(),
        ],
        sources: vec![InstallSource {
            id: SourceId::new(SOURCE_ID)?,
            display_name: "Official 7-Zip MSI".to_owned(),
            managed: true,
        }],
    })
}
fn version() -> Result<ExactVersion, TorbenError> {
    SevenZipProvider::official()?.resolve_version("latest")
}
fn ensure_app(app_id: &AppId) -> Result<(), TorbenError> {
    if app_id.as_str() == APP_ID {
        Ok(())
    } else {
        Err(TorbenError::new(
            "plugin_application_mismatch",
            "The 7-Zip plugin received another application.",
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
            "The 7-Zip plugin parameters are invalid.",
        )
        .with_detail("reason", error.to_string())
    })
}
fn value<T: Serialize>(value: T) -> Result<Value, TorbenError> {
    serde_json::to_value(value).map_err(serialize_error)
}
fn version_error(error: TorbenError) -> TorbenError {
    TorbenError::new("version_invalid", "The 7-Zip version is invalid.")
        .with_detail("reason", error.to_string())
}
fn serialize_error(error: serde_json::Error) -> TorbenError {
    TorbenError::internal("The 7-Zip plugin could not serialize a response.")
        .with_detail("reason", error.to_string())
}
fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "plugin_io_failed",
        "The 7-Zip plugin stdio operation failed.",
    )
    .with_detail("reason", error.to_string())
}
