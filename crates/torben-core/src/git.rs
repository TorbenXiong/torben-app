use std::{
    collections::BTreeSet,
    ffi::OsString,
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
    git_signature::{
        GIT_RELEASE_FINGERPRINT, verify_git_release_archive, verify_kernel_checksum_manifest,
    },
    node::{ArchiveKind, extract_archive, extract_archive_contents, sha256_file_checked},
    operation::{CancellationProbe, OperationJournal},
};

const GIT_FOR_WINDOWS_API: &str =
    "https://api.github.com/repos/git-for-windows/git/releases/latest";
const KERNEL_GIT_BASE: &str = "https://www.kernel.org/pub/software/scm/git/";
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
const MAX_CHECKSUM_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 64 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 160 * 1024 * 1024;
const MAX_CATALOG_VERSIONS: usize = 5;
const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(100);

trait GitChecksumVerifier: Send + Sync {
    fn verify(&self, manifest: &[u8]) -> TorbenResult<String>;
}

#[derive(Debug)]
struct ProductionGitChecksumVerifier;

impl GitChecksumVerifier for ProductionGitChecksumVerifier {
    fn verify(&self, manifest: &[u8]) -> TorbenResult<String> {
        verify_kernel_checksum_manifest(manifest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitInstallKind {
    WindowsMinGit,
    SourceArchive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDistribution {
    pub version: ExactVersion,
    pub released_at: String,
    pub archive_name: String,
    pub archive_url: Url,
    pub checksum: String,
    pub size: Option<u64>,
    pub signature_url: Option<Url>,
    pub kind: GitInstallKind,
}

#[derive(Clone)]
pub struct GitProvider {
    client: reqwest::Client,
    windows_api: Url,
    kernel_base: Url,
    checksum_verifier: Arc<dyn GitChecksumVerifier>,
    allow_fixture_http: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    published_at: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubAsset {
    name: String,
    state: String,
    size: u64,
    digest: Option<String>,
    browser_download_url: String,
}

#[derive(Debug, Clone, Copy)]
enum RequestKind {
    GitHubApi,
    GitHubAsset,
    Kernel,
}

impl GitProvider {
    pub fn official() -> TorbenResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
                .https_only(true)
                .build()
                .map_err(network_error)?,
            windows_api: Url::parse(GIT_FOR_WINDOWS_API).map_err(url_error)?,
            kernel_base: Url::parse(KERNEL_GIT_BASE).map_err(url_error)?,
            checksum_verifier: Arc::new(ProductionGitChecksumVerifier),
            allow_fixture_http: false,
        })
    }

    #[cfg(test)]
    fn with_fixture(
        windows_api: Url,
        kernel_base: Url,
        checksum_verifier: Arc<dyn GitChecksumVerifier>,
    ) -> TorbenResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("Torben-App-Test")
                .build()
                .map_err(network_error)?,
            windows_api,
            kernel_base,
            checksum_verifier,
            allow_fixture_http: true,
        })
    }

    pub async fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        self.list_versions_for_target(std::env::consts::OS, std::env::consts::ARCH)
            .await
    }

    async fn list_versions_for_target(
        &self,
        os: &str,
        architecture: &str,
    ) -> TorbenResult<Vec<VersionDescriptor>> {
        let mut versions = match os {
            "windows" => {
                let release = self.windows_release().await?;
                let distribution = windows_distribution(
                    &release,
                    architecture,
                    self.allow_fixture_http.then_some(&self.windows_api),
                )?;
                vec![VersionDescriptor {
                    version: distribution.version,
                    lts_name: None,
                    released_at: distribution.released_at,
                    recommended: true,
                }]
            }
            "linux" | "macos" => {
                let index = self.fetch_kernel_index().await?;
                parse_kernel_index(&index)?
                    .into_iter()
                    .take(MAX_CATALOG_VERSIONS)
                    .enumerate()
                    .map(|(index, (version, released_at))| VersionDescriptor {
                        version,
                        lts_name: None,
                        released_at,
                        recommended: index == 0,
                    })
                    .collect()
            }
            other => return Err(platform_error("os", other)),
        };
        if versions.is_empty() {
            return Err(TorbenError::new(
                "git_catalog_empty",
                "The official Git catalog contains no supported stable releases.",
            ));
        }
        versions.sort_by(|left, right| right.version.cmp(&left.version));
        if let Some(first) = versions.first_mut() {
            first.recommended = true;
        }
        Ok(versions)
    }

    pub async fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        self.resolve_version_for_target(requested, std::env::consts::OS, std::env::consts::ARCH)
            .await
    }

    async fn resolve_version_for_target(
        &self,
        requested: &str,
        os: &str,
        architecture: &str,
    ) -> TorbenResult<ExactVersion> {
        let versions = self.list_versions_for_target(os, architecture).await?;
        if let Ok(exact) = ExactVersion::from_str(requested) {
            return versions
                .into_iter()
                .find(|item| item.version == exact)
                .map(|item| item.version)
                .ok_or_else(|| version_not_found(requested));
        }
        if matches!(
            requested.trim().to_ascii_lowercase().as_str(),
            "current" | "latest"
        ) {
            return versions
                .into_iter()
                .next()
                .map(|item| item.version)
                .ok_or_else(|| version_not_found(requested));
        }
        Err(TorbenError::new(
            "version_alias_not_found",
            "Use an exact Git version, 'current', or 'latest'.",
        )
        .with_detail("requested", requested))
    }

    pub async fn distribution(&self, version: &ExactVersion) -> TorbenResult<GitDistribution> {
        self.distribution_for_target(version, std::env::consts::OS, std::env::consts::ARCH)
            .await
    }

    async fn distribution_for_target(
        &self,
        version: &ExactVersion,
        os: &str,
        architecture: &str,
    ) -> TorbenResult<GitDistribution> {
        match os {
            "windows" => {
                let release = self.windows_release().await?;
                let distribution = windows_distribution(
                    &release,
                    architecture,
                    self.allow_fixture_http.then_some(&self.windows_api),
                )?;
                if &distribution.version != version {
                    return Err(version_not_found(&version.to_string()));
                }
                Ok(distribution)
            }
            "linux" | "macos" => {
                if !matches!(architecture, "x86_64" | "aarch64") {
                    return Err(platform_error("architecture", architecture));
                }
                let index = self.fetch_kernel_index().await?;
                let versions = parse_kernel_index(&index)?;
                let released_at = versions
                    .into_iter()
                    .find(|(candidate, _)| candidate == version)
                    .map(|(_, released_at)| released_at)
                    .ok_or_else(|| version_not_found(&version.to_string()))?;
                let archive_name = format!("git-{version}.tar.xz");
                let archive_url = self.kernel_base.join(&archive_name).map_err(url_error)?;
                let signature_url = self
                    .kernel_base
                    .join(&format!("git-{version}.tar.sign"))
                    .map_err(url_error)?;
                let manifest_url = self.kernel_base.join("sha256sums.asc").map_err(url_error)?;
                let manifest = self
                    .fetch_limited(&manifest_url, MAX_CHECKSUM_BYTES, RequestKind::Kernel, None)
                    .await?;
                let signed_text = self.checksum_verifier.verify(&manifest)?;
                let checksum = checksum_for(&signed_text, &archive_name)?;
                Ok(GitDistribution {
                    version: version.clone(),
                    released_at,
                    archive_name,
                    archive_url,
                    checksum,
                    size: None,
                    signature_url: Some(signature_url),
                    kind: GitInstallKind::SourceArchive,
                })
            }
            other => Err(platform_error("os", other)),
        }
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
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            return Err(TorbenError::new(
                "install_path_exists",
                "The final Git installation directory already exists.",
            ));
        }
        let download_dir = paths.download_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&download_dir).map_err(io_error)?;
        let archive_path = download_dir.join(&distribution.archive_name);
        let cached = archive_path.is_file()
            && distribution.size.is_none_or(|expected| {
                archive_path
                    .metadata()
                    .is_ok_and(|item| item.len() == expected)
            })
            && sha256_file_checked(&archive_path, Some(&cancellation))? == distribution.checksum;
        if !cached {
            journal.record(
                OperationState::Running,
                "download",
                "Downloading the official Git archive",
                Some(0.18),
            )?;
            self.download_archive(&distribution, &archive_path, &cancellation)
                .await?;
        }
        cancellation.check()?;
        journal.record(
            OperationState::Running,
            "verify",
            "Verifying the official Git SHA-256 checksum",
            Some(0.38),
        )?;
        let actual_hash = sha256_file_checked(&archive_path, Some(&cancellation))?;
        if actual_hash != distribution.checksum {
            return Err(TorbenError::new(
                "archive_hash_mismatch",
                "The Git archive does not match its official checksum.",
            )
            .with_detail("expected", distribution.checksum)
            .with_detail("actual", actual_hash));
        }
        if let Some(signature_url) = &distribution.signature_url {
            let signature = self
                .fetch_limited(
                    signature_url,
                    MAX_SIGNATURE_BYTES,
                    RequestKind::Kernel,
                    Some(&cancellation),
                )
                .await?;
            journal.record(
                OperationState::Running,
                "verify",
                "Verifying the upstream Git source signature",
                Some(0.47),
            )?;
            let archive = archive_path.clone();
            tokio::task::spawn_blocking(move || verify_git_release_archive(&archive, &signature))
                .await
                .map_err(|error| {
                    TorbenError::new(
                        "git_signature_task_failed",
                        "The Git source signature task failed.",
                    )
                    .with_detail("reason", error.to_string())
                })??;
        }
        let staging =
            paths
                .staging_dir()
                .join(format!("install-{}-{}", app_id, journal.operation_id()));
        std::fs::create_dir_all(&staging).map_err(io_error)?;
        let staged_runtime = match distribution.kind {
            GitInstallKind::WindowsMinGit => {
                let runtime = staging.join("runtime");
                std::fs::create_dir_all(&runtime).map_err(io_error)?;
                journal.record(
                    OperationState::Running,
                    "extract",
                    "Extracting MinGit into transaction staging",
                    Some(0.58),
                )?;
                let archive = archive_path.clone();
                let target = runtime.clone();
                let extraction_cancellation = cancellation.clone();
                tokio::task::spawn_blocking(move || {
                    extract_archive_contents(
                        &archive,
                        ArchiveKind::Zip,
                        &target,
                        &extraction_cancellation,
                    )
                })
                .await
                .map_err(archive_task_error)??;
                runtime
            }
            GitInstallKind::SourceArchive => {
                self.build_source_runtime(
                    paths,
                    &archive_path,
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
            "Checking the staged Git command",
            Some(0.86),
        )?;
        self.health_check_path(&staged_runtime, version)?;
        cancellation.check()?;
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        std::fs::rename(&staged_runtime, &final_path).map_err(|error| {
            TorbenError::new(
                "install_commit_failed",
                "Could not atomically commit the Git installation.",
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

    pub fn health_check(&self, record: &InstallRecord) -> TorbenResult<()> {
        self.health_check_path(Path::new(&record.install_path), &record.version)
    }

    pub fn command_path(&self, install_path: &Path, command: &str) -> TorbenResult<PathBuf> {
        if command != "git" {
            return Err(TorbenError::new(
                "unsupported_command",
                "The Git plugin does not expose this command.",
            )
            .with_detail("command", command));
        }
        let path = if cfg!(windows) {
            install_path.join("cmd").join("git.exe")
        } else {
            install_path.join("bin").join("git")
        };
        ensure_regular_file(&path)?;
        Ok(path)
    }

    pub async fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let executable_name = if cfg!(windows) { "git.exe" } else { "git" };
        let mut candidates = BTreeSet::new();
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            candidates.insert(directory.join(executable_name));
        }
        let mut records = Vec::new();
        for candidate in candidates {
            let Ok(canonical) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) || ensure_regular_file(&canonical).is_err() {
                continue;
            }
            let Ok(result) = tokio::time::timeout(
                Duration::from_secs(3),
                tokio::process::Command::new(&canonical)
                    .arg("--version")
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            else {
                continue;
            };
            let Ok(output) = result else {
                continue;
            };
            if !output.status.success() {
                continue;
            }
            let Ok(version) = parse_git_version(&String::from_utf8_lossy(&output.stdout)) else {
                continue;
            };
            let install_path = canonical
                .parent()
                .and_then(Path::parent)
                .unwrap_or_else(|| canonical.parent().unwrap_or(&canonical));
            records.push(InstallRecord {
                app_id: AppId::new("git")?,
                version,
                source_id: SourceId::new("git.external")?,
                scope: InstallScope::External,
                install_path: install_path.display().to_string(),
                installed_at: String::new(),
                health: "healthy".to_owned(),
            });
        }
        Ok(records)
    }

    #[allow(clippy::too_many_lines)]
    async fn validate_install_plan(
        &self,
        plan: &InstallPlan,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<GitDistribution> {
        if app_id.as_str() != "git"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new("git.official")?
        {
            return Err(invalid_plan("identity or source owner"));
        }
        let distribution = self.distribution(version).await?;
        let install_method = match distribution.kind {
            GitInstallKind::WindowsMinGit => "mingit_archive",
            GitInstallKind::SourceArchive => "source_build",
        };
        if plan.metadata.get("target")
            != Some(&format!(
                "{}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
            || plan.metadata.get("installMethod").map(String::as_str) != Some(install_method)
        {
            return Err(invalid_plan("target or install method metadata"));
        }
        match (distribution.kind, plan.steps.as_slice()) {
            (
                GitInstallKind::WindowsMinGit,
                [
                    InstallStep::Download {
                        url,
                        destination_name,
                    },
                    InstallStep::VerifySha256 {
                        archive_name,
                        expected,
                    },
                    InstallStep::ExtractArchive {
                        archive_name: extracted_archive,
                        strip_components,
                    },
                    InstallStep::HealthCheck {
                        executable,
                        arguments,
                        expected_output,
                    },
                    InstallStep::CreateShims { commands },
                ],
            ) if Url::parse(url).ok().as_ref() == Some(&distribution.archive_url)
                && destination_name == &distribution.archive_name
                && archive_name == &distribution.archive_name
                && expected.to_ascii_lowercase() == distribution.checksum
                && extracted_archive == &distribution.archive_name
                && *strip_components == 0
                && executable == "git"
                && arguments.as_slice() == ["--version"]
                && expected_output == &version.to_string()
                && commands.as_slice() == ["git"] => {}
            (
                GitInstallKind::SourceArchive,
                [
                    InstallStep::Download {
                        url,
                        destination_name,
                    },
                    InstallStep::VerifySha256 {
                        archive_name,
                        expected,
                    },
                    InstallStep::VerifyGitReleaseSignature {
                        archive_name: signed_archive,
                        signature_url,
                        trusted_fingerprint,
                    },
                    InstallStep::ExtractArchive {
                        archive_name: extracted_archive,
                        strip_components,
                    },
                    InstallStep::BuildGitSource {
                        archive_name: build_archive,
                        make_arguments,
                    },
                    InstallStep::HealthCheck {
                        executable,
                        arguments,
                        expected_output,
                    },
                    InstallStep::CreateShims { commands },
                ],
            ) if Url::parse(url).ok().as_ref() == Some(&distribution.archive_url)
                && destination_name == &distribution.archive_name
                && archive_name == &distribution.archive_name
                && expected.to_ascii_lowercase() == distribution.checksum
                && signed_archive == &distribution.archive_name
                && distribution.signature_url.as_ref().is_some_and(|expected| {
                    Url::parse(signature_url).ok().as_ref() == Some(expected)
                })
                && trusted_fingerprint == GIT_RELEASE_FINGERPRINT
                && extracted_archive == &distribution.archive_name
                && build_archive == &distribution.archive_name
                && *strip_components == 0
                && make_arguments.as_slice() == ["NO_GETTEXT=YesPlease", "NO_TCLTK=YesPlease"]
                && executable == "git"
                && arguments.as_slice() == ["--version"]
                && expected_output == &version.to_string()
                && commands.as_slice() == ["git"] => {}
            _ => return Err(invalid_plan("official distribution details or step order")),
        }
        Ok(distribution)
    }

    fn health_check_path(&self, install_path: &Path, version: &ExactVersion) -> TorbenResult<()> {
        let executable = self.command_path(install_path, "git")?;
        let managed_bin = executable.parent().ok_or_else(|| {
            TorbenError::new(
                "managed_command_missing",
                "The managed Git command has no parent directory.",
            )
        })?;
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(managed_bin.to_path_buf()).chain(std::env::split_paths(&inherited)),
        )
        .map_err(|error| {
            TorbenError::new(
                "health_check_environment_invalid",
                "Could not prepare an isolated PATH for Git.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let output = std::process::Command::new(&executable)
            .arg("--version")
            .env("PATH", path)
            .output()
            .map_err(|error| {
                TorbenError::new(
                    "health_check_start_failed",
                    "Could not start the managed Git command.",
                )
                .with_detail("path", executable.display().to_string())
                .with_detail("reason", error.to_string())
            })?;
        if !output.status.success() {
            return Err(TorbenError::new(
                "health_check_failed",
                "The managed Git command returned an error.",
            )
            .with_detail("status", output.status.to_string()));
        }
        let actual = parse_git_version(&String::from_utf8_lossy(&output.stdout))?;
        if &actual != version {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The managed Git version does not match the requested version.",
            )
            .with_detail("expected", version.to_string())
            .with_detail("actual", actual.to_string()));
        }
        Ok(())
    }

    async fn build_source_runtime(
        &self,
        paths: &TorbenPaths,
        archive_path: &Path,
        final_path: &Path,
        staging: &Path,
        journal: &mut OperationJournal,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<PathBuf> {
        journal.record(
            OperationState::Running,
            "extract",
            "Extracting the verified Git source archive",
            Some(0.5),
        )?;
        let archive = archive_path.to_path_buf();
        let staging_task = staging.to_path_buf();
        let extraction_cancellation = cancellation.clone();
        let source_root = tokio::task::spawn_blocking(move || {
            extract_archive(
                &archive,
                ArchiveKind::TarXz,
                &staging_task,
                &extraction_cancellation,
            )
        })
        .await
        .map_err(archive_task_error)??;
        let configure = source_root.join("configure");
        ensure_regular_file(&configure)?;
        journal.record(
            OperationState::Running,
            "build",
            "Configuring the Git source build",
            Some(0.6),
        )?;
        run_process(
            &configure,
            &[
                OsString::from(format!("--prefix={}", final_path.display())),
                OsString::from("--without-tcltk"),
            ],
            Some(&source_root),
            cancellation,
        )
        .await?;
        let make = find_external_command("make", paths.data_dir())?;
        let jobs = std::thread::available_parallelism().map_or(1, usize::from);
        let make_options = [
            OsString::from("NO_GETTEXT=YesPlease"),
            OsString::from("NO_TCLTK=YesPlease"),
        ];
        journal.record(
            OperationState::Running,
            "build",
            "Building Git from the verified source archive",
            Some(0.7),
        )?;
        run_process(
            &make,
            &[
                vec![OsString::from(format!("-j{jobs}"))],
                make_options.to_vec(),
            ]
            .concat(),
            Some(&source_root),
            cancellation,
        )
        .await?;
        let install_root = staging.join("install-root");
        std::fs::create_dir_all(&install_root).map_err(io_error)?;
        journal.record(
            OperationState::Running,
            "build",
            "Installing the Git build into transaction staging",
            Some(0.78),
        )?;
        run_process(
            &make,
            &[
                vec![
                    OsString::from("install"),
                    OsString::from(format!("DESTDIR={}", install_root.display())),
                ],
                make_options.to_vec(),
            ]
            .concat(),
            Some(&source_root),
            cancellation,
        )
        .await?;
        let relative_prefix = final_path.strip_prefix(Path::new("/")).map_err(|_| {
            TorbenError::new(
                "git_install_prefix_invalid",
                "The managed Git prefix is not an absolute Unix path.",
            )
        })?;
        let runtime = install_root.join(relative_prefix);
        ensure_regular_directory(&runtime)?;
        Ok(runtime)
    }

    async fn windows_release(&self) -> TorbenResult<GitHubRelease> {
        let bytes = self
            .fetch_limited(
                &self.windows_api,
                MAX_METADATA_BYTES,
                RequestKind::GitHubApi,
                None,
            )
            .await?;
        serde_json::from_slice(&bytes).map_err(|error| {
            TorbenError::new(
                "git_metadata_invalid",
                "The Git for Windows release metadata is not valid JSON.",
            )
            .with_detail("reason", error.to_string())
        })
    }

    async fn fetch_kernel_index(&self) -> TorbenResult<String> {
        let bytes = self
            .fetch_limited(
                &self.kernel_base,
                MAX_METADATA_BYTES,
                RequestKind::Kernel,
                None,
            )
            .await?;
        String::from_utf8(bytes).map_err(|error| {
            TorbenError::new(
                "git_metadata_invalid",
                "The kernel.org Git release index is not valid UTF-8.",
            )
            .with_detail("reason", error.to_string())
        })
    }

    async fn fetch_limited(
        &self,
        url: &Url,
        maximum: u64,
        kind: RequestKind,
        cancellation: Option<&CancellationProbe>,
    ) -> TorbenResult<Vec<u8>> {
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
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
        self.validate_response(&response, url, kind)?;
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
            if let Some(cancellation) = cancellation {
                cancellation.check()?;
            }
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

    async fn download_archive(
        &self,
        distribution: &GitDistribution,
        destination: &Path,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<()> {
        cancellation.check()?;
        let request_kind = match distribution.kind {
            GitInstallKind::WindowsMinGit => RequestKind::GitHubAsset,
            GitInstallKind::SourceArchive => RequestKind::Kernel,
        };
        let response = await_with_cancellation(
            async {
                self.client
                    .get(distribution.archive_url.clone())
                    .send()
                    .await
                    .map_err(network_error)
            },
            Some(cancellation),
        )
        .await?;
        self.validate_response(&response, &distribution.archive_url, request_kind)?;
        let response = response.error_for_status().map_err(network_error)?;
        if response
            .content_length()
            .is_some_and(|size| distribution.size.is_some_and(|expected| size != expected))
        {
            return Err(size_mismatch(distribution.size, response.content_length()));
        }
        let partial = destination.with_extension("partial");
        let mut file = tokio::fs::File::create(&partial).await.map_err(io_error)?;
        let mut received = 0_u64;
        let mut stream = response.bytes_stream();
        let result = async {
            while let Some(chunk) = await_with_cancellation(
                async { stream.next().await.transpose().map_err(network_error) },
                Some(cancellation),
            )
            .await?
            {
                cancellation.check()?;
                received = received
                    .checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
                    .ok_or_else(|| resource_too_large(MAX_ARCHIVE_BYTES))?;
                if received > MAX_ARCHIVE_BYTES
                    || distribution
                        .size
                        .is_some_and(|expected| received > expected)
                {
                    return Err(size_mismatch(distribution.size, Some(received)));
                }
                file.write_all(&chunk).await.map_err(io_error)?;
            }
            file.flush().await.map_err(io_error)?;
            file.sync_all().await.map_err(io_error)?;
            drop(file);
            if distribution
                .size
                .is_some_and(|expected| received != expected)
            {
                return Err(size_mismatch(distribution.size, Some(received)));
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
            let _ = tokio::fs::remove_file(&partial).await;
        }
        result
    }

    fn validate_response(
        &self,
        response: &reqwest::Response,
        requested: &Url,
        kind: RequestKind,
    ) -> TorbenResult<()> {
        if self.allow_fixture_http {
            if same_origin(response.url(), requested) {
                return Ok(());
            }
            return Err(unexpected_origin(response.url()));
        }
        let valid = match kind {
            RequestKind::GitHubApi => {
                response.url().scheme() == "https"
                    && response.url().host_str() == Some("api.github.com")
            }
            RequestKind::GitHubAsset => {
                response.url().scheme() == "https"
                    && matches!(
                        response.url().host_str(),
                        Some("github.com" | "release-assets.githubusercontent.com")
                    )
            }
            RequestKind::Kernel => {
                response.url().scheme() == "https"
                    && response.url().host_str() == Some("www.kernel.org")
                    && response.url().path().starts_with("/pub/software/scm/git/")
            }
        };
        if valid {
            Ok(())
        } else {
            Err(unexpected_origin(response.url()))
        }
    }
}

fn windows_distribution(
    release: &GitHubRelease,
    architecture: &str,
    fixture_origin: Option<&Url>,
) -> TorbenResult<GitDistribution> {
    if release.draft || release.prerelease {
        return Err(metadata_invalid("release stability"));
    }
    let version = parse_windows_tag(&release.tag_name)?;
    let expected_name = format!("Git for Windows {}", release.tag_name);
    let expected_html = format!(
        "https://github.com/git-for-windows/git/releases/tag/{}",
        release.tag_name
    );
    if release.name != expected_name || release.html_url != expected_html {
        return Err(metadata_invalid("release identity"));
    }
    let base = version.as_semver();
    let patch_level = base
        .build
        .as_str()
        .strip_prefix("windows.")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| metadata_invalid("Windows patch level"))?;
    let asset_version = format!(
        "{}.{}.{}.{}",
        base.major, base.minor, base.patch, patch_level
    );
    let suffix = match architecture {
        "x86_64" => "64-bit.zip",
        "aarch64" => "arm64.zip",
        other => return Err(platform_error("architecture", other)),
    };
    let archive_name = format!("MinGit-{asset_version}-{suffix}");
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == archive_name)
        .ok_or_else(|| {
            TorbenError::new(
                "git_archive_missing",
                "The official Git for Windows release has no matching MinGit archive.",
            )
            .with_detail("archive", &archive_name)
        })?;
    let archive_url = Url::parse(&asset.browser_download_url).map_err(url_error)?;
    let expected_path = format!(
        "/git-for-windows/git/releases/download/{}/{}",
        release.tag_name, archive_name
    );
    let archive_origin_valid = fixture_origin.map_or_else(
        || {
            archive_url.scheme() == "https"
                && archive_url.host_str() == Some("github.com")
                && archive_url.path() == expected_path
        },
        |origin| {
            same_origin(&archive_url, origin)
                && archive_url.path().ends_with(&format!("/{archive_name}"))
        },
    );
    if asset.state != "uploaded"
        || asset.size == 0
        || asset.size > MAX_ARCHIVE_BYTES
        || !archive_origin_valid
    {
        return Err(metadata_invalid("Windows archive"));
    }
    let checksum = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .filter(|digest| is_sha256(digest))
        .ok_or_else(|| metadata_invalid("Windows archive digest"))?
        .to_ascii_lowercase();
    Ok(GitDistribution {
        version,
        released_at: release.published_at.clone(),
        archive_name,
        archive_url,
        checksum,
        size: Some(asset.size),
        signature_url: None,
        kind: GitInstallKind::WindowsMinGit,
    })
}

fn parse_windows_tag(tag: &str) -> TorbenResult<ExactVersion> {
    let raw = tag
        .strip_prefix('v')
        .ok_or_else(|| metadata_invalid("Windows release tag"))?;
    let (base, patch_level) = raw
        .split_once(".windows.")
        .ok_or_else(|| metadata_invalid("Windows release tag"))?;
    if patch_level.is_empty() || !patch_level.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(metadata_invalid("Windows release patch level"));
    }
    let version = ExactVersion::from_str(&format!("{base}+windows.{patch_level}"))?;
    if version.as_semver().major != 2 || !version.as_semver().pre.is_empty() {
        return Err(metadata_invalid("Windows Git version"));
    }
    Ok(version)
}

fn parse_kernel_index(index: &str) -> TorbenResult<Vec<(ExactVersion, String)>> {
    let mut versions = Vec::new();
    for line in index.lines() {
        let Some(href_start) = line.find("<a href=\"git-") else {
            continue;
        };
        let href = &line[href_start + 9..];
        let Some(href_end) = href.find('"') else {
            continue;
        };
        let filename = &href[..href_end];
        let expected_anchor = format!(">{filename}</a>");
        let Some(anchor_end) = line.find(&expected_anchor) else {
            continue;
        };
        let Some(raw_version) = filename
            .strip_prefix("git-")
            .and_then(|value| value.strip_suffix(".tar.xz"))
        else {
            continue;
        };
        let Ok(version) = ExactVersion::from_str(raw_version) else {
            continue;
        };
        if version.as_semver().major != 2 || !version.as_semver().pre.is_empty() {
            continue;
        }
        let metadata = &line[anchor_end + expected_anchor.len()..];
        let Some(raw_date) = metadata.split_whitespace().next() else {
            continue;
        };
        let Some(released_at) = normalize_kernel_date(raw_date) else {
            continue;
        };
        versions.push((version, released_at));
    }
    versions.sort_by(|left, right| right.0.cmp(&left.0));
    versions.dedup_by(|left, right| left.0 == right.0);
    if versions.is_empty() {
        return Err(TorbenError::new(
            "git_metadata_invalid",
            "The kernel.org Git index contains no stable release archives.",
        ));
    }
    Ok(versions)
}

fn normalize_kernel_date(value: &str) -> Option<String> {
    let mut parts = value.split('-');
    let day = parts.next()?.parse::<u8>().ok()?;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = parts.next()?.parse::<u16>().ok()?;
    if parts.next().is_some() || day == 0 || day > 31 || year < 2005 {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02}"))
}

fn checksum_for(manifest: &str, archive_name: &str) -> TorbenResult<String> {
    manifest
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?;
            let name = fields.next()?;
            (fields.next().is_none() && name == archive_name && is_sha256(hash))
                .then(|| hash.to_ascii_lowercase())
        })
        .ok_or_else(|| {
            TorbenError::new(
                "archive_checksum_missing",
                "The Git archive is missing from the signed kernel.org checksum manifest.",
            )
            .with_detail("archive", archive_name)
        })
}

fn parse_git_version(output: &str) -> TorbenResult<ExactVersion> {
    let raw = output
        .trim()
        .strip_prefix("git version ")
        .and_then(|value| value.split_whitespace().next())
        .ok_or_else(|| health_output_invalid(output))?;
    if let Some((base, patch_level)) = raw.split_once(".windows.") {
        return ExactVersion::from_str(&format!("{base}+windows.{patch_level}"))
            .map_err(|_| health_output_invalid(output));
    }
    ExactVersion::from_str(raw).map_err(|_| health_output_invalid(output))
}

fn find_external_command(command: &str, managed_root: &Path) -> TorbenResult<PathBuf> {
    for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
        let candidate = directory.join(command);
        let Ok(canonical) = std::fs::canonicalize(candidate) else {
            continue;
        };
        if !canonical.starts_with(managed_root) && ensure_regular_file(&canonical).is_ok() {
            return Ok(canonical);
        }
    }
    Err(TorbenError::new(
        "external_command_unavailable",
        "A required Git source-build command is unavailable.",
    )
    .with_detail("command", command))
}

async fn run_process(
    executable: &Path,
    arguments: &[OsString],
    current_directory: Option<&Path>,
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
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
    ] {
        command.env_remove(variable);
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
    cancellation: Option<&CancellationProbe>,
) -> TorbenResult<T>
where
    F: Future<Output = TorbenResult<T>>,
{
    let Some(cancellation) = cancellation else {
        return future.await;
    };
    tokio::pin!(future);
    loop {
        tokio::select! {
            result = &mut future => return result,
            () = tokio::time::sleep(PROCESS_POLL_INTERVAL) => cancellation.check()?,
        }
    }
}

fn ensure_regular_file(path: &Path) -> TorbenResult<()> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TorbenError::new(
            "git_path_invalid",
            "A Git transaction file is not a regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    Ok(())
}

fn ensure_regular_directory(path: &Path) -> TorbenResult<()> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TorbenError::new(
            "git_path_invalid",
            "A Git transaction directory is not a regular directory.",
        )
        .with_detail("path", path.display().to_string()));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
        .to_string()
}

fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested Git version was not found in the official stable catalog.",
    )
    .with_detail("requested", requested)
}

