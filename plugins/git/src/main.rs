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
use torben_core::{GitInstallKind, GitProvider};

const PLUGIN_ID: &str = "app.torben.plugin.git";
const APP_ID: &str = "git";
const SOURCE_ID: &str = "git.official";
const GIT_RELEASE_FINGERPRINT: &str = "96E07AF25771955980DAD10020D04E5A713660A7";

#[tokio::main]
async fn main() {
    if let Err(error) = serve().await {
        eprintln!("{}: {}", error.code, error.message);
        std::process::exit(1);
    }
}

async fn serve() -> Result<(), TorbenError> {
    let provider = GitProvider::official()?;
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

async fn handle(provider: &GitProvider, request: JsonRpcRequest) -> JsonRpcResponse {
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
    provider: &GitProvider,
    method_name: &str,
    params: Value,
) -> Result<Value, TorbenError> {
    match method_name {
        method::INITIALIZE => {
            let params: InitializeParams = parse(params)?;
            if params.protocol_version != PLUGIN_PROTOCOL_VERSION {
                return Err(TorbenError::new(
                    "plugin_protocol_incompatible",
                    "The host protocol is incompatible with the Git plugin.",
                ));
            }
            let plugin_version = ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?;
            if params.host_version < plugin_version || params.target != current_target() {
                return Err(TorbenError::new(
                    "plugin_host_incompatible",
                    "The Git plugin host version or target is incompatible.",
                ));
            }
            value(InitializeResult {
                protocol_version: PLUGIN_PROTOCOL_VERSION,
                plugin_id: PluginId::new(PLUGIN_ID)?,
                plugin_version,
                applications: vec![git_descriptor()?],
            })
        }
        method::APP_DESCRIBE => value(git_descriptor()?),
        method::VERSIONS_LIST => {
            let params: VersionListParams = parse(params)?;
            ensure_git(&params.app_id)?;
            value(VersionListResult {
                versions: provider.list_versions().await?,
            })
        }
        method::VERSION_RESOLVE => {
            let params: ResolveVersionParams = parse(params)?;
            ensure_git(&params.app_id)?;
            value(ResolveVersionResult {
                requested: params.requested.clone(),
                resolved: provider.resolve_version(&params.requested).await?,
            })
        }
        method::INSTALL_PLAN => {
            let params: InstallPlanParams = parse(params)?;
            ensure_git(&params.app_id)?;
            if params.source_id != SourceId::new(SOURCE_ID)? || params.target != current_target() {
                return Err(TorbenError::new(
                    "plugin_install_request_invalid",
                    "The Git install request changed source ownership or target.",
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
                    expected: distribution.checksum,
                },
            ];
            let install_method = match distribution.kind {
                GitInstallKind::WindowsMinGit => {
                    steps.push(InstallStep::ExtractArchive {
                        archive_name: distribution.archive_name,
                        strip_components: 0,
                    });
                    "mingit_archive"
                }
                GitInstallKind::SourceArchive => {
                    let signature_url = distribution.signature_url.ok_or_else(|| {
                        TorbenError::new(
                            "git_release_signature_missing",
                            "The official Git source signature URL is missing.",
                        )
                    })?;
                    steps.extend([
                        InstallStep::VerifyGitReleaseSignature {
                            archive_name: distribution.archive_name.clone(),
                            signature_url: signature_url.to_string(),
                            trusted_fingerprint: GIT_RELEASE_FINGERPRINT.to_owned(),
                        },
                        InstallStep::ExtractArchive {
                            archive_name: distribution.archive_name.clone(),
                            strip_components: 0,
                        },
                    ]);
                    steps.push(InstallStep::BuildGitSource {
                        archive_name: distribution.archive_name,
                        make_arguments: vec![
                            "NO_GETTEXT=YesPlease".to_owned(),
                            "NO_TCLTK=YesPlease".to_owned(),
                        ],
                    });
                    "source_build"
                }
            };
            steps.extend([
                InstallStep::HealthCheck {
                    executable: "git".to_owned(),
                    arguments: vec!["--version".to_owned()],
                    expected_output: params.version.to_string(),
                },
                InstallStep::CreateShims {
                    commands: vec!["git".to_owned()],
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
            ensure_git(&params.app_id)?;
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
                message: "Git command health check passed.".to_owned(),
            })
        }
        method::EXTERNAL_DISCOVER => {
            let params: ExternalDiscoverParams = parse(params)?;
            ensure_git(&params.app_id)?;
            value(ExternalDiscoverResult {
                installations: provider
                    .discover_external(std::path::Path::new(&params.managed_root))
                    .await?,
            })
        }
        method::UNINSTALL_PLAN => {
            let params: UninstallPlanParams = parse(params)?;
            ensure_git(&params.app_id)?;
            if params.source_id != SourceId::new(SOURCE_ID)? {
                return Err(TorbenError::new(
                    "source_owner_mismatch",
                    "The Git uninstall source owner is invalid.",
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
                pages: vec![git_schema_page()],
            })
        }
        _ => Err(TorbenError::new(
            "plugin_method_not_found",
            "The JSON-RPC method is not supported.",
        )
        .with_detail("method", method_name)),
    }
}

fn git_descriptor() -> Result<ApplicationDescriptor, TorbenError> {
    Ok(ApplicationDescriptor {
        id: AppId::new(APP_ID)?,
        display_name: "Git".to_owned(),
        summary: "Official Git command-line releases with managed terminal selection.".to_owned(),
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
            display_name: "Official Git distribution".to_owned(),
            managed: true,
        }],
    })
}

fn git_schema_page() -> SchemaPage {
    let (source, install, integrity) = if cfg!(windows) {
        (
            "git-for-windows/git stable release",
            "Official MinGit ZIP",
            "GitHub release asset SHA-256",
        )
    } else {
        (
            "kernel.org stable Git release index",
            "Verified source build with managed prefix",
            "Pinned OpenPGP signer + SHA-256",
        )
    };
    SchemaPage {
        id: "git".to_owned(),
        title: "Git provider".to_owned(),
        description: Some(
            "Official Git CLI metadata, transactional installation, and terminal selection."
                .to_owned(),
        ),
        sections: vec![SchemaSection {
            id: "trust".to_owned(),
            title: Some("Supply-chain status".to_owned()),
            description: Some(
                "Values are declared by the bundled plugin and rendered by Torben App.".to_owned(),
            ),
            fields: vec![
                status_field("source", "Release source", source),
                status_field("install", "Install method", install),
                status_field("integrity", "Integrity", integrity),
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

fn ensure_git(app_id: &AppId) -> Result<(), TorbenError> {
    if app_id.as_str() == APP_ID {
        Ok(())
    } else {
        Err(TorbenError::new(
            "plugin_application_mismatch",
            "The Git plugin received a request for another application.",
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
            "The Git plugin request parameters are invalid.",
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
        "The Git plugin could not serialize its response.",
    )
    .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new("plugin_io_failed", "The Git plugin stdio channel failed.")
        .with_detail("reason", error.to_string())
}
