use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    future::Future,
    path::{Path, PathBuf},
    process::Stdio,
    str::FromStr,
    sync::Arc,
    time::{Duration, SystemTime},
};

use futures_util::StreamExt;
use serde::Deserialize;
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

const PYTHON_API_BASE: &str = "https://www.python.org/api/v2/downloads/";
const MAX_RELEASE_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_RELEASE_FILE_METADATA_BYTES: u64 = 4 * 1024 * 1024;
const ACTIVE_MINOR_LINES: usize = 5;
const MAX_SIGSTORE_BUNDLE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SOURCE_ARCHIVE_BYTES: u64 = 128 * 1024 * 1024;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub trait PythonSigstoreVerifier: Send + Sync {
    fn verify(
        &self,
        sha256: &str,
        bundle: &[u8],
        certificate_identity: &str,
        oidc_issuer: &str,
    ) -> TorbenResult<()>;
}

#[derive(Debug)]
#[cfg(any(not(unix), test))]
struct UnavailableSigstoreVerifier;

#[cfg(any(not(unix), test))]
impl PythonSigstoreVerifier for UnavailableSigstoreVerifier {
    fn verify(
        &self,
        _sha256: &str,
        _bundle: &[u8],
        _certificate_identity: &str,
        _oidc_issuer: &str,
    ) -> TorbenResult<()> {
        Err(TorbenError::new(
            "python_sigstore_verifier_unavailable",
            "This Torben App build has no Python Sigstore verifier.",
        )
        .with_remediation(
            "Use a build that includes Sigstore verification before installing Python from source.",
        ))
    }
}

#[derive(Clone)]
pub struct PythonProvider {
    client: reqwest::Client,
    api_base: Url,
    sigstore_verifier: Arc<dyn PythonSigstoreVerifier>,
    #[cfg(test)]
    python_manager_override: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonDistribution {
    pub version: ExactVersion,
    pub released_at: String,
    pub kind: PythonInstallKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PythonInstallKind {
    WindowsManager { tag: String },
    SourceArchive(Box<PythonSourceArchive>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonSourceArchive {
    pub archive_name: String,
    pub archive_url: Url,
    pub sha256: String,
    pub size: u64,
    pub sigstore_bundle_url: Url,
    pub sigstore_identity: String,
    pub sigstore_oidc_issuer: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PythonRelease {
    resource_uri: String,
    name: String,
    slug: String,
    pre_release: bool,
    is_published: bool,
    release_date: String,
}

#[derive(Debug, Clone, Deserialize)]
struct PythonReleaseFile {
    name: String,
    url: String,
    sha256_sum: String,
    filesize: u64,
    is_source: bool,
    sigstore_bundle_file: Option<String>,
}

impl PythonProvider {
    pub fn official() -> TorbenResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(network_error)?;
        #[cfg(unix)]
        let sigstore_verifier: Arc<dyn PythonSigstoreVerifier> =
            Arc::new(crate::python_sigstore::ProductionPythonSigstoreVerifier::new()?);
        #[cfg(not(unix))]
        let sigstore_verifier: Arc<dyn PythonSigstoreVerifier> =
            Arc::new(UnavailableSigstoreVerifier);
        Ok(Self {
            client,
            api_base: Url::parse(PYTHON_API_BASE).map_err(url_error)?,
            sigstore_verifier,
            #[cfg(test)]
            python_manager_override: None,
        })
    }

    #[cfg(test)]
    fn with_base_url(api_base: Url) -> TorbenResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("Torben-App-Test")
                .build()
                .map_err(network_error)?,
            api_base,
            sigstore_verifier: Arc::new(UnavailableSigstoreVerifier),
            python_manager_override: None,
        })
    }

    #[cfg(test)]
    fn with_test_runtime(
        api_base: Url,
        verifier: Arc<dyn PythonSigstoreVerifier>,
        python_manager: Option<PathBuf>,
    ) -> TorbenResult<Self> {
        let mut provider = Self::with_base_url(api_base)?;
        provider.sigstore_verifier = verifier;
        provider.python_manager_override = python_manager;
        Ok(provider)
    }

    pub async fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        let releases = self.releases().await?;
        let mut latest_by_line = BTreeMap::<(u64, u64), (ExactVersion, String)>::new();
        for (version, release) in releases {
            let line = (version.as_semver().major, version.as_semver().minor);
            let candidate = (version, release.release_date);
            match latest_by_line.get(&line) {
                Some((current, _)) if current >= &candidate.0 => {}
                _ => {
                    latest_by_line.insert(line, candidate);
                }
            }
        }
        let mut lines = latest_by_line.into_values().collect::<Vec<_>>();
        lines.sort_by(|left, right| right.0.cmp(&left.0));
        lines.truncate(ACTIVE_MINOR_LINES);
        Ok(lines
            .into_iter()
            .enumerate()
            .map(|(index, (version, released_at))| VersionDescriptor {
                lts_name: None,
                recommended: index == 0,
                version,
                released_at,
            })
            .collect())
    }

