use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::AsyncWriteExt;
use torben_contracts::{
    AppId, ExactVersion, InstallRecord, InstallScope, OperationState, SourceId, TorbenError,
    TorbenResult, VersionDescriptor,
    plugin::{InstallPlan, InstallStep},
};
use url::Url;

use crate::{
    TorbenPaths,
    node::sha256_file_checked,
    operation::{CancellationProbe, OperationJournal},
};

const POSTGRESQL_SOURCE_ID: &str = "postgresql.edb";
const POSTGRESQL_BASE_URL: &str = "https://get.enterprisedb.com/postgresql/";
const POSTGRESQL_TARGET: &str = "x86_64-pc-windows-msvc";
const MAX_INSTALLER_BYTES: u64 = 1024 * 1024 * 1024;
const POSTGRESQL_COMMANDS: &[&str] = &[
    "postgres",
    "psql",
    "pg_ctl",
    "initdb",
    "pg_isready",
    "createdb",
    "dropdb",
    "createuser",
    "dropuser",
    "pg_dump",
    "pg_dumpall",
    "pg_restore",
    "pg_basebackup",
    "pgbench",
    "vacuumdb",
    "reindexdb",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DistributionSpec {
    version: &'static str,
    release_version: &'static str,
    installer_revision: &'static str,
    sha256: &'static str,
    released_at: &'static str,
    recommended: bool,
}

const DISTRIBUTIONS: &[DistributionSpec] = &[
    DistributionSpec {
        version: "18.6.0",
        release_version: "18.6",
        installer_revision: "18.6-3",
        sha256: "3bb55a421849fa5749fe807e45b05a9a7758a16389591ee0a41b7fcabf724b90",
        released_at: "2026-09-01",
        recommended: true,
    },
    DistributionSpec {
        version: "17.11.0",
        release_version: "17.11",
        installer_revision: "17.11-3",
        sha256: "2fd19749560be03020026f2d842b69af47f0ea2c7946bda17eed26a4a9235695",
        released_at: "2026-09-01",
        recommended: false,
    },
];

#[derive(Debug, Clone)]
pub struct PostgresqlProvider {
    client: reqwest::Client,
    base_url: Url,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PostgresqlDistribution {
    pub installer_name: String,
    pub installer_url: Url,
    pub checksum: String,
    pub release_version: String,
}

impl PostgresqlProvider {
    pub fn official() -> TorbenResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            base_url: Url::parse(POSTGRESQL_BASE_URL).map_err(url_error)?,
        })
    }

    pub fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            return Err(unsupported_platform());
        }
        DISTRIBUTIONS
            .iter()
            .map(|spec| {
                Ok(VersionDescriptor {
                    version: ExactVersion::from_str(spec.version)?,
                    lts_name: spec
                        .recommended
                        .then(|| "PostgreSQL current major".to_owned()),
                    released_at: spec.released_at.to_owned(),
                    recommended: spec.recommended,
                })
            })
            .collect()
    }

    pub fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            return Err(unsupported_platform());
        }
        if let Ok(version) = ExactVersion::from_str(requested) {
            if distribution_spec(&version).is_some() {
                return Ok(version);
            }
            return Err(version_not_found(requested));
        }
        if matches!(
            requested.trim().to_ascii_lowercase().as_str(),
            "stable" | "latest" | "recommended"
        ) {
            return ExactVersion::from_str(DISTRIBUTIONS[0].version);
        }
        Err(version_not_found(requested))
    }

    pub fn distribution(&self, version: &ExactVersion) -> TorbenResult<PostgresqlDistribution> {
        let spec =
            distribution_spec(version).ok_or_else(|| version_not_found(&version.to_string()))?;
        let installer_name = format!("postgresql-{}-windows-x64.exe", spec.installer_revision);
        let installer_url = self.base_url.join(&installer_name).map_err(url_error)?;
        Ok(PostgresqlDistribution {
            installer_name,
            installer_url,
            checksum: spec.sha256.to_owned(),
            release_version: spec.release_version.to_owned(),
        })
    }

    pub async fn install(
        &self,
        paths: &TorbenPaths,
        app_id: &AppId,
        version: &ExactVersion,
        plan: &InstallPlan,
        journal: &mut OperationJournal,
    ) -> TorbenResult<InstallRecord> {
        let distribution = self.validate_install_plan(plan, app_id, version)?;
        let cancellation = journal.cancellation_probe();
        cancellation.check()?;
        let download_dir = paths.download_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&download_dir).map_err(io_error)?;
        let installer_path = download_dir.join(&distribution.installer_name);
        journal.record(
            OperationState::Running,
            "download",
            format!("Downloading PostgreSQL {version}"),
            Some(0.2),
        )?;
        if !installer_path.is_file()
            || sha256_file_checked(&installer_path, Some(&cancellation))? != distribution.checksum
        {
            self.download(
                &distribution.installer_url,
                &installer_path,
                &cancellation,
                journal,
            )
            .await?;
        }
        let actual = sha256_file_checked(&installer_path, Some(&cancellation))?;
        if actual != distribution.checksum {
            return Err(TorbenError::new(
                "archive_hash_mismatch",
                "The PostgreSQL installer does not match the pinned WinGet SHA-256 checksum.",
            )
            .with_detail("expected", distribution.checksum)
            .with_detail("actual", actual));
        }
        let staging =
            paths
                .staging_dir()
                .join(format!("install-{}-{}", app_id, journal.operation_id()));
        std::fs::create_dir_all(&staging).map_err(io_error)?;
        journal.record(
            OperationState::Running,
            "extract",
            "Extracting PostgreSQL binaries into staging",
            Some(0.6),
        )?;
        let extracted = extract_windows_installer(&installer_path, &staging, &cancellation).await?;
        self.health_check_path(&extracted, version)?;
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            return Err(TorbenError::new(
                "install_path_exists",
                "The PostgreSQL version is already installed.",
            ));
        }
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        std::fs::rename(&extracted, &final_path).map_err(io_error)?;
        let _ = std::fs::remove_dir_all(staging);
        Ok(InstallRecord {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: plan.source_id.clone(),
            scope: InstallScope::Managed,
            install_path: final_path.display().to_string(),
            installed_at: timestamp(),
            health: "healthy".to_owned(),
        })
    }

    pub fn health_check(&self, record: &InstallRecord) -> TorbenResult<()> {
        self.health_check_path(Path::new(&record.install_path), &record.version)
    }

    pub fn command_path(&self, install_path: &Path, command: &str) -> TorbenResult<PathBuf> {
        if !POSTGRESQL_COMMANDS.contains(&command) {
            return Err(TorbenError::new(
                "unsupported_command",
                "PostgreSQL does not expose this command.",
            )
            .with_detail("command", command));
        }
        let path = install_path.join("bin").join(format!("{command}.exe"));
        if path.is_file() {
            Ok(path)
        } else {
            Err(TorbenError::new(
                "managed_command_missing",
                "A managed PostgreSQL command is missing.",
            )
            .with_detail("path", path.display().to_string()))
        }
    }

    pub fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let mut records = Vec::new();
        let mut seen = BTreeSet::new();
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            let candidate = directory.join("postgres.exe");
            let Ok(canonical) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) || !seen.insert(canonical.clone()) {
                continue;
            }
            let Ok(output) = std::process::Command::new(&canonical)
                .arg("--version")
                .output()
            else {
                continue;
            };
            let Some(version) = parse_postgresql_version(&String::from_utf8_lossy(&output.stdout))
            else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("postgresql")?,
                version,
                source_id: SourceId::new("postgresql.external")?,
                scope: InstallScope::External,
                install_path: canonical
                    .parent()
                    .and_then(Path::parent)
                    .unwrap_or(&canonical)
                    .display()
                    .to_string(),
                installed_at: String::new(),
                health: "healthy".to_owned(),
            });
        }
        Ok(records)
    }

    fn validate_install_plan(
        &self,
        plan: &InstallPlan,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<PostgresqlDistribution> {
        if app_id.as_str() != "postgresql"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new(POSTGRESQL_SOURCE_ID)?
        {
            return Err(invalid_plan("identity or source owner"));
        }
        let expected = self.distribution(version)?;
        let [
            InstallStep::Download {
                url,
                destination_name,
            },
            InstallStep::VerifySha256 {
                archive_name,
                expected: checksum,
            },
            InstallStep::ExtractArchive {
                archive_name: extracted,
                strip_components,
            },
            InstallStep::HealthCheck {
                executable,
                arguments,
                expected_output,
            },
            InstallStep::CreateShims { commands },
        ] = plan.steps.as_slice()
        else {
            return Err(invalid_plan("step order or shape"));
        };
        if Url::parse(url).ok().as_ref() != Some(&expected.installer_url)
            || destination_name != &expected.installer_name
            || archive_name != &expected.installer_name
            || extracted != &expected.installer_name
            || checksum != &expected.checksum
            || *strip_components != 0
            || executable != "postgres"
            || arguments.as_slice() != ["--version"]
            || expected_output != &format!("postgres (PostgreSQL) {}", expected.release_version)
            || commands
                .iter()
                .map(String::as_str)
                .ne(POSTGRESQL_COMMANDS.iter().copied())
        {
            return Err(invalid_plan("EDB distribution details"));
        }
        Ok(expected)
    }

    fn health_check_path(&self, install_path: &Path, version: &ExactVersion) -> TorbenResult<()> {
        for command in ["postgres", "psql"] {
            let executable = self.command_path(install_path, command)?;
            let output = std::process::Command::new(executable)
                .arg("--version")
                .output()
                .map_err(io_error)?;
            let actual = parse_postgresql_version(&String::from_utf8_lossy(&output.stdout))
                .ok_or_else(|| {
                    TorbenError::new(
                        "health_check_output_invalid",
                        format!("{command} returned invalid version output."),
                    )
                })?;
            if actual != *version {
                return Err(TorbenError::new(
                    "health_check_version_mismatch",
                    "The PostgreSQL version does not match the requested version.",
                ));
            }
        }
        for command in POSTGRESQL_COMMANDS {
            let _ = self.command_path(install_path, command)?;
        }
        Ok(())
    }

    async fn download(
        &self,
        url: &Url,
        destination: &Path,
        cancellation: &CancellationProbe,
        journal: &mut OperationJournal,
    ) -> TorbenResult<()> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(network_error)?;
        if response.url().host_str() != Some("get.enterprisedb.com") {
            return Err(unexpected_origin());
        }
        let response = response.error_for_status().map_err(network_error)?;
        let content_length = response.content_length();
        if content_length.is_some_and(|size| size > MAX_INSTALLER_BYTES) {
            return Err(installer_too_large());
        }
        let partial = destination.with_extension("partial");
        let mut file = tokio::fs::File::create(&partial).await.map_err(io_error)?;
        let mut stream = response.bytes_stream();
        let mut total = 0u64;
        let mut last_progress = 0.2_f32;
        let mut last_reported_bytes = 0u64;
        while let Some(chunk) = stream.next().await {
            cancellation.check()?;
            let chunk = chunk.map_err(network_error)?;
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_INSTALLER_BYTES {
                return Err(installer_too_large());
            }
            file.write_all(&chunk).await.map_err(io_error)?;
            if let Some(length) = content_length.filter(|length| *length > 0) {
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let fraction = (total as f64 / length as f64).clamp(0.0, 1.0);
                #[allow(clippy::cast_possible_truncation)]
                let progress = (0.2 + fraction * 0.35) as f32;
                if progress - last_progress >= 0.01 || progress >= 0.55 {
                    journal.record(
                        OperationState::Running,
                        "download",
                        format!("Downloading PostgreSQL ({total}/{length} bytes)"),
                        Some(progress),
                    )?;
                    last_progress = progress;
                }
            } else if total >= last_reported_bytes.saturating_add(4 * 1024 * 1024) {
                // Some EDB edge nodes omit Content-Length. Keep the UI alive
                // with bounded, monotonic progress until the stream ends.
                #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                let progress =
                    (0.2 + (total as f64 / MAX_INSTALLER_BYTES as f64) * 0.35).min(0.54) as f32;
                if progress > last_progress {
                    journal.record(
                        OperationState::Running,
                        "download",
                        format!("Downloading PostgreSQL ({total} bytes)"),
                        Some(progress),
                    )?;
                    last_progress = progress;
                    last_reported_bytes = total;
                }
            }
        }
        file.flush().await.map_err(io_error)?;
        file.sync_all().await.map_err(io_error)?;
        std::fs::rename(partial, destination).map_err(io_error)
    }
}

