use std::{
    collections::BTreeSet,
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime},
};

use futures_util::{StreamExt, stream};
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
    process,
    temurin_signature::{ADOPTIUM_RELEASE_FINGERPRINT, verify_archive_signature},
};

const ADOPTIUM_API_BASE: &str = "https://api.adoptium.net/v3/";
const ADOPTIUM_PUBLIC_KEY: &str = "https://packages.adoptium.net/artifactory/api/gpg/key/public";
const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;
const MAX_SIGNATURE_BYTES: u64 = 1024 * 1024;
const MAX_PUBLIC_KEY_BYTES: u64 = 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
const RELEASE_PAGE_SIZE: usize = 50;
const MAX_RELEASE_PAGES: usize = 20;
const METADATA_ATTEMPTS: usize = 3;
const METADATA_RETRY_DELAY: Duration = Duration::from_millis(200);

#[derive(Debug, Clone)]
pub struct TemurinProvider {
    client: reqwest::Client,
    api_base: Url,
    public_key_url: Url,
    #[cfg(test)]
    fixture_integrity: Option<(Vec<u8>, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemurinDistribution {
    pub archive_name: String,
    pub archive_url: Url,
    pub signature_url: Url,
    pub public_key_url: Url,
    pub checksum: String,
    pub size: u64,
    pub archive_kind: ArchiveKind,
    pub released_at: String,
    pub feature: u32,
}

#[derive(Debug, Deserialize)]
struct AvailableReleases {
    available_lts_releases: Vec<u32>,
}

#[derive(Debug, Deserialize)]
struct ReleaseAsset {
    binaries: Vec<ReleaseBinary>,
    release_type: String,
    timestamp: String,
    vendor: String,
    version_data: ReleaseVersion,
}

#[derive(Debug, Deserialize)]
struct ReleaseVersion {
    semver: String,
}

#[derive(Debug, Deserialize)]
struct ReleaseBinary {
    architecture: String,
    heap_size: String,
    image_type: String,
    jvm_impl: String,
    os: String,
    package: ReleasePackage,
    project: String,
}

#[derive(Debug, Deserialize)]
struct ReleasePackage {
    checksum: String,
    link: String,
    name: String,
    signature_link: Option<String>,
    size: u64,
}

impl TemurinProvider {
    pub fn official() -> TorbenResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            api_base: Url::parse(ADOPTIUM_API_BASE).map_err(url_error)?,
            public_key_url: Url::parse(ADOPTIUM_PUBLIC_KEY).map_err(url_error)?,
            #[cfg(test)]
            fixture_integrity: None,
        })
    }

    #[cfg(test)]
    pub fn with_base_url(base_url: Url) -> TorbenResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent("Torben-App-Test")
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            public_key_url: base_url.join("public-key.asc").map_err(url_error)?,
            api_base: base_url,
            fixture_integrity: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_fixture_integrity(
        base_url: Url,
        signature: Vec<u8>,
        public_key: Vec<u8>,
    ) -> TorbenResult<Self> {
        let mut provider = Self::with_base_url(base_url)?;
        provider.fixture_integrity = Some((signature, public_key));
        Ok(provider)
    }

    pub async fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        let metadata: AvailableReleases = self
            .fetch_json(
                &self
                    .api_base
                    .join("info/available_releases")
                    .map_err(url_error)?,
            )
            .await?;
        let mut features = metadata.available_lts_releases;
        features.sort_unstable_by(|left, right| right.cmp(left));
        features.dedup();
        if features.is_empty() || features.contains(&0) {
            return Err(TorbenError::new(
                "temurin_metadata_invalid",
                "Adoptium returned no valid LTS feature releases.",
            ));
        }
        let releases = stream::iter(features)
            .map(|feature| async move { (feature, self.feature_releases(feature).await) })
            .buffer_unordered(4)
            .collect::<Vec<_>>()
            .await;
        let mut versions = Vec::new();
        for (feature, releases) in releases {
            let releases = releases?;
            for (index, (version, distribution)) in releases.into_iter().enumerate() {
                versions.push(VersionDescriptor {
                    version,
                    lts_name: Some(format!("Java {feature} LTS")),
                    released_at: distribution.released_at,
                    recommended: index == 0,
                });
            }
        }
        versions.sort_by(|left, right| {
            right
                .recommended
                .cmp(&left.recommended)
                .then_with(|| right.version.cmp(&left.version))
        });
        versions.dedup_by(|left, right| left.version == right.version);
        Ok(versions)
    }

    pub async fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        let versions = self.list_versions().await?;
        if let Ok(exact) = ExactVersion::from_str(requested) {
            return versions
                .into_iter()
                .find(|candidate| candidate.version == exact)
                .map(|candidate| candidate.version)
                .ok_or_else(|| version_not_found(requested));
        }
        let normalized = requested.to_ascii_lowercase();
        if matches!(normalized.as_str(), "lts" | "latest" | "current") {
            return versions
                .into_iter()
                .next()
                .map(|candidate| candidate.version)
                .ok_or_else(|| version_not_found(requested));
        }
        if let Ok(feature) = normalized.parse::<u64>() {
            return versions
                .into_iter()
                .find(|candidate| candidate.version.as_semver().major == feature)
                .map(|candidate| candidate.version)
                .ok_or_else(|| version_not_found(requested));
        }
        Err(version_not_found(requested))
    }

    pub async fn distribution(&self, version: &ExactVersion) -> TorbenResult<TemurinDistribution> {
        let feature = u32::try_from(version.as_semver().major).map_err(|_| {
            TorbenError::new(
                "temurin_version_invalid",
                "The Eclipse Temurin feature version is unsupported.",
            )
        })?;
        self.feature_releases(feature)
            .await?
            .into_iter()
            .find(|(candidate, _)| candidate == version)
            .map(|(_, distribution)| distribution)
            .ok_or_else(|| version_not_found(&version.to_string()))
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
        let download_dir = paths.download_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&download_dir).map_err(io_error)?;
        let archive_path = download_dir.join(&distribution.archive_name);
        journal.record(
            OperationState::Running,
            "download",
            format!("Downloading {}", distribution.archive_name),
            Some(0.2),
        )?;
        let cached_valid = archive_path.is_file()
            && std::fs::metadata(&archive_path).map_err(io_error)?.len() == distribution.size
            && sha256_file_checked(&archive_path, Some(&cancellation))? == distribution.checksum;
        if !cached_valid {
            self.download_archive(
                &distribution.archive_url,
                &archive_path,
                distribution.size,
                &cancellation,
            )
            .await?;
        }
        cancellation.check()?;
        journal.record(
            OperationState::Running,
            "verify",
            "Verifying the official Eclipse Temurin SHA-256 checksum",
            Some(0.4),
        )?;
        let actual_hash = sha256_file_checked(&archive_path, Some(&cancellation))?;
        if actual_hash != distribution.checksum {
            return Err(TorbenError::new(
                "archive_hash_mismatch",
                "The Eclipse Temurin archive does not match its official checksum.",
            )
            .with_detail("expected", distribution.checksum)
            .with_detail("actual", actual_hash));
        }
        let signature = self
            .fetch_limited(
                &distribution.signature_url,
                MAX_SIGNATURE_BYTES,
                AssetKind::Release,
                Some(&cancellation),
            )
            .await?;
        let public_key = self
            .fetch_limited(
                &distribution.public_key_url,
                MAX_PUBLIC_KEY_BYTES,
                AssetKind::PublicKey,
                Some(&cancellation),
            )
            .await?;
        journal.record(
            OperationState::Running,
            "verify",
            "Verifying the official Eclipse Temurin detached signature",
            Some(0.5),
        )?;
        self.verify_signature(&archive_path, &signature, &public_key, &cancellation)
            .await?;
        cancellation.check()?;

        let staging =
            paths
                .staging_dir()
                .join(format!("install-{}-{}", app_id, journal.operation_id()));
        std::fs::create_dir_all(&staging).map_err(io_error)?;
        journal.record(
            OperationState::Running,
            "extract",
            "Extracting Eclipse Temurin into staging",
            Some(0.65),
        )?;
        let archive = archive_path.clone();
        let kind = distribution.archive_kind;
        let staging_task = staging.clone();
        let extraction_cancellation = cancellation.clone();
        let extracted_root = tokio::task::spawn_blocking(move || {
            extract_archive(&archive, kind, &staging_task, &extraction_cancellation)
        })
        .await
        .map_err(|error| {
            TorbenError::new("archive_task_failed", "The archive extraction task failed.")
                .with_detail("reason", error.to_string())
        })??;
        let extracted_home = if cfg!(target_os = "macos") {
            extracted_root.join("Contents").join("Home")
        } else {
            extracted_root.clone()
        };
        journal.record(
            OperationState::Running,
            "health_check",
            "Checking the extracted Eclipse Temurin JDK",
            Some(0.82),
        )?;
        if let Err(error) = self.health_check_path(&extracted_home, version) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        cancellation.check()?;
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(TorbenError::new(
                "install_path_exists",
                "The final Eclipse Temurin installation directory already exists.",
            ));
        }
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        std::fs::rename(&extracted_home, &final_path).map_err(|error| {
            TorbenError::new(
                "install_commit_failed",
                "Could not atomically commit the Eclipse Temurin installation.",
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
        if !matches!(command, "java" | "javac") {
            return Err(TorbenError::new(
                "unsupported_command",
                "The Eclipse Temurin plugin does not expose this command.",
            )
            .with_detail("command", command));
        }
        let filename = if cfg!(windows) {
            format!("{command}.exe")
        } else {
            command.to_owned()
        };
        let path = install_path.join("bin").join(filename);
        if path.is_file() {
            Ok(path)
        } else {
            Err(TorbenError::new(
                "managed_command_missing",
                "A managed Java command is missing.",
            )
            .with_detail("path", path.display().to_string()))
        }
    }

    pub async fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let executable_name = if cfg!(windows) { "java.exe" } else { "java" };
        let mut candidates = BTreeSet::new();
        if let Some(java_home) = std::env::var_os("JAVA_HOME") {
            candidates.insert(PathBuf::from(java_home).join("bin").join(executable_name));
        }
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            candidates.insert(directory.join(executable_name));
        }
        let mut records = Vec::new();
        for candidate in candidates {
            let Ok(canonical) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) {
                continue;
            }
            let Some(home) = canonical.parent().and_then(Path::parent) else {
                continue;
            };
            let Ok(output) = tokio::time::timeout(
                Duration::from_secs(3),
                process::async_command(&canonical)
                    .arg("-version")
                    .kill_on_drop(true)
                    .output(),
            )
            .await
            else {
                continue;
            };
            let Ok(output) = output else {
                continue;
            };
            if !output.status.success() {
                continue;
            }
            let combined = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if !combined.to_ascii_lowercase().contains("temurin") {
                continue;
            }
            let Some(actual) = extract_java_version(&combined) else {
                continue;
            };
            let Ok(version) = external_java_version(&actual) else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("temurin")?,
                version,
                source_id: SourceId::new("temurin.external")?,
                scope: InstallScope::External,
                install_path: home.display().to_string(),
                installed_at: String::new(),
                health: "healthy".to_owned(),
            });
        }
        Ok(records)
    }

    async fn feature_releases(
        &self,
        feature: u32,
    ) -> TorbenResult<Vec<(ExactVersion, TemurinDistribution)>> {
        let (os, architecture, archive_kind) = current_api_target()?;
        let mut result = Vec::new();
        for page in 0..MAX_RELEASE_PAGES {
            let mut url = self
                .api_base
                .join(&format!("assets/feature_releases/{feature}/ga"))
                .map_err(url_error)?;
            url.query_pairs_mut()
                .append_pair("architecture", architecture)
                .append_pair("heap_size", "normal")
                .append_pair("image_type", "jdk")
                .append_pair("jvm_impl", "hotspot")
                .append_pair("os", os)
                .append_pair("page", &page.to_string())
                .append_pair("page_size", &RELEASE_PAGE_SIZE.to_string())
                .append_pair("project", "jdk")
                .append_pair("sort_method", "DATE")
                .append_pair("sort_order", "DESC")
                .append_pair("vendor", "eclipse");
            let releases: Vec<ReleaseAsset> = self.fetch_json(&url).await?;
            let release_count = releases.len();
            for release in releases {
                if release.release_type != "ga" || release.vendor != "eclipse" {
                    return Err(metadata_invalid("release identity"));
                }
                let version = ExactVersion::from_str(&release.version_data.semver)?;
                if version.as_semver().major != u64::from(feature) {
                    return Err(metadata_invalid("feature version"));
                }
                for binary in release.binaries {
                    if binary.architecture != architecture
                        || binary.heap_size != "normal"
                        || binary.image_type != "jdk"
                        || binary.jvm_impl != "hotspot"
                        || binary.os != os
                        || binary.project != "jdk"
                    {
                        return Err(metadata_invalid("binary target"));
                    }
                    let package = binary.package;
                    let Some(signature_link) = package.signature_link else {
                        continue;
                    };
                    let archive_url = Url::parse(&package.link).map_err(url_error)?;
                    let signature_url = Url::parse(&signature_link).map_err(url_error)?;
                    validate_release_url(&self.api_base, &archive_url, feature, &package.name)?;
                    validate_release_url(
                        &self.api_base,
                        &signature_url,
                        feature,
                        &format!("{}.sig", package.name),
                    )?;
                    if !archive_name_matches_kind(&package.name, archive_kind)
                        || package.size == 0
                        || package.size > MAX_ARCHIVE_BYTES
                        || !is_sha256(&package.checksum)
                    {
                        return Err(metadata_invalid("package integrity"));
                    }
                    result.push((
                        version.clone(),
                        TemurinDistribution {
                            archive_name: package.name,
                            archive_url,
                            signature_url,
                            public_key_url: self.public_key_url.clone(),
                            checksum: package.checksum.to_ascii_lowercase(),
                            size: package.size,
                            archive_kind,
                            released_at: release.timestamp.clone(),
                            feature,
                        },
                    ));
                }
            }
            if release_count < RELEASE_PAGE_SIZE {
                break;
            }
            if page + 1 == MAX_RELEASE_PAGES {
                return Err(TorbenError::new(
                    "temurin_metadata_pagination_limit",
                    "The Adoptium release catalog exceeded the supported page limit.",
                ));
            }
        }
        result.sort_by(|left, right| right.0.cmp(&left.0));
        result.dedup_by(|left, right| left.0 == right.0);
        Ok(result)
    }

    async fn validate_install_plan(
        &self,
        plan: &InstallPlan,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<TemurinDistribution> {
        if app_id.as_str() != "temurin"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new("temurin.official")?
        {
            return Err(invalid_plan("identity or source owner"));
        }
        let expected = self.distribution(version).await?;
        let (os, architecture, _) = current_api_target()?;
        let target = format!("{os}-{architecture}");
        if plan.metadata.get("target") != Some(&target)
            || plan.metadata.get("feature") != Some(&expected.feature.to_string())
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
                expected: expected_hash,
            },
            InstallStep::VerifyDetachedSignature {
                archive_name: signed_archive,
                signature_url,
                public_key_url,
                trusted_fingerprint,
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
            || signed_archive != &expected.archive_name
            || extracted_archive != &expected.archive_name
            || expected_hash.to_ascii_lowercase() != expected.checksum
            || Url::parse(signature_url).ok().as_ref() != Some(&expected.signature_url)
            || Url::parse(public_key_url).ok().as_ref() != Some(&expected.public_key_url)
            || trusted_fingerprint != ADOPTIUM_RELEASE_FINGERPRINT
            || *strip_components != 0
            || executable != "java"
            || arguments.as_slice() != ["-version"]
            || expected_output != &java_version_core(version)
            || commands.as_slice() != ["java", "javac"]
        {
            return Err(invalid_plan("official distribution details"));
        }
        Ok(expected)
    }

    fn health_check_path(&self, install_path: &Path, version: &ExactVersion) -> TorbenResult<()> {
        let path = managed_command_path(install_path)?;
        for command in ["java", "javac"] {
            let executable = self.command_path(install_path, command)?;
            let output = process::command(&executable)
                .arg("-version")
                .env("PATH", &path)
                .output()
                .map_err(|error| health_start_error(command, &executable, error))?;
            if !output.status.success() {
                return Err(TorbenError::new(
                    "health_check_failed",
                    "A managed Eclipse Temurin command returned an error.",
                )
                .with_detail("command", command)
                .with_detail("status", output.status.to_string()));
            }
            let combined = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            if command == "java" && !combined.to_ascii_lowercase().contains("temurin") {
                return Err(TorbenError::new(
                    "health_check_vendor_mismatch",
                    "The managed Java runtime does not identify as Eclipse Temurin.",
                ));
            }
            let actual = extract_command_version(command, &combined).ok_or_else(|| {
                TorbenError::new(
                    "health_check_output_invalid",
                    "A managed Eclipse Temurin command returned an invalid version.",
                )
                .with_detail("command", command)
            })?;
            if actual != java_version_core(version) {
                return Err(TorbenError::new(
                    "health_check_version_mismatch",
                    "The managed Eclipse Temurin version does not match the requested version.",
                )
                .with_detail("expected", java_version_core(version))
                .with_detail("actual", actual));
            }
        }
        Ok(())
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(&self, url: &Url) -> TorbenResult<T> {
        for attempt in 0..METADATA_ATTEMPTS {
            match self
                .fetch_limited(url, MAX_METADATA_BYTES, AssetKind::Metadata, None)
                .await
            {
                Ok(bytes) => match serde_json::from_slice(&bytes) {
                    Ok(value) => return Ok(value),
                    Err(error) if attempt + 1 == METADATA_ATTEMPTS => {
                        return Err(metadata_json_error(error));
                    }
                    Err(_) => {}
                },
                Err(error) if error.code != "network_error" || attempt + 1 == METADATA_ATTEMPTS => {
                    return Err(error);
                }
                Err(_) => {}
            }
            tokio::time::sleep(METADATA_RETRY_DELAY).await;
        }
        unreachable!("the bounded metadata retry loop always returns")
    }

    async fn fetch_limited(
        &self,
        url: &Url,
        maximum: u64,
        kind: AssetKind,
        cancellation: Option<&CancellationProbe>,
    ) -> TorbenResult<Vec<u8>> {
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        let mut request = self.client.get(url.clone());
        if matches!(kind, AssetKind::Metadata) {
            request = request.header(reqwest::header::ACCEPT, "application/json");
        }
        let response = await_with_cancellation(
            async { request.send().await.map_err(network_error) },
            cancellation,
        )
        .await?;
        validate_response(&response, &self.api_base, kind)?;
        let response = response.error_for_status().map_err(network_error)?;
        if response.content_length().is_some_and(|size| size > maximum) {
            return Err(asset_too_large(maximum));
        }
        let mut result = Vec::new();
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
            let next = result
                .len()
                .checked_add(chunk.len())
                .ok_or_else(|| asset_too_large(maximum))?;
            if u64::try_from(next).unwrap_or(u64::MAX) > maximum {
                return Err(asset_too_large(maximum));
            }
            result.extend_from_slice(&chunk);
        }
        Ok(result)
    }

    async fn download_archive(
        &self,
        url: &Url,
        destination: &Path,
        expected_size: u64,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<()> {
        cancellation.check()?;
        let response = await_with_cancellation(
            async {
                self.client
                    .get(url.clone())
                    .send()
                    .await
                    .map_err(network_error)
            },
            Some(cancellation),
        )
        .await?;
        validate_response(&response, &self.api_base, AssetKind::Release)?;
        let response = response.error_for_status().map_err(network_error)?;
        if response
            .content_length()
            .is_some_and(|size| size != expected_size)
        {
            return Err(TorbenError::new(
                "archive_size_mismatch",
                "The Eclipse Temurin archive size does not match official metadata.",
            ));
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
                    .ok_or_else(|| size_mismatch(expected_size, received))?;
                if received > expected_size {
                    return Err(size_mismatch(expected_size, received));
                }
                file.write_all(&chunk).await.map_err(io_error)?;
            }
            file.flush().await.map_err(io_error)?;
            file.sync_all().await.map_err(io_error)?;
            drop(file);
            if received != expected_size {
                return Err(size_mismatch(expected_size, received));
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

    async fn verify_signature(
        &self,
        archive_path: &Path,
        signature: &[u8],
        public_key: &[u8],
        cancellation: &CancellationProbe,
    ) -> TorbenResult<()> {
        cancellation.check()?;
        #[cfg(test)]
        if let Some((expected_signature, expected_key)) = &self.fixture_integrity {
            if signature == expected_signature && public_key == expected_key {
                cancellation.check()?;
                return Ok(());
            }
            return Err(TorbenError::new(
                "temurin_signature_invalid",
                "The Eclipse Temurin fixture signature is invalid.",
            ));
        }
        let archive = archive_path.to_path_buf();
        let signature = signature.to_vec();
        let public_key = public_key.to_vec();
        let result = tokio::task::spawn_blocking(move || {
            let bytes = std::fs::read(archive).map_err(io_error)?;
            verify_archive_signature(&bytes, &signature, &public_key)
        })
        .await
        .map_err(|error| {
            TorbenError::new(
                "temurin_signature_task_failed",
                "The Eclipse Temurin signature verification task failed.",
            )
            .with_detail("reason", error.to_string())
        })?;
        cancellation.check()?;
        result
    }
}

#[derive(Clone, Copy)]
enum AssetKind {
    Metadata,
    Release,
    PublicKey,
}

fn current_api_target() -> TorbenResult<(&'static str, &'static str, ArchiveKind)> {
    let os = match std::env::consts::OS {
        "windows" => "windows",
        "linux" => "linux",
        "macos" => "mac",
        other => {
            return Err(TorbenError::new(
                "platform_not_supported",
                "Eclipse Temurin is not supported on this operating system.",
            )
            .with_detail("os", other));
        }
    };
    let architecture = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "aarch64",
        other => {
            return Err(TorbenError::new(
                "platform_not_supported",
                "Eclipse Temurin is not supported on this architecture.",
            )
            .with_detail("architecture", other));
        }
    };
    let kind = if os == "windows" {
        ArchiveKind::Zip
    } else {
        ArchiveKind::TarGz
    };
    Ok((os, architecture, kind))
}

fn validate_release_url(
    api_base: &Url,
    url: &Url,
    feature: u32,
    expected_name: &str,
) -> TorbenResult<()> {
    if api_base.host_str() == Some("127.0.0.1") {
        if same_origin(api_base, url) {
            return Ok(());
        }
        return Err(metadata_invalid("fixture release origin"));
    }
    let segments = url
        .path_segments()
        .map(Iterator::collect::<Vec<_>>)
        .unwrap_or_default();
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || segments.len() != 6
        || segments[0] != "adoptium"
        || segments[1] != format!("temurin{feature}-binaries")
        || segments[2] != "releases"
        || segments[3] != "download"
        || segments[5] != expected_name
    {
        return Err(metadata_invalid("official release URL"));
    }
    Ok(())
}

fn validate_response(
    response: &reqwest::Response,
    api_base: &Url,
    kind: AssetKind,
) -> TorbenResult<()> {
    let url = response.url();
    let valid = if api_base.host_str() == Some("127.0.0.1") {
        same_origin(api_base, url)
    } else {
        match kind {
            AssetKind::Metadata => same_origin(api_base, url),
            AssetKind::Release => {
                url.scheme() == "https"
                    && matches!(
                        url.host_str(),
                        Some("github.com" | "release-assets.githubusercontent.com")
                    )
            }
            AssetKind::PublicKey => {
                url.scheme() == "https" && url.host_str() == Some("packages.adoptium.net")
            }
        }
    };
    if valid {
        Ok(())
    } else {
        Err(TorbenError::new(
            "unexpected_download_origin",
            "An Eclipse Temurin request changed to an untrusted origin.",
        )
        .with_detail("url", url.to_string()))
    }
}

fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str() == right.host_str()
        && left.port_or_known_default() == right.port_or_known_default()
}

fn archive_name_matches_kind(name: &str, kind: ArchiveKind) -> bool {
    let path = Path::new(name);
    match kind {
        ArchiveKind::Zip => path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("zip")),
        ArchiveKind::TarGz => {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"))
                && path
                    .file_stem()
                    .map(Path::new)
                    .and_then(Path::extension)
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("tar"))
        }
        ArchiveKind::TarXz => false,
    }
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn java_version_core(version: &ExactVersion) -> String {
    let version = version.as_semver();
    if version.major == 8 {
        format!("1.8.0_{}", version.patch)
    } else {
        format!("{}.{}.{}", version.major, version.minor, version.patch)
    }
}