fn health_output_invalid(output: &str) -> TorbenError {
    TorbenError::new(
        "health_check_output_invalid",
        "The Git command returned an invalid version string.",
    )
    .with_detail(
        "actual",
        output.trim().chars().take(256).collect::<String>(),
    )
}

fn invalid_plan(reason: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The Git plugin returned an unsafe or inconsistent install plan.",
    )
    .with_detail("reason", reason)
}

fn metadata_invalid(field: &str) -> TorbenError {
    TorbenError::new(
        "git_metadata_invalid",
        "The official Git metadata contains an invalid field.",
    )
    .with_detail("field", field)
}

fn platform_error(field: &str, value: &str) -> TorbenError {
    TorbenError::new(
        "platform_not_supported",
        "Git is not available for this platform target.",
    )
    .with_detail(field, value)
}

fn resource_too_large(maximum: u64) -> TorbenError {
    TorbenError::new(
        "git_resource_too_large",
        "A Git metadata or archive response exceeds the allowed size.",
    )
    .with_detail("maximumBytes", maximum.to_string())
}

fn size_mismatch(expected: Option<u64>, actual: Option<u64>) -> TorbenError {
    TorbenError::new(
        "archive_size_mismatch",
        "The Git archive size does not match official metadata.",
    )
    .with_detail(
        "expectedBytes",
        expected.map_or_else(|| "bounded".to_owned(), |value| value.to_string()),
    )
    .with_detail(
        "actualBytes",
        actual.map_or_else(|| "unknown".to_owned(), |value| value.to_string()),
    )
}

