use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
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
    node::{ArchiveKind, extract_archive, sha256_file_checked},
    operation::{CancellationProbe, OperationJournal},
};

const MYSQL_SOURCE_ID: &str = "mysql.official";
const MYSQL_BASE_URL: &str = "https://cdn.mysql.com/Downloads/";
const MYSQL_TARGET: &str = "x86_64-pc-windows-msvc";
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const DOWNLOAD_ATTEMPTS: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DistributionSpec {
    version: &'static str,
    stream: &'static str,
    sha256: &'static str,
    released_at: &'static str,
    lts: bool,
}

const DISTRIBUTIONS: &[DistributionSpec] = &[
    DistributionSpec {
        version: "8.4.11",
        stream: "MySQL-8.4",
        sha256: "a492371d687d2bab088b0062581144a0044b8964baefdf4faa579292b423d25c",
        released_at: "2026-07-28",
        lts: true,
    },
    DistributionSpec {
        version: "8.0.46",
        stream: "MySQL-8.0",
        sha256: "28e9eda019d88eff4478d811ea2110b83f02a3966be157fe91cc55def3ab0d4d",
        released_at: "2026-04-22",
        lts: false,
    },
    DistributionSpec {
        version: "5.7.44",
        stream: "MySQL-5.7",
        sha256: "aed661fe8120254a1dc30f5a4d5de346681922f4847cf025e2d4084eca78e70e",
        released_at: "2023-10-25",
        lts: false,
    },
];

#[derive(Debug, Clone)]
pub struct MysqlProvider {
    client: reqwest::Client,
    base_url: Url,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MysqlDistribution {
    pub archive_name: String,
    pub archive_url: Url,
    pub checksum: String,
}

impl MysqlProvider {
    pub fn official() -> TorbenResult<Self> {
        let client = crate::download::client_builder()
            .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            base_url: Url::parse(MYSQL_BASE_URL).map_err(url_error)?,
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
                    lts_name: spec.lts.then(|| "MySQL LTS".to_owned()),
                    released_at: spec.released_at.to_owned(),
                    recommended: spec.lts,
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
            "stable" | "lts" | "latest"
        ) {
            return ExactVersion::from_str(DISTRIBUTIONS[0].version);
        }
        Err(version_not_found(requested))
    }