    pub async fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        let releases = self.releases().await?;
        if let Ok(exact) = ExactVersion::from_str(requested) {
            return releases
                .into_iter()
                .find(|(version, _)| version == &exact)
                .map(|(version, _)| version)
                .ok_or_else(|| version_not_found(requested));
        }
        let normalized = requested.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "current" | "latest" | "3") {
            return releases
                .into_iter()
                .next()
                .map(|(version, _)| version)
                .ok_or_else(|| version_not_found(requested));
        }
        let parts = normalized.split('.').collect::<Vec<_>>();
        if parts.len() == 2
            && let (Ok(major), Ok(minor)) = (parts[0].parse::<u64>(), parts[1].parse::<u64>())
        {
            return releases
                .into_iter()
                .find(|(version, _)| {
                    version.as_semver().major == major && version.as_semver().minor == minor
                })
                .map(|(version, _)| version)
                .ok_or_else(|| version_not_found(requested));
        }
        Err(version_not_found(requested))
    }

    pub async fn distribution(&self, version: &ExactVersion) -> TorbenResult<PythonDistribution> {
        let releases = self.releases().await?;
        let release = releases
            .into_iter()
            .find(|(candidate, _)| candidate == version)
            .map(|(_, release)| release)
            .ok_or_else(|| version_not_found(&version.to_string()))?;
        if cfg!(windows) {
            let architecture = match std::env::consts::ARCH {
                "x86_64" => "64",
                "aarch64" => "arm64",
                other => return Err(platform_error("architecture", other)),
            };
            return Ok(PythonDistribution {
                version: version.clone(),
                released_at: release.release_date,
                kind: PythonInstallKind::WindowsManager {
                    tag: format!("{version}-{architecture}"),
                },
            });
        }
        if !matches!(std::env::consts::OS, "linux" | "macos") {
            return Err(platform_error("os", std::env::consts::OS));
        }
        let release_id = release_id(&release.resource_uri)?;
        let mut files_url = self.api_base.join("release_file/").map_err(url_error)?;
        files_url
            .query_pairs_mut()
            .append_pair("release", &release_id.to_string());
        let files: Vec<PythonReleaseFile> = self
            .fetch_json(&files_url, MAX_RELEASE_FILE_METADATA_BYTES)
            .await?;
        source_distribution(version, release.release_date, files)
    }

    #[allow(clippy::too_many_lines)]
    pub async fn install(
        &self,
        paths: &TorbenPaths,
        app_id: &AppId,
        version: &ExactVersion,
        plan: &InstallPlan,
        journal: &mut OperationJournal,
    ) -> TorbenResult<InstallRecord> {
        let distribution = self.validate_install_plan(plan, app_id, version).await?;
        let cancellation = journal.cancellation_probe();
        cancellation.check()?;
        let staging =
            paths
                .staging_dir()
                .join(format!("install-{}-{}", app_id, journal.operation_id()));
        std::fs::create_dir_all(&staging).map_err(io_error)?;
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            return Err(TorbenError::new(
                "install_path_exists",
                "The final Python installation directory already exists.",
            ));
        }
        let staged_runtime = match &distribution.kind {
            PythonInstallKind::WindowsManager { tag } => {
                self.install_with_manager(paths, tag, &staging, journal, &cancellation)
                    .await?
            }
            PythonInstallKind::SourceArchive(source) => {
                self.build_source_runtime(
                    paths,
                    version,
                    source,
                    &final_path,
                    &staging,
                    journal,
                    &cancellation,
                )
                .await?
            }
        };
        cancellation.check()?;
        journal.record(
            OperationState::Running,
            "health_check",
            "Checking the staged CPython runtime and pip",
            Some(0.86),
        )?;
        self.health_check_path(&staged_runtime, version).await?;
        cancellation.check()?;
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        std::fs::rename(&staged_runtime, &final_path).map_err(|error| {
            TorbenError::new(
                "install_commit_failed",
                "Could not atomically commit the Python installation.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let _ = std::fs::remove_dir_all(&staging);
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

    pub async fn health_check(&self, record: &InstallRecord) -> TorbenResult<()> {
        self.health_check_path(Path::new(&record.install_path), &record.version)
            .await
    }

    pub fn command_path(&self, install_path: &Path, command: &str) -> TorbenResult<PathBuf> {
        let candidates = match (cfg!(windows), command) {
            (true, "python" | "python3") => vec![install_path.join("python.exe")],
            (true, "pip" | "pip3") => vec![
                install_path.join("Scripts").join("pip.exe"),
                install_path.join("pip.exe"),
            ],
            (false, "python" | "python3") => vec![install_path.join("bin").join("python3")],
            (false, "pip" | "pip3") => vec![install_path.join("bin").join("pip3")],
            _ => {
                return Err(TorbenError::new(
                    "unsupported_command",
                    "The Python plugin does not expose this command.",
                )
                .with_detail("command", command));
            }
        };
        candidates
            .into_iter()
            .find(|candidate| ensure_regular_file(candidate).is_ok())
            .ok_or_else(|| {
                TorbenError::new(
                    "managed_command_missing",
                    "A managed Python command is missing.",
                )
                .with_detail("command", command)
            })
    }

    pub async fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let names = if cfg!(windows) {
            ["python.exe", "python3.exe"].as_slice()
        } else {
            ["python3", "python"].as_slice()
        };
        let mut candidates = BTreeSet::new();
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            for name in names {
                candidates.insert(directory.join(name));
            }
        }
        let mut records = Vec::new();
        for candidate in candidates {
            let Ok(canonical) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) {
                continue;
            }
            let output = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::process::Command::new(&canonical)
                    .args([
                        "-c",
                        "import platform,sys;print(sys.implementation.name);print(platform.python_version())",
                    ])
                    .kill_on_drop(true)
                    .output(),
            )
            .await;
            let Ok(Ok(output)) = output else {
                continue;
            };
            if !output.status.success() {
                continue;
            }
            let lines = String::from_utf8_lossy(&output.stdout)
                .lines()
                .map(str::trim)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            if lines.first().map(String::as_str) != Some("cpython") {
                continue;
            }
            let Some(raw_version) = lines.get(1) else {
                continue;
            };
            let Ok(version) = ExactVersion::from_str(raw_version) else {
                continue;
            };
            let Some(home) = python_home_from_executable(&canonical) else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("python")?,
                version,
                source_id: SourceId::new("python.external")?,
                scope: InstallScope::External,
                install_path: home.display().to_string(),
                installed_at: String::new(),
                health: "healthy".to_owned(),
            });
        }
        Ok(records)
    }

    async fn validate_install_plan(
        &self,
        plan: &InstallPlan,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<PythonDistribution> {
        if app_id.as_str() != "python"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new("python.official")?
        {
            return Err(invalid_plan("identity or source owner"));
        }
        let distribution = self.distribution(version).await?;
        let install_method = match &distribution.kind {
            PythonInstallKind::WindowsManager { .. } => "python_manager",
            PythonInstallKind::SourceArchive(_) => "source_build",
        };
        let expected_metadata = BTreeMap::from([
            (
                "target".to_owned(),
                format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            ),
            ("installMethod".to_owned(), install_method.to_owned()),
        ]);
        if plan.metadata != expected_metadata {
            return Err(invalid_plan("target or install method metadata"));
        }
        let expected_commands = ["python", "python3", "pip", "pip3"];
        match (&distribution.kind, plan.steps.as_slice()) {
            (
                PythonInstallKind::WindowsManager { tag },
                [
                    InstallStep::InstallWithPythonManager { tag: planned_tag },
                    InstallStep::HealthCheck {
                        executable,
                        arguments,
                        expected_output,
                    },
                    InstallStep::CreateShims { commands },
                ],
            ) if tag == planned_tag
                && executable == "python"
                && arguments.as_slice() == ["--version"]
                && expected_output == &version.to_string()
                && commands.as_slice() == expected_commands => {}
            (
                PythonInstallKind::SourceArchive(source),
                [
                    InstallStep::Download {
                        url,
                        destination_name,
                    },
                    InstallStep::VerifySha256 {
                        archive_name,
                        expected,
                    },
                    InstallStep::VerifySigstoreBundle {
                        archive_name: signed_archive,
                        bundle_url,
                        certificate_identity,
                        oidc_issuer,
                    },
                    InstallStep::ExtractArchive {
                        archive_name: extracted_archive,
                        strip_components,
                    },
                    InstallStep::BuildPythonSource {
                        archive_name: build_archive,
                        configure_arguments,
                    },
                    InstallStep::HealthCheck {
                        executable,
                        arguments,
                        expected_output,
                    },
                    InstallStep::CreateShims { commands },
                ],
            ) if Url::parse(url).ok().as_ref() == Some(&source.archive_url)
                && destination_name == &source.archive_name
                && archive_name == &source.archive_name
                && signed_archive == &source.archive_name
                && extracted_archive == &source.archive_name
                && build_archive == &source.archive_name
                && expected.to_ascii_lowercase() == source.sha256
                && Url::parse(bundle_url).ok().as_ref() == Some(&source.sigstore_bundle_url)
                && certificate_identity == &source.sigstore_identity
                && oidc_issuer == &source.sigstore_oidc_issuer
                && *strip_components == 0
                && configure_arguments.as_slice()
                    == ["--with-ensurepip=install", "--disable-test-modules"]
                && executable == "python"
                && arguments.as_slice() == ["--version"]
                && expected_output == &version.to_string()
                && commands.as_slice() == expected_commands => {}
            _ => return Err(invalid_plan("steps or official distribution details")),
        }
        Ok(distribution)
    }

    async fn install_with_manager(
        &self,
        paths: &TorbenPaths,
        tag: &str,
        staging: &Path,
        journal: &mut OperationJournal,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<PathBuf> {
        if !cfg!(windows) {
            return Err(platform_error("os", std::env::consts::OS));
        }
        let manager = self.python_manager(paths.data_dir())?;
        let runtime = staging.join("runtime");
        journal.record(
            OperationState::Running,
            "install",
            format!("Extracting CPython {tag} with the official Python Install Manager"),
            Some(0.55),
        )?;
        let target = format!("--target={}", runtime.display());
        let download_dir = paths.cache_dir().join("python-manager");
        std::fs::create_dir_all(&download_dir).map_err(io_error)?;
        run_process(
            &manager,
            &[
                OsString::from("install"),
                OsString::from(target),
                OsString::from(tag),
            ],
            None,
            &[
                ("PYTHON_MANAGER_CONFIRM", OsStr::new("false")),
                ("PYTHON_MANAGER_DOWNLOAD_DIR", download_dir.as_os_str()),
            ],
            cancellation,
        )
        .await?;
        ensure_regular_directory(&runtime)?;
        Ok(runtime)
    }

    async fn health_check_path(
        &self,
        install_path: &Path,
        version: &ExactVersion,
    ) -> TorbenResult<()> {
        let python = self.command_path(install_path, "python")?;
        let output = tokio::process::Command::new(&python)
            .args([
                "-c",
                "import platform,sys;print(sys.implementation.name);print(platform.python_version())",
            ])
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|error| process_start_error(&python, error))?;
        if !output.status.success() {
            return Err(process_failure(&python, output.status));
        }
        let lines = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(str::trim)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let expected_version = version.to_string();
        if lines.first().map(String::as_str) != Some("cpython")
            || lines.get(1).map(String::as_str) != Some(expected_version.as_str())
        {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The staged Python runtime identity or version is incorrect.",
            )
            .with_detail("expected", version.to_string())
            .with_detail("actual", lines.join(";")));
        }
        let pip = self.command_path(install_path, "pip")?;
        let output = tokio::process::Command::new(&pip)
            .arg("--version")
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|error| process_start_error(&pip, error))?;
        if !output.status.success() || !String::from_utf8_lossy(&output.stdout).starts_with("pip ")
        {
            return Err(TorbenError::new(
                "health_check_failed",
                "The staged Python pip command failed its version check.",
            ));
        }
        Ok(())
    }

    fn python_manager(&self, managed_root: &Path) -> TorbenResult<PathBuf> {
        #[cfg(not(test))]
        let _ = self;
        #[cfg(test)]
        if let Some(manager) = &self.python_manager_override {
            ensure_regular_file(manager)?;
            return Ok(manager.clone());
        }
        find_external_command("py", managed_root).map_err(|error| {
            TorbenError::new(
                "python_install_manager_unavailable",
                "The official Python Install Manager is required on Windows.",
            )
            .with_detail("reasonCode", error.code)
            .with_remediation(
                "Install the Python Install Manager from python.org, then retry the managed installation.",
            )
        })
    }

    async fn fetch_bytes(
        &self,
        url: &Url,
        maximum: u64,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<Vec<u8>> {
        cancellation.check()?;
        let response = await_with_cancellation(
            async {
                self.client
                    .get(url.clone())
                    .send()
                    .await
                    .map_err(network_error)
            },
            cancellation,
        )
        .await?;
        validate_python_ftp_response(&response, url)?;
        let response = response.error_for_status().map_err(network_error)?;
        if response.content_length().is_some_and(|size| size > maximum) {
            return Err(resource_too_large(maximum));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = await_with_cancellation(
            async { stream.next().await.transpose().map_err(network_error) },
            cancellation,
        )
        .await?
        {
            let next = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| resource_too_large(maximum))?;
            if u64::try_from(next).unwrap_or(u64::MAX) > maximum {
                return Err(resource_too_large(maximum));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn download_file(
        &self,
        url: &Url,
        destination: &Path,
        expected_size: u64,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<()> {
        if expected_size == 0 || expected_size > MAX_SOURCE_ARCHIVE_BYTES {
            return Err(resource_too_large(MAX_SOURCE_ARCHIVE_BYTES));
        }
        cancellation.check()?;
        let response = await_with_cancellation(
            async {
                self.client
                    .get(url.clone())
                    .send()
                    .await
                    .map_err(network_error)
            },
            cancellation,
        )
        .await?;
        validate_python_ftp_response(&response, url)?;
        let response = response.error_for_status().map_err(network_error)?;
        if response
            .content_length()
            .is_some_and(|size| size != expected_size)
        {
            return Err(size_mismatch(expected_size, response.content_length()));
        }
        let partial = destination.with_extension("partial");
        let mut file = tokio::fs::File::create(&partial).await.map_err(io_error)?;
        let mut received = 0_u64;
        let mut stream = response.bytes_stream();
        let result = async {
            while let Some(chunk) = await_with_cancellation(
                async { stream.next().await.transpose().map_err(network_error) },
                cancellation,
            )
            .await?
            {
                received = received
                    .checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
                    .ok_or_else(|| size_mismatch(expected_size, Some(received)))?;
                if received > expected_size {
                    return Err(size_mismatch(expected_size, Some(received)));
                }
                file.write_all(&chunk).await.map_err(io_error)?;
            }
            file.flush().await.map_err(io_error)?;
            file.sync_all().await.map_err(io_error)?;
            drop(file);
            if received != expected_size {
                return Err(size_mismatch(expected_size, Some(received)));
            }
            if destination.exists() {
                tokio::fs::remove_file(destination)
                    .await
                    .map_err(io_error)?;
            }
            tokio::fs::rename(&partial, destination)
                .await
                .map_err(io_error)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(partial).await;
        }
        result
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    async fn build_source_runtime(
        &self,
        paths: &TorbenPaths,
        version: &ExactVersion,
        source: &PythonSourceArchive,
        final_path: &Path,
        staging: &Path,
        journal: &mut OperationJournal,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<PathBuf> {
        if !matches!(std::env::consts::OS, "linux" | "macos") {
            return Err(platform_error("os", std::env::consts::OS));
        }
        let download_dir = paths.download_dir("python", &version.to_string());
        std::fs::create_dir_all(&download_dir).map_err(io_error)?;
        let archive_path = download_dir.join(&source.archive_name);
        journal.record(
            OperationState::Running,
            "download",
            format!("Downloading {}", source.archive_name),
            Some(0.15),
        )?;
        if !archive_path.is_file()
            || std::fs::metadata(&archive_path).map_err(io_error)?.len() != source.size
            || sha256_file_checked(&archive_path, Some(cancellation))? != source.sha256
        {
            self.download_file(
                &source.archive_url,
                &archive_path,
                source.size,
                cancellation,
            )
            .await?;
        }
        let actual_hash = sha256_file_checked(&archive_path, Some(cancellation))?;
        if actual_hash != source.sha256 {
            return Err(TorbenError::new(
                "archive_hash_mismatch",
                "The CPython source archive does not match its official checksum.",
            ));
        }
        let bundle = self
            .fetch_bytes(
                &source.sigstore_bundle_url,
                MAX_SIGSTORE_BUNDLE_BYTES,
                cancellation,
            )
            .await?;
        journal.record(
            OperationState::Running,
            "verify",
            "Verifying the CPython release-manager Sigstore bundle",
            Some(0.3),
        )?;
        self.sigstore_verifier.verify(
            &actual_hash,
            &bundle,
            &source.sigstore_identity,
            &source.sigstore_oidc_issuer,
        )?;
        cancellation.check()?;
        let source_staging = staging.join("source");
        std::fs::create_dir_all(&source_staging).map_err(io_error)?;
        let archive = archive_path.clone();
        let extraction = source_staging.clone();
        let extraction_cancellation = cancellation.clone();
        let source_root = tokio::task::spawn_blocking(move || {
            extract_archive(
                &archive,
                ArchiveKind::TarXz,
                &extraction,
                &extraction_cancellation,
            )
        })
        .await
        .map_err(|error| {
            TorbenError::new("archive_task_failed", "The CPython extraction task failed.")
                .with_detail("reason", error.to_string())
        })??;
        let configure = source_root.join("configure");
        ensure_regular_file(&configure)?;
        let prefix = format!("--prefix={}", final_path.display());
        journal.record(
            OperationState::Running,
            "build",
            "Configuring the CPython source build",
            Some(0.45),
        )?;
        run_process(
            &configure,
            &[
                OsString::from(prefix),
                OsString::from("--with-ensurepip=install"),
                OsString::from("--disable-test-modules"),
            ],
            Some(&source_root),
            &[],
            cancellation,
        )
        .await?;
        let make = find_external_command("make", paths.data_dir())?;
        let jobs = std::thread::available_parallelism().map_or(1, usize::from);
        journal.record(
            OperationState::Running,
            "build",
            "Building CPython from the verified source archive",
            Some(0.58),
        )?;
        run_process(
            &make,
            &[OsString::from(format!("-j{jobs}"))],
            Some(&source_root),
            &[],
            cancellation,
        )
        .await?;
        let install_root = staging.join("install-root");
        std::fs::create_dir_all(&install_root).map_err(io_error)?;
        journal.record(
            OperationState::Running,
            "build",
            "Installing the CPython build into transaction staging",
            Some(0.74),
        )?;
        run_process(
            &make,
            &[
                OsString::from("install"),
                OsString::from(format!("DESTDIR={}", install_root.display())),
            ],
            Some(&source_root),
            &[],
            cancellation,
        )
        .await?;
        let relative_prefix = final_path.strip_prefix(Path::new("/")).map_err(|_| {
            TorbenError::new(
                "python_install_prefix_invalid",
                "The managed Python prefix is not an absolute Unix path.",
            )
        })?;
        let runtime = install_root.join(relative_prefix);
        ensure_regular_directory(&runtime)?;
        Ok(runtime)
    }

    async fn releases(&self) -> TorbenResult<Vec<(ExactVersion, PythonRelease)>> {
        let url = self.api_base.join("release/").map_err(url_error)?;
        let releases: Vec<PythonRelease> =
            self.fetch_json(&url, MAX_RELEASE_METADATA_BYTES).await?;
        let mut result = Vec::new();
        for release in releases {
            if !release.is_published || release.pre_release {
                continue;
            }
            let Some(raw_version) = release.name.strip_prefix("Python ") else {
                continue;
            };
            let Ok(version) = ExactVersion::from_str(raw_version) else {
                continue;
            };
            if version.as_semver().major != 3
                || release.slug != python_release_slug(&version)
                || release_id(&release.resource_uri).is_err()
            {
                continue;
            }
            result.push((version, release));
        }
        result.sort_by(|left, right| right.0.cmp(&left.0));
        result.dedup_by(|left, right| left.0 == right.0);
        if result.is_empty() {
            return Err(TorbenError::new(
                "python_metadata_invalid",
                "The official Python release catalog contains no stable Python 3 releases.",
            ));
        }
        Ok(result)
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &Url,
        maximum: u64,
    ) -> TorbenResult<T> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(network_error)?;
        if !same_origin(&self.api_base, response.url()) {
            return Err(TorbenError::new(
                "unexpected_download_origin",
                "The Python metadata request changed network origin.",
            ));
        }
        let response = response.error_for_status().map_err(network_error)?;
        if response.content_length().is_some_and(|size| size > maximum) {
            return Err(metadata_too_large(maximum));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await.transpose().map_err(network_error)? {
            let next = bytes
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| metadata_too_large(maximum))?;
            if u64::try_from(next).unwrap_or(u64::MAX) > maximum {
                return Err(metadata_too_large(maximum));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            TorbenError::new(
                "python_metadata_invalid",
                "The Python release metadata is not valid JSON.",
            )
            .with_detail("reason", error.to_string())
        })
    }
}

fn find_external_command(command: &str, managed_root: &Path) -> TorbenResult<PathBuf> {
    let names = if cfg!(windows) {
        vec![format!("{command}.exe"), format!("{command}.cmd")]
    } else {
        vec![command.to_owned()]
    };
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        for name in &names {
            let candidate = directory.join(name);
            let Ok(canonical) = std::fs::canonicalize(candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) {
                continue;
            }
            if ensure_regular_file(&canonical).is_ok() {
                return Ok(canonical);
            }
        }
    }
    Err(TorbenError::new(
        "external_command_unavailable",
        "A required external build or installation command is unavailable.",
    )
    .with_detail("command", command))
}

async fn run_process(
    executable: &Path,
    arguments: &[OsString],
    current_directory: Option<&Path>,
    environment: &[(&str, &OsStr)],
    cancellation: &CancellationProbe,
) -> TorbenResult<()> {
    ensure_regular_file(executable)?;
    cancellation.check()?;
    let mut command = tokio::process::Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    if let Some(directory) = current_directory {
        ensure_regular_directory(directory)?;
        command.current_dir(directory);
    }
    for variable in [
        "PYTHONHOME",
        "PYTHONPATH",
        "PYTHONSTARTUP",
        "PYTHONUSERBASE",
        "VIRTUAL_ENV",
    ] {
        command.env_remove(variable);
    }
    for (name, value) in environment {
        command.env(name, value);
    }
    let mut child = command
        .spawn()
        .map_err(|error| process_start_error(executable, error))?;
    loop {
        tokio::select! {
            result = child.wait() => {
                let status = result.map_err(io_error)?;
                return if status.success() {
                    Ok(())
                } else {
                    Err(process_failure(executable, status))
                };
            }
            () = tokio::time::sleep(PROCESS_POLL_INTERVAL) => {
                if let Err(error) = cancellation.check() {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    return Err(error);
                }
            }
        }
    }
}

async fn await_with_cancellation<F, T>(
    future: F,
    cancellation: &CancellationProbe,
) -> TorbenResult<T>
where
    F: Future<Output = TorbenResult<T>>,
{
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            () = tokio::time::sleep(PROCESS_POLL_INTERVAL) => cancellation.check()?,
        }
    }
}

fn validate_python_ftp_response(response: &reqwest::Response, requested: &Url) -> TorbenResult<()> {
    if response.url().scheme() != requested.scheme()
        || response.url().host_str() != requested.host_str()
        || response.url().port_or_known_default() != requested.port_or_known_default()
    {
        return Err(TorbenError::new(
            "unexpected_download_origin",
            "A Python release request changed network origin.",
        ));
    }
    Ok(())
}

fn ensure_regular_file(path: &Path) -> TorbenResult<()> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TorbenError::new(
            "python_path_invalid",
            "A Python transaction file is not a regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    Ok(())
}

fn ensure_regular_directory(path: &Path) -> TorbenResult<()> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TorbenError::new(
            "python_path_invalid",
            "A Python transaction directory is not a regular directory.",
        )
        .with_detail("path", path.display().to_string()));
    }
    Ok(())
}