fn unexpected_origin(url: &Url) -> TorbenError {
    TorbenError::new(
        "unexpected_download_origin",
        "An official Git request changed to an untrusted network origin.",
    )
    .with_detail("url", url.to_string())
}

fn archive_task_error(error: tokio::task::JoinError) -> TorbenError {
    TorbenError::new(
        "archive_task_failed",
        "The Git archive extraction task failed.",
    )
    .with_detail("reason", error.to_string())
}

fn process_start_error(path: &Path, error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "git_build_start_failed",
        "Could not start a Git source-build command.",
    )
    .with_detail("path", path.display().to_string())
    .with_detail("reason", error.to_string())
}

fn process_failure(path: &Path, status: std::process::ExitStatus) -> TorbenError {
    TorbenError::new("git_build_failed", "A Git source-build command failed.")
        .with_detail("path", path.display().to_string())
        .with_detail("status", status.to_string())
}

fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new("network_error", "An official Git request failed.")
        .with_detail("reason", error.to_string())
}

fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new("git_url_invalid", "An official Git URL is invalid.")
        .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new("filesystem_error", "A Git filesystem operation failed.")
        .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::{Read as _, Write as _},
        net::TcpListener,
        str::FromStr,
        sync::Arc,
        thread,
    };

    #[cfg(windows)]
    use sha2::{Digest, Sha256};
    #[cfg(windows)]
    use std::collections::BTreeMap;
    #[cfg(windows)]
    use tempfile::tempdir;
    use torben_contracts::ExactVersion;
    #[cfg(windows)]
    use torben_contracts::{
        AppId, OperationKind, SourceId,
        plugin::{InstallPlan, InstallStep},
    };

    use super::*;
    #[cfg(windows)]
    use crate::{
        StateStore, TorbenCore, bundled_shim::BundledShim, node_plugin::BundledPlugin,
        operation::OperationJournal,
    };

    struct AcceptingChecksumVerifier;

    impl GitChecksumVerifier for AcceptingChecksumVerifier {
        fn verify(&self, manifest: &[u8]) -> TorbenResult<String> {
            String::from_utf8(manifest.to_vec()).map_err(|error| {
                TorbenError::new("fixture_invalid", "The fixture is not UTF-8.")
                    .with_detail("reason", error.to_string())
            })
        }
    }

    #[test]
    fn parses_git_for_windows_release_and_pins_the_asset_digest() {
        let release = windows_release_fixture(
            "https://github.com/git-for-windows/git/releases/download/v2.55.0.windows.5/MinGit-2.55.0.5-64-bit.zip",
            58_000_000,
            &"11".repeat(32),
        );
        let distribution = windows_distribution(&release, "x86_64", None).unwrap();

        assert_eq!(distribution.version.to_string(), "2.55.0+windows.5");
        assert_eq!(distribution.archive_name, "MinGit-2.55.0.5-64-bit.zip");
        assert_eq!(distribution.checksum, "11".repeat(32));
        assert_eq!(distribution.kind, GitInstallKind::WindowsMinGit);
    }

    #[test]
    fn rejects_git_for_windows_release_without_a_sha256_digest() {
        let mut release = windows_release_fixture(
            "https://github.com/git-for-windows/git/releases/download/v2.55.0.windows.5/MinGit-2.55.0.5-64-bit.zip",
            58_000_000,
            &"11".repeat(32),
        );
        release.assets[0].digest = None;

        let error = windows_distribution(&release, "x86_64", None).unwrap_err();
        assert_eq!(error.code, "git_metadata_invalid");
        assert_eq!(
            error.details.get("field").map(String::as_str),
            Some("Windows archive digest")
        );
    }

    #[test]
    fn parses_stable_kernel_archives_and_normalizes_release_dates() {
        let index = r#"
<a href="git-2.54.0.tar.xz">git-2.54.0.tar.xz</a> 20-Apr-2026 15:21 8M
<a href="git-2.55.0.rc1.tar.xz">git-2.55.0.rc1.tar.xz</a> 17-Jun-2026 16:27 8M
<a href="git-2.55.0.tar.xz">git-2.55.0.tar.xz</a> 29-Jun-2026 16:55 8M
"#;

        let versions = parse_kernel_index(index).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].0.to_string(), "2.55.0");
        assert_eq!(versions[0].1, "2026-06-29");
        assert_eq!(versions[1].0.to_string(), "2.54.0");
    }

    #[test]
    fn parses_upstream_and_git_for_windows_version_output() {
        assert_eq!(
            parse_git_version("git version 2.55.0\n")
                .unwrap()
                .to_string(),
            "2.55.0"
        );
        assert_eq!(
            parse_git_version("git version 2.55.0.windows.5\n")
                .unwrap()
                .to_string(),
            "2.55.0+windows.5"
        );
        assert_eq!(
            parse_git_version("Apple Git").unwrap_err().code,
            "health_check_output_invalid"
        );
    }

    #[tokio::test]
    async fn local_kernel_catalog_resolves_and_pins_the_signed_checksum() {
        let index =
            b"<a href=\"git-2.55.0.tar.xz\">git-2.55.0.tar.xz</a> 29-Jun-2026 16:55 8M\n".to_vec();
        let manifest = format!("{}  git-2.55.0.tar.xz\n", "22".repeat(32)).into_bytes();
        let (base, server) = fixture_server(vec![
            ("/kernel/".to_owned(), index.clone()),
            ("/kernel/".to_owned(), index),
            ("/kernel/sha256sums.asc".to_owned(), manifest),
        ]);
        let provider = GitProvider::with_fixture(
            base.join("windows/latest").unwrap(),
            base.join("kernel/").unwrap(),
            Arc::new(AcceptingChecksumVerifier),
        )
        .unwrap();

        let versions = provider
            .list_versions_for_target("linux", "x86_64")
            .await
            .unwrap();
        let distribution = provider
            .distribution_for_target(
                &ExactVersion::from_str("2.55.0").unwrap(),
                "linux",
                "x86_64",
            )
            .await
            .unwrap();
        server.join().unwrap();

        assert_eq!(versions[0].version.to_string(), "2.55.0");
        assert_eq!(distribution.checksum, "22".repeat(32));
        assert_eq!(distribution.kind, GitInstallKind::SourceArchive);
        assert_eq!(
            distribution.archive_url.as_str(),
            format!("{base}kernel/git-2.55.0.tar.xz")
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_mingit_fixture_installs_health_checks_and_commits() {
        let root = tempdir().unwrap();
        let fixture_git = compile_fixture_git(root.path());
        let archive = mingit_archive(&fixture_git);
        let checksum = hex::encode(Sha256::digest(&archive));
        let (base, server) = fixture_server_with_release(archive.clone(), &checksum, 2);
        let provider = GitProvider::with_fixture(
            base.join("windows/latest").unwrap(),
            base.join("kernel/").unwrap(),
            Arc::new(AcceptingChecksumVerifier),
        )
        .unwrap();
        let version = ExactVersion::from_str("2.55.0+windows.5").unwrap();
        let distribution = provider
            .distribution_for_target(&version, "windows", "x86_64")
            .await
            .unwrap();
        let plan = InstallPlan {
            app_id: AppId::new("git").unwrap(),
            version: version.clone(),
            source_id: SourceId::new("git.official").unwrap(),
            steps: vec![
                InstallStep::Download {
                    url: distribution.archive_url.to_string(),
                    destination_name: distribution.archive_name.clone(),
                },
                InstallStep::VerifySha256 {
                    archive_name: distribution.archive_name.clone(),
                    expected: distribution.checksum,
                },
                InstallStep::ExtractArchive {
                    archive_name: distribution.archive_name,
                    strip_components: 0,
                },
                InstallStep::HealthCheck {
                    executable: "git".to_owned(),
                    arguments: vec!["--version".to_owned()],
                    expected_output: version.to_string(),
                },
                InstallStep::CreateShims {
                    commands: vec!["git".to_owned()],
                },
            ],
            metadata: BTreeMap::from([
                ("target".to_owned(), "windows-x86_64".to_owned()),
                ("installMethod".to_owned(), "mingit_archive".to_owned()),
            ]),
        };
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let app_id = AppId::new("git").unwrap();
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

        provider.health_check(&record).unwrap();
        assert!(
            provider
                .command_path(Path::new(&record.install_path), "git")
                .unwrap()
                .is_file()
        );
        assert_eq!(record.version, version);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_fixture_completes_core_install_select_and_uninstall_transaction() {
        let root = tempdir().unwrap();
        let fixture_git = compile_fixture_git(root.path());
        let archive = mingit_archive(&fixture_git);
        let checksum = hex::encode(Sha256::digest(&archive));
        let (base, server) = fixture_server_with_release(archive, &checksum, 1);
        let provider = GitProvider::with_fixture(
            base.join("windows/latest").unwrap(),
            base.join("kernel/").unwrap(),
            Arc::new(AcceptingChecksumVerifier),
        )
        .unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let version = ExactVersion::from_str("2.55.0+windows.5").unwrap();
        let final_path = paths.app_version_dir("git", &version.to_string());
        let plugin_script = root.path().join("git-plugin.cmd");
        let asset_url = base.join("assets/MinGit-2.55.0.5-64-bit.zip").unwrap();
        std::fs::write(
            &plugin_script,
            windows_plugin_fixture_script(&version, &final_path, &asset_url, &checksum),
        )
        .unwrap();
        let mut core = TorbenCore::open(paths).unwrap();
        core.git = provider;
        core.git_plugin = BundledPlugin::git_from_executable(plugin_script);
        core.bundled_shim = BundledShim::from_executable(fixture_git);
        let app_id = AppId::new("git").unwrap();

        let installed = core.install(&app_id, "current").await.unwrap();
        server.join().unwrap();
        core.select(&app_id, &version).await.unwrap();
        let command_available = core.executable_for(&app_id, "git").unwrap().is_file();
        core.clear_selection(&app_id).unwrap();
        core.uninstall(&app_id, &version).await.unwrap();

        assert_eq!(installed.version, version);
        assert!(command_available);
        assert!(core.installed().unwrap().is_empty());
        assert!(!Path::new(&installed.install_path).exists());
    }

    fn windows_release_fixture(url: &str, size: u64, checksum: &str) -> GitHubRelease {
        GitHubRelease {
            tag_name: "v2.55.0.windows.5".to_owned(),
            name: "Git for Windows v2.55.0.windows.5".to_owned(),
            html_url: "https://github.com/git-for-windows/git/releases/tag/v2.55.0.windows.5"
                .to_owned(),
            draft: false,
            prerelease: false,
            published_at: "2026-08-20T16:21:31Z".to_owned(),
            assets: vec![GitHubAsset {
                name: "MinGit-2.55.0.5-64-bit.zip".to_owned(),
                state: "uploaded".to_owned(),
                size,
                digest: Some(format!("sha256:{checksum}")),
                browser_download_url: url.to_owned(),
            }],
        }
    }

    #[cfg(windows)]
    fn compile_fixture_git(directory: &Path) -> PathBuf {
        let source = directory.join("fixture-git.rs");
        let executable = directory.join("git.exe");
        std::fs::write(
            &source,
            r#"fn main() { println!("git version 2.55.0.windows.5"); }"#,
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
    fn mingit_archive(executable: &Path) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("cmd/git.exe", options).unwrap();
        writer
            .write_all(&std::fs::read(executable).unwrap())
            .unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[cfg(windows)]
    fn fixture_server_with_release(
        archive: Vec<u8>,
        checksum: &str,
        api_requests: usize,
    ) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{address}/")).unwrap();
        let asset_url = base.join("assets/MinGit-2.55.0.5-64-bit.zip").unwrap();
        let release = serde_json::to_vec(&serde_json::json!({
            "tag_name": "v2.55.0.windows.5",
            "name": "Git for Windows v2.55.0.windows.5",
            "html_url": "https://github.com/git-for-windows/git/releases/tag/v2.55.0.windows.5",
            "draft": false,
            "prerelease": false,
            "published_at": "2026-08-20T16:21:31Z",
            "assets": [{
                "name": "MinGit-2.55.0.5-64-bit.zip",
                "state": "uploaded",
                "size": archive.len(),
                "digest": format!("sha256:{checksum}"),
                "browser_download_url": asset_url
            }]
        }))
        .unwrap();
        let mut routes = (0..api_requests)
            .map(|_| ("/windows/latest".to_owned(), release.clone()))
            .collect::<Vec<_>>();
        routes.push(("/assets/MinGit-2.55.0.5-64-bit.zip".to_owned(), archive));
        let server = spawn_fixture_server(listener, routes);
        (base, server)
    }

    #[cfg(windows)]
    fn windows_plugin_fixture_script(
        version: &ExactVersion,
        install_path: &Path,
        archive_url: &Url,
        checksum: &str,
    ) -> String {
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": torben_contracts::plugin::PLUGIN_PROTOCOL_VERSION,
                "pluginId": "app.torben.plugin.git",
                "pluginVersion": env!("CARGO_PKG_VERSION"),
                "applications": [{
                    "id": "git",
                    "displayName": "Git",
                    "summary": "fixture",
                    "categories": ["tool"],
                    "capabilities": ["versions", "install", "select", "uninstall"],
                    "sources": [{
                        "id": "git.official",
                        "displayName": "Official Git distribution",
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
                "appId": "git",
                "version": version,
                "sourceId": "git.official",
                "steps": [
                    {
                        "type": "download",
                        "url": archive_url,
                        "destination_name": "MinGit-2.55.0.5-64-bit.zip"
                    },
                    {
                        "type": "verify_sha256",
                        "archive_name": "MinGit-2.55.0.5-64-bit.zip",
                        "expected": checksum
                    },
                    {
                        "type": "extract_archive",
                        "archive_name": "MinGit-2.55.0.5-64-bit.zip",
                        "strip_components": 0
                    },
                    {
                        "type": "health_check",
                        "executable": "git",
                        "arguments": ["--version"],
                        "expected_output": version
                    },
                    { "type": "create_shims", "commands": ["git"] }
                ],
                "metadata": {
                    "target": "windows-x86_64",
                    "installMethod": "mingit_archive"
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
                "appId": "git",
                "version": version,
                "sourceId": "git.official",
                "installPath": install_path.display().to_string(),
                "preserveUserData": true
            }
        })
        .to_string();
        format!(
            "@echo off\r\n:loop\r\nset request=\r\nset /p request=\r\nif errorlevel 1 exit /b 0\r\necho %request%| findstr /c:\"initialize\" >nul && (echo {initialize}& goto loop)\r\necho %request%| findstr /c:\"version.resolve\" >nul && (echo {resolved}& goto loop)\r\necho %request%| findstr /c:\"uninstall.plan\" >nul && (echo {uninstall}& goto loop)\r\necho %request%| findstr /c:\"install.plan\" >nul && (echo {plan}& goto loop)\r\necho %request%| findstr /c:\"health.check\" >nul && (echo {health}& goto loop)\r\nexit /b 1\r\n"
        )
    }

    fn fixture_server(routes: Vec<(String, Vec<u8>)>) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{address}/")).unwrap();
        let server = spawn_fixture_server(listener, routes);
        (base, server)
    }

    fn spawn_fixture_server(
        listener: TcpListener,
        routes: Vec<(String, Vec<u8>)>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let mut routes = routes.into_iter().collect::<VecDeque<_>>();
            while let Some((expected_path, body)) = routes.pop_front() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 4096];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap();
                assert_eq!(path, expected_path);
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        })
    }
}