    pub fn distribution(&self, version: &ExactVersion) -> TorbenResult<MysqlDistribution> {
        let spec =
            distribution_spec(version).ok_or_else(|| version_not_found(&version.to_string()))?;
        let archive_name = format!("mysql-{version}-winx64.zip");
        let archive_url = self
            .base_url
            .join(&format!("{}/{archive_name}", spec.stream))
            .map_err(url_error)?;
        Ok(MysqlDistribution {
            archive_name,
            archive_url,
            checksum: spec.sha256.to_owned(),
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
        let archive_path = download_dir.join(&distribution.archive_name);
        journal.record(
            OperationState::Running,
            "download",
            format!("Downloading MySQL {version}"),
            Some(0.2),
        )?;
        if !archive_path.is_file()
            || sha256_file_checked(&archive_path, Some(&cancellation))? != distribution.checksum
        {
            crate::download::verified(
                &distribution.archive_url,
                &archive_path,
                &distribution.checksum,
                Some(&cancellation),
                |url| {
                    let official = &distribution.archive_url;
                    let destination = &archive_path;
                    let cancellation = &cancellation;
                    async move {
                        self.download(&url, official, destination, cancellation)
                            .await
                    }
                },
            )
            .await?;
        }
        let actual = sha256_file_checked(&archive_path, Some(&cancellation))?;
        if actual != distribution.checksum {
            return Err(TorbenError::new(
                "archive_hash_mismatch",
                "The MySQL archive does not match the official SHA-256 checksum.",
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
            "Extracting the MySQL server into staging",
            Some(0.6),
        )?;
        let archive_for_task = archive_path.clone();
        let staging_for_task = staging.clone();
        let extracted = tokio::task::spawn_blocking(move || {
            extract_archive(
                &archive_for_task,
                ArchiveKind::Zip,
                &staging_for_task,
                &cancellation,
            )
        })
        .await
        .map_err(|error| {
            TorbenError::new("archive_task_failed", "MySQL archive extraction failed.")
                .with_detail("reason", error.to_string())
        })??;
        self.health_check_path(&extracted, version)?;
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            return Err(TorbenError::new(
                "install_path_exists",
                "The MySQL version is already installed.",
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
        if !matches!(command, "mysql" | "mysqld" | "mysqladmin" | "mysqldump") {
            return Err(TorbenError::new(
                "unsupported_command",
                "MySQL does not expose this command.",
            )
            .with_detail("command", command));
        }
        let path = install_path.join("bin").join(format!("{command}.exe"));
        if path.is_file() {
            Ok(path)
        } else {
            Err(TorbenError::new(
                "managed_command_missing",
                "A managed MySQL command is missing.",
            )
            .with_detail("path", path.display().to_string()))
        }
    }

    pub fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let mut records = Vec::new();
        let mut seen = BTreeSet::new();
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            let candidate = directory.join("mysqld.exe");
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
            let Some(version) = parse_mysql_version(&String::from_utf8_lossy(&output.stdout))
            else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("mysql")?,
                version,
                source_id: SourceId::new("mysql.external")?,
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
    ) -> TorbenResult<MysqlDistribution> {
        if app_id.as_str() != "mysql"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new(MYSQL_SOURCE_ID)?
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
        if Url::parse(url).ok().as_ref() != Some(&expected.archive_url)
            || destination_name != &expected.archive_name
            || archive_name != &expected.archive_name
            || extracted != &expected.archive_name
            || checksum != &expected.checksum
            || *strip_components != 0
            || executable != "mysqld"
            || arguments.as_slice() != ["--version"]
            || !expected_output.starts_with(&format!("mysqld Ver {version}"))
            || commands.as_slice() != ["mysql", "mysqld", "mysqladmin", "mysqldump"]
        {
            return Err(invalid_plan("official distribution details"));
        }
        Ok(expected)
    }

    fn health_check_path(&self, install_path: &Path, version: &ExactVersion) -> TorbenResult<()> {
        let mysqld = self.command_path(install_path, "mysqld")?;
        let output = std::process::Command::new(mysqld)
            .arg("--version")
            .output()
            .map_err(io_error)?;
        let actual =
            parse_mysql_version(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
                TorbenError::new(
                    "health_check_output_invalid",
                    "mysqld returned invalid version output.",
                )
            })?;
        if actual != *version {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The MySQL version does not match the requested version.",
            ));
        }
        for command in ["mysql", "mysqladmin", "mysqldump"] {
            let _ = self.command_path(install_path, command)?;
        }
        Ok(())
    }

    async fn download(
        &self,
        url: &Url,
        official: &Url,
        destination: &Path,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<()> {
        let partial = destination.with_extension("partial");
        for attempt in 0..DOWNLOAD_ATTEMPTS {
            cancellation.check()?;
            let offset = partial_file_size(&partial)?;
            let mut request = self.client.get(url.clone());
            if offset > 0 {
                request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(_error) if attempt + 1 < DOWNLOAD_ATTEMPTS => {
                    retry_download(attempt).await;
                    continue;
                }
                Err(error) => return Err(network_error(error)),
            };
            if !crate::download::mirror_response(official, url, response.url())
                && (response.url().scheme() != official.scheme()
                    || response.url().host_str() != official.host_str()
                    || response.url().port_or_known_default() != official.port_or_known_default())
            {
                return Err(unexpected_origin());
            }
            if (response.status().is_server_error()
                || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                || response.status() == reqwest::StatusCode::REQUEST_TIMEOUT)
                && attempt + 1 < DOWNLOAD_ATTEMPTS
            {
                retry_download(attempt).await;
                continue;
            }
            let response = response.error_for_status().map_err(network_error)?;
            let append = offset > 0 && response.status() == reqwest::StatusCode::PARTIAL_CONTENT;
            let base = if append { offset } else { 0 };
            if response
                .content_length()
                .is_some_and(|size| size.saturating_add(base) > MAX_ARCHIVE_BYTES)
            {
                return Err(archive_too_large());
            }
            let mut file = crate::download::open_partial(&partial, append).map_err(io_error)?;
            let mut stream = response.bytes_stream();
            let mut total = base;
            let mut transfer = crate::download::TransferRate::new();
            let mut stream_error = None;
            while let Some(chunk) = stream.next().await {
                cancellation.check()?;
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        stream_error = Some(network_error(error));
                        break;
                    }
                };
                transfer.record(chunk.len(), "mysql_network_error")?;
                total = total.saturating_add(chunk.len() as u64);
                if total > MAX_ARCHIVE_BYTES {
                    return Err(archive_too_large());
                }
                file.write_all(&chunk).await.map_err(io_error)?;
            }
            file.flush().await.map_err(io_error)?;
            file.sync_all().await.map_err(io_error)?;
            drop(file);
            if let Some(error) = stream_error {
                if attempt + 1 < DOWNLOAD_ATTEMPTS {
                    retry_download(attempt).await;
                    continue;
                }
                return Err(error);
            }
            if destination.exists() {
                tokio::fs::remove_file(destination)
                    .await
                    .map_err(io_error)?;
            }
            std::fs::rename(&partial, destination).map_err(io_error)?;
            return Ok(());
        }
        Err(TorbenError::new(
            "mysql_network_error",
            "A MySQL archive request failed after retries.",
        ))
    }
}

fn partial_file_size(path: &Path) -> TorbenResult<u64> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            Ok(metadata.len())
        }
        Ok(_) => Err(io_error(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "The MySQL partial download is not a regular file.",
        ))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(io_error(error)),
    }
}

async fn retry_download(attempt: usize) {
    tokio::time::sleep(Duration::from_secs((attempt + 1) as u64)).await;
}

