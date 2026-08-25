use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    time::{Duration, SystemTime},
};

use futures_util::future::join_all;
use serde_json::Value;
use torben_contracts::{
    PackageCoordinate, SourceAction, SourceAdapterAvailability, SourceAdapterKind,
    SourceAdapterStatus, SourceId, SourceOperationPlan, SourcePackageKind, SourcePackageState,
    SourcePackageVersion, TorbenError, TorbenResult,
};

const COMMAND_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_EXPORT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct AdapterCommands {
    pub(crate) primary: PathBuf,
    pub(crate) query: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub(crate) struct CommandOutput {
    pub(crate) success: bool,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
}

pub(crate) type CommandFuture =
    Pin<Box<dyn Future<Output = TorbenResult<CommandOutput>> + Send + 'static>>;

pub(crate) trait SourceCommandRunner: Send + Sync {
    fn run(
        &self,
        executable: PathBuf,
        arguments: Vec<String>,
        environment: BTreeMap<String, String>,
    ) -> CommandFuture;
}

struct SystemCommandRunner;

impl SourceCommandRunner for SystemCommandRunner {
    fn run(
        &self,
        executable: PathBuf,
        arguments: Vec<String>,
        environment: BTreeMap<String, String>,
    ) -> CommandFuture {
        Box::pin(run_system_command(executable, arguments, environment))
    }
}

pub(crate) struct SourceAdapterService {
    commands: BTreeMap<SourceAdapterKind, AdapterCommands>,
    runner: Arc<dyn SourceCommandRunner>,
    allow_unsupported_platform: bool,
}

impl SourceAdapterService {
    pub(crate) fn discover() -> Self {
        let mut commands = BTreeMap::new();
        if cfg!(windows)
            && let Some(primary) = find_command(&["winget.exe"])
        {
            commands.insert(
                SourceAdapterKind::Winget,
                AdapterCommands {
                    primary,
                    query: None,
                },
            );
        }
        if matches!(std::env::consts::OS, "macos" | "linux")
            && let Some(primary) = find_command(&["brew"])
        {
            commands.insert(
                SourceAdapterKind::Homebrew,
                AdapterCommands {
                    primary,
                    query: None,
                },
            );
        }
        if cfg!(target_os = "linux")
            && let (Some(primary), Some(query)) =
                (find_command(&["apt-get"]), find_command(&["dpkg-query"]))
        {
            commands.insert(
                SourceAdapterKind::Apt,
                AdapterCommands {
                    primary,
                    query: Some(query),
                },
            );
        }
        if cfg!(target_os = "linux")
            && let (Some(primary), Some(query)) =
                (find_command(&["dnf5", "dnf"]), find_command(&["rpm"]))
        {
            commands.insert(
                SourceAdapterKind::Dnf,
                AdapterCommands {
                    primary,
                    query: Some(query),
                },
            );
        }
        Self {
            commands,
            runner: Arc::new(SystemCommandRunner),
            allow_unsupported_platform: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test(
        adapter: SourceAdapterKind,
        primary: PathBuf,
        query: Option<PathBuf>,
        runner: Arc<dyn SourceCommandRunner>,
    ) -> Self {
        Self::for_tests(
            BTreeMap::from([(adapter, AdapterCommands { primary, query })]),
            runner,
        )
    }

    #[cfg(test)]
    pub(crate) fn for_tests(
        commands: BTreeMap<SourceAdapterKind, AdapterCommands>,
        runner: Arc<dyn SourceCommandRunner>,
    ) -> Self {
        Self {
            commands,
            runner,
            allow_unsupported_platform: true,
        }
    }

    pub(crate) fn discovered_statuses(&self) -> TorbenResult<Vec<SourceAdapterStatus>> {
        SourceAdapterKind::ALL
            .into_iter()
            .map(|adapter| self.base_status(adapter))
            .collect()
    }

    pub(crate) async fn statuses(&self) -> TorbenResult<Vec<SourceAdapterStatus>> {
        let mut statuses = self.discovered_statuses()?;
        let probes = statuses.iter().enumerate().filter_map(|(index, status)| {
            self.commands.get(&status.adapter).map(|commands| {
                let executable = commands.primary.clone();
                async move {
                    (
                        index,
                        run_command_owned(
                            self.runner.as_ref(),
                            executable,
                            vec!["--version".to_owned()],
                            BTreeMap::new(),
                        )
                        .await,
                    )
                }
            })
        });
        for (index, result) in join_all(probes).await {
            let status = &mut statuses[index];
            match result {
                Ok(output) if output.success => {
                    status.version = first_nonempty_line(&output.stdout);
                    status.message = status.version.as_ref().map_or_else(
                        || "Package manager is available.".to_owned(),
                        |version| format!("Package manager is available ({version})."),
                    );
                }
                Ok(output) => {
                    status.availability = SourceAdapterAvailability::Missing;
                    status.message = bounded_text(&output.stderr);
                }
                Err(error) => {
                    status.availability = SourceAdapterAvailability::Missing;
                    status.message = format!("{}: {}", error.code, error.message);
                }
            }
        }
        Ok(statuses)
    }

    pub(crate) async fn inspect(
        &self,
        adapter: SourceAdapterKind,
        coordinate: PackageCoordinate,
        package_kind: SourcePackageKind,
    ) -> TorbenResult<SourcePackageState> {
        validate_package_kind(adapter, package_kind)?;
        let commands = self.available_commands(adapter)?;
        match adapter {
            SourceAdapterKind::Winget => {
                inspect_winget(
                    self.runner.as_ref(),
                    &commands.primary,
                    coordinate,
                    package_kind,
                )
                .await
            }
            SourceAdapterKind::Homebrew => {
                inspect_homebrew(
                    self.runner.as_ref(),
                    &commands.primary,
                    coordinate,
                    package_kind,
                )
                .await
            }
            SourceAdapterKind::Apt => {
                inspect_dpkg(
                    self.runner.as_ref(),
                    commands.query.as_ref().unwrap(),
                    coordinate,
                    package_kind,
                )
                .await
            }
            SourceAdapterKind::Dnf => {
                inspect_rpm(
                    self.runner.as_ref(),
                    commands.query.as_ref().unwrap(),
                    coordinate,
                    package_kind,
                )
                .await
            }
        }
    }

    pub(crate) async fn execute(&self, plan: &SourceOperationPlan) -> TorbenResult<()> {
        let commands = self.available_commands(plan.adapter)?;
        if commands.primary.display().to_string() != plan.executable {
            return Err(TorbenError::new(
                "source_plan_stale",
                "The package-manager executable changed after the plan was created.",
            )
            .with_detail("plannedExecutable", &plan.executable)
            .with_detail("currentExecutable", commands.primary.display().to_string())
            .with_remediation("Generate and review a new source operation plan."));
        }
        let output = run_command_owned(
            self.runner.as_ref(),
            commands.primary.clone(),
            plan.execute_arguments.clone(),
            plan.environment.clone(),
        )
        .await?;
        if output.success {
            Ok(())
        } else {
            Err(TorbenError::new(
                "source_execution_failed",
                "The package manager did not complete the requested mutation.",
            )
            .with_detail("adapter", plan.adapter.to_string())
            .with_detail("stdout", bounded_text(&output.stdout))
            .with_detail("stderr", bounded_text(&output.stderr))
            .with_remediation(
                "Review the package manager output; Torben will re-inspect external state before changing ownership.",
            ))
        }
    }

    pub(crate) async fn health_check(
        &self,
        app_id: &str,
        app_version: &str,
        executable: &Path,
    ) -> TorbenResult<PathBuf> {
        let canonical = validate_health_executable(app_id, executable)?;
        let arguments = if app_id == "temurin" {
            vec!["-version".to_owned()]
        } else {
            vec!["--version".to_owned()]
        };
        let output = run_command_owned(
            self.runner.as_ref(),
            canonical.clone(),
            arguments,
            BTreeMap::new(),
        )
        .await?;
        if !output.success {
            return Err(TorbenError::new(
                "source_health_check_failed",
                "The package-manager application health check failed.",
            )
            .with_detail("path", canonical.display().to_string())
            .with_detail("stdout", bounded_text(&output.stdout))
            .with_detail("stderr", bounded_text(&output.stderr)));
        }
        let expected = app_version.split(['+', '-']).next().unwrap_or(app_version);
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if !combined.contains(expected) {
            return Err(TorbenError::new(
                "source_health_check_version_mismatch",
                "The package-manager application reported a different version.",
            )
            .with_detail("path", canonical.display().to_string())
            .with_detail("expectedVersion", expected)
            .with_detail(
                "output",
                combined.trim().chars().take(512).collect::<String>(),
            ));
        }
        Ok(canonical)
    }

    pub(crate) fn plan(
        &self,
        action: SourceAction,
        adapter: SourceAdapterKind,
        coordinate: PackageCoordinate,
        package_kind: SourcePackageKind,
        package_version: Option<SourcePackageVersion>,
    ) -> TorbenResult<SourceOperationPlan> {
        validate_package_kind(adapter, package_kind)?;
        let commands = self.available_commands(adapter)?;
        match adapter {
            SourceAdapterKind::Winget => {
                plan_winget(action, commands, coordinate, package_kind, package_version)
            }
            SourceAdapterKind::Homebrew => {
                plan_homebrew(action, commands, coordinate, package_kind, package_version)
            }
            SourceAdapterKind::Apt => {
                plan_apt(action, commands, coordinate, package_kind, package_version)
            }
            SourceAdapterKind::Dnf => {
                plan_dnf(action, commands, coordinate, package_kind, package_version)
            }
        }
    }

    pub(crate) async fn reviewed_plan(
        &self,
        action: SourceAction,
        adapter: SourceAdapterKind,
        coordinate: PackageCoordinate,
        package_kind: SourcePackageKind,
        package_version: Option<SourcePackageVersion>,
    ) -> TorbenResult<SourceOperationPlan> {
        let execution_identity = if adapter == SourceAdapterKind::Dnf {
            let commands = self.available_commands(adapter)?;
            Some(match action {
                SourceAction::Install => {
                    let version = package_version.as_ref().ok_or_else(|| {
                        TorbenError::new(
                            "source_package_version_required",
                            "An exact raw package version is required for this install plan.",
                        )
                    })?;
                    resolve_dnf_available_nevra(
                        self.runner.as_ref(),
                        &commands.primary,
                        &coordinate,
                        version,
                    )
                    .await?
                }
                SourceAction::Uninstall => {
                    resolve_dnf_installed_nevra(
                        self.runner.as_ref(),
                        commands.query.as_ref().expect("DNF requires rpm"),
                        &coordinate,
                        package_version.as_ref(),
                    )
                    .await?
                }
            })
        } else {
            None
        };
        let mut plan = self.plan(action, adapter, coordinate, package_kind, package_version)?;
        if let Some(identity) = execution_identity {
            let preview = plan
                .preview_arguments
                .last_mut()
                .expect("DNF plan has a spec");
            preview.clone_from(&identity);
            let execute = plan
                .execute_arguments
                .last_mut()
                .expect("DNF plan has a spec");
            execute.clone_from(&identity);
            plan.execution_identity = Some(identity);
            plan.warnings.push(
                "The full repository NEVRA is locked; execution fails if a newly reviewed plan does not match."
                    .to_owned(),
            );
        }
        Ok(plan)
    }

    fn base_status(&self, adapter: SourceAdapterKind) -> TorbenResult<SourceAdapterStatus> {
        let platform_supported = self.allow_unsupported_platform || platform_supported(adapter);
        let command = self.commands.get(&adapter);
        let availability = if !platform_supported {
            SourceAdapterAvailability::Unsupported
        } else if command.is_some() {
            SourceAdapterAvailability::Available
        } else {
            SourceAdapterAvailability::Missing
        };
        Ok(SourceAdapterStatus {
            adapter,
            source_id: source_id(adapter)?,
            availability,
            executable: command.map(|commands| commands.primary.display().to_string()),
            version: None,
            supports_exact_version: adapter != SourceAdapterKind::Homebrew,
            requires_elevation: matches!(adapter, SourceAdapterKind::Apt | SourceAdapterKind::Dnf),
            message: if availability == SourceAdapterAvailability::Unsupported {
                "This adapter is not supported on the current operating system.".to_owned()
            } else if availability == SourceAdapterAvailability::Available {
                "Package manager executable was discovered.".to_owned()
            } else {
                "Package manager executable is not available on PATH.".to_owned()
            },
        })
    }

    fn available_commands(&self, adapter: SourceAdapterKind) -> TorbenResult<&AdapterCommands> {
        if !self.allow_unsupported_platform && !platform_supported(adapter) {
            return Err(TorbenError::new(
                "source_adapter_platform_unsupported",
                "The source adapter is not supported on this operating system.",
            )
            .with_detail("adapter", adapter.to_string()));
        }
        self.commands.get(&adapter).ok_or_else(|| {
            TorbenError::new(
                "source_adapter_unavailable",
                "The package manager executable is not available on PATH.",
            )
            .with_detail("adapter", adapter.to_string())
        })
    }
}

async fn inspect_winget(
    runner: &dyn SourceCommandRunner,
    executable: &Path,
    coordinate: PackageCoordinate,
    package_kind: SourcePackageKind,
) -> TorbenResult<SourcePackageState> {
    let export_path = std::env::temp_dir().join(format!(
        "torben-winget-export-{}-{}.json",
        std::process::id(),
        timestamp_nanos()
    ));
    let path = export_path.display().to_string();
    let output = run_command(
        runner,
        executable,
        &[
            "export",
            "--output",
            &path,
            "--include-versions",
            "--source",
            "winget",
            "--accept-source-agreements",
            "--disable-interactivity",
        ],
        &BTreeMap::new(),
    )
    .await;
    let bytes = match output {
        Ok(output) if output.success => read_bounded_file(&export_path, MAX_EXPORT_BYTES),
        Ok(output) => Err(command_failed(SourceAdapterKind::Winget, &output)),
        Err(error) => Err(error),
    };
    let _ = std::fs::remove_file(&export_path);
    let bytes = bytes?;
    let version = parse_winget_export(&bytes, &coordinate)?;
    package_state(
        SourceAdapterKind::Winget,
        coordinate,
        package_kind,
        version,
        None,
    )
}

async fn inspect_homebrew(
    runner: &dyn SourceCommandRunner,
    executable: &Path,
    coordinate: PackageCoordinate,
    package_kind: SourcePackageKind,
) -> TorbenResult<SourcePackageState> {
    let output = run_command(
        runner,
        executable,
        &["info", "--json=v2", coordinate.as_str()],
        &homebrew_environment(),
    )
    .await?;
    if !output.success {
        return package_state(
            SourceAdapterKind::Homebrew,
            coordinate,
            package_kind,
            None,
            None,
        );
    }
    let version = parse_homebrew_info(&output.stdout, &coordinate, package_kind)?;
    package_state(
        SourceAdapterKind::Homebrew,
        coordinate,
        package_kind,
        version,
        None,
    )
}

async fn inspect_dpkg(
    runner: &dyn SourceCommandRunner,
    executable: &Path,
    coordinate: PackageCoordinate,
    package_kind: SourcePackageKind,
) -> TorbenResult<SourcePackageState> {
    let output = run_command(
        runner,
        executable,
        &[
            "-W",
            "-f=${db:Status-Abbrev}\\t${Version}\\t${Architecture}\\n",
            coordinate.as_str(),
        ],
        &BTreeMap::new(),
    )
    .await?;
    let (version, architecture) = if output.success {
        parse_dpkg_query(&output.stdout)?
    } else {
        (None, None)
    };
    package_state(
        SourceAdapterKind::Apt,
        coordinate,
        package_kind,
        version,
        architecture,
    )
}

async fn inspect_rpm(
    runner: &dyn SourceCommandRunner,
    executable: &Path,
    coordinate: PackageCoordinate,
    package_kind: SourcePackageKind,
) -> TorbenResult<SourcePackageState> {
    let output = run_command(
        runner,
        executable,
        &[
            "-q",
            "--qf",
            "%{NAME}\\t%{EVR}\\t%{ARCH}\\n",
            coordinate.as_str(),
        ],
        &BTreeMap::new(),
    )
    .await?;
    let (version, architecture) = if output.success {
        parse_rpm_query(&output.stdout, &coordinate)?
    } else {
        (None, None)
    };
    package_state(
        SourceAdapterKind::Dnf,
        coordinate,
        package_kind,
        version,
        architecture,
    )
}

async fn resolve_dnf_available_nevra(
    runner: &dyn SourceCommandRunner,
    executable: &Path,
    coordinate: &PackageCoordinate,
    requested_version: &SourcePackageVersion,
) -> TorbenResult<String> {
    let output = run_command(
        runner,
        executable,
        &[
            "repoquery",
            "--available",
            "--queryformat",
            "%{name}\\t%{epoch}\\t%{version}\\t%{release}\\t%{arch}\\n",
            coordinate.as_str(),
        ],
        &BTreeMap::new(),
    )
    .await?;
    if !output.success {
        return Err(command_failed(SourceAdapterKind::Dnf, &output));
    }
    let candidates = parse_dnf_repoquery(&output.stdout, coordinate, requested_version)?;
    match candidates.as_slice() {
        [identity] => Ok(identity.clone()),
        [] => Err(TorbenError::new(
            "source_dnf_nevra_not_found",
            "No available DNF package matches the requested exact raw version.",
        )
        .with_detail("coordinate", coordinate.to_string())
        .with_detail("packageVersion", requested_version.to_string())
        .with_remediation("Refresh repository metadata or review a different exact version.")),
        _ => Err(TorbenError::new(
            "source_dnf_nevra_ambiguous",
            "More than one repository package matches the requested DNF name and version.",
        )
        .with_detail("coordinate", coordinate.to_string())
        .with_detail("packageVersion", requested_version.to_string())
        .with_detail("matches", candidates.join(","))
        .with_remediation("Narrow the enabled repositories or architecture, then review again.")),
    }
}

async fn resolve_dnf_installed_nevra(
    runner: &dyn SourceCommandRunner,
    executable: &Path,
    coordinate: &PackageCoordinate,
    requested_version: Option<&SourcePackageVersion>,
) -> TorbenResult<String> {
    let state = inspect_rpm(
        runner,
        executable,
        coordinate.clone(),
        SourcePackageKind::Native,
    )
    .await?;
    let version = state.installed_version.ok_or_else(|| {
        TorbenError::new(
            "source_dnf_package_not_installed",
            "The DNF package is not installed and cannot be locked for removal.",
        )
    })?;
    if requested_version.is_some_and(|requested| requested != &version) {
        return Err(TorbenError::new(
            "source_package_state_drifted",
            "The installed DNF package version differs from the requested version.",
        )
        .with_detail("installedVersion", version.to_string()));
    }
    let architecture = state.architecture.ok_or_else(|| {
        TorbenError::new(
            "source_metadata_invalid",
            "The installed DNF package did not report an architecture.",
        )
    })?;
    Ok(format!("{coordinate}-{version}.{architecture}"))
}

fn parse_dnf_repoquery(
    bytes: &[u8],
    coordinate: &PackageCoordinate,
    requested_version: &SourcePackageVersion,
) -> TorbenResult<Vec<String>> {
    let text = std::str::from_utf8(bytes).map_err(metadata_error)?;
    let mut matches = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(metadata_invalid("dnfRepoquery"));
        }
        let [name, epoch, version, release, architecture] = fields.as_slice() else {
            unreachable!()
        };
        if *name != coordinate.as_str() {
            return Err(metadata_invalid("dnfRepoqueryName"));
        }
        let raw_version = if matches!(*epoch, "" | "0" | "(none)") {
            format!("{version}-{release}")
        } else {
            format!("{epoch}:{version}-{release}")
        };
        let parsed = SourcePackageVersion::new(&raw_version)?;
        if &parsed == requested_version {
            matches.push(format!("{name}-{raw_version}.{architecture}"));
        }
    }
    Ok(matches)
}

fn plan_winget(
    action: SourceAction,
    commands: &AdapterCommands,
    coordinate: PackageCoordinate,
    package_kind: SourcePackageKind,
    package_version: Option<SourcePackageVersion>,
) -> TorbenResult<SourceOperationPlan> {
    let mut preview = vec![
        "show".to_owned(),
        "--id".to_owned(),
        coordinate.to_string(),
        "--exact".to_owned(),
        "--source".to_owned(),
        "winget".to_owned(),
        "--scope".to_owned(),
        "user".to_owned(),
        "--accept-source-agreements".to_owned(),
        "--disable-interactivity".to_owned(),
    ];
    if let Some(version) = &package_version {
        preview.extend(["--version".to_owned(), version.to_string()]);
    }
    let mut execute = vec![
        action.to_string(),
        "--id".to_owned(),
        coordinate.to_string(),
        "--exact".to_owned(),
        "--source".to_owned(),
        "winget".to_owned(),
        "--scope".to_owned(),
        "user".to_owned(),
        "--silent".to_owned(),
        "--disable-interactivity".to_owned(),
        "--accept-source-agreements".to_owned(),
    ];
    if action == SourceAction::Install {
        execute.extend([
            "--accept-package-agreements".to_owned(),
            "--no-upgrade".to_owned(),
        ]);
    }
    if let Some(version) = &package_version {
        execute.extend(["--version".to_owned(), version.to_string()]);
    }
    let exact = action == SourceAction::Uninstall || package_version.is_some();
    Ok(SourceOperationPlan {
        action,
        adapter: SourceAdapterKind::Winget,
        source_id: source_id(SourceAdapterKind::Winget)?,
        coordinate,
        package_kind,
        package_version,
        executable: commands.primary.display().to_string(),
        preview_arguments: preview,
        execute_arguments: execute,
        execution_identity: None,
        environment: BTreeMap::new(),
        requires_elevation: false,
        exact_version_guaranteed: exact,
        mutates_system: true,
        warnings: vec![
            "Execution requires explicit acceptance of the package and source agreements."
                .to_owned(),
            "The plan requires a user-scope installer and must fail rather than fall back to machine scope."
                .to_owned(),
        ],
    })
}

fn plan_homebrew(
    action: SourceAction,
    commands: &AdapterCommands,
    coordinate: PackageCoordinate,
    package_kind: SourcePackageKind,
    package_version: Option<SourcePackageVersion>,
) -> TorbenResult<SourceOperationPlan> {
    if package_version.is_some() {
        return Err(TorbenError::new(
            "source_exact_version_unsupported",
            "Homebrew does not guarantee installation of an arbitrary raw historical version.",
        )
        .with_remediation(
            "Use a versioned formula coordinate or the Torben official archive source.",
        ));
    }
    let kind_flag = match package_kind {
        SourcePackageKind::Formula => "--formula",
        SourcePackageKind::Cask => "--cask",
        SourcePackageKind::Native => unreachable!(),
    };
    let preview = if action == SourceAction::Install {
        vec![
            "install".to_owned(),
            "--dry-run".to_owned(),
            kind_flag.to_owned(),
            coordinate.to_string(),
        ]
    } else {
        vec![
            "info".to_owned(),
            "--json=v2".to_owned(),
            coordinate.to_string(),
        ]
    };
    let execute = if action == SourceAction::Install {
        vec![
            "install".to_owned(),
            "--no-ask".to_owned(),
            kind_flag.to_owned(),
            coordinate.to_string(),
        ]
    } else {
        vec![
            "uninstall".to_owned(),
            kind_flag.to_owned(),
            coordinate.to_string(),
        ]
    };
    Ok(SourceOperationPlan {
        action,
        adapter: SourceAdapterKind::Homebrew,
        source_id: source_id(SourceAdapterKind::Homebrew)?,
        coordinate,
        package_kind,
        package_version: None,
        executable: commands.primary.display().to_string(),
        preview_arguments: preview,
        execute_arguments: execute,
        execution_identity: None,
        environment: homebrew_environment(),
        requires_elevation: false,
        exact_version_guaranteed: false,
        mutates_system: true,
        warnings: vec![
            "Homebrew owns a shared prefix and can change dependencies outside Torben's managed library."
                .to_owned(),
            "The selected formula or cask must be re-inspected after execution to lock its actual version."
                .to_owned(),
        ],
    })
}

fn plan_apt(
    action: SourceAction,
    commands: &AdapterCommands,
    coordinate: PackageCoordinate,
    package_kind: SourcePackageKind,
    package_version: Option<SourcePackageVersion>,
) -> TorbenResult<SourceOperationPlan> {
    let spec = package_spec(action, &coordinate, package_version.as_ref(), '=')?;
    let operation = if action == SourceAction::Install {
        "install"
    } else {
        "remove"
    };
    let mut preview = vec!["--simulate".to_owned()];
    if action == SourceAction::Install {
        preview.push("--no-remove".to_owned());
    }
    preview.extend([operation.to_owned(), spec.clone()]);
    let mut execute = vec![
        "-y".to_owned(),
        "-o".to_owned(),
        "Dpkg::Use-Pty=0".to_owned(),
    ];
    if action == SourceAction::Install {
        execute.push("--no-remove".to_owned());
    }
    execute.extend([operation.to_owned(), spec]);
    Ok(SourceOperationPlan {
        action,
        adapter: SourceAdapterKind::Apt,
        source_id: source_id(SourceAdapterKind::Apt)?,
        coordinate,
        package_kind,
        package_version: package_version.clone(),
        executable: commands.primary.display().to_string(),
        preview_arguments: preview,
        execute_arguments: execute,
        execution_identity: None,
        environment: BTreeMap::from([
            ("DEBIAN_FRONTEND".to_owned(), "noninteractive".to_owned()),
            ("APT_LISTCHANGES_FRONTEND".to_owned(), "none".to_owned()),
        ]),
        requires_elevation: true,
        exact_version_guaranteed: action == SourceAction::Uninstall || package_version.is_some(),
        mutates_system: true,
        warnings: privilege_warnings("apt-get"),
    })
}

fn plan_dnf(
    action: SourceAction,
    commands: &AdapterCommands,
    coordinate: PackageCoordinate,
    package_kind: SourcePackageKind,
    package_version: Option<SourcePackageVersion>,
) -> TorbenResult<SourceOperationPlan> {
    let spec = package_spec(action, &coordinate, package_version.as_ref(), '-')?;
    let operation = if action == SourceAction::Install {
        "install"
    } else {
        "remove"
    };
    let common = vec![
        "--best".to_owned(),
        "--setopt=clean_requirements_on_remove=False".to_owned(),
        operation.to_owned(),
        spec,
    ];
    let mut preview = vec!["--assumeno".to_owned()];
    preview.extend(common.clone());
    let mut execute = vec!["--assumeyes".to_owned()];
    execute.extend(common);
    Ok(SourceOperationPlan {
        action,
        adapter: SourceAdapterKind::Dnf,
        source_id: source_id(SourceAdapterKind::Dnf)?,
        coordinate,
        package_kind,
        package_version: package_version.clone(),
        executable: commands.primary.display().to_string(),
        preview_arguments: preview,
        execute_arguments: execute,
        execution_identity: None,
        environment: BTreeMap::new(),
        requires_elevation: true,
        exact_version_guaranteed: action == SourceAction::Uninstall || package_version.is_some(),
        mutates_system: true,
        warnings: privilege_warnings("dnf"),
    })
}

fn package_spec(
    action: SourceAction,
    coordinate: &PackageCoordinate,
    version: Option<&SourcePackageVersion>,
    separator: char,
) -> TorbenResult<String> {
    if action == SourceAction::Install && version.is_none() {
        return Err(TorbenError::new(
            "source_package_version_required",
            "An exact raw package version is required for this install plan.",
        ));
    }
    Ok(version.map_or_else(
        || coordinate.to_string(),
        |version| format!("{coordinate}{separator}{version}"),
    ))
}

fn package_state(
    adapter: SourceAdapterKind,
    coordinate: PackageCoordinate,
    package_kind: SourcePackageKind,
    version: Option<SourcePackageVersion>,
    architecture: Option<String>,
) -> TorbenResult<SourcePackageState> {
    Ok(SourcePackageState {
        adapter,
        source_id: source_id(adapter)?,
        coordinate,
        package_kind,
        installed: version.is_some(),
        installed_version: version,
        architecture,
        manager_owned: true,
    })
}

fn parse_winget_export(
    bytes: &[u8],
    coordinate: &PackageCoordinate,
) -> TorbenResult<Option<SourcePackageVersion>> {
    let root: Value = serde_json::from_slice(bytes).map_err(metadata_error)?;
    let sources = root
        .get("Sources")
        .and_then(Value::as_array)
        .ok_or_else(|| metadata_invalid("Sources"))?;
    for package in sources
        .iter()
        .filter_map(|source| source.get("Packages").and_then(Value::as_array))
        .flatten()
    {
        if package.get("PackageIdentifier").and_then(Value::as_str) == Some(coordinate.as_str()) {
            return package
                .get("Version")
                .and_then(Value::as_str)
                .map(SourcePackageVersion::new)
                .transpose();
        }
    }
    Ok(None)
}

fn parse_homebrew_info(
    bytes: &[u8],
    coordinate: &PackageCoordinate,
    package_kind: SourcePackageKind,
) -> TorbenResult<Option<SourcePackageVersion>> {
    let root: Value = serde_json::from_slice(bytes).map_err(metadata_error)?;
    let section = match package_kind {
        SourcePackageKind::Formula => "formulae",
        SourcePackageKind::Cask => "casks",
        SourcePackageKind::Native => unreachable!(),
    };
    let items = root
        .get(section)
        .and_then(Value::as_array)
        .ok_or_else(|| metadata_invalid(section))?;
    for item in items {
        let identity = if package_kind == SourcePackageKind::Formula {
            item.get("full_name")
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
        } else {
            item.get("full_token")
                .or_else(|| item.get("token"))
                .and_then(Value::as_str)
        };
        if identity != Some(coordinate.as_str()) {
            continue;
        }
        let installed = item
            .get("installed")
            .and_then(Value::as_array)
            .and_then(|values| values.last());
        let raw = installed.and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("version").and_then(Value::as_str))
        });
        return raw.map(SourcePackageVersion::new).transpose();
    }
    Ok(None)
}

