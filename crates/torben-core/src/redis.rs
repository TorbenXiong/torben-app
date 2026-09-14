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
    node::{ArchiveKind, extract_archive_contents, sha256_file_checked},
    operation::{CancellationProbe, OperationJournal},
};

const REDIS_SOURCE_ID: &str = "redis.windows";
const REDIS_BASE_URL: &str = "https://github.com/redis-windows/redis-windows/releases/download/";
const REDIS_TARGET: &str = "x86_64-pc-windows-msvc";
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

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
        version: "8.8.0",
        stream: "8.8.0",
        sha256: "8af6fd6c4aac3e13ded36f249da8114b3be32df60ab589da7c3513aa8b1a86cd",
        released_at: "2026-05-26",
        lts: true,
    },
    DistributionSpec {
        version: "7.4.9",
        stream: "7.4.9",
        sha256: "98af6511ca35601cc8d8200a92318e00f9d2d5425a9f7f3e8f699d3bdd59dcf6",
        released_at: "2026-05-06",
        lts: false,
    },
];

#[derive(Debug, Clone)]
pub struct RedisProvider {
    client: reqwest::Client,
    base_url: Url,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RedisDistribution {
    pub archive_name: String,
    pub archive_url: Url,
    pub checksum: String,
}

impl RedisProvider {
    pub fn windows_community() -> TorbenResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            base_url: Url::parse(REDIS_BASE_URL).map_err(url_error)?,
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
                    lts_name: None,
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