pub(crate) fn configure_command_environment(
    command: &mut std::process::Command,
    data_root: &Path,
    bin: &Path,
) -> TorbenResult<()> {
    let client_root = data_root.join("client");
    let config_root = client_root.join("config");
    for directory in [&config_root, &data_root.join("instances")] {
        std::fs::create_dir_all(directory).map_err(io_error)?;
    }
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        [bin.to_path_buf()]
            .into_iter()
            .chain(std::env::split_paths(&inherited)),
    )
    .map_err(|error| {
        TorbenError::new(
            "postgresql_environment_failed",
            "Could not prepare the managed PostgreSQL PATH.",
        )
        .with_detail("reason", error.to_string())
    })?;
    command.env("PGPASSFILE", client_root.join("pgpass.conf"));
    command.env("PGSERVICEFILE", client_root.join("pg_service.conf"));
    command.env("PGSYSCONFDIR", config_root);
    command.env("PATH", path);
    Ok(())
}

async fn extract_windows_installer(
    installer: &Path,
    staging: &Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<PathBuf> {
    if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        return Err(unsupported_platform());
    }
    cancellation.check()?;
    let extracted = staging.join("edb-extracted");
    std::fs::create_dir_all(&extracted).map_err(io_error)?;
    let mut child = crate::process::async_command(installer)
        .args([
            std::ffi::OsString::from("--mode"),
            std::ffi::OsString::from("unattended"),
            std::ffi::OsString::from("--unattendedmodeui"),
            std::ffi::OsString::from("none"),
            std::ffi::OsString::from("--extract-only"),
            std::ffi::OsString::from("yes"),
            std::ffi::OsString::from("--install_runtimes"),
            std::ffi::OsString::from("no"),
            std::ffi::OsString::from("--prefix"),
            extracted.as_os_str().to_owned(),
        ])
        .kill_on_drop(true)
        .spawn()
        .map_err(io_error)?;
    loop {
        cancellation.check()?;
        if let Some(status) = child.try_wait().map_err(io_error)? {
            if !status.success() {
                return Err(TorbenError::new(
                    "postgresql_installer_extract_failed",
                    "The EDB installer could not extract the PostgreSQL binaries.",
                )
                .with_detail("status", status.to_string()));
            }
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    find_postgresql_prefix(&extracted)
}

fn find_postgresql_prefix(root: &Path) -> TorbenResult<PathBuf> {
    let mut prefixes = BTreeSet::new();
    for entry in walkdir::WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| {
            TorbenError::new(
                "postgresql_installer_layout_invalid",
                "Could not inspect the extracted PostgreSQL binaries.",
            )
            .with_detail("reason", error.to_string())
        })?;
        if entry.file_type().is_file()
            && entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.eq_ignore_ascii_case("postgres.exe"))
            && entry
                .path()
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
            && let Some(prefix) = entry.path().parent().and_then(Path::parent)
        {
            prefixes.insert(prefix.to_path_buf());
        }
    }
    if prefixes.len() != 1 {
        return Err(TorbenError::new(
            "postgresql_installer_layout_invalid",
            "The EDB installer did not contain one PostgreSQL binary prefix.",
        )
        .with_detail("prefixCount", prefixes.len().to_string()));
    }
    Ok(prefixes.pop_first().expect("one PostgreSQL prefix"))
}