fn parse_dpkg_query(bytes: &[u8]) -> TorbenResult<(Option<SourcePackageVersion>, Option<String>)> {
    let text = std::str::from_utf8(bytes).map_err(metadata_error)?;
    let Some(line) = text.lines().find(|line| !line.trim().is_empty()) else {
        return Ok((None, None));
    };
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.len() != 3 || !fields[0].starts_with("ii") {
        return Ok((None, None));
    }
    Ok((
        Some(SourcePackageVersion::new(fields[1])?),
        Some(fields[2].to_owned()),
    ))
}

fn parse_rpm_query(
    bytes: &[u8],
    coordinate: &PackageCoordinate,
) -> TorbenResult<(Option<SourcePackageVersion>, Option<String>)> {
    let text = std::str::from_utf8(bytes).map_err(metadata_error)?;
    for line in text.lines() {
        let fields = line.split('\t').collect::<Vec<_>>();
        if fields.len() == 3 && fields[0] == coordinate.as_str() {
            return Ok((
                Some(SourcePackageVersion::new(fields[1])?),
                Some(fields[2].to_owned()),
            ));
        }
    }
    Ok((None, None))
}

fn validate_package_kind(
    adapter: SourceAdapterKind,
    package_kind: SourcePackageKind,
) -> TorbenResult<()> {
    let valid = if adapter == SourceAdapterKind::Homebrew {
        matches!(
            package_kind,
            SourcePackageKind::Formula | SourcePackageKind::Cask
        )
    } else {
        package_kind == SourcePackageKind::Native
    };
    if valid {
        Ok(())
    } else {
        Err(TorbenError::new(
            "source_package_kind_mismatch",
            "The package kind is not valid for this source adapter.",
        )
        .with_detail("adapter", adapter.to_string())
        .with_detail("packageKind", package_kind.to_string()))
    }
}