fn extract_java_version(output: &str) -> Option<String> {
    let start = output.find('"')? + 1;
    let end = output[start..].find('"')? + start;
    Some(output[start..end].to_owned())
}

fn extract_command_version(command: &str, output: &str) -> Option<String> {
    match command {
        "java" => extract_java_version(output),
        "javac" => output
            .lines()
            .map(str::trim)
            .find_map(|line| line.strip_prefix("javac "))
            .and_then(|value| value.split_whitespace().next())
            .map(str::to_owned),
        _ => None,
    }
}

fn external_java_version(value: &str) -> TorbenResult<ExactVersion> {
    if let Some(update) = value.strip_prefix("1.8.0_") {
        return ExactVersion::from_str(&format!("8.0.{update}"));
    }
    ExactVersion::from_str(value)
}

fn managed_command_path(install_path: &Path) -> TorbenResult<OsString> {
    let bin = install_path.join("bin");
    let mut paths = vec![bin];
    paths.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    std::env::join_paths(paths).map_err(|error| {
        TorbenError::new(
            "health_check_path_invalid",
            "Could not prepare the Eclipse Temurin health-check PATH.",
        )
        .with_detail("reason", error.to_string())
    })
}

fn health_start_error(command: &str, executable: &Path, error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "health_check_start_failed",
        "Could not start a managed Eclipse Temurin command.",
    )
    .with_detail("command", command)
    .with_detail("path", executable.display().to_string())
    .with_detail("reason", error.to_string())
}

