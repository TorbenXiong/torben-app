use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
    str::FromStr,
};

use clap::{Args, Parser, Subcommand};
#[cfg(feature = "test-fixtures")]
use serde::Deserialize;
use serde::Serialize;
use serde_json::{Value, json};
use torben_contracts::{
    ApiEnvelope, AppId, ExactVersion, ManagedLibraryMigrationResult, OperationEvent, OperationId,
    PackageCoordinate, PackageToManagedMigrationRequest, PluginId, SourceAction,
    SourceAdapterAvailability, SourceAdapterKind, SourceExecutionRequest, SourceMigrationRequest,
    SourcePackageKind, SourcePackageVersion, TorbenError, TorbenResult,
};
#[cfg(feature = "test-fixtures")]
use torben_core::{NodeFixtureConfiguration, TorbenPaths};
use torben_core::{TorbenCore, TorbenTaskClient};

#[cfg(feature = "test-fixtures")]
const NODE_FIXTURE_CONFIG_ENV: &str = "TORBEN_TEST_FIXTURE_CONFIG";

#[cfg(feature = "test-fixtures")]
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CliNodeFixtureConfiguration {
    base_url: String,
    checksum_signature_hex: String,
    plugin_executable: PathBuf,
    shim_executable: PathBuf,
}

#[derive(Parser)]
#[command(name = "torben", version, about = "Cross-platform application manager")]
struct Cli {
    #[arg(long, global = true, help = "Print the stable JSON response envelope")]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    App(AppCommand),
    Version(VersionCommand),
    Install(SpecArgs),
    Use(SpecArgs),
    Uninstall(SpecArgs),
    Update(UpdateCommand),
    Source(SourceCommand),
    Plugin(PluginCommand),
    Task(TaskCommand),
    Shell(ShellCommand),
    Library(LibraryCommand),
    Shim(ShimCommand),
    Doctor,
}

#[derive(Args)]
struct AppCommand {
    #[command(subcommand)]
    command: AppSubcommand,
}

#[derive(Subcommand)]
enum AppSubcommand {
    List,
    Search { query: String },
    Info { app: String },
}

#[derive(Args)]
struct VersionCommand {
    #[command(subcommand)]
    command: VersionSubcommand,
}

#[derive(Subcommand)]
enum VersionSubcommand {
    List {
        #[arg(default_value = "node")]
        app: String,
    },
}

#[derive(Args)]
struct SpecArgs {
    #[arg(value_name = "APP@VERSION")]
    spec: String,
}

#[derive(Args)]
struct SourceCommand {
    #[command(subcommand)]
    command: SourceSubcommand,
}

#[derive(Args)]
struct UpdateCommand {
    #[command(subcommand)]
    command: UpdateSubcommand,
}

#[derive(Subcommand)]
enum UpdateSubcommand {
    List {
        app: Option<String>,
    },
    Apply {
        app: String,
    },
    Auto {
        app: String,
        #[arg(value_name = "ON|OFF")]
        state: String,
    },
}

#[derive(Subcommand)]
enum SourceSubcommand {
    List,
    Owned,
    Inspect {
        adapter: String,
        package: String,
        #[arg(long, default_value = "native")]
        package_kind: String,
    },
    Plan {
        action: String,
        adapter: String,
        package: String,
        #[arg(long, default_value = "native")]
        package_kind: String,
        #[arg(long)]
        package_version: Option<String>,
    },
    Execute {
        action: String,
        #[arg(value_name = "APP@VERSION")]
        spec: String,
        adapter: String,
        package: String,
        #[arg(long, default_value = "native")]
        package_kind: String,
        #[arg(long)]
        package_version: Option<String>,
        #[arg(long)]
        executable_path: Option<PathBuf>,
        #[arg(long)]
        approved_execution_identity: Option<String>,
        #[arg(long)]
        accept_system_changes: bool,
    },
    Migrate(SourceMigrationCommand),
}

#[derive(Args)]
struct SourceMigrationCommand {
    #[command(subcommand)]
    command: SourceMigrationSubcommand,
}

#[derive(Subcommand)]
enum SourceMigrationSubcommand {
    Plan(SourceMigrationArgs),
    Execute {
        #[command(flatten)]
        migration: SourceMigrationArgs,
        #[arg(long)]
        approved_plan_token: String,
        #[arg(long)]
        accept_system_changes: bool,
    },
    ToPackage(SourceMigrationToPackageCommand),
    ToManaged(SourceMigrationToManagedCommand),
}

#[derive(Args)]
struct SourceMigrationToPackageCommand {
    #[command(subcommand)]
    command: SourceMigrationToPackageSubcommand,
}

#[derive(Subcommand)]
enum SourceMigrationToPackageSubcommand {
    Plan(SourceMigrationArgs),
    Execute {
        #[command(flatten)]
        migration: SourceMigrationArgs,
        #[arg(long)]
        approved_plan_token: String,
        #[arg(long)]
        accept_system_changes: bool,
    },
}

#[derive(Args)]
struct SourceMigrationToManagedCommand {
    #[command(subcommand)]
    command: SourceMigrationToManagedSubcommand,
}

#[derive(Subcommand)]
enum SourceMigrationToManagedSubcommand {
    Plan(SpecArgs),
    Execute {
        #[arg(value_name = "APP@VERSION")]
        spec: String,
        #[arg(long)]
        approved_plan_token: String,
        #[arg(long)]
        accept_system_changes: bool,
    },
}

#[derive(Args)]
struct SourceMigrationArgs {
    #[arg(value_name = "APP@VERSION")]
    spec: String,
    target_adapter: String,
    target_package: String,
    #[arg(long, default_value = "native")]
    target_package_kind: String,
    #[arg(long)]
    target_package_version: Option<String>,
    #[arg(long)]
    target_executable_path: PathBuf,
}

#[derive(Args)]
struct PluginCommand {
    #[command(subcommand)]
    command: PluginSubcommand,
}