fn source_id(adapter: SourceAdapterKind) -> TorbenResult<SourceId> {
    SourceId::new(format!("source.{adapter}"))
}

fn platform_supported(adapter: SourceAdapterKind) -> bool {
    match adapter {
        SourceAdapterKind::Winget => cfg!(windows),
        SourceAdapterKind::Homebrew => matches!(std::env::consts::OS, "macos" | "linux"),
        SourceAdapterKind::Apt | SourceAdapterKind::Dnf => cfg!(target_os = "linux"),
    }
}

fn find_command(names: &[&str]) -> Option<PathBuf> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for name in names {
            let candidate = directory.join(name);
            let Ok(canonical) = std::fs::canonicalize(candidate) else {
                continue;
            };
            let Ok(metadata) = canonical.symlink_metadata() else {
                continue;
            };
            if !metadata.file_type().is_symlink() && metadata.is_file() {
                return Some(canonical);
            }
        }
    }
    None
}

async fn run_command(
    runner: &dyn SourceCommandRunner,
    executable: &Path,
    arguments: &[&str],
    environment: &BTreeMap<String, String>,
) -> TorbenResult<CommandOutput> {
    run_command_owned(
        runner,
        executable.to_path_buf(),
        arguments.iter().map(|value| (*value).to_owned()).collect(),
        environment.clone(),
    )
    .await
}