    pub fn distribution(&self, version: &ExactVersion) -> TorbenResult<RedisDistribution> {
        let spec =
            distribution_spec(version).ok_or_else(|| version_not_found(&version.to_string()))?;
        let archive_name = format!("Redis-{version}-Windows-x64-msys2.zip");
        let archive_url = self
            .base_url
            .join(&format!("{}/{archive_name}", spec.stream))
            .map_err(url_error)?;
        Ok(RedisDistribution {
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
            format!("Downloading Redis {version}"),
            Some(0.2),
        )?;
        if !archive_path.is_file()
            || sha256_file_checked(&archive_path, Some(&cancellation))? != distribution.checksum
        {
            self.download(&distribution.archive_url, &archive_path, &cancellation)
                .await?;
        }
        let actual = sha256_file_checked(&archive_path, Some(&cancellation))?;
        if actual != distribution.checksum {
            return Err(TorbenError::new(
                "archive_hash_mismatch",
                "The Redis archive does not match the pinned Redis for Windows SHA-256 checksum.",
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
            "Extracting the Redis server into staging",
            Some(0.6),
        )?;
        let archive_for_task = archive_path.clone();
        let staging_for_task = staging.clone();
        let extracted = tokio::task::spawn_blocking(move || {
            extract_archive_contents(
                &archive_for_task,
                ArchiveKind::Zip,
                &staging_for_task,
                &cancellation,
            )
            .map(|()| staging_for_task)
        })
        .await
        .map_err(|error| {
            TorbenError::new("archive_task_failed", "Redis archive extraction failed.")
                .with_detail("reason", error.to_string())
        })??;
        self.health_check_path(&extracted, version)?;
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            return Err(TorbenError::new(
                "install_path_exists",
                "The Redis version is already installed.",
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
        if !matches!(command, "redis-server" | "redis-cli" | "redis-benchmark") {
            return Err(TorbenError::new(
                "unsupported_command",
                "Redis does not expose this command.",
            )
            .with_detail("command", command));
        }
        let filename = format!("{command}.exe");
        let path = [
            install_path.join("bin").join(&filename),
            install_path.join(&filename),
        ]
        .into_iter()
        .find(|path| path.is_file());
        if let Some(path) = path {
            Ok(path)
        } else {
            Err(TorbenError::new(
                "managed_command_missing",
                "A managed Redis command is missing.",
            )
            .with_detail("path", install_path.display().to_string()))
        }
    }

    pub fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let mut records = Vec::new();
        let mut seen = BTreeSet::new();
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            let candidate = directory.join("redis-server.exe");
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
            let Some(version) = parse_redis_version(&String::from_utf8_lossy(&output.stdout))
            else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("redis")?,
                version,
                source_id: SourceId::new("redis.external")?,
                scope: InstallScope::External,
                install_path: canonical
                    .parent()
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
    ) -> TorbenResult<RedisDistribution> {
        if app_id.as_str() != "redis"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new(REDIS_SOURCE_ID)?
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
            || executable != "redis-server"
            || arguments.as_slice() != ["--version"]
            || !expected_output.starts_with(&format!("Redis server v={version}"))
            || commands.as_slice() != ["redis-server", "redis-cli", "redis-benchmark"]
        {
            return Err(invalid_plan("pinned Windows distribution details"));
        }
        Ok(expected)
    }

    fn health_check_path(&self, install_path: &Path, version: &ExactVersion) -> TorbenResult<()> {
        let redis_server = self.command_path(install_path, "redis-server")?;
        let output = std::process::Command::new(redis_server)
            .arg("--version")
            .output()
            .map_err(io_error)?;
        let actual =
            parse_redis_version(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
                TorbenError::new(
                    "health_check_output_invalid",
                    "redis-server returned invalid version output.",
                )
            })?;
        if actual != *version {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The Redis version does not match the requested version.",
            ));
        }
        for command in ["redis-cli", "redis-benchmark"] {
            let _ = self.command_path(install_path, command)?;
        }
        Ok(())
    }

    async fn download(
        &self,
        url: &Url,
        destination: &Path,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<()> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(network_error)?;
        if !matches!(
            response.url().host_str(),
            Some(
                "github.com"
                    | "objects.githubusercontent.com"
                    | "release-assets.githubusercontent.com"
            )
        ) {
            return Err(unexpected_origin());
        }
        let response = response.error_for_status().map_err(network_error)?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_ARCHIVE_BYTES)
        {
            return Err(archive_too_large());
        }
        let partial = destination.with_extension("partial");
        let mut file = tokio::fs::File::create(&partial).await.map_err(io_error)?;
        let mut stream = response.bytes_stream();
        let mut total = 0u64;
        while let Some(chunk) = stream.next().await {
            cancellation.check()?;
            let chunk = chunk.map_err(network_error)?;
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_ARCHIVE_BYTES {
                return Err(archive_too_large());
            }
            file.write_all(&chunk).await.map_err(io_error)?;
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
    std::fs::create_dir_all(data_root).map_err(io_error)?;
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        [bin.to_path_buf()]
            .into_iter()
            .chain(std::env::split_paths(&inherited)),
    )
    .map_err(|error| {
        TorbenError::new(
            "redis_environment_failed",
            "Could not prepare the managed Redis PATH.",
        )
        .with_detail("reason", error.to_string())
    })?;
    let redis_home = if bin
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
    {
        bin.parent().unwrap_or(bin)
    } else {
        bin
    };
    command.env("REDIS_HOME", redis_home);
    command.env("REDISCLI_HISTFILE", data_root.join("redis_history"));
    command.env("PATH", path);
    command.current_dir(data_root);
    Ok(())
}

fn distribution_spec(version: &ExactVersion) -> Option<&'static DistributionSpec> {
    DISTRIBUTIONS
        .iter()
        .find(|spec| spec.version == version.to_string())
}
fn parse_redis_version(output: &str) -> Option<ExactVersion> {
    output.split_whitespace().find_map(|token| {
        let token = token
            .trim_start_matches("v=")
            .trim_start_matches('v')
            .trim_end_matches(',');
        ExactVersion::from_str(token).ok()
    })
}
fn unsupported_platform() -> TorbenError {
    TorbenError::new(
        "redis_platform_unsupported",
        "Managed Redis currently supports Windows x64 only.",
    )
    .with_detail("expected_target", REDIS_TARGET)
}
fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested Redis version was not found.",
    )
    .with_detail("requested", requested)
}
fn invalid_plan(field: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The Redis plugin returned an invalid installation plan.",
    )
    .with_detail("field", field)
}
fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new("redis_network_error", "A Redis archive request failed.")
        .with_detail("reason", error.to_string())
}
fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new(
        "redis_url_invalid",
        "The Redis distribution URL is invalid.",
    )
    .with_detail("reason", error.to_string())
}
fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "redis_io_failed",
        "A Redis managed filesystem operation failed.",
    )
    .with_detail("reason", error.to_string())
}
fn unexpected_origin() -> TorbenError {
    TorbenError::new(
        "unexpected_download_origin",
        "Redis redirected outside the approved GitHub release origins.",
    )
}
fn archive_too_large() -> TorbenError {
    TorbenError::new("redis_archive_too_large", "The Redis archive is too large.")
}
fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| String::new(), |value| value.as_secs().to_string())
}