#[derive(Subcommand)]
enum PluginSubcommand {
    List,
    Registry(PluginRegistryCommand),
    Install {
        manifest: PathBuf,
        #[arg(long)]
        developer_mode: bool,
    },
    InstallOfficial {
        registry: PathBuf,
        plugin: String,
        #[arg(long)]
        version: Option<String>,
    },
    InstallFromRegistry {
        plugin: String,
        #[arg(long)]
        version: Option<String>,
    },
    Enable {
        plugin: String,
    },
    Disable {
        plugin: String,
    },
    Pages {
        plugin: String,
    },
    Action {
        plugin: String,
        page: String,
        section: String,
        action: String,
        #[arg(long = "value", value_parser = parse_key_value)]
        values: Vec<(String, String)>,
        #[arg(long)]
        confirm: bool,
    },
}

#[derive(Args)]
struct PluginRegistryCommand {
    #[command(subcommand)]
    command: PluginRegistrySubcommand,
}

#[derive(Subcommand)]
enum PluginRegistrySubcommand {
    Status,
    Refresh,
}

#[derive(Args)]
struct TaskCommand {
    #[command(subcommand)]
    command: TaskSubcommand,
}

#[derive(Subcommand)]
enum TaskSubcommand {
    List,
    Cancel {
        #[arg(value_name = "OPERATION_ID")]
        operation: String,
    },
}

#[derive(Args)]
struct ShellCommand {
    #[command(subcommand)]
    command: ShellSubcommand,
}

#[derive(Subcommand)]
enum ShellSubcommand {
    Status,
    Enable,
    Disable,
}

#[derive(Args)]
struct LibraryCommand {
    #[command(subcommand)]
    command: LibrarySubcommand,
}

#[derive(Subcommand)]
enum LibrarySubcommand {
    Status,
    Migrate {
        #[arg(value_name = "ABSOLUTE_PATH")]
        target: PathBuf,
    },
}

#[derive(Args)]
struct ShimCommand {
    #[command(subcommand)]
    command: ShimSubcommand,
}

#[derive(Subcommand)]
enum ShimSubcommand {
    Path,
    Install { binary: PathBuf },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = parse_cli_or_exit();
    let json_output = cli.json;
    let result = run(cli).await;
    match result {
        Ok(output) => print_success(json_output, output),
        Err(error) => {
            print_error(json_output, &error);
            std::process::exit(1);
        }
    }
}

fn parse_cli_or_exit() -> Cli {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    let json_output = arguments
        .iter()
        .skip(1)
        .any(|argument| argument == std::ffi::OsStr::new("--json"));

    match Cli::try_parse_from(arguments) {
        Ok(cli) => cli,
        Err(error)
            if json_output
                && !matches!(
                    error.kind(),
                    clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
                ) =>
        {
            let exit_code = error.exit_code();
            let error = TorbenError::new(
                "cli_argument_invalid",
                "The command-line arguments are invalid.",
            )
            .with_detail("reason", error.to_string().trim().to_owned())
            .with_remediation("Run `torben --help` or the subcommand's `--help` and retry.");
            print_error(true, &error);
            std::process::exit(exit_code);
        }
        Err(error) => error.exit(),
    }
}