async fn run_command_owned(
    runner: &dyn SourceCommandRunner,
    executable: PathBuf,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
) -> TorbenResult<CommandOutput> {
    runner.run(executable, arguments, environment).await
}

async fn run_system_command(
    executable: PathBuf,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
) -> TorbenResult<CommandOutput> {
    let mut command = tokio::process::Command::new(&executable);
    command
        .args(arguments)
        .envs(environment)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(COMMAND_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            TorbenError::new(
                "source_command_timeout",
                "The package manager command timed out.",
            )
            .with_detail("path", executable.display().to_string())
        })?
        .map_err(|error| {
            TorbenError::new(
                "source_command_start_failed",
                "Could not start the package manager command.",
            )
            .with_detail("path", executable.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
    if output.stdout.len() > MAX_OUTPUT_BYTES || output.stderr.len() > MAX_OUTPUT_BYTES {
        return Err(TorbenError::new(
            "source_command_output_too_large",
            "The package manager output exceeds the allowed size.",
        ));
    }
    Ok(CommandOutput {
        success: output.status.success(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn validate_health_executable(app_id: &str, executable: &Path) -> TorbenResult<PathBuf> {
    if !executable.is_absolute() {
        return Err(TorbenError::new(
            "source_health_path_invalid",
            "The package-manager health-check executable path must be absolute.",
        )
        .with_detail("path", executable.display().to_string()));
    }
    let metadata = executable.symlink_metadata().map_err(|error| {
        TorbenError::new(
            "source_health_path_invalid",
            "The package-manager health-check executable is unavailable.",
        )
        .with_detail("path", executable.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TorbenError::new(
            "source_health_path_invalid",
            "The package-manager health-check executable must be a regular non-link file.",
        )
        .with_detail("path", executable.display().to_string()));
    }
    let actual_name = executable
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let expected_names: &[&str] = match app_id {
        "node" => &["node"],
        "temurin" => &["java"],
        "python" => &["python", "python3"],
        "git" => &["git"],
        "vscode" => &["code"],
        "codex" => &["codex"],
        _ => {
            return Err(TorbenError::new(
                "source_application_unsupported",
                "This application does not define a package-manager health check.",
            )
            .with_detail("appId", app_id));
        }
    };
    if !expected_names.contains(&actual_name.as_str()) {
        return Err(TorbenError::new(
            "source_health_executable_mismatch",
            "The health-check executable name does not match the application.",
        )
        .with_detail("appId", app_id)
        .with_detail("path", executable.display().to_string())
        .with_detail("expectedNames", expected_names.join(",")));
    }
    std::fs::canonicalize(executable).map_err(io_error)
}

fn read_bounded_file(path: &Path, maximum: u64) -> TorbenResult<Vec<u8>> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > maximum {
        return Err(TorbenError::new(
            "source_export_invalid",
            "The package manager export is not a bounded regular file.",
        ));
    }
    std::fs::read(path).map_err(io_error)
}

fn homebrew_environment() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("HOMEBREW_NO_AUTO_UPDATE".to_owned(), "1".to_owned()),
        ("HOMEBREW_NO_INSTALL_UPGRADE".to_owned(), "1".to_owned()),
        ("HOMEBREW_NO_ANALYTICS".to_owned(), "1".to_owned()),
        ("HOMEBREW_NO_ENV_HINTS".to_owned(), "1".to_owned()),
    ])
}

fn privilege_warnings(manager: &str) -> Vec<String> {
    vec![
        format!(
            "{manager} usually requires root privileges; Torben will never invoke sudo or elevate itself."
        ),
        "The preview must be reviewed and the post-operation package state reconciled before ownership is committed."
            .to_owned(),
    ]
}

fn first_nonempty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(256).collect())
}

fn bounded_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim()
        .chars()
        .take(512)
        .collect()
}

