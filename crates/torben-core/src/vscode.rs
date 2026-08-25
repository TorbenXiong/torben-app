use std::{
    collections::BTreeSet,
    future::Future,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime},
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
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

const GITHUB_RELEASES_URL: &str = "https://api.github.com/repos/microsoft/vscode/releases/";
const UPDATE_VERSIONS_URL: &str = "https://update.code.visualstudio.com/api/versions/";
const MAX_RELEASES: usize = 5;
const MAX_METADATA_BYTES: u64 = 2 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const PROCESS_TIMEOUT: Duration = Duration::from_secs(8);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsCodeDistribution {
    pub version: ExactVersion,
    pub released_at: String,
    pub commit: String,
    pub platform: String,
    pub archive_name: String,
    pub archive_url: Url,
    pub checksum: String,
    pub archive_kind: ArchiveKind,
}

#[derive(Clone)]
pub struct VsCodeProvider {
    client: reqwest::Client,
    github_releases: Url,
    update_versions: Url,
    fixture_mode: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct GitHubRelease {
    tag_name: String,
    name: String,
    html_url: String,
    draft: bool,
    prerelease: bool,
    published_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateMetadata {
    url: String,
    name: String,
    version: String,
    product_version: String,
    hash: String,
    timestamp: i64,
    sha256hash: String,
}

#[derive(Debug, Clone)]
struct TargetArchive {
    platform: &'static str,
    kind: ArchiveKind,
    filename: FilenameRule,
}

#[derive(Debug, Clone)]
enum FilenameRule {
    Exact(String),
    Linux { architecture: &'static str },
}

#[derive(Debug, Clone, Copy)]
enum RequestKind {
    GitHub,
    Update,
    Download,
}

impl VsCodeProvider {
    pub fn official() -> TorbenResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
                .https_only(true)
                .build()
                .map_err(network_error)?,
            github_releases: Url::parse(GITHUB_RELEASES_URL).map_err(url_error)?,
            update_versions: Url::parse(UPDATE_VERSIONS_URL).map_err(url_error)?,
            fixture_mode: false,
        })
    }