fn metadata_invalid(field: &str) -> TorbenError {
    TorbenError::new(
        "temurin_metadata_invalid",
        "The Adoptium metadata contains an invalid or unexpected field.",
    )
    .with_detail("field", field)
}

fn metadata_json_error(error: serde_json::Error) -> TorbenError {
    TorbenError::new(
        "temurin_metadata_invalid",
        "The Adoptium metadata is not valid JSON.",
    )
    .with_detail("reason", error.to_string())
}

fn invalid_plan(field: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The Eclipse Temurin plugin returned an invalid installation plan.",
    )
    .with_detail("field", field)
}

fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested Eclipse Temurin version was not found in the official LTS catalog.",
    )
    .with_detail("requested", requested)
}

fn size_mismatch(expected: u64, actual: u64) -> TorbenError {
    TorbenError::new(
        "archive_size_mismatch",
        "The Eclipse Temurin archive size does not match official metadata.",
    )
    .with_detail("expected", expected.to_string())
    .with_detail("actual", actual.to_string())
}

fn asset_too_large(maximum: u64) -> TorbenError {
    TorbenError::new(
        "temurin_asset_too_large",
        "An Eclipse Temurin metadata or signature asset is too large.",
    )
    .with_detail("maximumBytes", maximum.to_string())
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
        .to_string()
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
            () = tokio::time::sleep(Duration::from_millis(50)) => cancellation.check()?,
        }
    }
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "temurin_io_failed",
        "An Eclipse Temurin filesystem operation failed.",
    )
    .with_detail("reason", error.to_string())
}

fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new(
        "temurin_network_error",
        "An Eclipse Temurin network request failed.",
    )
    .with_detail("reason", error.to_string())
}

fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new("temurin_url_invalid", "An Eclipse Temurin URL is invalid.")
        .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::TcpListener,
        process::Command,
        sync::Arc,
        thread,
    };

    use sha2::Digest;
    use tempfile::tempdir;
    use torben_contracts::{OperationKind, plugin::InstallStep};

    use crate::{
        StateStore, TorbenCore, bundled_shim::BundledShim, node_plugin::BundledPlugin,
        operation::OperationJournal,
    };

    use super::*;

    #[test]
    fn parses_java_version_output_for_modern_and_java_eight() {
        assert_eq!(
            extract_java_version("openjdk version \"21.0.2\" 2024-01-16"),
            Some("21.0.2".to_owned())
        );
        assert_eq!(
            external_java_version("1.8.0_472").unwrap().to_string(),
            "8.0.472"
        );
        assert_eq!(
            extract_command_version("javac", "javac 21.0.2\n"),
            Some("21.0.2".to_owned())
        );
    }

    #[tokio::test]
    async fn local_metadata_ignores_unsigned_release_and_resolves_versions() {
        let (base_url, server) = metadata_fixture_server(6);
        let provider = TemurinProvider::with_base_url(base_url).unwrap();

        let versions = provider.list_versions().await.unwrap();
        let lts = provider.resolve_version("lts").await.unwrap();
        let feature = provider.resolve_version("21").await.unwrap();

        server.join().unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version.to_string(), "21.0.2+13.0.LTS");
        assert_eq!(lts, versions[0].version);
        assert_eq!(feature, versions[0].version);
    }

    #[tokio::test]
    async fn metadata_request_retries_invalid_json_and_requests_json() {
        let valid =
            serde_json::to_vec(&serde_json::json!({ "available_lts_releases": [21] })).unwrap();
        let (base_url, server) = metadata_sequence_server(vec![
            (
                "text/html".to_owned(),
                b"<html>temporary response</html>".to_vec(),
            ),
            ("application/json".to_owned(), valid),
        ]);
        let provider = TemurinProvider::with_base_url(base_url.clone()).unwrap();

        let metadata: AvailableReleases = provider
            .fetch_json(&base_url.join("info/available_releases").unwrap())
            .await
            .unwrap();

        server.join().unwrap();
        assert_eq!(metadata.available_lts_releases, vec![21]);
    }

    #[tokio::test]
    async fn metadata_request_fails_after_bounded_invalid_json_attempts() {
        let responses = (0..METADATA_ATTEMPTS)
            .map(|_| ("text/html".to_owned(), b"<html>invalid</html>".to_vec()))
            .collect();
        let (base_url, server) = metadata_sequence_server(responses);
        let provider = TemurinProvider::with_base_url(base_url.clone()).unwrap();

        let error = provider
            .fetch_json::<AvailableReleases>(&base_url.join("info/available_releases").unwrap())
            .await
            .unwrap_err();

        server.join().unwrap();
        assert_eq!(error.code, "temurin_metadata_invalid");
    }

    #[tokio::test]
    async fn cancellation_stops_temurin_request_before_network() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let app_id = AppId::new("temurin").unwrap();
        let version = ExactVersion::from_str("21.0.2+13.0.LTS").unwrap();
        let journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            Some(&version),
        )
        .unwrap();
        OperationJournal::request_cancellation(&paths, &store, journal.operation_id()).unwrap();
        let cancellation = journal.cancellation_probe();
        let provider =
            TemurinProvider::with_base_url(Url::parse("http://127.0.0.1:9/v3/").unwrap()).unwrap();

        let error = provider
            .fetch_limited(
                &Url::parse("http://127.0.0.1:9/v3/metadata").unwrap(),
                MAX_METADATA_BYTES,
                AssetKind::Metadata,
                Some(&cancellation),
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "operation_cancelled");
    }

    #[tokio::test]
    async fn local_fixture_installs_verifies_and_health_checks_temurin() {
        let root = tempdir().unwrap();
        let version = ExactVersion::from_str("21.0.2+13.0.LTS").unwrap();
        let (_, _, kind) = current_api_target().unwrap();
        let java = compile_fixture_java(root.path(), &version);
        let archive_name = if kind == ArchiveKind::Zip {
            "OpenJDK21U-jdk_fixture.zip"
        } else {
            "OpenJDK21U-jdk_fixture.tar.gz"
        };
        let archive = fixture_archive(kind, archive_name, &java);
        let checksum = hex::encode(sha2::Sha256::digest(&archive));
        let signature = b"fixture-signature".to_vec();
        let public_key = b"fixture-public-key".to_vec();
        let (base_url, server) = vertical_fixture_server(
            archive_name,
            archive.clone(),
            &checksum,
            signature.clone(),
            public_key.clone(),
            5,
        );
        let provider =
            TemurinProvider::with_fixture_integrity(base_url, signature, public_key).unwrap();
        let distribution = provider.distribution(&version).await.unwrap();
        let app_id = AppId::new("temurin").unwrap();
        let source_id = SourceId::new("temurin.official").unwrap();
        let (os, architecture, _) = current_api_target().unwrap();
        let plan = InstallPlan {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id,
            steps: vec![
                InstallStep::Download {
                    url: distribution.archive_url.to_string(),
                    destination_name: distribution.archive_name.clone(),
                },
                InstallStep::VerifySha256 {
                    archive_name: distribution.archive_name.clone(),
                    expected: distribution.checksum.clone(),
                },
                InstallStep::VerifyDetachedSignature {
                    archive_name: distribution.archive_name.clone(),
                    signature_url: distribution.signature_url.to_string(),
                    public_key_url: distribution.public_key_url.to_string(),
                    trusted_fingerprint: ADOPTIUM_RELEASE_FINGERPRINT.to_owned(),
                },
                InstallStep::ExtractArchive {
                    archive_name: distribution.archive_name.clone(),
                    strip_components: 0,
                },
                InstallStep::HealthCheck {
                    executable: "java".to_owned(),
                    arguments: vec!["-version".to_owned()],
                    expected_output: "21.0.2".to_owned(),
                },
                InstallStep::CreateShims {
                    commands: vec!["java".to_owned(), "javac".to_owned()],
                },
            ],
            metadata: BTreeMap::from([
                ("target".to_owned(), format!("{os}-{architecture}")),
                ("feature".to_owned(), "21".to_owned()),
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
        assert_eq!(
            std::fs::read(
                paths
                    .download_dir("temurin", &record.version.to_string())
                    .join(archive_name)
            )
            .unwrap(),
            archive
        );
        provider.health_check(&record).unwrap();
        assert!(
            Path::new(&record.install_path)
                .join("bin")
                .join(if cfg!(windows) { "java.exe" } else { "java" })
                .is_file()
        );
    }

    #[tokio::test]
    async fn local_fixture_completes_core_install_select_and_uninstall_transaction() {
        let root = tempdir().unwrap();
        let version = ExactVersion::from_str("21.0.2+13.0.LTS").unwrap();
        let (_, _, kind) = current_api_target().unwrap();
        let java = compile_fixture_java(root.path(), &version);
        let archive_name = if kind == ArchiveKind::Zip {
            "OpenJDK21U-jdk_core-fixture.zip"
        } else {
            "OpenJDK21U-jdk_core-fixture.tar.gz"
        };
        let archive = fixture_archive(kind, archive_name, &java);
        let checksum = hex::encode(sha2::Sha256::digest(&archive));
        let signature = b"core-fixture-signature".to_vec();
        let public_key = b"core-fixture-key".to_vec();
        let (base_url, server) = vertical_fixture_server(
            archive_name,
            archive,
            &checksum,
            signature.clone(),
            public_key.clone(),
            4,
        );
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let install_path = paths.app_version_dir("temurin", &version.to_string());
        let distribution_url = base_url.join(&format!("releases/{archive_name}")).unwrap();
        let plugin = root.path().join(if cfg!(windows) {
            "temurin-plugin-fixture.cmd"
        } else {
            "temurin-plugin-fixture"
        });
        std::fs::write(
            &plugin,
            plugin_fixture_script(
                archive_name,
                distribution_url.as_str(),
                &format!("{distribution_url}.sig"),
                base_url.join("public-key.asc").unwrap().as_str(),
                &checksum,
                &version,
                &install_path,
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&plugin).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&plugin, permissions).unwrap();
        }
        let shim = root.path().join("torben-shim-fixture");
        std::fs::write(&shim, b"shim fixture").unwrap();
        let mut core = TorbenCore::open(paths).unwrap();
        core.temurin =
            TemurinProvider::with_fixture_integrity(base_url, signature, public_key).unwrap();
        core.temurin_plugin = BundledPlugin::temurin_from_executable(plugin);
        core.bundled_shim = BundledShim::from_executable(shim);
        let app_id = AppId::new("temurin").unwrap();

        let installed = core.install(&app_id, "lts").await.unwrap();
        core.select(&app_id, &version).await.unwrap();
        let java_command = core.executable_for(&app_id, "java").unwrap();
        let java_command_existed = java_command.is_file();
        core.clear_selection(&app_id).unwrap();
        core.uninstall(&app_id, &version).await.unwrap();
        server.join().unwrap();

        assert_eq!(installed.version, version);
        assert!(java_command_existed);
        assert_eq!(core.selected_version(&app_id).unwrap(), None);
        assert!(core.installed().unwrap().is_empty());
        assert!(!Path::new(&installed.install_path).exists());
        let events = core.operation_events().unwrap();
        assert!(events.iter().any(|event| {
            event.state == OperationState::Succeeded && event.message == "Installation committed"
        }));
        assert!(events.iter().any(|event| {
            event.state == OperationState::Succeeded && event.message == "Uninstall committed"
        }));
    }

    fn metadata_fixture_server(expected_requests: usize) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}/v3/");
        let archive_name = if cfg!(windows) {
            "OpenJDK21U-jdk_fixture.zip"
        } else {
            "OpenJDK21U-jdk_fixture.tar.gz"
        };
        let archive_url = format!("http://{address}/v3/releases/{archive_name}");
        let binary = serde_json::json!({
            "architecture": if std::env::consts::ARCH == "x86_64" { "x64" } else { "aarch64" },
            "heap_size": "normal",
            "image_type": "jdk",
            "jvm_impl": "hotspot",
            "os": if cfg!(windows) { "windows" } else if cfg!(target_os = "macos") { "mac" } else { "linux" },
            "package": {
                "checksum": "00".repeat(32),
                "link": archive_url,
                "name": archive_name,
                "signature_link": format!("{archive_url}.sig"),
                "size": 42
            },
            "project": "jdk"
        });
        let mut unsigned_binary = binary.clone();
        unsigned_binary["package"]
            .as_object_mut()
            .unwrap()
            .remove("signature_link");
        let releases = serde_json::json!([
            {
                "binaries": [unsigned_binary],
                "release_type": "ga",
                "timestamp": "2026-01-19T00:00:00Z",
                "vendor": "eclipse",
                "version_data": { "semver": "21.0.1+12.0.LTS" }
            },
            {
                "binaries": [binary],
                "release_type": "ga",
                "timestamp": "2026-01-20T00:00:00Z",
                "vendor": "eclipse",
                "version_data": { "semver": "21.0.2+13.0.LTS" }
            }
        ]);
        let routes = BTreeMap::from([
            (
                "/v3/info/available_releases".to_owned(),
                serde_json::to_vec(&serde_json::json!({ "available_lts_releases": [21] })).unwrap(),
            ),
            (
                "/v3/assets/feature_releases/21/ga".to_owned(),
                serde_json::to_vec(&releases).unwrap(),
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

    fn metadata_sequence_server(
        responses: Vec<(String, Vec<u8>)>,
    ) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}/v3/");
        let server = thread::spawn(move || {
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut served = 0;
            while served < responses.len() && std::time::Instant::now() < deadline {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_nonblocking(false).unwrap();
                        stream
                            .set_read_timeout(Some(Duration::from_secs(1)))
                            .unwrap();
                        let mut request = [0_u8; 4096];
                        let read = stream.read(&mut request).unwrap();
                        let request = String::from_utf8_lossy(&request[..read]);
                        assert!(request.starts_with("GET /v3/info/available_releases HTTP/1.1"));
                        assert!(
                            request
                                .to_ascii_lowercase()
                                .contains("\r\naccept: application/json\r\n")
                        );
                        let (content_type, body) = &responses[served];
                        write!(
                            stream,
                            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .unwrap();
                        stream.write_all(body).unwrap();
                        served += 1;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                    }
                    Err(error) => panic!("fixture accept failed: {error}"),
                }
            }
            assert_eq!(served, responses.len(), "metadata retry count changed");
        });
        (Url::parse(&base).unwrap(), server)
    }

    fn compile_fixture_java(directory: &Path, version: &ExactVersion) -> Vec<u8> {
        let source = directory.join("fixture-java.rs");
        let executable = directory.join(format!("fixture-java{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(
            &source,
            format!(
                "fn main() {{ let name=std::env::current_exe().unwrap(); let name=name.file_stem().unwrap().to_string_lossy(); if name.starts_with(\"javac\") {{ println!(\"javac {}\"); }} else {{ eprintln!(\"openjdk version \\\"{}\\\"\\nOpenJDK Runtime Environment Temurin-fixture\"); }} }}\n",
                java_version_core(version),
                java_version_core(version),
            ),
        )
        .unwrap();
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let output = Command::new(rustc)
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
        std::fs::read(executable).unwrap()
    }

    fn fixture_archive(kind: ArchiveKind, archive_name: &str, java: &[u8]) -> Vec<u8> {
        let root = if kind == ArchiveKind::Zip {
            archive_name.strip_suffix(".zip").unwrap()
        } else {
            archive_name.strip_suffix(".tar.gz").unwrap()
        };
        let home = if cfg!(target_os = "macos") {
            format!("{root}/Contents/Home")
        } else {
            root.to_owned()
        };
        let extension = if cfg!(windows) { ".exe" } else { "" };
        let files = [
            (format!("{home}/bin/java{extension}"), java),
            (format!("{home}/bin/javac{extension}"), java),
        ];
        if kind == ArchiveKind::Zip {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
            for (path, content) in files {
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored)
                    .unix_permissions(0o755);
                writer.start_file(path, options).unwrap();
                writer.write_all(content).unwrap();
            }
            writer.finish().unwrap().into_inner()
        } else {
            let mut builder = tar::Builder::new(Vec::new());
            for (path, content) in files {
                let mut header = tar::Header::new_gnu();
                header.set_size(content.len().try_into().unwrap());
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, path, content).unwrap();
            }
            builder.finish().unwrap();
            let tar = builder.into_inner().unwrap();
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&tar).unwrap();
            encoder.finish().unwrap()
        }
    }

    fn vertical_fixture_server(
        archive_name: &str,
        archive: Vec<u8>,
        checksum: &str,
        signature: Vec<u8>,
        public_key: Vec<u8>,
        expected_requests: usize,
    ) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let base = format!("http://{address}/v3/");
        let archive_url = format!("{base}releases/{archive_name}");
        let (os, architecture, _) = current_api_target().unwrap();
        let release = serde_json::to_vec(&serde_json::json!([{
            "binaries": [{
                "architecture": architecture,
                "heap_size": "normal",
                "image_type": "jdk",
                "jvm_impl": "hotspot",
                "os": os,
                "package": {
                    "checksum": checksum,
                    "link": archive_url,
                    "name": archive_name,
                    "signature_link": format!("{archive_url}.sig"),
                    "size": archive.len()
                },
                "project": "jdk"
            }],
            "release_type": "ga",
            "timestamp": "2026-01-20T00:00:00Z",
            "vendor": "eclipse",
            "version_data": { "semver": "21.0.2+13.0.LTS" }
        }]))
        .unwrap();
        let routes = BTreeMap::from([
            ("/v3/assets/feature_releases/21/ga".to_owned(), release),
            (format!("/v3/releases/{archive_name}"), archive),
            (format!("/v3/releases/{archive_name}.sig"), signature),
            ("/v3/public-key.asc".to_owned(), public_key),
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

    fn plugin_fixture_script(
        archive_name: &str,
        archive_url: &str,
        signature_url: &str,
        public_key_url: &str,
        checksum: &str,
        version: &ExactVersion,
        install_path: &Path,
    ) -> String {
        let (os, architecture, _) = current_api_target().unwrap();
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": torben_contracts::plugin::PLUGIN_PROTOCOL_VERSION,
                "pluginId": "app.torben.plugin.temurin",
                "pluginVersion": env!("CARGO_PKG_VERSION"),
                "applications": [{
                    "id": "temurin",
                    "displayName": "Eclipse Temurin",
                    "summary": "fixture",
                    "categories": ["runtime"],
                    "capabilities": ["versions", "install", "select", "uninstall"],
                    "sources": [{
                        "id": "temurin.official",
                        "displayName": "Official archive",
                        "managed": true
                    }]
                }]
            }
        })
        .to_string();
        let resolved = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": { "requested": "lts", "resolved": version }
        })
        .to_string();
        let plan = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {
                "appId": "temurin",
                "version": version,
                "sourceId": "temurin.official",
                "steps": [
                    { "type": "download", "url": archive_url, "destination_name": archive_name },
                    { "type": "verify_sha256", "archive_name": archive_name, "expected": checksum },
                    {
                        "type": "verify_detached_signature",
                        "archive_name": archive_name,
                        "signature_url": signature_url,
                        "public_key_url": public_key_url,
                        "trusted_fingerprint": ADOPTIUM_RELEASE_FINGERPRINT
                    },
                    { "type": "extract_archive", "archive_name": archive_name, "strip_components": 0 },
                    {
                        "type": "health_check",
                        "executable": "java",
                        "arguments": ["-version"],
                        "expected_output": java_version_core(version)
                    },
                    { "type": "create_shims", "commands": ["java", "javac"] }
                ],
                "metadata": { "target": format!("{os}-{architecture}"), "feature": "21" }
            }
        })
        .to_string();
        let health = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "healthy": true,
                "actualVersion": version,
                "message": "healthy"
            }
        })
        .to_string();
        let uninstall = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "appId": "temurin",
                "version": version,
                "sourceId": "temurin.official",
                "installPath": install_path.display().to_string(),
                "preserveUserData": true
            }
        })
        .to_string();
        if cfg!(windows) {
            format!(
                "@echo off\r\n:loop\r\nset request=\r\nset /p request=\r\nif errorlevel 1 exit /b 0\r\necho %request%| findstr /c:\"initialize\" >nul && (echo {initialize}& goto loop)\r\necho %request%| findstr /c:\"version.resolve\" >nul && (echo {resolved}& goto loop)\r\necho %request%| findstr /c:\"uninstall.plan\" >nul && (echo {uninstall}& goto loop)\r\necho %request%| findstr /c:\"install.plan\" >nul && (echo {plan}& goto loop)\r\necho %request%| findstr /c:\"health.check\" >nul && (echo {health}& goto loop)\r\nexit /b 1\r\n"
            )
        } else {
            format!(
                "#!/bin/sh\nwhile IFS= read -r request; do\ncase \"$request\" in\n  *initialize*) printf '%s\\n' '{initialize}' ;;\n  *version.resolve*) printf '%s\\n' '{resolved}' ;;\n  *uninstall.plan*) printf '%s\\n' '{uninstall}' ;;\n  *install.plan*) printf '%s\\n' '{plan}' ;;\n  *health.check*) printf '%s\\n' '{health}' ;;\n  *) exit 1 ;;\nesac\ndone\n"
            )
        }
    }
}