fn command_failed(adapter: SourceAdapterKind, output: &CommandOutput) -> TorbenError {
    TorbenError::new(
        "source_command_failed",
        "The package manager query returned an error.",
    )
    .with_detail("adapter", adapter.to_string())
    .with_detail("stderr", bounded_text(&output.stderr))
}

fn metadata_invalid(field: &str) -> TorbenError {
    TorbenError::new(
        "source_metadata_invalid",
        "The package manager returned malformed metadata.",
    )
    .with_detail("field", field)
}

fn metadata_error(error: impl std::fmt::Display) -> TorbenError {
    TorbenError::new(
        "source_metadata_invalid",
        "The package manager returned malformed metadata.",
    )
    .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "source_filesystem_error",
        "A package manager query file operation failed.",
    )
    .with_detail("reason", error.to_string())
}

fn timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf, str::FromStr, sync::Arc, time::Duration};

    use tokio::sync::Barrier;

    use torben_contracts::{
        PackageCoordinate, SourceAction, SourceAdapterKind, SourcePackageKind, SourcePackageVersion,
    };

    use super::{
        AdapterCommands, CommandFuture, CommandOutput, SourceAdapterService, SourceCommandRunner,
        SystemCommandRunner, parse_dnf_repoquery, parse_dpkg_query, parse_homebrew_info,
        parse_rpm_query, parse_winget_export, plan_apt, plan_dnf, plan_homebrew,
    };

    #[derive(Clone)]
    struct StaticRunner {
        output: CommandOutput,
    }

    impl SourceCommandRunner for StaticRunner {
        fn run(
            &self,
            _executable: PathBuf,
            _arguments: Vec<String>,
            _environment: BTreeMap<String, String>,
        ) -> CommandFuture {
            let output = self.output.clone();
            Box::pin(async move { Ok(output) })
        }
    }

    struct CoordinatedRunner {
        barrier: Arc<Barrier>,
    }

    impl SourceCommandRunner for CoordinatedRunner {
        fn run(
            &self,
            executable: PathBuf,
            _arguments: Vec<String>,
            _environment: BTreeMap<String, String>,
        ) -> CommandFuture {
            let barrier = Arc::clone(&self.barrier);
            Box::pin(async move {
                barrier.wait().await;
                Ok(CommandOutput {
                    success: true,
                    stdout: format!("{} 1.0\n", executable.display()).into_bytes(),
                    stderr: Vec::new(),
                })
            })
        }
    }

    #[tokio::test]
    async fn status_probes_start_concurrently_and_keep_catalog_order() {
        let service = SourceAdapterService::for_tests(
            BTreeMap::from([
                (
                    SourceAdapterKind::Winget,
                    AdapterCommands {
                        primary: PathBuf::from("winget"),
                        query: None,
                    },
                ),
                (
                    SourceAdapterKind::Homebrew,
                    AdapterCommands {
                        primary: PathBuf::from("brew"),
                        query: None,
                    },
                ),
            ]),
            Arc::new(CoordinatedRunner {
                barrier: Arc::new(Barrier::new(2)),
            }),
        );

        let statuses = tokio::time::timeout(Duration::from_secs(1), service.statuses())
            .await
            .expect("both package-manager probes should be polled together")
            .unwrap();

        assert_eq!(statuses.len(), SourceAdapterKind::ALL.len());
        assert_eq!(statuses[0].adapter, SourceAdapterKind::Winget);
        assert_eq!(statuses[0].version.as_deref(), Some("winget 1.0"));
        assert_eq!(statuses[1].adapter, SourceAdapterKind::Homebrew);
        assert_eq!(statuses[1].version.as_deref(), Some("brew 1.0"));
    }

    #[test]
    fn parses_machine_readable_installed_package_outputs() {
        let coordinate = PackageCoordinate::new("Microsoft.VisualStudioCode").unwrap();
        let winget = br#"{"Sources":[{"Packages":[{"PackageIdentifier":"Microsoft.VisualStudioCode","Version":"1.134.0"}]}]}"#;
        assert_eq!(
            parse_winget_export(winget, &coordinate)
                .unwrap()
                .unwrap()
                .as_str(),
            "1.134.0"
        );
        let formula = PackageCoordinate::new("homebrew/core/node@24").unwrap();
        let brew = br#"{"formulae":[{"full_name":"homebrew/core/node@24","installed":[{"version":"24.9.0"}]}],"casks":[]}"#;
        assert_eq!(
            parse_homebrew_info(brew, &formula, SourcePackageKind::Formula)
                .unwrap()
                .unwrap()
                .as_str(),
            "24.9.0"
        );
        let (apt, architecture) =
            parse_dpkg_query(b"ii \t1:20.11.1+dfsg-2~deb12u1\tamd64\n").unwrap();
        assert_eq!(apt.unwrap().as_str(), "1:20.11.1+dfsg-2~deb12u1");
        assert_eq!(architecture.as_deref(), Some("amd64"));
        let rpm_coordinate = PackageCoordinate::new("git").unwrap();
        let (rpm, architecture) =
            parse_rpm_query(b"git\t2.48.1-1.fc42\tx86_64\n", &rpm_coordinate).unwrap();
        assert_eq!(rpm.unwrap().as_str(), "2.48.1-1.fc42");
        assert_eq!(architecture.as_deref(), Some("x86_64"));
    }

    #[test]
    fn parses_dnf_repoquery_into_one_full_nevra() {
        let coordinate = PackageCoordinate::new("code").unwrap();
        let version = SourcePackageVersion::new("1.134.0-1.fc42").unwrap();

        let matches =
            parse_dnf_repoquery(b"code\t0\t1.134.0\t1.fc42\tx86_64\n", &coordinate, &version)
                .unwrap();

        assert_eq!(matches, ["code-1.134.0-1.fc42.x86_64"]);
    }

    #[tokio::test]
    async fn reviewed_dnf_plan_locks_one_repository_nevra() {
        let service = SourceAdapterService::for_test(
            SourceAdapterKind::Dnf,
            PathBuf::from("dnf"),
            Some(PathBuf::from("rpm")),
            Arc::new(StaticRunner {
                output: CommandOutput {
                    success: true,
                    stdout: b"code\t0\t1.134.0\t1.fc42\tx86_64\n".to_vec(),
                    stderr: Vec::new(),
                },
            }),
        );

        let plan = service
            .reviewed_plan(
                SourceAction::Install,
                SourceAdapterKind::Dnf,
                PackageCoordinate::new("code").unwrap(),
                SourcePackageKind::Native,
                Some(SourcePackageVersion::new("1.134.0-1.fc42").unwrap()),
            )
            .await
            .unwrap();

        assert_eq!(
            plan.execution_identity.as_deref(),
            Some("code-1.134.0-1.fc42.x86_64")
        );
        assert_eq!(
            plan.execute_arguments.last().map(String::as_str),
            plan.execution_identity.as_deref()
        );
        assert_eq!(
            plan.preview_arguments.last().map(String::as_str),
            plan.execution_identity.as_deref()
        );
    }

    #[tokio::test]
    async fn reviewed_dnf_plan_rejects_missing_and_ambiguous_nevra() {
        for (stdout, code) in [
            (Vec::new(), "source_dnf_nevra_not_found"),
            (
                b"code\t0\t1.134.0\t1.fc42\tx86_64\ncode\t0\t1.134.0\t1.fc42\taarch64\n".to_vec(),
                "source_dnf_nevra_ambiguous",
            ),
        ] {
            let service = SourceAdapterService::for_test(
                SourceAdapterKind::Dnf,
                PathBuf::from("dnf"),
                Some(PathBuf::from("rpm")),
                Arc::new(StaticRunner {
                    output: CommandOutput {
                        success: true,
                        stdout,
                        stderr: Vec::new(),
                    },
                }),
            );
            let error = service
                .reviewed_plan(
                    SourceAction::Install,
                    SourceAdapterKind::Dnf,
                    PackageCoordinate::new("code").unwrap(),
                    SourcePackageKind::Native,
                    Some(SourcePackageVersion::new("1.134.0-1.fc42").unwrap()),
                )
                .await
                .unwrap_err();
            assert_eq!(error.code, code);
        }
    }

    #[test]
    fn plans_never_add_elevation_or_integrity_bypass_flags() {
        let fake = PathBuf::from(if cfg!(windows) {
            r"C:\fixture\winget.exe"
        } else {
            "/fixture/winget"
        });
        let service = SourceAdapterService {
            commands: BTreeMap::from([(
                SourceAdapterKind::Winget,
                AdapterCommands {
                    primary: fake,
                    query: None,
                },
            )]),
            runner: Arc::new(SystemCommandRunner),
            allow_unsupported_platform: false,
        };
        let plan = service
            .plan(
                SourceAction::Install,
                SourceAdapterKind::Winget,
                PackageCoordinate::new("Microsoft.VisualStudioCode").unwrap(),
                SourcePackageKind::Native,
                Some(SourcePackageVersion::from_str("1.134.0").unwrap()),
            )
            .unwrap();
        let joined = plan.execute_arguments.join(" ");
        assert!(joined.contains("--scope user"));
        assert!(joined.contains("--disable-interactivity"));
        assert!(!joined.contains("--force"));
        assert!(!joined.contains("ignore-security-hash"));
        assert!(!joined.contains("sudo"));
    }

    #[test]
    fn apt_and_dnf_require_exact_versions_for_install_plans() {
        let coordinate = PackageCoordinate::new("nodejs").unwrap();
        let kind = SourcePackageKind::Native;
        let commands = AdapterCommands {
            primary: PathBuf::from("fixture"),
            query: Some(PathBuf::from("query")),
        };
        for adapter in [SourceAdapterKind::Apt, SourceAdapterKind::Dnf] {
            let result = match adapter {
                SourceAdapterKind::Apt => plan_apt(
                    SourceAction::Install,
                    &commands,
                    coordinate.clone(),
                    kind,
                    None,
                ),
                SourceAdapterKind::Dnf => plan_dnf(
                    SourceAction::Install,
                    &commands,
                    coordinate.clone(),
                    kind,
                    None,
                ),
                _ => unreachable!(),
            };
            assert_eq!(result.unwrap_err().code, "source_package_version_required");
        }
    }

    #[test]
    fn homebrew_rejects_arbitrary_raw_version_installation() {
        let commands = AdapterCommands {
            primary: PathBuf::from("brew"),
            query: None,
        };
        assert_eq!(
            plan_homebrew(
                SourceAction::Install,
                &commands,
                PackageCoordinate::new("node@24").unwrap(),
                SourcePackageKind::Formula,
                Some(SourcePackageVersion::new("24.9.0").unwrap()),
            )
            .unwrap_err()
            .code,
            "source_exact_version_unsupported"
        );
    }

    #[test]
    fn homebrew_uninstall_uses_only_supported_flags() {
        let commands = AdapterCommands {
            primary: PathBuf::from("brew"),
            query: None,
        };
        let plan = plan_homebrew(
            SourceAction::Uninstall,
            &commands,
            PackageCoordinate::new("visual-studio-code").unwrap(),
            SourcePackageKind::Cask,
            None,
        )
        .unwrap();
        assert_eq!(
            plan.execute_arguments,
            ["uninstall", "--cask", "visual-studio-code"]
        );
    }
}