#[allow(clippy::too_many_lines)]
async fn run(cli: Cli) -> TorbenResult<Output> {
    if let Command::Task(command) = &cli.command {
        return run_task(command);
    }
    let core = open_core()?;
    match cli.command {
        Command::App(command) => match command.command {
            AppSubcommand::List => {
                let applications = core.applications()?;
                Ok(Output::new(
                    applications
                        .iter()
                        .map(|application| {
                            format!("{:<12} {}", application.id, application.display_name)
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    applications,
                )?)
            }
            AppSubcommand::Search { query } => {
                let applications = core.search_applications(&query)?;
                Ok(Output::new(
                    applications
                        .iter()
                        .map(|application| {
                            format!("{:<12} {}", application.id, application.display_name)
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    applications,
                )?)
            }
            AppSubcommand::Info { app } => {
                let application = core.application(&AppId::new(app)?)?;
                Ok(Output::new(
                    format!(
                        "{} ({})\n{}\nCapabilities: {}",
                        application.display_name,
                        application.id,
                        application.summary,
                        application.capabilities.join(", ")
                    ),
                    application,
                )?)
            }
        },
        Command::Version(command) => match command.command {
            VersionSubcommand::List { app } => {
                let versions = core.versions(&AppId::new(app)?).await?;
                Ok(Output::new(
                    versions
                        .iter()
                        .map(|version| {
                            format!(
                                "{:<12} {}",
                                version.version,
                                version.lts_name.as_deref().unwrap_or("Current")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    versions,
                )?)
            }
        },
        Command::Install(args) => {
            let (app_id, requested) = parse_spec(&args.spec)?;
            let record = core.install(&app_id, &requested).await?;
            Ok(Output::new(
                format!("Installed {} {}", record.app_id, record.version),
                record,
            )?)
        }
        Command::Use(args) => {
            let (app_id, requested) = parse_spec(&args.spec)?;
            if requested == "none" {
                core.clear_selection(&app_id)?;
                Ok(Output::new(
                    format!("Cleared the selected version for {app_id}"),
                    json!({ "appId": app_id, "version": null }),
                )?)
            } else {
                let version = ExactVersion::from_str(&requested)?;
                core.select(&app_id, &version).await?;
                Ok(Output::new(
                    format!("Selected {app_id} {version}"),
                    json!({ "appId": app_id, "version": version }),
                )?)
            }
        }
        Command::Uninstall(args) => {
            let (app_id, requested) = parse_spec(&args.spec)?;
            let version = ExactVersion::from_str(&requested)?;
            core.uninstall(&app_id, &version).await?;
            Ok(Output::new(
                format!("Uninstalled {app_id} {version}"),
                json!({ "appId": app_id, "version": version }),
            )?)
        }
        Command::Update(command) => match command.command {
            UpdateSubcommand::List { app } => {
                let app_id = app.map(AppId::new).transpose()?;
                let check = core.managed_update_check(app_id.as_ref()).await?;
                Ok(Output::new(
                    if check.candidates.is_empty() {
                        format!(
                            "No managed updates found ({} applications checked, {} warnings).",
                            check.checked_apps,
                            check.warnings.len()
                        )
                    } else {
                        check
                            .candidates
                            .iter()
                            .map(|candidate| {
                                format!(
                                    "{} {} -> {} ({}){}",
                                    candidate.app_id,
                                    candidate.installed_version,
                                    candidate.available_version,
                                    candidate.channel,
                                    if candidate.automatic { " [auto]" } else { "" }
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    },
                    check,
                )?)
            }
            UpdateSubcommand::Apply { app } => {
                let app_id = AppId::new(app)?;
                let check = core.managed_update_check(Some(&app_id)).await?;
                if let Some(warning) = check.warnings.into_iter().next() {
                    let mut error = TorbenError::new(warning.code, warning.message);
                    error.details = warning.details;
                    error.remediation = warning.remediation;
                    return Err(error);
                }
                if check.candidates.is_empty() {
                    return Output::new(
                        format!("No managed updates are available for {app_id}."),
                        Vec::<torben_contracts::ManagedUpdateResult>::new(),
                    );
                }
                let mut results = Vec::new();
                for candidate in check.candidates {
                    results.push(
                        core.apply_managed_update(
                            &candidate.app_id,
                            &candidate.installed_version,
                            &candidate.available_version,
                        )
                        .await?,
                    );
                }
                Ok(Output::new(
                    results
                        .iter()
                        .map(|result| {
                            format!(
                                "Updated {} {} -> {}{}",
                                result.candidate.app_id,
                                result.candidate.installed_version,
                                result.candidate.available_version,
                                if result.selection_updated {
                                    " and updated the terminal selection"
                                } else {
                                    ""
                                }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    results,
                )?)
            }
            UpdateSubcommand::Auto { app, state } => {
                let app_id = AppId::new(app)?;
                let enabled = match state.trim().to_ascii_lowercase().as_str() {
                    "on" | "enable" | "enabled" => true,
                    "off" | "disable" | "disabled" => false,
                    _ => {
                        return Err(TorbenError::new(
                            "update_auto_state_invalid",
                            "Expected on or off for automatic managed updates.",
                        )
                        .with_detail("value", state));
                    }
                };
                core.set_managed_auto_update(&app_id, enabled)?;
                Ok(Output::new(
                    format!(
                        "Automatic managed updates for {app_id} are {}.",
                        if enabled { "enabled" } else { "disabled" }
                    ),
                    json!({ "appId": app_id, "automatic": enabled }),
                )?)
            }
        },
        Command::Source(command) => match command.command {
            SourceSubcommand::List => {
                let statuses = core.source_adapter_statuses().await?;
                Ok(Output::new(
                    statuses
                        .iter()
                        .map(|status| {
                            format!(
                                "{:<10} {:<12} {}",
                                status.adapter,
                                match status.availability {
                                    SourceAdapterAvailability::Available => "available",
                                    SourceAdapterAvailability::Missing => "missing",
                                    SourceAdapterAvailability::Unsupported => "unsupported",
                                },
                                status.version.as_deref().unwrap_or(&status.message)
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    statuses,
                )?)
            }
            SourceSubcommand::Owned => {
                let records = core.package_installations()?;
                Ok(Output::new(
                    if records.is_empty() {
                        "No Torben-owned package-manager installations.".to_owned()
                    } else {
                        records
                            .iter()
                            .map(|record| {
                                format!(
                                    "{}@{} {} {} {}",
                                    record.app_id,
                                    record.app_version,
                                    record.adapter,
                                    record.coordinate,
                                    record.package_version
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    },
                    records,
                )?)
            }
            SourceSubcommand::Inspect {
                adapter,
                package,
                package_kind,
            } => {
                let adapter = SourceAdapterKind::from_str(&adapter)?;
                let coordinate = PackageCoordinate::from_str(&package)?;
                let package_kind = SourcePackageKind::from_str(&package_kind)?;
                let state = core
                    .inspect_source_package(adapter, coordinate, package_kind)
                    .await?;
                Ok(Output::new(
                    if state.installed {
                        format!(
                            "{} {} is installed ({})",
                            state.adapter,
                            state.coordinate,
                            state
                                .installed_version
                                .as_ref()
                                .map_or("unknown", SourcePackageVersion::as_str)
                        )
                    } else {
                        format!("{} {} is not installed", state.adapter, state.coordinate)
                    },
                    state,
                )?)
            }
            SourceSubcommand::Plan {
                action,
                adapter,
                package,
                package_kind,
                package_version,
            } => {
                let plan = core
                    .plan_source_operation(
                        SourceAction::from_str(&action)?,
                        SourceAdapterKind::from_str(&adapter)?,
                        PackageCoordinate::from_str(&package)?,
                        SourcePackageKind::from_str(&package_kind)?,
                        package_version
                            .as_deref()
                            .map(SourcePackageVersion::from_str)
                            .transpose()?,
                    )
                    .await?;
                let identity = plan
                    .execution_identity
                    .as_ref()
                    .map_or_else(String::new, |value| {
                        format!("\nExecution identity: {value}")
                    });
                Ok(Output::new(
                    format!(
                        "{} {} with {}\nPreview: {} {}{}\nReview this plan before running source execute.",
                        plan.action,
                        plan.coordinate,
                        plan.adapter,
                        plan.executable,
                        plan.preview_arguments.join(" "),
                        identity
                    ),
                    plan,
                )?)
            }
            SourceSubcommand::Execute {
                action,
                spec,
                adapter,
                package,
                package_kind,
                package_version,
                executable_path,
                approved_execution_identity,
                accept_system_changes,
            } => {
                let (app_id, version) = parse_spec(&spec)?;
                let request = SourceExecutionRequest {
                    app_id,
                    app_version: ExactVersion::from_str(&version)?,
                    action: SourceAction::from_str(&action)?,
                    adapter: SourceAdapterKind::from_str(&adapter)?,
                    coordinate: PackageCoordinate::from_str(&package)?,
                    package_kind: SourcePackageKind::from_str(&package_kind)?,
                    package_version: package_version
                        .as_deref()
                        .map(SourcePackageVersion::from_str)
                        .transpose()?,
                    executable_path: executable_path.map(|path| path.display().to_string()),
                    approved_execution_identity,
                    accept_system_changes,
                };
                let result = core.execute_source_operation(request).await?;
                Ok(Output::new(
                    match result.outcome {
                        torben_contracts::SourceExecutionOutcome::OwnershipCommitted => format!(
                            "Installed {} {} with {} and committed Torben ownership",
                            result.plan.coordinate,
                            result
                                .after
                                .installed_version
                                .as_ref()
                                .map_or("unknown", SourcePackageVersion::as_str),
                            result.plan.adapter
                        ),
                        torben_contracts::SourceExecutionOutcome::OwnershipRemoved => format!(
                            "Uninstalled {} with {} and removed Torben ownership",
                            result.plan.coordinate, result.plan.adapter
                        ),
                    },
                    result,
                )?)
            }
            SourceSubcommand::Migrate(command) => match command.command {
                SourceMigrationSubcommand::Plan(arguments) => {
                    let request = source_migration_request(&arguments, None, false)?;
                    let plan = core.plan_source_migration(request).await?;
                    Ok(Output::new(
                        format!(
                            "Migrate {}@{} from {} {} to {} {}\nRemove: {} {}\nInstall: {} {}\nApproval token: {}\nReview all cleanup and restore commands in --json output before execution.",
                            plan.app_id,
                            plan.app_version,
                            plan.current_owner.adapter,
                            plan.current_owner.coordinate,
                            plan.install_target.adapter,
                            plan.install_target.coordinate,
                            plan.uninstall_current.executable,
                            plan.uninstall_current.execute_arguments.join(" "),
                            plan.install_target.executable,
                            plan.install_target.execute_arguments.join(" "),
                            plan.approval_token,
                        ),
                        plan,
                    )?)
                }
                SourceMigrationSubcommand::Execute {
                    migration,
                    approved_plan_token,
                    accept_system_changes,
                } => {
                    let request = source_migration_request(
                        &migration,
                        Some(approved_plan_token),
                        accept_system_changes,
                    )?;
                    let result = core.execute_source_migration(request).await?;
                    Ok(Output::new(
                        format!(
                            "Migrated {}@{} ownership to {} {}",
                            result.installation.app_id,
                            result.installation.app_version,
                            result.installation.adapter,
                            result.installation.coordinate,
                        ),
                        result,
                    )?)
                }
                SourceMigrationSubcommand::ToPackage(command) => match command.command {
                    SourceMigrationToPackageSubcommand::Plan(arguments) => {
                        let request = source_migration_request(&arguments, None, false)?;
                        let plan = core.plan_managed_to_package_migration(request).await?;
                        Ok(Output::new(
                            format!(
                                "Migrate managed {}@{} to {} {}\nStage: {}\nInstall: {} {}\nApproval token: {}",
                                plan.app_id,
                                plan.app_version,
                                plan.install_target.adapter,
                                plan.install_target.coordinate,
                                plan.current_installation.install_path,
                                plan.install_target.executable,
                                plan.install_target.execute_arguments.join(" "),
                                plan.approval_token,
                            ),
                            plan,
                        )?)
                    }
                    SourceMigrationToPackageSubcommand::Execute {
                        migration,
                        approved_plan_token,
                        accept_system_changes,
                    } => {
                        let request = source_migration_request(
                            &migration,
                            Some(approved_plan_token),
                            accept_system_changes,
                        )?;
                        let result = core.execute_managed_to_package_migration(request).await?;
                        Ok(Output::new(
                            format!(
                                "Migrated managed {}@{} to {} {}",
                                result.installation.app_id,
                                result.installation.app_version,
                                result.installation.adapter,
                                result.installation.coordinate,
                            ),
                            result,
                        )?)
                    }
                },
                SourceMigrationSubcommand::ToManaged(command) => match command.command {
                    SourceMigrationToManagedSubcommand::Plan(arguments) => {
                        let request = package_to_managed_request(&arguments.spec, None, false)?;
                        let plan = core.plan_package_to_managed_migration(request).await?;
                        Ok(Output::new(
                            format!(
                                "Migrate package-owned {}@{} from {} {} to managed storage\nInstall target: {}\nRemove: {} {}\nApproval token: {}",
                                plan.app_id,
                                plan.app_version,
                                plan.current_owner.adapter,
                                plan.current_owner.coordinate,
                                plan.managed_target_path,
                                plan.uninstall_current.executable,
                                plan.uninstall_current.execute_arguments.join(" "),
                                plan.approval_token,
                            ),
                            plan,
                        )?)
                    }
                    SourceMigrationToManagedSubcommand::Execute {
                        spec,
                        approved_plan_token,
                        accept_system_changes,
                    } => {
                        let request = package_to_managed_request(
                            &spec,
                            Some(approved_plan_token),
                            accept_system_changes,
                        )?;
                        let result = core.execute_package_to_managed_migration(request).await?;
                        Ok(Output::new(
                            format!(
                                "Migrated package-owned {}@{} to official managed storage",
                                result.installation.app_id, result.installation.version,
                            ),
                            result,
                        )?)
                    }
                },
            },
        },
        Command::Plugin(command) => match command.command {
            PluginSubcommand::List => {
                let plugins = core.plugins()?;
                Ok(Output::new(
                    plugins
                        .iter()
                        .map(|plugin| {
                            format!(
                                "{:<32} {:<10} {}",
                                plugin.id,
                                plugin.version,
                                if plugin.enabled {
                                    "enabled"
                                } else {
                                    "disabled"
                                }
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n"),
                    plugins,
                )?)
            }
            PluginSubcommand::Registry(command) => {
                let status = match command.command {
                    PluginRegistrySubcommand::Status => core.official_plugin_registry_status()?,
                    PluginRegistrySubcommand::Refresh => {
                        core.refresh_official_plugin_registry().await?
                    }
                };
                Ok(Output::new(
                    match status.sequence {
                        Some(sequence) => format!(
                            "Official plugin registry sequence {sequence} ({})",
                            status.generated_at.as_deref().unwrap_or("unknown time")
                        ),
                        None if status.configured => {
                            "Official plugin registry is configured but not cached".to_owned()
                        }
                        None => {
                            "Official plugin registry is not configured in this build".to_owned()
                        }
                    },
                    status,
                )?)
            }
            PluginSubcommand::Install {
                manifest,
                developer_mode,
            } => {
                let plugin = core.install_plugin(&manifest, developer_mode)?;
                Ok(Output::new(
                    format!("Installed plugin {} {}", plugin.id, plugin.version),
                    plugin,
                )?)
            }
            PluginSubcommand::InstallOfficial {
                registry,
                plugin,
                version,
            } => {
                let plugin_id = PluginId::new(plugin)?;
                let version = version.as_deref().map(ExactVersion::from_str).transpose()?;
                let installed =
                    core.install_official_plugin(&registry, &plugin_id, version.as_ref())?;
                Ok(Output::new(
                    format!(
                        "Installed official plugin {} {}",
                        installed.id, installed.version
                    ),
                    installed,
                )?)
            }
            PluginSubcommand::InstallFromRegistry { plugin, version } => {
                let plugin_id = PluginId::new(plugin)?;
                let version = version.as_deref().map(ExactVersion::from_str).transpose()?;
                let installed = core
                    .install_official_plugin_from_registry(&plugin_id, version.as_ref())
                    .await?;
                Ok(Output::new(
                    format!(
                        "Installed registry plugin {} {}",
                        installed.id, installed.version
                    ),
                    installed,
                )?)
            }
            PluginSubcommand::Enable { plugin } => {
                let plugin_id = PluginId::new(plugin)?;
                core.set_plugin_enabled(&plugin_id, true)?;
                Ok(Output::new(
                    format!("Enabled {plugin_id}"),
                    json!({ "pluginId": plugin_id, "enabled": true }),
                )?)
            }
            PluginSubcommand::Disable { plugin } => {
                let plugin_id = PluginId::new(plugin)?;
                core.set_plugin_enabled(&plugin_id, false)?;
                Ok(Output::new(
                    format!("Disabled {plugin_id}"),
                    json!({ "pluginId": plugin_id, "enabled": false }),
                )?)
            }
            PluginSubcommand::Pages { plugin } => {
                let plugin_id = PluginId::new(plugin)?;
                let pages = core.plugin_schema_pages(&plugin_id).await?;
                Ok(Output::new(
                    pages
                        .iter()
                        .map(|page| format!("{:<24} {}", page.id, page.title))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    pages,
                )?)
            }
            PluginSubcommand::Action {
                plugin,
                page,
                section,
                action,
                values,
                confirm,
            } => {
                let plugin_id = PluginId::new(plugin)?;
                let result = core
                    .invoke_plugin_schema_action(
                        &plugin_id,
                        &page,
                        &section,
                        &action,
                        collect_schema_values(values)?,
                        confirm,
                    )
                    .await?;
                Ok(Output::new(
                    result
                        .message
                        .clone()
                        .unwrap_or_else(|| format!("Completed schema action {action}")),
                    result,
                )?)
            }
        },
        Command::Task(_) => unreachable!("task commands return before opening the full Core"),
        Command::Shell(command) => {
            let status = match command.command {
                ShellSubcommand::Status => core.shell_integration_status()?,
                ShellSubcommand::Enable => core.enable_shell_integration()?,
                ShellSubcommand::Disable => core.disable_shell_integration()?,
            };
            Ok(Output::new(
                format!(
                    "Shell integration is {} ({})",
                    format!("{:?}", status.state).to_ascii_lowercase(),
                    status.shim_path
                ),
                status,
            )?)
        }
        Command::Library(command) => match command.command {
            LibrarySubcommand::Status => {
                let status = core.managed_library_status()?;
                Ok(Output::new(
                    format!("{} ({} bytes)", status.path, status.bytes_used),
                    status,
                )?)
            }
            LibrarySubcommand::Migrate { target } => {
                let result = core.migrate_managed_library(&target)?;
                Ok(Output::new(
                    managed_library_migration_message(&result),
                    result,
                )?)
            }
        },
        Command::Shim(command) => match command.command {
            ShimSubcommand::Path => {
                let path = core.paths().shim_dir();
                Ok(Output::new(
                    path.display().to_string(),
                    json!({ "path": path }),
                )?)
            }
            ShimSubcommand::Install { binary } => {
                let installed = core.install_shims(&binary)?;
                Ok(Output::new(
                    format!("Installed {} command shims", installed.len()),
                    json!({ "installed": installed }),
                )?)
            }
        },
        Command::Doctor => {
            let checks = core.doctor()?;
            Ok(Output::new(
                checks
                    .iter()
                    .map(|check| {
                        format!(
                            "{} {:<24} {}",
                            if check.healthy { "PASS" } else { "FAIL" },
                            check.id,
                            check.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                checks,
            )?)
        }
    }
}

fn run_task(command: &TaskCommand) -> TorbenResult<Output> {
    let tasks = TorbenTaskClient::open_default()?;
    match &command.command {
        TaskSubcommand::List => {
            let operations = latest_operations(tasks.operation_events()?);
            Output::new(
                operations
                    .iter()
                    .map(|event| {
                        format!(
                            "{} {:<12} {:<16} {}",
                            event.operation_id,
                            format!("{:?}", event.state).to_ascii_lowercase(),
                            event.phase,
                            event.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                operations,
            )
        }
        TaskSubcommand::Cancel { operation } => {
            let operation_id = OperationId::from_str(operation)?;
            tasks.cancel_operation(operation_id)?;
            Output::new(
                format!("Cancellation requested for {operation_id}"),
                json!({ "operationId": operation_id, "requested": true }),
            )
        }
    }
}

#[cfg(not(feature = "test-fixtures"))]
fn open_core() -> TorbenResult<TorbenCore> {
    TorbenCore::open_default()
}

#[cfg(feature = "test-fixtures")]
fn open_core() -> TorbenResult<TorbenCore> {
    let Some(config_path) = std::env::var_os(NODE_FIXTURE_CONFIG_ENV) else {
        return TorbenCore::open_default();
    };
    let config_path = PathBuf::from(config_path);
    if !config_path.is_absolute() || !config_path.is_file() {
        return Err(invalid_fixture_config(
            "configPath",
            "The fixture config must be an existing absolute file path.",
        )
        .with_detail("path", config_path.display().to_string()));
    }
    let bytes = std::fs::read(&config_path).map_err(|error| {
        invalid_fixture_config("configPath", error.to_string())
            .with_detail("path", config_path.display().to_string())
    })?;
    let config: CliNodeFixtureConfiguration = serde_json::from_slice(&bytes).map_err(|error| {
        invalid_fixture_config(
            "configPath",
            format!("The fixture JSON is invalid: {error}"),
        )
        .with_detail("path", config_path.display().to_string())
    })?;
    TorbenCore::open_node_fixture(
        TorbenPaths::discover()?,
        NodeFixtureConfiguration {
            base_url: config.base_url,
            checksum_signature: decode_fixture_hex(&config.checksum_signature_hex)?,
            plugin_executable: config.plugin_executable,
            shim_executable: config.shim_executable,
        },
    )
}

#[cfg(feature = "test-fixtures")]
fn decode_fixture_hex(value: &str) -> TorbenResult<Vec<u8>> {
    if value.is_empty() || !value.len().is_multiple_of(2) {
        return Err(invalid_fixture_config(
            "checksumSignatureHex",
            "The fixture signature must contain a non-empty even number of hexadecimal digits.",
        ));
    }
    let (pairs, remainder) = value.as_bytes().as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    pairs
        .iter()
        .map(|[high, low]| {
            let high = decode_hex_digit(*high);
            let low = decode_hex_digit(*low);
            high.zip(low)
                .map(|(high, low)| high << 4 | low)
                .ok_or_else(|| {
                    invalid_fixture_config(
                        "checksumSignatureHex",
                        "The fixture signature contains a non-hexadecimal character.",
                    )
                })
        })
        .collect()
}

#[cfg(feature = "test-fixtures")]
const fn decode_hex_digit(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(feature = "test-fixtures")]
fn invalid_fixture_config(field: &str, reason: impl Into<String>) -> TorbenError {
    TorbenError::new(
        "test_fixture_configuration_invalid",
        "The Node.js CLI test fixture configuration is invalid.",
    )
    .with_detail("field", field)
    .with_detail("reason", reason.into())
}

fn latest_operations(events: Vec<OperationEvent>) -> Vec<OperationEvent> {
    let mut latest = HashMap::<OperationId, OperationEvent>::new();
    for event in events {
        let replace = latest
            .get(&event.operation_id)
            .is_none_or(|current| event.sequence > current.sequence);
        if replace {
            latest.insert(event.operation_id, event);
        }
    }
    let mut operations = latest.into_values().collect::<Vec<_>>();
    operations.sort_by(|left, right| {
        right.timestamp.cmp(&left.timestamp).then_with(|| {
            right
                .operation_id
                .to_string()
                .cmp(&left.operation_id.to_string())
        })
    });
    operations
}

struct Output {
    human: String,
    json: Value,
}

impl Output {
    fn new<T: Serialize>(human: String, value: T) -> TorbenResult<Self> {
        Ok(Self {
            human,
            json: serde_json::to_value(value).map_err(|error| {
                TorbenError::internal("Could not serialize CLI output.")
                    .with_detail("reason", error.to_string())
            })?,
        })
    }
}

fn managed_library_migration_message(result: &ManagedLibraryMigrationResult) -> String {
    let mut message = format!(
        "Migrated the managed application library to {}",
        result.current_path
    );
    if result.source_cleanup_pending {
        message.push_str(
            ". The old library could not be removed; Torben App will retry cleanup on its next startup",
        );
    }
    message
}

fn parse_spec(spec: &str) -> TorbenResult<(AppId, String)> {
    let (app, version) = spec.rsplit_once('@').ok_or_else(|| {
        TorbenError::new("invalid_app_spec", "Expected APP@VERSION.").with_detail("value", spec)
    })?;
    if version.is_empty() {
        return Err(TorbenError::new(
            "invalid_app_spec",
            "The version cannot be empty.",
        ));
    }
    Ok((AppId::new(app)?, version.to_owned()))
}

fn source_migration_request(
    arguments: &SourceMigrationArgs,
    approved_plan_token: Option<String>,
    accept_system_changes: bool,
) -> TorbenResult<SourceMigrationRequest> {
    let (app_id, version) = parse_spec(&arguments.spec)?;
    Ok(SourceMigrationRequest {
        app_id,
        app_version: ExactVersion::from_str(&version)?,
        target_adapter: SourceAdapterKind::from_str(&arguments.target_adapter)?,
        target_coordinate: PackageCoordinate::from_str(&arguments.target_package)?,
        target_package_kind: SourcePackageKind::from_str(&arguments.target_package_kind)?,
        target_package_version: arguments
            .target_package_version
            .as_deref()
            .map(SourcePackageVersion::from_str)
            .transpose()?,
        target_executable_path: arguments.target_executable_path.display().to_string(),
        approved_plan_token,
        accept_system_changes,
    })
}

fn package_to_managed_request(
    spec: &str,
    approved_plan_token: Option<String>,
    accept_system_changes: bool,
) -> TorbenResult<PackageToManagedMigrationRequest> {
    let (app_id, version) = parse_spec(spec)?;
    Ok(PackageToManagedMigrationRequest {
        app_id,
        app_version: ExactVersion::from_str(&version)?,
        approved_plan_token,
        accept_system_changes,
    })
}

fn parse_key_value(value: &str) -> Result<(String, String), String> {
    let (key, value) = value
        .split_once('=')
        .ok_or_else(|| "schema values must use FIELD=VALUE".to_owned())?;
    if key.is_empty() {
        return Err("schema value field cannot be empty".to_owned());
    }
    Ok((key.to_owned(), value.to_owned()))
}

fn collect_schema_values(values: Vec<(String, String)>) -> TorbenResult<BTreeMap<String, String>> {
    let mut collected = BTreeMap::new();
    for (key, value) in values {
        if collected.insert(key.clone(), value).is_some() {
            return Err(TorbenError::new(
                "plugin_schema_value_duplicate",
                "A schema field value was supplied more than once.",
            )
            .with_detail("fieldId", key));
        }
    }
    Ok(collected)
}

fn print_success(json_output: bool, output: Output) {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&ApiEnvelope::success(output.json))
                .expect("serializing a JSON value cannot fail")
        );
    } else {
        println!("{}", output.human);
    }
}

fn print_error(json_output: bool, error: &TorbenError) {
    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&ApiEnvelope::<Value>::failure(error.clone()))
                .expect("serializing an error cannot fail")
        );
    } else {
        eprintln!("error[{}]: {}", error.code, error.message);
        for (key, value) in &error.details {
            eprintln!("  {key}: {value}");
        }
        if let Some(remediation) = &error.remediation {
            eprintln!("  help: {remediation}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, str::FromStr};

    use clap::Parser;
    use torben_contracts::{
        ManagedLibraryMigrationResult, OperationEvent, OperationId, OperationState,
    };

    use super::{
        Cli, Command, LibrarySubcommand, PluginRegistrySubcommand, PluginSubcommand,
        ShellSubcommand, SourceMigrationSubcommand, SourceMigrationToManagedSubcommand,
        SourceMigrationToPackageSubcommand, SourceSubcommand, TaskSubcommand, UpdateSubcommand,
        collect_schema_values, latest_operations, managed_library_migration_message,
        parse_key_value, parse_spec,
    };

    #[test]
    fn parses_app_version_spec() {
        let (app, version) = parse_spec("node@24.19.0").unwrap();
        assert_eq!(app.as_str(), "node");
        assert_eq!(version, "24.19.0");
        assert!(parse_spec("node").is_err());
    }

    #[test]
    fn parses_task_cancellation_and_keeps_the_latest_event() {
        let operation_id = OperationId::from_str("11111111-1111-4111-8111-111111111111").unwrap();
        let cli =
            Cli::try_parse_from(["torben", "task", "cancel", &operation_id.to_string()]).unwrap();
        let Command::Task(command) = cli.command else {
            panic!("expected task command");
        };
        let TaskSubcommand::Cancel { operation } = command.command else {
            panic!("expected cancel command");
        };
        assert_eq!(operation, operation_id.to_string());

        let latest = latest_operations(vec![
            OperationEvent {
                operation_id,
                sequence: 3,
                state: OperationState::Running,
                phase: "download".to_owned(),
                message: "Downloading".to_owned(),
                progress: Some(0.3),
                timestamp: "2".to_owned(),
            },
            OperationEvent {
                operation_id,
                sequence: 0,
                state: OperationState::Running,
                phase: "prepare".to_owned(),
                message: "Started".to_owned(),
                progress: Some(0.0),
                timestamp: "1".to_owned(),
            },
        ]);
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].sequence, 3);
    }

    #[test]
    fn parses_explicit_shell_integration_actions() {
        let cli = Cli::try_parse_from(["torben", "shell", "enable", "--json"]).unwrap();
        assert!(cli.json);
        let Command::Shell(command) = cli.command else {
            panic!("expected shell command");
        };
        assert!(matches!(command.command, ShellSubcommand::Enable));
    }

    #[test]
    fn parses_managed_library_migration_target() {
        let cli = Cli::try_parse_from(["torben", "library", "migrate", "C:\\Torben Apps"]).unwrap();
        let Command::Library(command) = cli.command else {
            panic!("expected library command");
        };
        let LibrarySubcommand::Migrate { target } = command.command else {
            panic!("expected library migration");
        };
        assert_eq!(target, PathBuf::from("C:\\Torben Apps"));
    }

    #[test]
    fn reports_pending_old_library_cleanup_in_human_output() {
        let result = ManagedLibraryMigrationResult {
            previous_path: "C:\\Old Torben Apps".to_owned(),
            current_path: "D:\\Torben Apps".to_owned(),
            bytes_copied: 42,
            source_cleanup_pending: true,
        };

        assert_eq!(
            managed_library_migration_message(&result),
            "Migrated the managed application library to D:\\Torben Apps. The old library could not be removed; Torben App will retry cleanup on its next startup"
        );
    }

    #[test]
    fn parses_official_registry_refresh() {
        let cli =
            Cli::try_parse_from(["torben", "plugin", "registry", "refresh", "--json"]).unwrap();
        assert!(cli.json);
        let Command::Plugin(command) = cli.command else {
            panic!("expected plugin command");
        };
        let PluginSubcommand::Registry(command) = command.command else {
            panic!("expected registry command");
        };
        assert!(matches!(command.command, PluginRegistrySubcommand::Refresh));

        let cli = Cli::try_parse_from([
            "torben",
            "plugin",
            "install-from-registry",
            "app.example.plugin",
            "--version",
            "1.2.3",
        ])
        .unwrap();
        let Command::Plugin(command) = cli.command else {
            panic!("expected plugin command");
        };
        let PluginSubcommand::InstallFromRegistry { plugin, version } = command.command else {
            panic!("expected registry install command");
        };
        assert_eq!(plugin, "app.example.plugin");
        assert_eq!(version.as_deref(), Some("1.2.3"));

        let cli = Cli::try_parse_from([
            "torben",
            "plugin",
            "action",
            "app.example.plugin",
            "settings",
            "general",
            "save",
            "--value",
            "channel=lts",
            "--confirm",
        ])
        .unwrap();
        let Command::Plugin(command) = cli.command else {
            panic!("expected plugin command");
        };
        let PluginSubcommand::Action {
            values, confirm, ..
        } = command.command
        else {
            panic!("expected schema action command");
        };
        assert_eq!(values, [("channel".to_owned(), "lts".to_owned())]);
        assert!(confirm);
        assert!(parse_key_value("missing-separator").is_err());
        assert!(
            collect_schema_values(vec![
                ("channel".to_owned(), "lts".to_owned()),
                ("channel".to_owned(), "current".to_owned()),
            ])
            .is_err()
        );
    }

    #[test]
    fn parses_source_inspection_plan_and_explicit_execution_commands() {
        let cli = Cli::try_parse_from([
            "torben",
            "source",
            "inspect",
            "homebrew",
            "visual-studio-code",
            "--package-kind",
            "cask",
        ])
        .unwrap();
        let Command::Source(command) = cli.command else {
            panic!("expected source command");
        };
        assert!(matches!(command.command, SourceSubcommand::Inspect { .. }));

        let cli = Cli::try_parse_from([
            "torben",
            "source",
            "plan",
            "install",
            "apt",
            "nodejs",
            "--package-version",
            "1:20.11.1+dfsg-2~deb12u1",
        ])
        .unwrap();
        let Command::Source(command) = cli.command else {
            panic!("expected source command");
        };
        let SourceSubcommand::Plan {
            package_version, ..
        } = command.command
        else {
            panic!("expected source plan");
        };
        assert_eq!(package_version.as_deref(), Some("1:20.11.1+dfsg-2~deb12u1"));

        let cli = Cli::try_parse_from([
            "torben",
            "source",
            "execute",
            "install",
            "vscode@1.134.0",
            "winget",
            "Microsoft.VisualStudioCode",
            "--package-version",
            "1.134.0",
            "--executable-path",
            r"C:\Users\fixture\AppData\Local\Microsoft\WinGet\Links\code.exe",
            "--approved-execution-identity",
            "Microsoft.VisualStudioCode-1.134.0-x64",
            "--accept-system-changes",
            "--json",
        ])
        .unwrap();
        assert!(cli.json);
        let Command::Source(command) = cli.command else {
            panic!("expected source command");
        };
        let SourceSubcommand::Execute {
            action,
            spec,
            approved_execution_identity,
            accept_system_changes,
            ..
        } = command.command
        else {
            panic!("expected source execute");
        };
        assert_eq!(action, "install");
        assert_eq!(spec, "vscode@1.134.0");
        assert_eq!(
            approved_execution_identity.as_deref(),
            Some("Microsoft.VisualStudioCode-1.134.0-x64")
        );
        assert!(accept_system_changes);
    }

    #[test]
    fn parses_reviewed_source_migration_execution() {
        let cli = Cli::try_parse_from([
            "torben",
            "source",
            "migrate",
            "execute",
            "vscode@1.134.0",
            "dnf",
            "code",
            "--target-package-version",
            "1.134.0-1.fc42",
            "--target-executable-path",
            "/usr/bin/code",
            "--approved-plan-token",
            "reviewed-token",
            "--accept-system-changes",
        ])
        .unwrap();
        let Command::Source(command) = cli.command else {
            panic!("expected source command");
        };
        let SourceSubcommand::Migrate(command) = command.command else {
            panic!("expected source migration command");
        };
        let SourceMigrationSubcommand::Execute {
            migration,
            approved_plan_token,
            accept_system_changes,
        } = command.command
        else {
            panic!("expected source migration execution");
        };
        assert_eq!(migration.spec, "vscode@1.134.0");
        assert_eq!(approved_plan_token, "reviewed-token");
        assert!(accept_system_changes);
    }

    #[test]
    fn parses_managed_to_package_migration_execution() {
        let cli = Cli::try_parse_from([
            "torben",
            "source",
            "migrate",
            "to-package",
            "execute",
            "vscode@1.134.0",
            "dnf",
            "code",
            "--target-package-version",
            "1.134.0-1.fc42",
            "--target-executable-path",
            "/usr/bin/code",
            "--approved-plan-token",
            "reviewed-token",
            "--accept-system-changes",
        ])
        .unwrap();
        let Command::Source(command) = cli.command else {
            panic!("expected source command");
        };
        let SourceSubcommand::Migrate(command) = command.command else {
            panic!("expected source migration command");
        };
        let SourceMigrationSubcommand::ToPackage(command) = command.command else {
            panic!("expected managed-to-package command");
        };
        let SourceMigrationToPackageSubcommand::Execute {
            migration,
            approved_plan_token,
            accept_system_changes,
        } = command.command
        else {
            panic!("expected managed-to-package execution");
        };
        assert_eq!(migration.spec, "vscode@1.134.0");
        assert_eq!(approved_plan_token, "reviewed-token");
        assert!(accept_system_changes);
    }

    #[test]
    fn parses_package_to_managed_migration_execution() {
        let cli = Cli::try_parse_from([
            "torben",
            "source",
            "migrate",
            "to-managed",
            "execute",
            "vscode@1.134.0",
            "--approved-plan-token",
            "reviewed-token",
            "--accept-system-changes",
        ])
        .unwrap();
        let Command::Source(command) = cli.command else {
            panic!("expected source command");
        };
        let SourceSubcommand::Migrate(command) = command.command else {
            panic!("expected source migration command");
        };
        let SourceMigrationSubcommand::ToManaged(command) = command.command else {
            panic!("expected package-to-managed command");
        };
        let SourceMigrationToManagedSubcommand::Execute {
            spec,
            approved_plan_token,
            accept_system_changes,
        } = command.command
        else {
            panic!("expected package-to-managed execution");
        };
        assert_eq!(spec, "vscode@1.134.0");
        assert_eq!(approved_plan_token, "reviewed-token");
        assert!(accept_system_changes);
    }

    #[test]
    fn parses_managed_update_queries_application_and_auto_policy() {
        let cli = Cli::try_parse_from(["torben", "update", "list", "node", "--json"]).unwrap();
        let Command::Update(command) = cli.command else {
            panic!("expected update command");
        };
        let UpdateSubcommand::List { app } = command.command else {
            panic!("expected update list command");
        };
        assert_eq!(app.as_deref(), Some("node"));

        let cli = Cli::try_parse_from(["torben", "update", "apply", "python"]).unwrap();
        let Command::Update(command) = cli.command else {
            panic!("expected update command");
        };
        assert!(matches!(command.command, UpdateSubcommand::Apply { .. }));

        let cli = Cli::try_parse_from(["torben", "update", "auto", "vscode", "on"]).unwrap();
        let Command::Update(command) = cli.command else {
            panic!("expected update command");
        };
        let UpdateSubcommand::Auto { app, state } = command.command else {
            panic!("expected update auto command");
        };
        assert_eq!(app, "vscode");
        assert_eq!(state, "on");
    }
}