fn distribution_spec(version: &ExactVersion) -> Option<&'static DistributionSpec> {
    DISTRIBUTIONS
        .iter()
        .find(|spec| spec.version == version.to_string())
}

fn parse_postgresql_version(output: &str) -> Option<ExactVersion> {
    output.split_whitespace().find_map(|token| {
        let token = token.trim_end_matches(',');
        ExactVersion::from_str(token).ok().or_else(|| {
            let parts = token.split('.').collect::<Vec<_>>();
            (parts.len() == 2 && parts.iter().all(|part| part.parse::<u64>().is_ok()))
                .then(|| ExactVersion::from_str(&format!("{token}.0")).ok())
                .flatten()
        })
    })
}

fn unsupported_platform() -> TorbenError {
    TorbenError::new(
        "postgresql_platform_unsupported",
        "Managed PostgreSQL currently supports Windows x64 only.",
    )
    .with_detail("expected_target", POSTGRESQL_TARGET)
}

fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested PostgreSQL version was not found.",
    )
    .with_detail("requested", requested)
}

fn invalid_plan(field: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The PostgreSQL plugin returned an invalid installation plan.",
    )
    .with_detail("field", field)
}

fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new(
        "postgresql_network_error",
        "A PostgreSQL installer request failed.",
    )
    .with_detail("reason", error.to_string())
}

fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new(
        "postgresql_url_invalid",
        "The PostgreSQL distribution URL is invalid.",
    )
    .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "postgresql_io_failed",
        "A PostgreSQL managed filesystem operation failed.",
    )
    .with_detail("reason", error.to_string())
}

fn unexpected_origin() -> TorbenError {
    TorbenError::new(
        "unexpected_download_origin",
        "PostgreSQL redirected outside the reviewed EDB download origin.",
    )
}

fn installer_too_large() -> TorbenError {
    TorbenError::new(
        "postgresql_installer_too_large",
        "The PostgreSQL installer is too large.",
    )
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| String::new(), |value| value.as_secs().to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        PostgresqlProvider, configure_command_environment, find_postgresql_prefix,
        parse_postgresql_version,
    };
    use std::{ffi::OsStr, str::FromStr};
    use tempfile::tempdir;
    use torben_contracts::ExactVersion;

    #[test]
    fn builds_pinned_edb_distributions_for_core_versions() {
        let provider = PostgresqlProvider::official().unwrap();
        let current = provider
            .distribution(&ExactVersion::from_str("18.6.0").unwrap())
            .unwrap();
        assert_eq!(current.installer_name, "postgresql-18.6-3-windows-x64.exe");
        assert_eq!(
            current.installer_url.as_str(),
            "https://get.enterprisedb.com/postgresql/postgresql-18.6-3-windows-x64.exe"
        );
        assert_eq!(
            current.checksum,
            "3bb55a421849fa5749fe807e45b05a9a7758a16389591ee0a41b7fcabf724b90"
        );

        let previous = provider
            .distribution(&ExactVersion::from_str("17.11.0").unwrap())
            .unwrap();
        assert_eq!(
            previous.installer_name,
            "postgresql-17.11-3-windows-x64.exe"
        );
        assert_eq!(
            previous.installer_url.host_str(),
            Some("get.enterprisedb.com")
        );
        assert_eq!(previous.checksum.len(), 64);
    }

    #[test]
    fn parses_postgres_and_psql_version_output() {
        let version = parse_postgresql_version("postgres (PostgreSQL) 18.6").unwrap();
        assert_eq!(version, ExactVersion::from_str("18.6.0").unwrap());
        let version = parse_postgresql_version("psql (PostgreSQL) 17.11").unwrap();
        assert_eq!(version, ExactVersion::from_str("17.11.0").unwrap());
        assert!(parse_postgresql_version("postgres: unknown option").is_none());
    }

    #[test]
    fn managed_commands_keep_configuration_below_postgresql_data_root_without_pgdata() {
        let root = tempdir().unwrap();
        let data_root = root
            .path()
            .join("userData")
            .join("application-data/postgresql");
        let bin = root.path().join("apps").join("postgresql").join("bin");
        let mut command = std::process::Command::new("psql");

        configure_command_environment(&mut command, &data_root, &bin).unwrap();

        let environment = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            environment
                .get(OsStr::new("PGPASSFILE"))
                .unwrap()
                .as_deref(),
            Some(data_root.join("client").join("pgpass.conf").as_os_str())
        );
        assert_eq!(
            environment
                .get(OsStr::new("PGSERVICEFILE"))
                .unwrap()
                .as_deref(),
            Some(data_root.join("client").join("pg_service.conf").as_os_str())
        );
        assert_eq!(
            environment
                .get(OsStr::new("PGSYSCONFDIR"))
                .unwrap()
                .as_deref(),
            Some(data_root.join("client").join("config").as_os_str())
        );
        assert!(!environment.contains_key(OsStr::new("PGDATA")));
        assert!(data_root.join("client").join("config").is_dir());
        assert!(data_root.join("instances").is_dir());
    }

    #[test]
    fn extracted_layout_requires_one_postgresql_prefix() {
        let root = tempdir().unwrap();
        let prefix = root.path().join("pgsql");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        std::fs::write(prefix.join("bin").join("postgres.exe"), b"fixture").unwrap();

        assert_eq!(find_postgresql_prefix(root.path()).unwrap(), prefix);
    }
}