pub(crate) fn configure_command_environment(
    command: &mut std::process::Command,
    data_root: &Path,
    bin: &Path,
) -> TorbenResult<()> {
    let client_root = data_root.join("client");
    let config_root = data_root.join("config");
    for directory in [&client_root, &config_root, &data_root.join("instances")] {
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
            "mysql_environment_failed",
            "Could not prepare the managed MySQL PATH.",
        )
        .with_detail("reason", error.to_string())
    })?;
    command.env("MYSQL_HOME", config_root);
    command.env("MYSQL_HISTFILE", client_root.join("history"));
    command.env("PATH", path);
    Ok(())
}

fn distribution_spec(version: &ExactVersion) -> Option<&'static DistributionSpec> {
    DISTRIBUTIONS
        .iter()
        .find(|spec| spec.version == version.to_string())
}
fn parse_mysql_version(output: &str) -> Option<ExactVersion> {
    output.split_whitespace().find_map(|token| {
        let token = token.trim_start_matches('v').trim_end_matches(',');
        ExactVersion::from_str(token).ok()
    })
}
fn unsupported_platform() -> TorbenError {
    TorbenError::new(
        "mysql_platform_unsupported",
        "Managed MySQL currently supports Windows x64 only.",
    )
    .with_detail("expected_target", MYSQL_TARGET)
}
fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested MySQL version was not found.",
    )
    .with_detail("requested", requested)
}
fn invalid_plan(field: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The MySQL plugin returned an invalid installation plan.",
    )
    .with_detail("field", field)
}
fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new("mysql_network_error", "A MySQL archive request failed.")
        .with_detail("reason", error.to_string())
}
fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new(
        "mysql_url_invalid",
        "The MySQL distribution URL is invalid.",
    )
    .with_detail("reason", error.to_string())
}
fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "mysql_io_failed",
        "A MySQL managed filesystem operation failed.",
    )
    .with_detail("reason", error.to_string())
}
fn unexpected_origin() -> TorbenError {
    TorbenError::new(
        "unexpected_download_origin",
        "MySQL redirected outside the official CDN origin.",
    )
}
fn archive_too_large() -> TorbenError {
    TorbenError::new("mysql_archive_too_large", "The MySQL archive is too large.")
}
fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| String::new(), |value| value.as_secs().to_string())
}

#[cfg(test)]
mod tests {
    use super::{MysqlProvider, configure_command_environment, parse_mysql_version};
    use std::{ffi::OsStr, str::FromStr};
    use tempfile::tempdir;
    use torben_contracts::ExactVersion;

    #[test]
    fn builds_pinned_official_distribution_for_core_versions() {
        let provider = MysqlProvider::official().unwrap();
        let lts = provider
            .distribution(&ExactVersion::from_str("8.4.11").unwrap())
            .unwrap();
        assert_eq!(lts.archive_name, "mysql-8.4.11-winx64.zip");
        assert_eq!(
            lts.archive_url.as_str(),
            "https://cdn.mysql.com/Downloads/MySQL-8.4/mysql-8.4.11-winx64.zip"
        );
        assert_eq!(lts.checksum.len(), 64);

        let legacy = provider
            .distribution(&ExactVersion::from_str("8.0.46").unwrap())
            .unwrap();
        assert_eq!(legacy.archive_name, "mysql-8.0.46-winx64.zip");
        assert_eq!(legacy.archive_url.host_str(), Some("cdn.mysql.com"));
        assert_eq!(legacy.checksum.len(), 64);

        let mysql57 = provider
            .distribution(&ExactVersion::from_str("5.7.44").unwrap())
            .unwrap();
        assert_eq!(mysql57.archive_name, "mysql-5.7.44-winx64.zip");
        assert_eq!(
            mysql57.archive_url.as_str(),
            "https://cdn.mysql.com/Downloads/MySQL-5.7/mysql-5.7.44-winx64.zip"
        );
        assert_eq!(mysql57.checksum.len(), 64);
    }

    #[test]
    fn parses_mysqld_version_output() {
        let version = parse_mysql_version(
            "C:\\mysql\\bin\\mysqld.exe  Ver 8.4.11 for Win64 on x86_64 (MySQL Community Server - GPL)",
        )
        .unwrap();
        assert_eq!(version, ExactVersion::from_str("8.4.11").unwrap());
        assert!(parse_mysql_version("mysqld: unknown option").is_none());
    }

    #[test]
    fn managed_commands_separate_mysql_client_state_from_instances() {
        let root = tempdir().unwrap();
        let data_root = root.path().join("userData/application-data/mysql");
        let bin = root.path().join("apps/mysql/8.4.11/bin");
        let mut command = std::process::Command::new("mysql");

        configure_command_environment(&mut command, &data_root, &bin).unwrap();

        let environment = command
            .get_envs()
            .map(|(key, value)| (key.to_owned(), value.map(OsStr::to_owned)))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(
            environment
                .get(OsStr::new("MYSQL_HOME"))
                .unwrap()
                .as_deref(),
            Some(data_root.join("config").as_os_str())
        );
        assert_eq!(
            environment
                .get(OsStr::new("MYSQL_HISTFILE"))
                .unwrap()
                .as_deref(),
            Some(data_root.join("client").join("history").as_os_str())
        );
        assert!(data_root.join("instances").is_dir());
    }
}