#[cfg(test)]
mod tests {
    use super::{RedisProvider, configure_command_environment, parse_redis_version};
    use std::str::FromStr;
    use std::{ffi::OsStr, path::Path};
    use torben_contracts::ExactVersion;

    #[test]
    fn builds_pinned_windows_distribution_for_core_versions() {
        let provider = RedisProvider::windows_community().unwrap();
        let lts = provider
            .distribution(&ExactVersion::from_str("8.8.0").unwrap())
            .unwrap();
        assert_eq!(lts.archive_name, "Redis-8.8.0-Windows-x64-msys2.zip");
        assert_eq!(
            lts.archive_url.as_str(),
            "https://github.com/redis-windows/redis-windows/releases/download/8.8.0/Redis-8.8.0-Windows-x64-msys2.zip"
        );
        assert_eq!(lts.checksum.len(), 64);

        let legacy = provider
            .distribution(&ExactVersion::from_str("7.4.9").unwrap())
            .unwrap();
        assert_eq!(legacy.archive_name, "Redis-7.4.9-Windows-x64-msys2.zip");
        assert_eq!(legacy.archive_url.host_str(), Some("github.com"));
        assert_eq!(legacy.checksum.len(), 64);
    }

    #[test]
    fn parses_redis_server_version_output() {
        let version = parse_redis_version(
            "Redis server v=8.8.0 sha=abc bits=64 build=release malloc=jemalloc",
        )
        .unwrap();
        assert_eq!(version, ExactVersion::from_str("8.8.0").unwrap());
        assert!(parse_redis_version("redis-server: unknown option").is_none());
    }

    #[test]
    fn managed_commands_keep_redis_state_in_the_provider_data_directory() {
        let root = tempfile::tempdir().unwrap();
        let data_root = root.path().join("data");
        let bin = root.path().join("runtime");
        let mut command = std::process::Command::new("redis-server");

        configure_command_environment(&mut command, &data_root, &bin).unwrap();

        assert_eq!(command.get_current_dir(), Some(data_root.as_path()));
        let history = command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("REDISCLI_HISTFILE"))
            .and_then(|(_, value)| value);
        let expected_history = data_root.join("redis_history");
        assert_eq!(history, Some(expected_history.as_os_str()));
        let redis_home = command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("REDIS_HOME"))
            .and_then(|(_, value)| value);
        assert_eq!(redis_home, Some(bin.as_os_str()));

        let nested_bin = root.path().join("nested-runtime").join("bin");
        let mut nested_command = std::process::Command::new("redis-cli");
        configure_command_environment(&mut nested_command, &data_root, &nested_bin).unwrap();
        let nested_home = nested_command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new("REDIS_HOME"))
            .and_then(|(_, value)| value);
        assert_eq!(nested_home, nested_bin.parent().map(Path::as_os_str));
    }
}