fn python_home_from_executable(executable: &Path) -> Option<PathBuf> {
    let parent = executable.parent()?;
    if parent.file_name().is_some_and(|name| name == "bin") {
        parent.parent().map(Path::to_path_buf)
    } else {
        Some(parent.to_path_buf())
    }
}

fn python_release_slug(version: &ExactVersion) -> String {
    format!("python-{}", version.to_string().replace('.', ""))
}

fn release_id(resource_uri: &str) -> TorbenResult<u64> {
    let url = Url::parse(resource_uri).map_err(url_error)?;
    if url.scheme() != "https" || url.host_str() != Some("www.python.org") {
        return Err(metadata_invalid("release resource URI"));
    }
    let segments = url
        .path_segments()
        .map(|segments| {
            segments
                .filter(|segment| !segment.is_empty())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if segments.len() != 5 || segments[..4] != ["api", "v2", "downloads", "release"] {
        return Err(metadata_invalid("release resource URI"));
    }
    segments[4]
        .parse::<u64>()
        .map_err(|_| metadata_invalid("release resource ID"))
}

fn validate_python_ftp_url(url: &Url, version: &ExactVersion) -> TorbenResult<()> {
    let expected_prefix = format!("/ftp/python/{version}/");
    if url.scheme() != "https"
        || url.host_str() != Some("www.python.org")
        || !url.path().starts_with(&expected_prefix)
    {
        return Err(metadata_invalid("source archive URL"));
    }
    Ok(())
}

fn source_distribution(
    version: &ExactVersion,
    released_at: String,
    files: Vec<PythonReleaseFile>,
) -> TorbenResult<PythonDistribution> {
    let file = files
        .into_iter()
        .find(|file| {
            file.is_source
                && file.name == "XZ compressed source tarball"
                && file.url.ends_with(".tar.xz")
        })
        .ok_or_else(|| {
            TorbenError::new(
                "python_source_archive_missing",
                "The official Python release has no XZ source archive.",
            )
        })?;
    if file.filesize == 0
        || file.filesize > MAX_SOURCE_ARCHIVE_BYTES
        || !is_sha256(&file.sha256_sum)
    {
        return Err(metadata_invalid("source archive integrity"));
    }
    let archive_url = Url::parse(&file.url).map_err(url_error)?;
    validate_python_ftp_url(&archive_url, version)?;
    let archive_name = archive_url
        .path_segments()
        .and_then(Iterator::last)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| metadata_invalid("source archive name"))?
        .to_owned();
    let sigstore_bundle_url = file
        .sigstore_bundle_file
        .as_deref()
        .ok_or_else(|| {
            TorbenError::new(
                "python_sigstore_bundle_missing",
                "The official Python release metadata has no Sigstore bundle.",
            )
        })
        .and_then(|url| Url::parse(url).map_err(url_error))?;
    let expected_bundle_url = Url::parse(&format!("{archive_url}.sigstore")).map_err(url_error)?;
    if sigstore_bundle_url != expected_bundle_url {
        return Err(metadata_invalid("Sigstore bundle URL"));
    }
    let (sigstore_identity, sigstore_oidc_issuer) = sigstore_identity(version)?;
    Ok(PythonDistribution {
        version: version.clone(),
        released_at,
        kind: PythonInstallKind::SourceArchive(Box::new(PythonSourceArchive {
            archive_name,
            archive_url,
            sha256: file.sha256_sum.to_ascii_lowercase(),
            size: file.filesize,
            sigstore_bundle_url,
            sigstore_identity: sigstore_identity.to_owned(),
            sigstore_oidc_issuer: sigstore_oidc_issuer.to_owned(),
        })),
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn sigstore_identity(version: &ExactVersion) -> TorbenResult<(&'static str, &'static str)> {
    match (version.as_semver().major, version.as_semver().minor) {
        (3, 10 | 11) => Ok(("pablogsal@python.org", "https://accounts.google.com")),
        (3, 12 | 13) => Ok(("thomas@python.org", "https://accounts.google.com")),
        (3, 14 | 15) => Ok(("hugo@python.org", "https://github.com/login/oauth")),
        (3, 16 | 17) => Ok(("savannah@python.org", "https://github.com/login/oauth")),
        _ => Err(TorbenError::new(
            "python_sigstore_identity_untrusted",
            "This Python release line has no reviewed Sigstore identity in Torben App.",
        )
        .with_detail("version", version.to_string())),
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested Python version was not found in the official stable release catalog.",
    )
    .with_detail("requested", requested)
}

fn metadata_invalid(field: &str) -> TorbenError {
    TorbenError::new(
        "python_metadata_invalid",
        "The Python release metadata contains an invalid field.",
    )
    .with_detail("field", field)
}

fn metadata_too_large(maximum: u64) -> TorbenError {
    TorbenError::new(
        "python_metadata_too_large",
        "The Python release metadata exceeds the allowed size.",
    )
    .with_detail("maximumBytes", maximum.to_string())
}

fn resource_too_large(maximum: u64) -> TorbenError {
    TorbenError::new(
        "python_resource_too_large",
        "A Python source or verification resource exceeds the allowed size.",
    )
    .with_detail("maximumBytes", maximum.to_string())
}

fn size_mismatch(expected: u64, actual: Option<u64>) -> TorbenError {
    TorbenError::new(
        "archive_size_mismatch",
        "The CPython source archive size does not match official metadata.",
    )
    .with_detail("expected", expected.to_string())
    .with_detail(
        "actual",
        actual.map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
    )
}

fn invalid_plan(field: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The Python plugin returned an invalid installation plan.",
    )
    .with_detail("field", field)
}

fn platform_error(field: &str, value: &str) -> TorbenError {
    TorbenError::new(
        "platform_not_supported",
        "Managed Python is not supported on this platform.",
    )
    .with_detail(field, value)
}

fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new("python_network_error", "A Python metadata request failed.")
        .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "python_io_failed",
        "A managed Python filesystem or process operation failed.",
    )
    .with_detail("reason", error.to_string())
}

fn process_start_error(path: &Path, error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "python_process_start_failed",
        "A Python build, installation, or health-check process could not start.",
    )
    .with_detail("path", path.display().to_string())
    .with_detail("reason", error.to_string())
}