    #[cfg(test)]
    fn with_fixture(github_releases: Url, update_versions: Url) -> TorbenResult<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .user_agent("Torben-App-Test")
                .build()
                .map_err(network_error)?,
            github_releases,
            update_versions,
            fixture_mode: true,
        })
    }

    pub async fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        let target = current_target_archive()?;
        let mut url = self.github_releases.clone();
        let normalized_path = url.path().trim_end_matches('/').to_owned();
        url.set_path(&normalized_path);
        url.query_pairs_mut()
            .append_pair("per_page", &MAX_RELEASES.to_string());
        let releases: Vec<GitHubRelease> = self.fetch_json(&url, RequestKind::GitHub).await?;
        let mut versions = Vec::new();
        for release in releases {
            let version = validate_release(&release)?;
            let metadata = self.metadata(&version, &target).await?;
            distribution_from(
                &version,
                release.published_at.clone(),
                &target,
                metadata,
                self.fixture_mode,
            )?;
            versions.push(VersionDescriptor {
                version,
                lts_name: None,
                released_at: release.published_at,
                recommended: versions.is_empty(),
            });
        }
        versions.sort_by(|left, right| right.version.cmp(&left.version));
        versions.truncate(MAX_RELEASES);
        if versions.is_empty() {
            return Err(TorbenError::new(
                "vscode_catalog_empty",
                "The official Visual Studio Code catalog contains no stable releases.",
            ));
        }
        if let Some(first) = versions.first_mut() {
            first.recommended = true;
        }
        Ok(versions)
    }

    pub async fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        if let Ok(version) = ExactVersion::from_str(requested) {
            self.distribution(&version).await?;
            return Ok(version);
        }
        if matches!(
            requested.trim().to_ascii_lowercase().as_str(),
            "current" | "latest"
        ) {
            return self
                .list_versions()
                .await?
                .into_iter()
                .next()
                .map(|item| item.version)
                .ok_or_else(|| version_not_found(requested));
        }
        Err(TorbenError::new(
            "version_alias_not_found",
            "Use an exact Visual Studio Code version, 'current', or 'latest'.",
        )
        .with_detail("requested", requested))
    }

    pub async fn distribution(&self, version: &ExactVersion) -> TorbenResult<VsCodeDistribution> {
        let target = current_target_archive()?;
        let release = self.release(version).await?;
        let metadata = self.metadata(version, &target).await?;
        distribution_from(
            version,
            release.published_at,
            &target,
            metadata,
            self.fixture_mode,
        )
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
                "The final Visual Studio Code installation directory already exists.",
            ));
        }
        let download_dir = paths.download_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&download_dir).map_err(io_error)?;
        let archive_path = download_dir.join(&distribution.archive_name);
        let cached = archive_path.is_file()
            && sha256_file_checked(&archive_path, Some(&cancellation))? == distribution.checksum;
        if !cached {
            journal.record(
                OperationState::Running,
                "download",
                "Downloading the official Visual Studio Code archive",
                Some(0.2),
            )?;
            self.download_archive(&distribution, &archive_path, &cancellation)
                .await?;
        }
        journal.record(
            OperationState::Running,
            "verify",
            "Verifying the Microsoft Visual Studio Code SHA-256 checksum",
            Some(0.42),
        )?;
        let actual = sha256_file_checked(&archive_path, Some(&cancellation))?;
        if actual != distribution.checksum {
            return Err(TorbenError::new(
                "archive_hash_mismatch",
                "The Visual Studio Code archive does not match Microsoft metadata.",
            )
            .with_detail("expected", distribution.checksum)
            .with_detail("actual", actual));
        }
        cancellation.check()?;
        let staging =
            paths
                .staging_dir()
                .join(format!("install-{}-{}", app_id, journal.operation_id()));
        let extracted = staging.join("extracted");
        std::fs::create_dir_all(&extracted).map_err(io_error)?;
        journal.record(
            OperationState::Running,
            "extract",
            "Extracting Visual Studio Code into transaction staging",
            Some(0.62),
        )?;
        let archive = archive_path.clone();
        let kind = distribution.archive_kind;
        let extraction_target = extracted.clone();
        let extraction_cancellation = cancellation.clone();
        tokio::task::spawn_blocking(move || {
            extract_archive_contents(&archive, kind, &extraction_target, &extraction_cancellation)
        })
        .await
        .map_err(archive_task_error)??;
        let runtime = runtime_root(&extracted)?;
        journal.record(
            OperationState::Running,
            "health_check",
            "Checking the extracted Visual Studio Code command",
            Some(0.84),
        )?;
        self.health_check_path(&runtime, version).await?;
        cancellation.check()?;
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        std::fs::rename(&runtime, &final_path).map_err(|error| {
            TorbenError::new(
                "install_commit_failed",
                "Could not atomically commit the Visual Studio Code installation.",
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
        if command != "code" {
            return Err(TorbenError::new(
                "unsupported_command",
                "The Visual Studio Code plugin does not expose this command.",
            )
            .with_detail("command", command));
        }
        let path = install_path.join(command_relative_path());
        ensure_regular_file(&path)?;
        Ok(path)
    }

    pub async fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let names: &[&str] = if cfg!(windows) {
            &["code.exe", "code.cmd"]
        } else {
            &["code"]
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
            if canonical.starts_with(managed_root) || ensure_regular_file(&canonical).is_err() {
                continue;
            }
            let Ok(result) = tokio::time::timeout(
                PROCESS_TIMEOUT,
                tokio::process::Command::new(&canonical)
                    .args(["--version", "--disable-updates"])
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
            let Ok(version) = parse_version_output(&String::from_utf8_lossy(&output.stdout)) else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("vscode")?,
                version,
                source_id: SourceId::new("vscode.external")?,
                scope: InstallScope::External,
                install_path: canonical.display().to_string(),
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
    ) -> TorbenResult<VsCodeDistribution> {
        if app_id.as_str() != "vscode"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new("vscode.official")?
        {
            return Err(invalid_plan("identity or source owner"));
        }
        let expected = self.distribution(version).await?;
        if plan.metadata.get("target")
            != Some(&format!(
                "{}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
            || plan.metadata.get("platform") != Some(&expected.platform)
            || plan.metadata.get("commit") != Some(&expected.commit)
        {
            return Err(invalid_plan("target metadata"));
        }
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
                archive_name: extracted_archive,
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
            || checksum.to_ascii_lowercase() != expected.checksum
            || extracted_archive != &expected.archive_name
            || *strip_components != 0
            || executable != "code"
            || arguments.as_slice() != ["--version", "--disable-updates"]
            || expected_output != &version.to_string()
            || commands.as_slice() != ["code"]
        {
            return Err(invalid_plan("official distribution details"));
        }
        Ok(expected)
    }

    async fn health_check_path(
        &self,
        install_path: &Path,
        version: &ExactVersion,
    ) -> TorbenResult<()> {
        let executable = self.command_path(install_path, "code")?;
        let result = tokio::time::timeout(
            PROCESS_TIMEOUT,
            tokio::process::Command::new(&executable)
                .args(["--version", "--disable-updates"])
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| {
            TorbenError::new(
                "health_check_timeout",
                "The Visual Studio Code version check timed out.",
            )
        })?
        .map_err(|error| {
            TorbenError::new(
                "health_check_start_failed",
                "Could not start the managed Visual Studio Code command.",
            )
            .with_detail("path", executable.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
        if !result.status.success() {
            return Err(TorbenError::new(
                "health_check_failed",
                "The managed Visual Studio Code command returned an error.",
            )
            .with_detail("status", result.status.to_string()));
        }
        let actual = parse_version_output(&String::from_utf8_lossy(&result.stdout))?;
        if &actual != version {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The managed Visual Studio Code version does not match the requested version.",
            )
            .with_detail("expected", version.to_string())
            .with_detail("actual", actual.to_string()));
        }
        Ok(())
    }

    async fn release(&self, version: &ExactVersion) -> TorbenResult<GitHubRelease> {
        let url = self
            .github_releases
            .join(&format!("tags/{version}"))
            .map_err(url_error)?;
        let release: GitHubRelease = self.fetch_json(&url, RequestKind::GitHub).await?;
        let actual = validate_release(&release)?;
        if &actual != version {
            return Err(version_not_found(&version.to_string()));
        }
        Ok(release)
    }

    async fn metadata(
        &self,
        version: &ExactVersion,
        target: &TargetArchive,
    ) -> TorbenResult<UpdateMetadata> {
        let url = self
            .update_versions
            .join(&format!("{version}/{}/stable", target.platform))
            .map_err(url_error)?;
        self.fetch_json(&url, RequestKind::Update).await
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: &Url,
        kind: RequestKind,
    ) -> TorbenResult<T> {
        let bytes = self
            .fetch_limited(url, MAX_METADATA_BYTES, kind, None)
            .await?;
        serde_json::from_slice(&bytes).map_err(|error| {
            TorbenError::new(
                "vscode_metadata_invalid",
                "The Visual Studio Code metadata is not valid JSON.",
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
        distribution: &VsCodeDistribution,
        destination: &Path,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<()> {
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
        self.validate_response(&response, &distribution.archive_url, RequestKind::Download)?;
        let response = response.error_for_status().map_err(network_error)?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_ARCHIVE_BYTES)
        {
            return Err(resource_too_large(MAX_ARCHIVE_BYTES));
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
                received = received
                    .checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
                    .ok_or_else(|| resource_too_large(MAX_ARCHIVE_BYTES))?;
                if received > MAX_ARCHIVE_BYTES {
                    return Err(resource_too_large(MAX_ARCHIVE_BYTES));
                }
                file.write_all(&chunk).await.map_err(io_error)?;
            }
            file.flush().await.map_err(io_error)?;
            file.sync_all().await.map_err(io_error)?;
            drop(file);
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
        if self.fixture_mode {
            return same_origin(response.url(), requested)
                .then_some(())
                .ok_or_else(|| unexpected_origin(response.url()));
        }
        let valid = match kind {
            RequestKind::GitHub => {
                response.url().scheme() == "https"
                    && response.url().host_str() == Some("api.github.com")
                    && response
                        .url()
                        .path()
                        .starts_with("/repos/microsoft/vscode/releases")
            }
            RequestKind::Update => {
                response.url().scheme() == "https"
                    && response.url().host_str() == Some("update.code.visualstudio.com")
                    && response.url().path().starts_with("/api/versions/")
            }
            RequestKind::Download => {
                response.url().scheme() == "https"
                    && response.url().host_str() == Some("vscode.download.prss.microsoft.com")
                    && response
                        .url()
                        .path()
                        .starts_with("/dbazure/download/stable/")
            }
        };
        valid
            .then_some(())
            .ok_or_else(|| unexpected_origin(response.url()))
    }
}

fn validate_release(release: &GitHubRelease) -> TorbenResult<ExactVersion> {
    if release.draft || release.prerelease || release.name != release.tag_name {
        return Err(metadata_invalid("release stability or identity"));
    }
    let version = ExactVersion::from_str(&release.tag_name)?;
    if version.as_semver().major != 1
        || !version.as_semver().pre.is_empty()
        || !version.as_semver().build.is_empty()
    {
        return Err(metadata_invalid("release version"));
    }
    let expected = format!(
        "https://github.com/microsoft/vscode/releases/tag/{}",
        release.tag_name
    );
    if release.html_url != expected {
        return Err(metadata_invalid("release URL"));
    }
    Ok(version)
}

fn distribution_from(
    version: &ExactVersion,
    released_at: String,
    target: &TargetArchive,
    metadata: UpdateMetadata,
    fixture_mode: bool,
) -> TorbenResult<VsCodeDistribution> {
    if metadata.name != version.to_string()
        || metadata.product_version != version.to_string()
        || !is_hex(&metadata.version, 40)
        || !is_hex(&metadata.hash, 40)
        || !is_hex(&metadata.sha256hash, 64)
        || metadata.timestamp <= 0
    {
        return Err(metadata_invalid("update identity or integrity"));
    }
    let archive_url = Url::parse(&metadata.url).map_err(url_error)?;
    let archive_name = archive_url
        .path_segments()
        .and_then(Iterator::last)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| metadata_invalid("archive filename"))?
        .to_owned();
    if !fixture_mode {
        let expected_prefix = format!("/dbazure/download/stable/{}/", metadata.version);
        if archive_url.scheme() != "https"
            || archive_url.host_str() != Some("vscode.download.prss.microsoft.com")
            || !archive_url.path().starts_with(&expected_prefix)
        {
            return Err(metadata_invalid("archive URL"));
        }
    }
    if !target.filename.matches(&archive_name, version) {
        return Err(metadata_invalid("archive filename"));
    }
    Ok(VsCodeDistribution {
        version: version.clone(),
        released_at,
        commit: metadata.version,
        platform: target.platform.to_owned(),
        archive_name,
        archive_url,
        checksum: metadata.sha256hash.to_ascii_lowercase(),
        archive_kind: target.kind,
    })
}

impl FilenameRule {
    fn matches(&self, name: &str, version: &ExactVersion) -> bool {
        match self {
            Self::Exact(expected) => name == expected.replace("{version}", &version.to_string()),
            Self::Linux { architecture } => {
                let prefix = format!("code-stable-{architecture}-");
                name.strip_prefix(&prefix)
                    .and_then(|value| value.strip_suffix(".tar.gz"))
                    .is_some_and(|timestamp| {
                        !timestamp.is_empty() && timestamp.bytes().all(|byte| byte.is_ascii_digit())
                    })
                    && version.as_semver().major == 1
            }
        }
    }
}

fn current_target_archive() -> TorbenResult<TargetArchive> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Ok(TargetArchive {
            platform: "win32-x64-archive",
            kind: ArchiveKind::Zip,
            filename: FilenameRule::Exact("VSCode-win32-x64-{version}.zip".to_owned()),
        }),
        ("windows", "aarch64") => Ok(TargetArchive {
            platform: "win32-arm64-archive",
            kind: ArchiveKind::Zip,
            filename: FilenameRule::Exact("VSCode-win32-arm64-{version}.zip".to_owned()),
        }),
        ("linux", "x86_64") => Ok(TargetArchive {
            platform: "linux-x64",
            kind: ArchiveKind::TarGz,
            filename: FilenameRule::Linux {
                architecture: "x64",
            },
        }),
        ("linux", "aarch64") => Ok(TargetArchive {
            platform: "linux-arm64",
            kind: ArchiveKind::TarGz,
            filename: FilenameRule::Linux {
                architecture: "arm64",
            },
        }),
        ("macos", "x86_64") => Ok(TargetArchive {
            platform: "darwin",
            kind: ArchiveKind::Zip,
            filename: FilenameRule::Exact("VSCode-darwin.zip".to_owned()),
        }),
        ("macos", "aarch64") => Ok(TargetArchive {
            platform: "darwin-arm64",
            kind: ArchiveKind::Zip,
            filename: FilenameRule::Exact("VSCode-darwin-arm64.zip".to_owned()),
        }),
        (os, architecture) => Err(TorbenError::new(
            "platform_not_supported",
            "Visual Studio Code is not available for this platform target.",
        )
        .with_detail("os", os)
        .with_detail("architecture", architecture)),
    }
}

fn command_relative_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from("Code.exe")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("Visual Studio Code.app")
            .join("Contents")
            .join("Resources")
            .join("app")
            .join("bin")
            .join("code")
    } else {
        PathBuf::from("bin").join("code")
    }
}

fn runtime_root(extracted: &Path) -> TorbenResult<PathBuf> {
    if extracted.join(command_relative_path()).is_file() {
        return Ok(extracted.to_path_buf());
    }
    let directories = std::fs::read_dir(extracted)
        .map_err(io_error)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = path.symlink_metadata().ok()?;
            (!metadata.file_type().is_symlink() && metadata.is_dir()).then_some(path)
        })
        .collect::<Vec<_>>();
    if directories.len() == 1 && directories[0].join(command_relative_path()).is_file() {
        return Ok(directories[0].clone());
    }
    Err(TorbenError::new(
        "archive_layout_invalid",
        "The Visual Studio Code archive layout is invalid for this platform.",
    ))
}

fn parse_version_output(output: &str) -> TorbenResult<ExactVersion> {
    output
        .lines()
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .ok_or_else(|| health_output_invalid(output))
        .and_then(|line| ExactVersion::from_str(line).map_err(|_| health_output_invalid(output)))
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
            () = tokio::time::sleep(POLL_INTERVAL) => cancellation.check()?,
        }
    }
}