fn process_failure(path: &Path, status: std::process::ExitStatus) -> TorbenError {
    TorbenError::new(
        "python_process_failed",
        "A Python build, installation, or health-check process returned an error.",
    )
    .with_detail("path", path.display().to_string())
    .with_detail("status", status.to_string())
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
        .to_string()
}

fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new("python_url_invalid", "A Python release URL is invalid.")
        .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::TcpListener,
        sync::Arc,
        thread,
    };

    #[cfg(windows)]
    use tempfile::tempdir;
    #[cfg(windows)]
    use torben_contracts::OperationKind;

    #[cfg(windows)]
    use crate::{
        StateStore, TorbenCore, bundled_shim::BundledShim, node_plugin::BundledPlugin,
        operation::OperationJournal,
    };

    use super::*;

    struct AcceptingVerifier;

    impl PythonSigstoreVerifier for AcceptingVerifier {
        fn verify(
            &self,
            _sha256: &str,
            _bundle: &[u8],
            _certificate_identity: &str,
            _oidc_issuer: &str,
        ) -> TorbenResult<()> {
            Ok(())
        }
    }

    #[test]
    fn production_placeholder_fails_closed_without_sigstore_support() {
        let error = UnavailableSigstoreVerifier
            .verify(
                &"00".repeat(32),
                b"bundle",
                "release@python.org",
                "https://issuer.example",
            )
            .unwrap_err();
        assert_eq!(error.code, "python_sigstore_verifier_unavailable");
    }

    #[tokio::test]
    async fn local_catalog_lists_latest_active_lines_and_resolves_aliases() {
        let (base_url, server) = fixture_server(4);
        let provider =
            PythonProvider::with_test_runtime(base_url, Arc::new(AcceptingVerifier), None).unwrap();

        let versions = provider.list_versions().await.unwrap();
        let current = provider.resolve_version("current").await.unwrap();
        let line = provider.resolve_version("3.13").await.unwrap();
        let exact = provider.resolve_version("3.12.8").await.unwrap();

        server.join().unwrap();
        assert_eq!(versions[0].version.to_string(), "3.14.7");
        assert!(versions[0].recommended);
        assert_eq!(current.to_string(), "3.14.7");
        assert_eq!(line.to_string(), "3.13.15");
        assert_eq!(exact.to_string(), "3.12.8");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_manager_target_install_health_checks_and_commits() {
        let root = tempdir().unwrap();
        let manager = compile_fixture_manager(root.path());
        let (base_url, server) = fixture_server(2);
        let provider =
            PythonProvider::with_test_runtime(base_url, Arc::new(AcceptingVerifier), Some(manager))
                .unwrap();
        let version = ExactVersion::from_str("3.14.7").unwrap();
        let distribution = provider.distribution(&version).await.unwrap();
        let PythonInstallKind::WindowsManager { tag } = distribution.kind else {
            panic!("expected Python Install Manager distribution");
        };
        let app_id = AppId::new("python").unwrap();
        let plan = InstallPlan {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: SourceId::new("python.official").unwrap(),
            steps: vec![
                InstallStep::InstallWithPythonManager { tag },
                InstallStep::HealthCheck {
                    executable: "python".to_owned(),
                    arguments: vec!["--version".to_owned()],
                    expected_output: version.to_string(),
                },
                InstallStep::CreateShims {
                    commands: vec![
                        "python".to_owned(),
                        "python3".to_owned(),
                        "pip".to_owned(),
                        "pip3".to_owned(),
                    ],
                },
            ],
            metadata: BTreeMap::from([
                (
                    "target".to_owned(),
                    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                ),
                ("installMethod".to_owned(), "python_manager".to_owned()),
            ]),
        };
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let mut journal = OperationJournal::start(
            &paths,
            store,
            OperationKind::Install,
            &app_id,
            Some(&version),
        )
        .unwrap();

        let record = provider
            .install(&paths, &app_id, &version, &plan, &mut journal)
            .await
            .unwrap();
        server.join().unwrap();

        assert_eq!(record.version, version);
        assert!(Path::new(&record.install_path).join("python.exe").is_file());
        assert!(
            Path::new(&record.install_path)
                .join("Scripts")
                .join("pip.exe")
                .is_file()
        );
        provider.health_check(&record).await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_fixture_completes_core_install_select_and_uninstall_transaction() {
        let root = tempdir().unwrap();
        let manager = compile_fixture_manager(root.path());
        let (base_url, server) = fixture_server(1);
        let provider =
            PythonProvider::with_test_runtime(base_url, Arc::new(AcceptingVerifier), Some(manager))
                .unwrap();
        let version = ExactVersion::from_str("3.14.7").unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let install_path = paths.app_version_dir("python", &version.to_string());
        let plugin = root.path().join("python-plugin-fixture.cmd");
        std::fs::write(
            &plugin,
            windows_plugin_fixture_script(&version, &install_path),
        )
        .unwrap();
        let shim = root.path().join("torben-shim-fixture");
        std::fs::write(&shim, b"shim fixture").unwrap();
        let mut core = TorbenCore::open(paths).unwrap();
        core.python = provider;
        core.python_plugin = BundledPlugin::python_from_executable(plugin);
        core.bundled_shim = BundledShim::from_executable(shim);
        let app_id = AppId::new("python").unwrap();

        let installed = core.install(&app_id, "current").await.unwrap();
        core.select(&app_id, &version).await.unwrap();
        let command_available = core.executable_for(&app_id, "python").unwrap().is_file();
        core.clear_selection(&app_id).unwrap();
        core.uninstall(&app_id, &version).await.unwrap();
        server.join().unwrap();

        assert_eq!(installed.version, version);
        assert!(command_available);
        assert!(core.installed().unwrap().is_empty());
        assert!(!Path::new(&installed.install_path).exists());
    }

    #[cfg(windows)]
    fn compile_fixture_manager(directory: &Path) -> PathBuf {
        let source = directory.join("fixture-python-manager.rs");
        let executable = directory.join("py.exe");
        std::fs::write(
            &source,
            r#"
use std::{fs, path::PathBuf};
fn main() {
    let current = std::env::current_exe().unwrap();
    let stem = current.file_stem().unwrap().to_string_lossy().to_ascii_lowercase();
    if stem.starts_with("python") {
        println!("cpython\n3.14.7");
        return;
    }
    if stem.starts_with("pip") {
        println!("pip 26.0 from fixture");
        return;
    }
    let target = std::env::args()
        .find_map(|arg| arg.strip_prefix("--target=").map(PathBuf::from))
        .expect("target argument");
    let scripts = target.join("Scripts");
    fs::create_dir_all(&scripts).unwrap();
    fs::copy(&current, target.join("python.exe")).unwrap();
    fs::copy(&current, scripts.join("pip.exe")).unwrap();
}
"#,
        )
        .unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = std::process::Command::new(rustc)
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "fixture rustc failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        executable
    }

    #[cfg(windows)]
    fn windows_plugin_fixture_script(version: &ExactVersion, install_path: &Path) -> String {
        let architecture = if std::env::consts::ARCH == "aarch64" {
            "arm64"
        } else {
            "64"
        };
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": torben_contracts::plugin::PLUGIN_PROTOCOL_VERSION,
                "pluginId": "app.torben.plugin.python",
                "pluginVersion": env!("CARGO_PKG_VERSION"),
                "applications": [{
                    "id": "python",
                    "displayName": "Python",
                    "summary": "fixture",
                    "categories": ["runtime"],
                    "capabilities": ["versions", "install", "select", "uninstall"],
                    "sources": [{
                        "id": "python.official",
                        "displayName": "Official Python distribution",
                        "managed": true
                    }]
                }]
            }
        })
        .to_string();
        let resolved = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": { "requested": "current", "resolved": version }
        })
        .to_string();
        let plan = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {
                "appId": "python",
                "version": version,
                "sourceId": "python.official",
                "steps": [
                    { "type": "install_with_python_manager", "tag": format!("{version}-{architecture}") },
                    {
                        "type": "health_check",
                        "executable": "python",
                        "arguments": ["--version"],
                        "expected_output": version
                    },
                    {
                        "type": "create_shims",
                        "commands": ["python", "python3", "pip", "pip3"]
                    }
                ],
                "metadata": {
                    "target": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                    "installMethod": "python_manager"
                }
            }
        })
        .to_string();
        let health = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": { "healthy": true, "actualVersion": version, "message": "healthy" }
        })
        .to_string();
        let uninstall = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "appId": "python",
                "version": version,
                "sourceId": "python.official",
                "installPath": install_path.display().to_string(),
                "preserveUserData": true
            }
        })
        .to_string();
        format!(
            "@echo off\r\n:loop\r\nset request=\r\nset /p request=\r\nif errorlevel 1 exit /b 0\r\necho %request%| findstr /c:\"initialize\" >nul && (echo {initialize}& goto loop)\r\necho %request%| findstr /c:\"version.resolve\" >nul && (echo {resolved}& goto loop)\r\necho %request%| findstr /c:\"uninstall.plan\" >nul && (echo {uninstall}& goto loop)\r\necho %request%| findstr /c:\"install.plan\" >nul && (echo {plan}& goto loop)\r\necho %request%| findstr /c:\"health.check\" >nul && (echo {health}& goto loop)\r\nexit /b 1\r\n"
        )
    }

    #[tokio::test]
    async fn local_release_selects_the_official_source_archive_on_unix() {
        let (base_url, server) = fixture_server(if cfg!(windows) { 1 } else { 2 });
        let provider = PythonProvider::with_base_url(base_url).unwrap();
        let version = ExactVersion::from_str("3.14.7").unwrap();

        let distribution = provider.distribution(&version).await.unwrap();
        server.join().unwrap();

        if cfg!(windows) {
            assert_eq!(
                distribution.kind,
                PythonInstallKind::WindowsManager {
                    tag: if std::env::consts::ARCH == "aarch64" {
                        "3.14.7-arm64".to_owned()
                    } else {
                        "3.14.7-64".to_owned()
                    }
                }
            );
        } else {
            let PythonInstallKind::SourceArchive(source) = distribution.kind else {
                panic!("expected source archive");
            };
            assert_eq!(source.archive_name, "Python-3.14.7.tar.xz");
            assert_eq!(source.sha256, "11".repeat(32));
            assert_eq!(source.sigstore_identity, "hugo@python.org");
            assert_eq!(
                source.sigstore_oidc_issuer,
                "https://github.com/login/oauth"
            );
        }
    }

    #[test]
    fn source_archive_requires_the_adjacent_sigstore_bundle_from_api_metadata() {
        let version = ExactVersion::from_str("3.14.7").unwrap();
        let archive_url = "https://www.python.org/ftp/python/3.14.7/Python-3.14.7.tar.xz";
        let release_file = |bundle: Option<&str>| PythonReleaseFile {
            name: "XZ compressed source tarball".to_owned(),
            url: archive_url.to_owned(),
            sha256_sum: "11".repeat(32),
            filesize: 24_000_000,
            is_source: true,
            sigstore_bundle_file: bundle.map(str::to_owned),
        };

        let distribution = source_distribution(
            &version,
            "2026-08-05T12:00:00Z".to_owned(),
            vec![release_file(Some(&format!("{archive_url}.sigstore")))],
        )
        .unwrap();
        let PythonInstallKind::SourceArchive(source) = distribution.kind else {
            panic!("expected source archive");
        };
        assert_eq!(
            source.sigstore_bundle_url.as_str(),
            format!("{archive_url}.sigstore")
        );

        let missing = source_distribution(
            &version,
            "2026-08-05T12:00:00Z".to_owned(),
            vec![release_file(None)],
        )
        .unwrap_err();
        assert_eq!(missing.code, "python_sigstore_bundle_missing");

        let mismatched = source_distribution(
            &version,
            "2026-08-05T12:00:00Z".to_owned(),
            vec![release_file(Some(
                "https://www.python.org/ftp/python/3.14.7/other.sigstore",
            ))],
        )
        .unwrap_err();
        assert_eq!(mismatched.code, "python_metadata_invalid");
        assert_eq!(
            mismatched.details.get("field").map(String::as_str),
            Some("Sigstore bundle URL")
        );
    }

    fn fixture_server(expected_requests: usize) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}/api/v2/downloads/");
        let releases = [
            (1094, "3.14.7", "2026-08-05T12:00:00Z"),
            (1093, "3.13.15", "2026-08-05T11:00:00Z"),
            (1000, "3.12.8", "2024-12-03T00:00:00Z"),
            (999, "3.12.7", "2024-10-01T00:00:00Z"),
            (900, "3.11.9", "2024-04-02T00:00:00Z"),
            (800, "3.10.11", "2023-04-05T00:00:00Z"),
            (700, "3.9.13", "2022-05-17T00:00:00Z"),
        ]
        .into_iter()
        .map(|(id, version, date)| {
            serde_json::json!({
                "resource_uri": format!("https://www.python.org/api/v2/downloads/release/{id}/"),
                "name": format!("Python {version}"),
                "slug": format!("python-{}", version.replace('.', "")),
                "pre_release": false,
                "is_published": true,
                "release_date": date
            })
        })
        .collect::<Vec<_>>();
        let files = serde_json::json!([
            {
                "name": "XZ compressed source tarball",
                "url": "https://www.python.org/ftp/python/3.14.7/Python-3.14.7.tar.xz",
                "sha256_sum": "11".repeat(32),
                "filesize": 24_000_000,
                "is_source": true,
                "sigstore_bundle_file": "https://www.python.org/ftp/python/3.14.7/Python-3.14.7.tar.xz.sigstore"
            },
            {
                "name": "Gzipped source tarball",
                "url": "https://www.python.org/ftp/python/3.14.7/Python-3.14.7.tgz",
                "sha256_sum": "22".repeat(32),
                "filesize": 30_000_000,
                "is_source": true,
                "sigstore_bundle_file": "https://www.python.org/ftp/python/3.14.7/Python-3.14.7.tgz.sigstore"
            }
        ]);
        let routes = BTreeMap::from([
            (
                "/api/v2/downloads/release/".to_owned(),
                serde_json::to_vec(&releases).unwrap(),
            ),
            (
                "/api/v2/downloads/release_file/".to_owned(),
                serde_json::to_vec(&files).unwrap(),
            ),
        ]);
        let server = thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|path| path.split('?').next())
                    .unwrap();
                let body = routes.get(path).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        (Url::parse(&base).unwrap(), server)
    }
}