fn ensure_regular_file(path: &Path) -> TorbenResult<()> {
    let metadata = path.symlink_metadata().map_err(io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TorbenError::new(
            "vscode_path_invalid",
            "A Visual Studio Code transaction file is not a regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    Ok(())
}

fn is_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
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
        "The requested Visual Studio Code version is not a published stable release.",
    )
    .with_detail("requested", requested)
}

fn metadata_invalid(field: &str) -> TorbenError {
    TorbenError::new(
        "vscode_metadata_invalid",
        "The official Visual Studio Code metadata contains an invalid field.",
    )
    .with_detail("field", field)
}

fn health_output_invalid(output: &str) -> TorbenError {
    TorbenError::new(
        "health_check_output_invalid",
        "Visual Studio Code returned an invalid version string.",
    )
    .with_detail(
        "actual",
        output.trim().chars().take(256).collect::<String>(),
    )
}

fn invalid_plan(reason: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The Visual Studio Code plugin returned an unsafe or inconsistent install plan.",
    )
    .with_detail("reason", reason)
}

fn resource_too_large(maximum: u64) -> TorbenError {
    TorbenError::new(
        "vscode_resource_too_large",
        "A Visual Studio Code response exceeds the allowed size.",
    )
    .with_detail("maximumBytes", maximum.to_string())
}

fn unexpected_origin(url: &Url) -> TorbenError {
    TorbenError::new(
        "unexpected_download_origin",
        "A Visual Studio Code request changed to an untrusted network origin.",
    )
    .with_detail("url", url.to_string())
}

fn archive_task_error(error: tokio::task::JoinError) -> TorbenError {
    TorbenError::new(
        "archive_task_failed",
        "The Visual Studio Code archive extraction task failed.",
    )
    .with_detail("reason", error.to_string())
}

fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new(
        "network_error",
        "An official Visual Studio Code request failed.",
    )
    .with_detail("reason", error.to_string())
}

fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new(
        "vscode_url_invalid",
        "An official Visual Studio Code URL is invalid.",
    )
    .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "filesystem_error",
        "A Visual Studio Code filesystem operation failed.",
    )
    .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::VecDeque,
        io::{Read as _, Write as _},
        net::TcpListener,
        thread,
    };

    #[cfg(windows)]
    use sha2::{Digest, Sha256};
    #[cfg(windows)]
    use tempfile::tempdir;
    #[cfg(windows)]
    use torben_contracts::AppId;
    use torben_contracts::ExactVersion;

    use super::*;
    #[cfg(windows)]
    use crate::{TorbenCore, bundled_shim::BundledShim, node_plugin::BundledPlugin};

    #[test]
    fn validates_published_release_and_exact_microsoft_archive_metadata() {
        let version = ExactVersion::from_str("1.134.0").unwrap();
        let release = release_fixture(&version);
        let target = TargetArchive {
            platform: "win32-x64-archive",
            kind: ArchiveKind::Zip,
            filename: FilenameRule::Exact("VSCode-win32-x64-{version}.zip".to_owned()),
        };
        let metadata = metadata_fixture(
            &version,
            "https://vscode.download.prss.microsoft.com/dbazure/download/stable/110a328ea54b42367b803ec53ee0bf52ef26b419/VSCode-win32-x64-1.134.0.zip",
            &"11".repeat(32),
        );

        assert_eq!(validate_release(&release).unwrap(), version);
        let distribution =
            distribution_from(&version, release.published_at, &target, metadata, false).unwrap();
        assert_eq!(distribution.platform, "win32-x64-archive");
        assert_eq!(distribution.checksum, "11".repeat(32));
        assert_eq!(distribution.archive_kind, ArchiveKind::Zip);
    }

    #[test]
    fn rejects_mismatched_microsoft_product_version_and_filename() {
        let version = ExactVersion::from_str("1.134.0").unwrap();
        let target = TargetArchive {
            platform: "win32-x64-archive",
            kind: ArchiveKind::Zip,
            filename: FilenameRule::Exact("VSCode-win32-x64-{version}.zip".to_owned()),
        };
        let mut metadata = metadata_fixture(
            &version,
            "https://vscode.download.prss.microsoft.com/dbazure/download/stable/110a328ea54b42367b803ec53ee0bf52ef26b419/VSCode-win32-x64-1.134.0.zip",
            &"11".repeat(32),
        );
        metadata.product_version = "1.133.0".to_owned();
        assert_eq!(
            distribution_from(&version, String::new(), &target, metadata, false)
                .unwrap_err()
                .code,
            "vscode_metadata_invalid"
        );
        let metadata = metadata_fixture(
            &version,
            "https://vscode.download.prss.microsoft.com/dbazure/download/stable/110a328ea54b42367b803ec53ee0bf52ef26b419/VSCode-win32-arm64-1.134.0.zip",
            &"11".repeat(32),
        );
        assert_eq!(
            distribution_from(&version, String::new(), &target, metadata, false)
                .unwrap_err()
                .details
                .get("field")
                .map(String::as_str),
            Some("archive filename")
        );
    }

    #[test]
    fn parses_only_the_first_exact_version_line() {
        assert_eq!(
            parse_version_output("1.134.0\n110a328ea54b42367b803ec53ee0bf52ef26b419\nx64\n")
                .unwrap()
                .to_string(),
            "1.134.0"
        );
        assert_eq!(
            parse_version_output("Visual Studio Code").unwrap_err().code,
            "health_check_output_invalid"
        );
    }

    #[tokio::test]
    async fn local_catalog_requires_a_published_release_and_platform_metadata() {
        let version = ExactVersion::from_str("1.134.0").unwrap();
        let release = serde_json::to_vec(&vec![release_fixture(&version)]).unwrap();
        let (base, server) = fixture_server(vec![
            ("/github?per_page=5".to_owned(), release),
            (
                "/update/1.134.0/win32-x64-archive/stable".to_owned(),
                serde_json::to_vec(&metadata_fixture(
                    &version,
                    "http://127.0.0.1:1/assets/VSCode-win32-x64-1.134.0.zip",
                    &"11".repeat(32),
                ))
                .unwrap(),
            ),
        ]);
        let provider = VsCodeProvider::with_fixture(
            base.join("github/").unwrap(),
            base.join("update/").unwrap(),
        )
        .unwrap();

        let versions = provider.list_versions().await.unwrap();
        server.join().unwrap();

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version, version);
        assert!(versions[0].recommended);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_fixture_completes_core_install_select_and_uninstall_transaction() {
        let root = tempdir().unwrap();
        let fixture_code = compile_fixture_code(root.path());
        let archive = vscode_archive(&fixture_code);
        let checksum = hex::encode(Sha256::digest(&archive));
        let version = ExactVersion::from_str("1.134.0").unwrap();
        let (base, server) = vscode_install_server(&version, archive, &checksum);
        let provider = VsCodeProvider::with_fixture(
            base.join("github/").unwrap(),
            base.join("update/").unwrap(),
        )
        .unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let final_path = paths.app_version_dir("vscode", &version.to_string());
        let plugin_script = root.path().join("vscode-plugin.cmd");
        let asset_url = base.join("assets/VSCode-win32-x64-1.134.0.zip").unwrap();
        std::fs::write(
            &plugin_script,
            windows_plugin_fixture_script(&version, &final_path, &asset_url, &checksum),
        )
        .unwrap();
        let mut core = TorbenCore::open(paths).unwrap();
        core.vscode = provider;
        core.vscode_plugin = BundledPlugin::vscode_from_executable(plugin_script);
        core.bundled_shim = BundledShim::from_executable(fixture_code);
        let app_id = AppId::new("vscode").unwrap();

        let installed = core.install(&app_id, "current").await.unwrap();
        server.join().unwrap();
        core.select(&app_id, &version).await.unwrap();
        let command_available = core.executable_for(&app_id, "code").unwrap().is_file();
        core.clear_selection(&app_id).unwrap();
        core.uninstall(&app_id, &version).await.unwrap();

        assert_eq!(installed.version, version);
        assert!(command_available);
        assert!(core.installed().unwrap().is_empty());
        assert!(!Path::new(&installed.install_path).exists());
    }

    fn release_fixture(version: &ExactVersion) -> GitHubRelease {
        GitHubRelease {
            tag_name: version.to_string(),
            name: version.to_string(),
            html_url: format!("https://github.com/microsoft/vscode/releases/tag/{version}"),
            draft: false,
            prerelease: false,
            published_at: "2026-08-19T09:08:11Z".to_owned(),
        }
    }

    fn metadata_fixture(version: &ExactVersion, url: &str, checksum: &str) -> UpdateMetadata {
        UpdateMetadata {
            url: url.to_owned(),
            name: version.to_string(),
            version: "110a328ea54b42367b803ec53ee0bf52ef26b419".to_owned(),
            product_version: version.to_string(),
            hash: "3f42ac7b4095c36205e0ac5122ef335dc37873c9".to_owned(),
            timestamp: 1_787_078_154_886,
            sha256hash: checksum.to_owned(),
        }
    }

    #[cfg(windows)]
    fn compile_fixture_code(directory: &Path) -> PathBuf {
        let source = directory.join("fixture-code.rs");
        let executable = directory.join("Code.exe");
        std::fs::write(
            &source,
            r#"fn main() { println!("1.134.0\n110a328ea54b42367b803ec53ee0bf52ef26b419\nx64"); }"#,
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
    fn vscode_archive(executable: &Path) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        writer.start_file("Code.exe", options).unwrap();
        writer
            .write_all(&std::fs::read(executable).unwrap())
            .unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[cfg(windows)]
    fn vscode_install_server(
        version: &ExactVersion,
        archive: Vec<u8>,
        checksum: &str,
    ) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{address}/")).unwrap();
        let asset_url = base.join("assets/VSCode-win32-x64-1.134.0.zip").unwrap();
        let release = serde_json::to_vec(&release_fixture(version)).unwrap();
        let metadata =
            serde_json::to_vec(&metadata_fixture(version, asset_url.as_str(), checksum)).unwrap();
        let routes = vec![
            ("/github/tags/1.134.0".to_owned(), release),
            (
                "/update/1.134.0/win32-x64-archive/stable".to_owned(),
                metadata,
            ),
            ("/assets/VSCode-win32-x64-1.134.0.zip".to_owned(), archive),
        ];
        (base, spawn_fixture_server(listener, routes))
    }

    #[cfg(windows)]
    fn windows_plugin_fixture_script(
        version: &ExactVersion,
        install_path: &Path,
        archive_url: &Url,
        checksum: &str,
    ) -> String {
        let initialize = serde_json::json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {
                "protocolVersion": torben_contracts::plugin::PLUGIN_PROTOCOL_VERSION,
                "pluginId": "app.torben.plugin.vscode",
                "pluginVersion": env!("CARGO_PKG_VERSION"),
                "applications": [{
                    "id": "vscode", "displayName": "Visual Studio Code", "summary": "fixture",
                    "categories": ["editor"],
                    "capabilities": ["versions", "install", "select", "uninstall"],
                    "sources": [{"id": "vscode.official", "displayName": "Official", "managed": true}]
                }]
            }
        }).to_string();
        let resolved = serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": {"requested": "current", "resolved": version}
        })
        .to_string();
        let plan = serde_json::json!({
            "jsonrpc": "2.0", "id": 3,
            "result": {
                "appId": "vscode", "version": version, "sourceId": "vscode.official",
                "steps": [
                    {"type": "download", "url": archive_url, "destination_name": "VSCode-win32-x64-1.134.0.zip"},
                    {"type": "verify_sha256", "archive_name": "VSCode-win32-x64-1.134.0.zip", "expected": checksum},
                    {"type": "extract_archive", "archive_name": "VSCode-win32-x64-1.134.0.zip", "strip_components": 0},
                    {"type": "health_check", "executable": "code", "arguments": ["--version", "--disable-updates"], "expected_output": version},
                    {"type": "create_shims", "commands": ["code"]}
                ],
                "metadata": {
                    "target": "windows-x86_64", "platform": "win32-x64-archive",
                    "commit": "110a328ea54b42367b803ec53ee0bf52ef26b419"
                }
            }
        }).to_string();
        let health = serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": {"healthy": true, "actualVersion": version, "message": "healthy"}
        })
        .to_string();
        let uninstall = serde_json::json!({
            "jsonrpc": "2.0", "id": 2,
            "result": {"appId": "vscode", "version": version, "sourceId": "vscode.official", "installPath": install_path.display().to_string(), "preserveUserData": true}
        }).to_string();
        format!(
            "@echo off\r\n:loop\r\nset request=\r\nset /p request=\r\nif errorlevel 1 exit /b 0\r\necho %request%| findstr /c:\"initialize\" >nul && (echo {initialize}& goto loop)\r\necho %request%| findstr /c:\"version.resolve\" >nul && (echo {resolved}& goto loop)\r\necho %request%| findstr /c:\"uninstall.plan\" >nul && (echo {uninstall}& goto loop)\r\necho %request%| findstr /c:\"install.plan\" >nul && (echo {plan}& goto loop)\r\necho %request%| findstr /c:\"health.check\" >nul && (echo {health}& goto loop)\r\nexit /b 1\r\n"
        )
    }

    fn fixture_server(routes: Vec<(String, Vec<u8>)>) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = Url::parse(&format!("http://{address}/")).unwrap();
        (base, spawn_fixture_server(listener, routes))
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
