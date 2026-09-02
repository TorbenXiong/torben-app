use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs::File,
    future::Future,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime},
};

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tar::Archive;
use tokio::io::AsyncWriteExt;
use torben_contracts::{
    AppId, ExactVersion, InstallRecord, InstallScope, OperationState, SourceId, TorbenError,
    TorbenResult, VersionDescriptor,
    plugin::{InstallPlan, InstallStep},
};
use url::Url;
use xz2::read::XzDecoder;
use zip::ZipArchive;

use crate::{
    TorbenPaths,
    operation::{CancellationProbe, OperationJournal},
    process,
};

const NODE_BASE_URL: &str = "https://nodejs.org/dist/";

#[derive(Debug, Clone)]
pub struct NodeProvider {
    client: reqwest::Client,
    base_url: Url,
    #[cfg(any(test, feature = "test-fixtures"))]
    fixture_signature: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeDistribution {
    pub archive_name: String,
    pub archive_url: Url,
    pub checksums_url: Url,
    pub signature_url: Url,
    pub archive_kind: ArchiveKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    Zip,
    TarGz,
    TarXz,
}

#[derive(Debug, Deserialize)]
struct NodeRelease {
    version: String,
    date: String,
    lts: serde_json::Value,
}

impl NodeProvider {
    pub fn official() -> TorbenResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            base_url: Url::parse(NODE_BASE_URL).map_err(|error| {
                TorbenError::internal("The built-in Node.js URL is invalid.")
                    .with_detail("reason", error.to_string())
            })?,
            #[cfg(any(test, feature = "test-fixtures"))]
            fixture_signature: None,
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub(crate) fn with_base_url(base_url: Url) -> TorbenResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent("Torben-App-Test")
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            base_url,
            fixture_signature: None,
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub(crate) fn with_fixture_base_url(
        base_url: Url,
        fixture_signature: Vec<u8>,
    ) -> TorbenResult<Self> {
        let mut provider = Self::with_base_url(base_url)?;
        provider.fixture_signature = Some(fixture_signature);
        Ok(provider)
    }

    pub async fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        let releases = self.fetch_index().await?;
        releases
            .into_iter()
            .map(|release| {
                Ok(VersionDescriptor {
                    version: ExactVersion::from_str(&release.version)?,
                    lts_name: release.lts.as_str().map(ToOwned::to_owned),
                    released_at: release.date,
                    recommended: release.lts.is_string(),
                })
            })
            .collect()
    }

    pub async fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        let versions = self.list_versions().await?;
        resolve_from_versions(requested, versions)
    }

    pub fn distribution(&self, version: &ExactVersion) -> TorbenResult<NodeDistribution> {
        let (platform, architecture, extension, archive_kind) =
            match (std::env::consts::OS, std::env::consts::ARCH) {
                ("windows", "x86_64") => ("win", "x64", "zip", ArchiveKind::Zip),
                ("windows", "aarch64") => ("win", "arm64", "zip", ArchiveKind::Zip),
                ("macos", "x86_64") => ("darwin", "x64", "tar.gz", ArchiveKind::TarGz),
                ("macos", "aarch64") => ("darwin", "arm64", "tar.gz", ArchiveKind::TarGz),
                ("linux", "x86_64") => ("linux", "x64", "tar.xz", ArchiveKind::TarXz),
                ("linux", "aarch64") => ("linux", "arm64", "tar.xz", ArchiveKind::TarXz),
                (os, architecture) => {
                    return Err(TorbenError::new(
                        "unsupported_target",
                        "Node.js is not supported on this target.",
                    )
                    .with_detail("os", os)
                    .with_detail("architecture", architecture));
                }
            };
        let version_segment = format!("v{version}/");
        let version_url = self.base_url.join(&version_segment).map_err(url_error)?;
        let archive_name = format!("node-v{version}-{platform}-{architecture}.{extension}");
        Ok(NodeDistribution {
            archive_url: version_url.join(&archive_name).map_err(url_error)?,
            checksums_url: version_url.join("SHASUMS256.txt").map_err(url_error)?,
            signature_url: version_url.join("SHASUMS256.txt.sig").map_err(url_error)?,
            archive_name,
            archive_kind,
        })
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
        let distribution = self.validate_install_plan(plan, app_id, version)?;
        let cancellation = journal.cancellation_probe();
        cancellation.check()?;
        let download_dir = paths.download_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&download_dir).map_err(io_error)?;
        let archive_path = download_dir.join(&distribution.archive_name);
        journal.record(
            OperationState::Running,
            "download",
            "Downloading signed Node.js release metadata",
            Some(0.1),
        )?;
        let manifest = self
            .fetch_bytes_checked(&distribution.checksums_url, Some(&cancellation))
            .await?;
        let signature = self
            .fetch_checksum_signature_checked(&distribution.signature_url, Some(&cancellation))
            .await?;
        cancellation.check()?;
        journal.record(
            OperationState::Running,
            "verify",
            "Verifying the official checksum manifest signature",
            Some(0.2),
        )?;
        self.verify_checksum_signature(&manifest, &signature)?;
        let manifest = std::str::from_utf8(&manifest).map_err(|error| {
            TorbenError::new(
                "checksum_manifest_invalid",
                "The signed Node.js checksum manifest is not valid UTF-8.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let expected_hash = checksum_for(manifest, &distribution.archive_name)?;
        cancellation.check()?;
        journal.record(
            OperationState::Running,
            "download",
            format!("Downloading {}", distribution.archive_name),
            Some(0.3),
        )?;
        if !archive_path.is_file()
            || sha256_file_checked(&archive_path, Some(&cancellation))? != expected_hash
        {
            self.download_archive_checked(
                &distribution.archive_url,
                &archive_path,
                Some(&cancellation),
            )
            .await?;
        }
        cancellation.check()?;
        journal.record(
            OperationState::Running,
            "verify",
            "Verifying official SHA-256 checksum",
            Some(0.45),
        )?;
        let actual_hash = sha256_file_checked(&archive_path, Some(&cancellation))?;
        if actual_hash != expected_hash {
            return Err(TorbenError::new(
                "archive_hash_mismatch",
                "The Node.js archive does not match the official checksum.",
            )
            .with_detail("expected", expected_hash)
            .with_detail("actual", actual_hash));
        }

        let staging =
            paths
                .staging_dir()
                .join(format!("install-{}-{}", app_id, journal.operation_id()));
        std::fs::create_dir_all(&staging).map_err(io_error)?;
        journal.record(
            OperationState::Running,
            "extract",
            "Extracting into the staging directory",
            Some(0.6),
        )?;
        let archive_kind = distribution.archive_kind;
        let archive_for_task = archive_path.clone();
        let staging_for_task = staging.clone();
        let extraction_cancellation = cancellation.clone();
        let extracted_root = match tokio::task::spawn_blocking(move || {
            extract_archive(
                &archive_for_task,
                archive_kind,
                &staging_for_task,
                &extraction_cancellation,
            )
        })
        .await
        .map_err(|error| {
            TorbenError::new("archive_task_failed", "The archive extraction task failed.")
                .with_detail("reason", error.to_string())
        }) {
            Ok(Ok(path)) => path,
            Ok(Err(error)) | Err(error) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(error);
            }
        };

        cancellation.check()?;
        journal.record(
            OperationState::Running,
            "health_check",
            "Checking the extracted Node.js version",
            Some(0.8),
        )?;
        if let Err(error) = self.health_check_path(&extracted_root, version) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        cancellation.check()?;
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(TorbenError::new(
                "install_path_exists",
                "The final installation directory already exists.",
            )
            .with_detail("path", final_path.display().to_string()));
        }
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        if let Err(error) = std::fs::rename(&extracted_root, &final_path) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(TorbenError::new(
                "install_commit_failed",
                "Could not atomically commit the installation.",
            )
            .with_detail("reason", error.to_string()));
        }
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

    fn validate_install_plan(
        &self,
        plan: &InstallPlan,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<NodeDistribution> {
        if &plan.app_id != app_id || &plan.version != version {
            return Err(invalid_install_plan(
                "The application or version does not match the active operation.",
            ));
        }
        let expected_source = SourceId::new("node.official")?;
        if plan.source_id != expected_source {
            return Err(invalid_install_plan(
                "The Node.js plan changed the immutable source owner.",
            ));
        }
        if plan.metadata.get("target") != Some(&crate::node_plugin::current_target()) {
            return Err(invalid_install_plan(
                "The Node.js plan target does not match the current host.",
            ));
        }
        let [
            InstallStep::Download {
                url: archive_url,
                destination_name,
            },
            InstallStep::VerifySha256Manifest {
                manifest_url,
                signature_url,
                archive_name: verified_archive_name,
            },
            InstallStep::ExtractArchive {
                archive_name: extracted_archive_name,
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
            return Err(invalid_install_plan(
                "The Node.js plan has an unsupported step order or shape.",
            ));
        };

        let expected = self.distribution(version)?;
        let parsed_archive_url = Url::parse(archive_url).map_err(|error| {
            invalid_install_plan("The archive URL is invalid.")
                .with_detail("reason", error.to_string())
        })?;
        let parsed_manifest_url = Url::parse(manifest_url).map_err(|error| {
            invalid_install_plan("The checksum manifest URL is invalid.")
                .with_detail("reason", error.to_string())
        })?;
        let signature_url = signature_url.as_deref().ok_or_else(|| {
            invalid_install_plan("The official checksum signature URL is missing.")
        })?;
        let parsed_signature_url = Url::parse(signature_url).map_err(|error| {
            invalid_install_plan("The checksum signature URL is invalid.")
                .with_detail("reason", error.to_string())
        })?;
        let distribution = NodeDistribution {
            archive_name: destination_name.clone(),
            archive_url: parsed_archive_url,
            checksums_url: parsed_manifest_url,
            signature_url: parsed_signature_url,
            archive_kind: expected.archive_kind,
        };
        if distribution != expected
            || verified_archive_name != destination_name
            || extracted_archive_name != destination_name
            || *strip_components != 0
        {
            return Err(invalid_install_plan(
                "The Node.js archive plan does not match the official target distribution.",
            ));
        }
        if executable != "node"
            || arguments.as_slice() != ["--version"]
            || expected_output != &format!("v{version}")
        {
            return Err(invalid_install_plan(
                "The Node.js health check is not the required exact-version check.",
            ));
        }
        if commands.as_slice() != ["node", "npm", "npx"] {
            return Err(invalid_install_plan(
                "The Node.js plan exposes an unexpected set of terminal commands.",
            ));
        }
        Ok(distribution)
    }

    fn verify_checksum_signature(&self, manifest: &[u8], signature: &[u8]) -> TorbenResult<()> {
        #[cfg(not(any(test, feature = "test-fixtures")))]
        let _ = self;
        #[cfg(any(test, feature = "test-fixtures"))]
        if let Some(expected) = &self.fixture_signature {
            if !manifest.is_empty() && signature == expected {
                return Ok(());
            }
            return Err(TorbenError::new(
                "checksum_signature_invalid",
                "The local fixture checksum signature is invalid.",
            ));
        }
        crate::node_signature::verify_checksum_signature(manifest, signature).map(|_| ())
    }

    pub fn health_check(&self, record: &InstallRecord) -> TorbenResult<()> {
        self.health_check_path(Path::new(&record.install_path), &record.version)
    }

    pub async fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let executable_name = if cfg!(windows) { "node.exe" } else { "node" };
        let mut seen = BTreeSet::new();
        let mut records = Vec::new();
        let path = std::env::var_os("PATH").unwrap_or_default();
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(executable_name);
            if !candidate.is_file() {
                continue;
            }
            let Ok(canonical) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) || !seen.insert(canonical.clone()) {
                continue;
            }
            let Ok(child) = process::async_command(&canonical)
                .arg("--version")
                .kill_on_drop(true)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            else {
                continue;
            };
            let output = match tokio::time::timeout(
                std::time::Duration::from_secs(3),
                child.wait_with_output(),
            )
            .await
            {
                Ok(Ok(output)) if output.status.success() => output,
                _ => continue,
            };
            let actual = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            let Ok(version) = ExactVersion::from_str(&actual) else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("node")?,
                version,
                source_id: SourceId::new("node.external")?,
                scope: InstallScope::External,
                install_path: canonical.display().to_string(),
                installed_at: String::new(),
                health: "healthy".to_owned(),
            });
        }
        Ok(records)
    }

    pub fn command_path(&self, install_path: &Path, command: &str) -> TorbenResult<PathBuf> {
        if !matches!(command, "node" | "npm" | "npx") {
            return Err(TorbenError::new(
                "unsupported_command",
                "The Node.js plugin does not expose this command.",
            )
            .with_detail("command", command));
        }
        let path = if cfg!(windows) {
            let extension = if command == "node" { "exe" } else { "cmd" };
            install_path.join(format!("{command}.{extension}"))
        } else {
            install_path.join("bin").join(command)
        };
        if path.is_file() {
            Ok(path)
        } else {
            Err(
                TorbenError::new("managed_command_missing", "A managed command is missing.")
                    .with_detail("path", path.display().to_string()),
            )
        }
    }

    fn health_check_path(&self, install_path: &Path, version: &ExactVersion) -> TorbenResult<()> {
        let node = self.command_path(install_path, "node")?;
        let npm = self.command_path(install_path, "npm")?;
        let npx = self.command_path(install_path, "npx")?;
        let path = managed_command_path(install_path)?;

        let actual = run_health_command("node", &node, &path)?;
        let expected = format!("v{version}");
        if actual != expected {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The extracted Node.js version is not the requested version.",
            )
            .with_detail("expected", expected)
            .with_detail("actual", actual));
        }

        for (command, executable) in [("npm", npm), ("npx", npx)] {
            let actual = run_health_command(command, &executable, &path)?;
            validate_package_manager_version(command, &actual)?;
        }
        Ok(())
    }

    async fn fetch_index(&self) -> TorbenResult<Vec<NodeRelease>> {
        let url = self.base_url.join("index.json").map_err(url_error)?;
        let response = self.client.get(url).send().await.map_err(network_error)?;
        validate_official_response(&response, &self.base_url)?;
        let response = response.error_for_status().map_err(network_error)?;
        response.json().await.map_err(network_error)
    }

    #[cfg(test)]
    async fn fetch_bytes(&self, url: &Url) -> TorbenResult<Vec<u8>> {
        self.fetch_bytes_checked(url, None).await
    }

    async fn fetch_bytes_checked(
        &self,
        url: &Url,
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
        validate_official_response(&response, &self.base_url)?;
        let response = response.error_for_status().map_err(network_error)?;
        await_with_cancellation(
            async {
                response
                    .bytes()
                    .await
                    .map(|bytes| bytes.to_vec())
                    .map_err(network_error)
            },
            cancellation,
        )
        .await
    }

    #[cfg(test)]
    async fn fetch_checksum_signature(&self, url: &Url) -> TorbenResult<Vec<u8>> {
        self.fetch_checksum_signature_checked(url, None).await
    }

    async fn fetch_checksum_signature_checked(
        &self,
        url: &Url,
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
        validate_official_response(&response, &self.base_url)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(missing_checksum_signature(url));
        }
        let response = response.error_for_status().map_err(network_error)?;
        let signature = await_with_cancellation(
            async { response.bytes().await.map_err(network_error) },
            cancellation,
        )
        .await?;
        if signature.is_empty() {
            return Err(missing_checksum_signature(url));
        }
        Ok(signature.to_vec())
    }

    #[cfg(test)]
    async fn download_archive(&self, url: &Url, destination: &Path) -> TorbenResult<()> {
        self.download_archive_checked(url, destination, None).await
    }

    async fn download_archive_checked(
        &self,
        url: &Url,
        destination: &Path,
        cancellation: Option<&CancellationProbe>,
    ) -> TorbenResult<()> {
        let partial = destination.with_extension("partial");
        let result = async {
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
            validate_official_response(&response, &self.base_url)?;
            let response = response.error_for_status().map_err(network_error)?;
            let mut file = tokio::fs::File::create(&partial).await.map_err(io_error)?;
            let mut stream = response.bytes_stream();
            while let Some(chunk) = await_with_cancellation(
                async { stream.next().await.transpose().map_err(network_error) },
                cancellation,
            )
            .await?
            {
                file.write_all(&chunk).await.map_err(io_error)?;
            }
            file.flush().await.map_err(io_error)?;
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
            () = tokio::time::sleep(Duration::from_millis(100)) => cancellation.check()?,
        }
    }
}

fn managed_command_path(install_path: &Path) -> TorbenResult<OsString> {
    let managed_bin = if cfg!(windows) {
        install_path.to_path_buf()
    } else {
        install_path.join("bin")
    };
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    std::env::join_paths(std::iter::once(managed_bin).chain(std::env::split_paths(&inherited)))
        .map_err(|error| {
            TorbenError::new(
                "health_check_environment_invalid",
                "Could not prepare an isolated PATH for the managed health check.",
            )
            .with_detail("reason", error.to_string())
        })
}

fn run_health_command(command: &str, executable: &Path, path: &OsString) -> TorbenResult<String> {
    let output = process::command(executable)
        .arg("--version")
        .env("PATH", path)
        .output()
        .map_err(|error| {
            TorbenError::new(
                "health_check_start_failed",
                "Could not start a managed Node.js command.",
            )
            .with_detail("command", command)
            .with_detail("path", executable.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
    if !output.status.success() {
        return Err(TorbenError::new(
            "health_check_failed",
            "A managed Node.js command returned an error.",
        )
        .with_detail("command", command)
        .with_detail("status", output.status.to_string())
        .with_detail("stderr", bounded_output(&output.stderr)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn validate_package_manager_version(command: &str, actual: &str) -> TorbenResult<()> {
    ExactVersion::from_str(actual).map(|_| ()).map_err(|error| {
        TorbenError::new(
            "health_check_output_invalid",
            "A managed Node.js package command returned an invalid version.",
        )
        .with_detail("command", command)
        .with_detail("actual", actual)
        .with_detail("reason", error.message)
    })
}

fn bounded_output(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .trim()
        .chars()
        .take(512)
        .collect()
}

fn validate_official_response(response: &reqwest::Response, base_url: &Url) -> TorbenResult<()> {
    if response.url().scheme() != base_url.scheme()
        || response.url().host_str() != base_url.host_str()
        || response.url().port_or_known_default() != base_url.port_or_known_default()
    {
        return Err(TorbenError::new(
            "unexpected_download_origin",
            "The Node.js request was redirected outside the official origin.",
        )
        .with_detail("url", response.url().to_string()));
    }
    Ok(())
}

fn checksum_for(manifest: &str, archive_name: &str) -> TorbenResult<String> {
    manifest
        .lines()
        .filter_map(|line| line.split_once("  "))
        .find(|(_, name)| *name == archive_name)
        .map(|(hash, _)| hash.to_ascii_lowercase())
        .ok_or_else(|| {
            TorbenError::new(
                "archive_checksum_missing",
                "The archive is missing from the official checksum manifest.",
            )
            .with_detail("archive", archive_name)
        })
}

fn resolve_from_versions(
    requested: &str,
    versions: Vec<VersionDescriptor>,
) -> TorbenResult<ExactVersion> {
    if let Ok(exact) = ExactVersion::from_str(requested) {
        return versions
            .into_iter()
            .find(|item| item.version == exact)
            .map(|item| item.version)
            .ok_or_else(|| {
                TorbenError::new(
                    "version_not_found",
                    "The exact Node.js version was not found in the official index.",
                )
                .with_detail("requested", requested)
            });
    }
    let normalized = requested.to_ascii_lowercase();
    match normalized.as_str() {
        "lts" => versions
            .into_iter()
            .find(|item| item.lts_name.is_some())
            .map(|item| item.version),
        "current" | "latest" => versions.into_iter().next().map(|item| item.version),
        _ => None,
    }
    .ok_or_else(|| {
        TorbenError::new(
            "version_alias_not_found",
            "Use an exact Node.js version, 'lts', or 'current'.",
        )
        .with_detail("requested", requested)
    })
}

#[cfg(test)]
fn sha256_file(path: &Path) -> TorbenResult<String> {
    sha256_file_checked(path, None)
}

pub(crate) fn sha256_file_checked(
    path: &Path,
    cancellation: Option<&CancellationProbe>,
) -> TorbenResult<String> {
    let mut file = File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        if let Some(cancellation) = cancellation {
            cancellation.check()?;
        }
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn extract_archive(
    archive_path: &Path,
    kind: ArchiveKind,
    staging: &Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<PathBuf> {
    extract_archive_contents(archive_path, kind, staging, cancellation)?;
    let roots: Vec<PathBuf> = std::fs::read_dir(staging)
        .map_err(io_error)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    if roots.len() != 1 {
        return Err(TorbenError::new(
            "archive_layout_invalid",
            "The Node.js archive must contain one top-level directory.",
        )
        .with_detail("directoryCount", roots.len().to_string()));
    }
    Ok(roots[0].clone())
}

pub(crate) fn extract_archive_contents(
    archive_path: &Path,
    kind: ArchiveKind,
    staging: &Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<()> {
    match kind {
        ArchiveKind::Zip => extract_zip(archive_path, staging, cancellation)?,
        ArchiveKind::TarGz => {
            let file = File::open(archive_path).map_err(io_error)?;
            extract_tar(GzDecoder::new(file), staging, cancellation)?;
        }
        ArchiveKind::TarXz => {
            let file = File::open(archive_path).map_err(io_error)?;
            extract_tar(XzDecoder::new(file), staging, cancellation)?;
        }
    }
    Ok(())
}

fn extract_zip(
    archive_path: &Path,
    staging: &Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<()> {
    let file = File::open(archive_path).map_err(io_error)?;
    let mut archive = ZipArchive::new(file).map_err(|error| archive_error(error.to_string()))?;
    for index in 0..archive.len() {
        cancellation.check()?;
        let mut entry = archive
            .by_index(index)
            .map_err(|error| archive_error(error.to_string()))?;
        let enclosed = entry.enclosed_name().ok_or_else(|| {
            TorbenError::new(
                "archive_path_unsafe",
                "The archive contains an unsafe path.",
            )
            .with_detail("entry", entry.name())
        })?;
        if enclosed.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(TorbenError::new(
                "archive_path_unsafe",
                "The archive contains a path outside the staging directory.",
            ));
        }
        let output_path = staging.join(&enclosed);
        #[cfg(unix)]
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170_000 == 0o120_000)
        {
            if entry.size() > 4096 {
                return Err(archive_error(
                    "A symbolic-link target is too long.".to_owned(),
                ));
            }
            let mut target = String::new();
            entry.read_to_string(&mut target).map_err(io_error)?;
            let target = PathBuf::from(target);
            validate_archive_symlink(&enclosed, &target)?;
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent).map_err(io_error)?;
            }
            std::os::unix::fs::symlink(&target, &output_path).map_err(io_error)?;
            continue;
        }
        if entry.is_dir() {
            std::fs::create_dir_all(&output_path).map_err(io_error)?;
        } else {
            if let Some(parent) = output_path.parent() {
                std::fs::create_dir_all(parent).map_err(io_error)?;
            }
            let mut output = File::create(&output_path).map_err(io_error)?;
            std::io::copy(&mut entry, &mut output).map_err(io_error)?;
            output.flush().map_err(io_error)?;
        }
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode().filter(|mode| mode & 0o777 != 0) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&output_path, std::fs::Permissions::from_mode(mode & 0o777))
                .map_err(io_error)?;
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_archive_symlink(entry: &Path, target: &Path) -> TorbenResult<()> {
    if target.is_absolute() {
        return Err(TorbenError::new(
            "archive_path_unsafe",
            "The archive contains an absolute symbolic-link target.",
        ));
    }
    let mut depth = entry
        .parent()
        .map_or(0, |parent| parent.components().count());
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(TorbenError::new(
                    "archive_path_unsafe",
                    "The archive contains a symbolic link outside the staging directory.",
                ));
            }
        }
    }
    Ok(())
}

fn extract_tar<R: Read>(
    reader: R,
    staging: &Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<()> {
    for entry in Archive::new(reader).entries().map_err(io_error)? {
        cancellation.check()?;
        let mut entry = entry.map_err(io_error)?;
        if !entry.unpack_in(staging).map_err(io_error)? {
            return Err(TorbenError::new(
                "archive_path_unsafe",
                "The archive contains a path outside the staging directory.",
            ));
        }
    }
    Ok(())
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or_else(
            |_| "0".to_owned(),
            |duration| duration.as_secs().to_string(),
        )
}

fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new("network_error", "The official Node.js request failed.")
        .with_detail("reason", error.to_string())
}

fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::internal("Could not construct an official Node.js URL.")
        .with_detail("reason", error.to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new("filesystem_error", "A filesystem operation failed.")
        .with_detail("reason", error.to_string())
}

fn archive_error(reason: String) -> TorbenError {
    TorbenError::new(
        "archive_error",
        "The Node.js archive could not be extracted.",
    )
    .with_detail("reason", reason)
}

fn invalid_install_plan(reason: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The Node.js plugin returned an unsafe or inconsistent install plan.",
    )
    .with_detail("reason", reason)
}

fn missing_checksum_signature(url: &Url) -> TorbenError {
    TorbenError::new(
        "checksum_signature_missing",
        "The official Node.js checksum signature is missing.",
    )
    .with_detail("url", url.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{Read as _, Write as _},
        net::TcpListener,
        str::FromStr,
        sync::{Arc, mpsc},
        thread,
        time::Duration,
    };

    use sha2::{Digest, Sha256};
    use tempfile::tempdir;
    use torben_contracts::{
        AppId, ExactVersion, InstallRecord, InstallScope, OperationKind, OperationState, SourceId,
        VersionDescriptor,
        plugin::{InstallPlan, InstallStep, UninstallPlan},
    };

    use super::{
        ArchiveKind, NodeProvider, checksum_for, resolve_from_versions, sha256_file,
        validate_package_manager_version,
    };
    use crate::{
        StateStore, TorbenCore, TorbenPaths,
        bundled_shim::BundledShim,
        execute_uninstall_transaction,
        node_plugin::{BundledPlugin, current_target},
        operation::OperationJournal,
        validate_uninstall_plan,
    };

    const OFFICIAL_MANIFEST: &[u8] =
        include_bytes!("../assets/node-signature-fixtures/v24.19.0-SHASUMS256.txt");
    const OFFICIAL_SIGNATURE_HEX: &str = "887504001608001D1621045BE8A3F6C8A5C01D106C0AD820B1A390B168D35605026A709B58000A091020B1A390B168D356914300FF4E7E884D9979816A9982E075022E19D56D91F6BAAC4481A2790E53931438CA730100E97B359FC84D02DC2BFB3A3D5E2B754A5E23DC0EC144E6E187D7E977D597D40B";

    #[test]
    fn validates_npm_and_npx_semantic_versions() {
        assert!(validate_package_manager_version("npm", "11.6.2").is_ok());
        assert!(validate_package_manager_version("npx", "v11.6.2").is_ok());
        let error = validate_package_manager_version("npm", "unexpected output").unwrap_err();
        assert_eq!(error.code, "health_check_output_invalid");
        assert_eq!(
            error.details.get("command").map(String::as_str),
            Some("npm")
        );
    }

    #[test]
    fn health_check_requires_all_managed_commands_before_execution() {
        let root = tempdir().unwrap();
        let install_path = if cfg!(windows) {
            root.path().to_path_buf()
        } else {
            root.path().join("bin")
        };
        std::fs::create_dir_all(&install_path).unwrap();
        let node_name = if cfg!(windows) { "node.exe" } else { "node" };
        let npm_name = if cfg!(windows) { "npm.cmd" } else { "npm" };
        std::fs::write(install_path.join(node_name), []).unwrap();
        std::fs::write(install_path.join(npm_name), []).unwrap();
        let provider = NodeProvider::official().unwrap();

        let error = provider
            .health_check_path(root.path(), &ExactVersion::from_str("24.19.0").unwrap())
            .unwrap_err();

        assert_eq!(error.code, "managed_command_missing");
        assert!(
            error
                .details
                .get("path")
                .is_some_and(|path| path.ends_with(if cfg!(windows) { "npx.cmd" } else { "npx" }))
        );
    }

    #[test]
    fn finds_exact_checksum_entry() {
        let manifest = "abc  node-v24.19.0-win-x64.zip\ndef  other.zip\n";
        assert_eq!(
            checksum_for(manifest, "node-v24.19.0-win-x64.zip").unwrap(),
            "abc"
        );
    }

    #[test]
    fn builds_official_distribution_for_current_target() {
        let provider = NodeProvider::official().unwrap();
        let distribution = provider
            .distribution(&ExactVersion::from_str("24.19.0").unwrap())
            .unwrap();
        assert!(
            distribution
                .archive_url
                .as_str()
                .starts_with("https://nodejs.org/dist/v24.19.0/")
        );
        assert_eq!(distribution.archive_url.host_str(), Some("nodejs.org"));
    }

    #[test]
    fn resolves_aliases_to_deterministic_versions() {
        let versions = vec![
            VersionDescriptor {
                version: ExactVersion::from_str("26.7.0").unwrap(),
                lts_name: None,
                released_at: "2026-08-05".to_owned(),
                recommended: false,
            },
            VersionDescriptor {
                version: ExactVersion::from_str("24.19.0").unwrap(),
                lts_name: Some("Krypton".to_owned()),
                released_at: "2026-08-03".to_owned(),
                recommended: true,
            },
        ];
        assert_eq!(
            resolve_from_versions("current", versions.clone())
                .unwrap()
                .to_string(),
            "26.7.0"
        );
        assert_eq!(
            resolve_from_versions("lts", versions).unwrap().to_string(),
            "24.19.0"
        );
    }

    #[test]
    fn validates_exact_official_plugin_plan() {
        let provider = NodeProvider::official().unwrap();
        let app_id = AppId::new("node").unwrap();
        let version = ExactVersion::from_str("24.19.0").unwrap();
        let distribution = provider.distribution(&version).unwrap();
        let plan = InstallPlan {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: SourceId::new("node.official").unwrap(),
            steps: vec![
                InstallStep::Download {
                    url: distribution.archive_url.to_string(),
                    destination_name: distribution.archive_name.clone(),
                },
                InstallStep::VerifySha256Manifest {
                    manifest_url: distribution.checksums_url.to_string(),
                    signature_url: Some(distribution.signature_url.to_string()),
                    archive_name: distribution.archive_name.clone(),
                },
                InstallStep::ExtractArchive {
                    archive_name: distribution.archive_name,
                    strip_components: 0,
                },
                InstallStep::HealthCheck {
                    executable: "node".to_owned(),
                    arguments: vec!["--version".to_owned()],
                    expected_output: "v24.19.0".to_owned(),
                },
                InstallStep::CreateShims {
                    commands: vec!["node".to_owned(), "npm".to_owned(), "npx".to_owned()],
                },
            ],
            metadata: BTreeMap::from([("target".to_owned(), crate::node_plugin::current_target())]),
        };

        assert!(
            provider
                .validate_install_plan(&plan, &app_id, &version)
                .is_ok()
        );

        let mut redirected = plan;
        let InstallStep::Download { url, .. } = &mut redirected.steps[0] else {
            unreachable!();
        };
        *url = "https://example.invalid/node.zip".to_owned();
        let error = provider
            .validate_install_plan(&redirected, &app_id, &version)
            .unwrap_err();
        assert_eq!(error.code, "plugin_install_plan_invalid");
    }

    #[tokio::test]
    async fn discovers_and_resolves_versions_from_local_fixture() {
        let index = serde_json::json!([
            {
                "version": "v26.7.0",
                "date": "2026-08-05",
                "lts": false
            },
            {
                "version": "v24.19.0",
                "date": "2026-08-03",
                "lts": "Krypton"
            }
        ]);
        let (base_url, server) = fixture_server(
            BTreeMap::from([(
                "/dist/index.json".to_owned(),
                serde_json::to_vec(&index).unwrap(),
            )]),
            2,
        );
        let provider = NodeProvider::with_base_url(base_url).unwrap();

        let versions = provider.list_versions().await.unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[1].lts_name.as_deref(), Some("Krypton"));
        assert_eq!(
            provider.resolve_version("lts").await.unwrap().to_string(),
            "24.19.0"
        );
        server.join().unwrap();
    }

    #[tokio::test]
    async fn fetches_and_verifies_signed_manifest_from_local_fixture() {
        let signature = hex::decode(OFFICIAL_SIGNATURE_HEX).unwrap();
        let (base_url, server) = fixture_server(
            BTreeMap::from([
                (
                    "/dist/v24.19.0/SHASUMS256.txt".to_owned(),
                    OFFICIAL_MANIFEST.to_vec(),
                ),
                ("/dist/v24.19.0/SHASUMS256.txt.sig".to_owned(), signature),
            ]),
            2,
        );
        let provider = NodeProvider::with_base_url(base_url.clone()).unwrap();
        let manifest_url = base_url.join("v24.19.0/SHASUMS256.txt").unwrap();
        let signature_url = base_url.join("v24.19.0/SHASUMS256.txt.sig").unwrap();

        let fetched_manifest = provider.fetch_bytes(&manifest_url).await.unwrap();
        let fetched_signature = provider
            .fetch_checksum_signature(&signature_url)
            .await
            .unwrap();
        crate::node_signature::verify_checksum_signature(&fetched_manifest, &fetched_signature)
            .unwrap();
        server.join().unwrap();
    }

    #[tokio::test]
    async fn reports_a_missing_checksum_signature_from_local_fixture() {
        let (base_url, server) = fixture_server(BTreeMap::new(), 1);
        let provider = NodeProvider::with_base_url(base_url.clone()).unwrap();
        let signature_url = base_url.join("v24.19.0/SHASUMS256.txt.sig").unwrap();

        let error = provider
            .fetch_checksum_signature(&signature_url)
            .await
            .unwrap_err();
        assert_eq!(error.code, "checksum_signature_missing");
        server.join().unwrap();
    }

    #[tokio::test]
    async fn downloads_and_checks_archive_from_local_fixture() {
        let archive = b"local node archive fixture";
        let archive_hash = hex::encode(Sha256::digest(archive));
        let archive_name = "node-v24.19.0-win-x64.zip";
        let manifest = format!("{archive_hash}  {archive_name}\n");
        let (base_url, server) = fixture_server(
            BTreeMap::from([
                (
                    "/dist/v24.19.0/SHASUMS256.txt".to_owned(),
                    manifest.as_bytes().to_vec(),
                ),
                (format!("/dist/v24.19.0/{archive_name}"), archive.to_vec()),
            ]),
            2,
        );
        let provider = NodeProvider::with_base_url(base_url.clone()).unwrap();
        let manifest_url = base_url.join("v24.19.0/SHASUMS256.txt").unwrap();
        let archive_url = base_url.join(&format!("v24.19.0/{archive_name}")).unwrap();
        let directory = tempdir().unwrap();
        let destination = directory.path().join(archive_name);

        let fetched_manifest = provider.fetch_bytes(&manifest_url).await.unwrap();
        let fetched_manifest = std::str::from_utf8(&fetched_manifest).unwrap();
        let expected_hash = checksum_for(fetched_manifest, archive_name).unwrap();
        provider
            .download_archive(&archive_url, &destination)
            .await
            .unwrap();
        assert_eq!(sha256_file(&destination).unwrap(), expected_hash);
        server.join().unwrap();
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn local_fixture_completes_node_install_select_and_uninstall_transaction() {
        let root = tempdir().unwrap();
        let version = ExactVersion::from_str("24.19.0").unwrap();
        let app_id = AppId::new("node").unwrap();
        let source_id = SourceId::new("node.official").unwrap();
        let distribution = NodeProvider::official()
            .unwrap()
            .distribution(&version)
            .unwrap();
        let fixture_node = compile_fixture_node(root.path(), &version);
        let archive = fixture_archive(
            distribution.archive_kind,
            &distribution.archive_name,
            &fixture_node,
        );
        let archive_hash = hex::encode(Sha256::digest(&archive));
        let manifest = format!("{archive_hash}  {}\n", distribution.archive_name);
        let signature = b"torben-node-fixture-signature-v1".to_vec();
        let index = serde_json::json!([{
            "version": format!("v{version}"),
            "date": "2026-08-03",
            "lts": "Krypton"
        }]);
        let version_prefix = format!("/dist/v{version}");
        let (base_url, server) = fixture_server(
            BTreeMap::from([
                (
                    "/dist/index.json".to_owned(),
                    serde_json::to_vec(&index).unwrap(),
                ),
                (
                    format!("{version_prefix}/SHASUMS256.txt"),
                    manifest.into_bytes(),
                ),
                (
                    format!("{version_prefix}/SHASUMS256.txt.sig"),
                    signature.clone(),
                ),
                (
                    format!("{version_prefix}/{}", distribution.archive_name),
                    archive,
                ),
            ]),
            5,
        );
        let provider = NodeProvider::with_fixture_base_url(base_url, signature).unwrap();

        let versions = provider.list_versions().await.unwrap();
        assert_eq!(versions.len(), 1);
        assert_eq!(provider.resolve_version("lts").await.unwrap(), version);
        let distribution = provider.distribution(&version).unwrap();
        let plan = InstallPlan {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: source_id.clone(),
            steps: vec![
                InstallStep::Download {
                    url: distribution.archive_url.to_string(),
                    destination_name: distribution.archive_name.clone(),
                },
                InstallStep::VerifySha256Manifest {
                    manifest_url: distribution.checksums_url.to_string(),
                    signature_url: Some(distribution.signature_url.to_string()),
                    archive_name: distribution.archive_name.clone(),
                },
                InstallStep::ExtractArchive {
                    archive_name: distribution.archive_name,
                    strip_components: 0,
                },
                InstallStep::HealthCheck {
                    executable: "node".to_owned(),
                    arguments: vec!["--version".to_owned()],
                    expected_output: format!("v{version}"),
                },
                InstallStep::CreateShims {
                    commands: vec!["node".to_owned(), "npm".to_owned(), "npx".to_owned()],
                },
            ],
            metadata: BTreeMap::from([("target".to_owned(), current_target())]),
        };
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let mut install_journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            Some(&version),
        )
        .unwrap();

        let record = provider
            .install(&paths, &app_id, &version, &plan, &mut install_journal)
            .await
            .unwrap();
        store.add_installation(&record).unwrap();
        install_journal
            .succeed("Fixture installation committed")
            .unwrap();
        server.join().unwrap();

        assert_eq!(
            store.get_installation(&app_id, &version).unwrap(),
            Some(record.clone())
        );
        provider.health_check(&record).unwrap();
        store.set_selection(&app_id, &version).unwrap();
        assert_eq!(
            store.selected_version(&app_id).unwrap(),
            Some(version.clone())
        );
        store.clear_selection(&app_id).unwrap();

        let mut uninstall_journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Uninstall,
            &app_id,
            Some(&version),
        )
        .unwrap();
        let uninstall_plan = UninstallPlan {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id,
            install_path: record.install_path.clone(),
            preserve_user_data: true,
        };
        validate_uninstall_plan(&record, &uninstall_plan).unwrap();
        let source = std::path::PathBuf::from(&record.install_path);
        let staged = paths.staging_dir().join(format!(
            "uninstall-{}-{}",
            app_id,
            uninstall_journal.operation_id()
        ));
        execute_uninstall_transaction(
            &paths,
            &store,
            &record,
            &source,
            &staged,
            &mut uninstall_journal,
        )
        .unwrap();

        assert!(store.get_installation(&app_id, &version).unwrap().is_none());
        assert!(!source.exists());
        assert!(!staged.exists());
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn local_fixture_applies_managed_update_and_moves_the_selected_release_line() {
        let root = tempdir().unwrap();
        let app_id = AppId::new("node").unwrap();
        let source_id = SourceId::new("node.official").unwrap();
        let installed_version = ExactVersion::from_str("24.19.0").unwrap();
        let available_version = ExactVersion::from_str("24.20.1").unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let installed_path = paths.app_version_dir("node", &installed_version.to_string());
        write_fixture_node_installation(&installed_path, &installed_version);

        let fixture_node = compile_fixture_node(root.path(), &available_version);
        let official = NodeProvider::official().unwrap();
        let distribution = official.distribution(&available_version).unwrap();
        let archive = fixture_archive(
            distribution.archive_kind,
            &distribution.archive_name,
            &fixture_node,
        );
        let archive_hash = hex::encode(Sha256::digest(&archive));
        let manifest = format!("{archive_hash}  {}\n", distribution.archive_name);
        let signature = b"torben-managed-update-fixture-signature-v1".to_vec();
        let version_prefix = format!("/dist/v{available_version}");
        let (base_url, server) = fixture_server(
            BTreeMap::from([
                (
                    format!("{version_prefix}/SHASUMS256.txt"),
                    manifest.into_bytes(),
                ),
                (
                    format!("{version_prefix}/SHASUMS256.txt.sig"),
                    signature.clone(),
                ),
                (
                    format!("{version_prefix}/{}", distribution.archive_name),
                    archive,
                ),
            ]),
            3,
        );
        let provider = NodeProvider::with_fixture_base_url(base_url, signature).unwrap();
        let distribution = provider.distribution(&available_version).unwrap();
        let plugin = root.path().join(if cfg!(windows) {
            "node-update-plugin-fixture.cmd"
        } else {
            "node-update-plugin-fixture"
        });
        std::fs::write(
            &plugin,
            managed_update_plugin_fixture_script(
                &installed_version,
                &available_version,
                &distribution,
            ),
        )
        .unwrap();
        make_executable(&plugin);
        let shim = root.path().join("torben-shim-fixture");
        std::fs::write(&shim, b"managed update shim fixture").unwrap();

        let mut core = TorbenCore::open(paths).unwrap();
        core.node = provider.clone();
        core.node_plugin = BundledPlugin::node_from_executable(plugin);
        core.bundled_shim = BundledShim::from_executable(shim);
        core.store
            .add_installation(&InstallRecord {
                app_id: app_id.clone(),
                version: installed_version.clone(),
                source_id: source_id.clone(),
                scope: InstallScope::Managed,
                install_path: installed_path.display().to_string(),
                installed_at: "fixture".to_owned(),
                health: "healthy".to_owned(),
            })
            .unwrap();
        core.store
            .set_selection(&app_id, &installed_version)
            .unwrap();

        let check = core.managed_update_check(Some(&app_id)).await.unwrap();
        assert_eq!(check.checked_apps, 1);
        assert!(check.warnings.is_empty());
        assert_eq!(check.candidates.len(), 1);
        assert_eq!(
            check.candidates[0].installed_version,
            installed_version.clone()
        );
        assert_eq!(
            check.candidates[0].available_version,
            available_version.clone()
        );
        assert_eq!(
            check.candidates[0].selected_version.as_ref(),
            Some(&installed_version)
        );

        let result = core
            .apply_managed_update(&app_id, &installed_version, &available_version)
            .await
            .unwrap();
        server.join().unwrap();

        assert!(result.selection_updated);
        assert_eq!(result.installation.version, available_version);
        assert_eq!(result.installation.source_id, source_id);
        assert_eq!(result.installation.scope, InstallScope::Managed);
        assert_eq!(
            core.selected_version(&app_id).unwrap(),
            Some(result.installation.version.clone())
        );
        assert!(
            installed_path.is_dir(),
            "the previous version must be retained"
        );
        assert!(std::path::Path::new(&result.installation.install_path).is_dir());
        provider.health_check(&result.installation).unwrap();
        let installed = core.installed().unwrap();
        assert_eq!(installed.len(), 2);
        assert!(
            installed
                .iter()
                .any(|record| record.version == installed_version)
        );
        assert!(
            installed
                .iter()
                .any(|record| record.version == result.installation.version)
        );
        let events = core.operation_events().unwrap();
        assert!(events.iter().any(|event| {
            event.state == OperationState::Succeeded && event.message == "Installation committed"
        }));
        assert!(events.iter().any(|event| {
            event.state == OperationState::Succeeded
                && event.message == format!("Selected {app_id} {}", result.installation.version)
        }));
        assert!(events.iter().all(|event| {
            !matches!(
                event.state,
                OperationState::Failed | OperationState::RolledBack
            )
        }));
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn managed_update_health_failure_keeps_the_previous_selection_and_rolls_back_staging() {
        let root = tempdir().unwrap();
        let app_id = AppId::new("node").unwrap();
        let source_id = SourceId::new("node.official").unwrap();
        let installed_version = ExactVersion::from_str("24.19.0").unwrap();
        let available_version = ExactVersion::from_str("24.20.1").unwrap();
        let incorrect_archive_version = ExactVersion::from_str("24.20.0").unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let installed_path = paths.app_version_dir("node", &installed_version.to_string());
        write_fixture_node_installation(&installed_path, &installed_version);

        let fixture_node = compile_fixture_node(root.path(), &incorrect_archive_version);
        let official = NodeProvider::official().unwrap();
        let distribution = official.distribution(&available_version).unwrap();
        let archive = fixture_archive(
            distribution.archive_kind,
            &distribution.archive_name,
            &fixture_node,
        );
        let archive_hash = hex::encode(Sha256::digest(&archive));
        let manifest = format!("{archive_hash}  {}\n", distribution.archive_name);
        let signature = b"torben-managed-update-failure-fixture-v1".to_vec();
        let version_prefix = format!("/dist/v{available_version}");
        let (base_url, server) = fixture_server(
            BTreeMap::from([
                (
                    format!("{version_prefix}/SHASUMS256.txt"),
                    manifest.into_bytes(),
                ),
                (
                    format!("{version_prefix}/SHASUMS256.txt.sig"),
                    signature.clone(),
                ),
                (
                    format!("{version_prefix}/{}", distribution.archive_name),
                    archive,
                ),
            ]),
            3,
        );
        let provider = NodeProvider::with_fixture_base_url(base_url, signature).unwrap();
        let distribution = provider.distribution(&available_version).unwrap();
        let plugin = root.path().join(if cfg!(windows) {
            "node-update-failure-plugin-fixture.cmd"
        } else {
            "node-update-failure-plugin-fixture"
        });
        std::fs::write(
            &plugin,
            managed_update_plugin_fixture_script(
                &installed_version,
                &available_version,
                &distribution,
            ),
        )
        .unwrap();
        make_executable(&plugin);
        let shim = root.path().join("torben-shim-fixture");
        std::fs::write(&shim, b"managed update shim fixture").unwrap();

        let mut core = TorbenCore::open(paths.clone()).unwrap();
        core.node = provider;
        core.node_plugin = BundledPlugin::node_from_executable(plugin);
        core.bundled_shim = BundledShim::from_executable(shim);
        core.store
            .add_installation(&InstallRecord {
                app_id: app_id.clone(),
                version: installed_version.clone(),
                source_id,
                scope: InstallScope::Managed,
                install_path: installed_path.display().to_string(),
                installed_at: "fixture".to_owned(),
                health: "healthy".to_owned(),
            })
            .unwrap();
        core.store
            .set_selection(&app_id, &installed_version)
            .unwrap();

        let error = core
            .apply_managed_update(&app_id, &installed_version, &available_version)
            .await
            .unwrap_err();
        server.join().unwrap();

        assert_eq!(error.code, "health_check_version_mismatch");
        assert_eq!(
            core.selected_version(&app_id).unwrap(),
            Some(installed_version.clone())
        );
        assert_eq!(core.installed().unwrap().len(), 1);
        assert!(
            core.store
                .get_installation(&app_id, &available_version)
                .unwrap()
                .is_none()
        );
        assert!(installed_path.is_dir());
        assert!(
            !paths
                .app_version_dir("node", &available_version.to_string())
                .exists()
        );
        assert!(
            std::fs::read_dir(paths.staging_dir())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("install-node-"))
        );
        let events = core.operation_events().unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.state == OperationState::Failed)
        );
        assert!(
            events
                .iter()
                .any(|event| event.state == OperationState::RolledBack)
        );
    }

    #[tokio::test]
    async fn cancellation_stops_download_before_network_and_removes_partial_data() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let app_id = AppId::new("node").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            Some(&ExactVersion::from_str("24.19.0").unwrap()),
        )
        .unwrap();
        OperationJournal::request_cancellation(&paths, &store, journal.operation_id()).unwrap();
        let provider =
            NodeProvider::with_base_url(url::Url::parse("http://127.0.0.1:9/dist/").unwrap())
                .unwrap();
        let destination = root.path().join("node-fixture.zip");
        let cancellation = journal.cancellation_probe();

        let error = provider
            .download_archive_checked(
                &url::Url::parse("http://127.0.0.1:9/dist/node-fixture.zip").unwrap(),
                &destination,
                Some(&cancellation),
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "operation_cancelled");
        assert!(!destination.exists());
        assert!(!destination.with_extension("partial").exists());
        journal.acknowledge_cancellation().unwrap();
        journal.fail_and_rollback(&error).unwrap();
    }

    #[tokio::test]
    async fn cancellation_interrupts_an_in_flight_archive_stream() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1048581\r\nConnection: close\r\n\r\nfirst",
                )
                .unwrap();
            stream.flush().unwrap();
            started_tx.send(()).unwrap();
            thread::sleep(Duration::from_millis(500));
            let _ = stream.write_all(&vec![b'x'; 1024 * 1024]);
        });
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let app_id = AppId::new("node").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            Some(&ExactVersion::from_str("24.19.0").unwrap()),
        )
        .unwrap();
        let operation_id = journal.operation_id();
        let requester_paths = paths.clone();
        let requester_store = Arc::clone(&store);
        let requester = tokio::task::spawn_blocking(move || {
            started_rx.recv().unwrap();
            OperationJournal::request_cancellation(
                &requester_paths,
                &requester_store,
                operation_id,
            )
            .unwrap();
        });
        let base_url = url::Url::parse(&format!("http://{address}/dist/")).unwrap();
        let provider = NodeProvider::with_base_url(base_url.clone()).unwrap();
        let destination = root.path().join("streamed-node.zip");
        let cancellation = journal.cancellation_probe();

        let error = provider
            .download_archive_checked(
                &base_url.join("streamed-node.zip").unwrap(),
                &destination,
                Some(&cancellation),
            )
            .await
            .unwrap_err();

        requester.await.unwrap();
        server.join().unwrap();
        assert_eq!(error.code, "operation_cancelled");
        assert!(!destination.exists());
        assert!(!destination.with_extension("partial").exists());
        journal.acknowledge_cancellation().unwrap();
        journal.fail_and_rollback(&error).unwrap();
    }

    fn compile_fixture_node(directory: &std::path::Path, version: &ExactVersion) -> Vec<u8> {
        let source = directory.join("fixture-node.rs");
        let executable = directory.join(format!("fixture-node{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(
            &source,
            format!("fn main() {{ println!(\"v{version}\"); }}\n"),
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
        std::fs::read(executable).unwrap()
    }

    fn write_fixture_node_installation(path: &std::path::Path, version: &ExactVersion) {
        std::fs::create_dir_all(path).unwrap();
        let commands = if cfg!(windows) {
            vec![
                ("node.exe", compile_fixture_node(path, version)),
                ("npm.cmd", b"@echo off\r\necho 11.0.0\r\n".to_vec()),
                ("npx.cmd", b"@echo off\r\necho 11.0.0\r\n".to_vec()),
            ]
        } else {
            let executable = compile_fixture_node(path, version);
            vec![
                ("bin/node", executable.clone()),
                ("bin/npm", executable.clone()),
                ("bin/npx", executable),
            ]
        };
        for (relative, contents) in commands {
            let destination = path.join(relative);
            std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
            std::fs::write(&destination, contents).unwrap();
            make_executable(&destination);
        }
    }

    #[allow(clippy::too_many_lines)]
    fn managed_update_plugin_fixture_script(
        installed_version: &ExactVersion,
        available_version: &ExactVersion,
        distribution: &super::NodeDistribution,
    ) -> String {
        let initialize = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "protocolVersion": torben_contracts::plugin::PLUGIN_PROTOCOL_VERSION,
                "pluginId": "app.torben.plugin.node",
                "pluginVersion": env!("CARGO_PKG_VERSION"),
                "applications": [{
                    "id": "node",
                    "displayName": "Node.js",
                    "summary": "managed update fixture",
                    "categories": ["runtime"],
                    "capabilities": ["versions", "install", "select", "uninstall"],
                    "sources": [{
                        "id": "node.official",
                        "displayName": "Official Node.js archive",
                        "managed": true
                    }]
                }]
            }
        })
        .to_string();
        let versions = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "versions": [
                    {
                        "version": installed_version,
                        "ltsName": "Krypton",
                        "releasedAt": "2026-08-03",
                        "recommended": false
                    },
                    {
                        "version": available_version,
                        "ltsName": "Krypton",
                        "releasedAt": "2026-08-24",
                        "recommended": true
                    }
                ]
            }
        })
        .to_string();
        let resolved = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "requested": available_version.to_string(),
                "resolved": available_version
            }
        })
        .to_string();
        let plan = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "result": {
                "appId": "node",
                "version": available_version,
                "sourceId": "node.official",
                "steps": [
                    {
                        "type": "download",
                        "url": distribution.archive_url,
                        "destination_name": distribution.archive_name
                    },
                    {
                        "type": "verify_sha256_manifest",
                        "manifest_url": distribution.checksums_url,
                        "signature_url": distribution.signature_url,
                        "archive_name": distribution.archive_name
                    },
                    {
                        "type": "extract_archive",
                        "archive_name": distribution.archive_name,
                        "strip_components": 0
                    },
                    {
                        "type": "health_check",
                        "executable": "node",
                        "arguments": ["--version"],
                        "expected_output": format!("v{available_version}")
                    },
                    { "type": "create_shims", "commands": ["node", "npm", "npx"] }
                ],
                "metadata": { "target": current_target() }
            }
        })
        .to_string();
        let health = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "healthy": true,
                "actualVersion": available_version,
                "message": "healthy"
            }
        })
        .to_string();
        if cfg!(windows) {
            format!(
                "@echo off\r\n:loop\r\nset request=\r\nset /p request=\r\nif errorlevel 1 exit /b 0\r\necho %request%| findstr /c:\"initialize\" >nul && (echo {initialize}& goto loop)\r\necho %request%| findstr /c:\"versions.list\" >nul && (echo {versions}& goto loop)\r\necho %request%| findstr /c:\"version.resolve\" >nul && (echo {resolved}& goto loop)\r\necho %request%| findstr /c:\"install.plan\" >nul && (echo {plan}& goto loop)\r\necho %request%| findstr /c:\"health.check\" >nul && (echo {health}& goto loop)\r\nexit /b 1\r\n"
            )
        } else {
            format!(
                "#!/bin/sh\nwhile IFS= read -r request; do\ncase \"$request\" in\n  *initialize*) printf '%s\\n' '{initialize}' ;;\n  *versions.list*) printf '%s\\n' '{versions}' ;;\n  *version.resolve*) printf '%s\\n' '{resolved}' ;;\n  *install.plan*) printf '%s\\n' '{plan}' ;;\n  *health.check*) printf '%s\\n' '{health}' ;;\n  *) exit 1 ;;\nesac\ndone\n"
            )
        }
    }

    fn make_executable(path: &std::path::Path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mut permissions = std::fs::metadata(path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        }
        #[cfg(not(unix))]
        let _ = path;
    }

    fn fixture_archive(kind: ArchiveKind, archive_name: &str, node: &[u8]) -> Vec<u8> {
        let root = match kind {
            ArchiveKind::Zip => archive_name.strip_suffix(".zip"),
            ArchiveKind::TarGz => archive_name.strip_suffix(".tar.gz"),
            ArchiveKind::TarXz => archive_name.strip_suffix(".tar.xz"),
        }
        .unwrap();
        let files = if cfg!(windows) {
            vec![
                (format!("{root}/node.exe"), node.to_vec(), 0o755),
                (
                    format!("{root}/npm.cmd"),
                    b"@echo off\r\necho 11.0.0\r\n".to_vec(),
                    0o755,
                ),
                (
                    format!("{root}/npx.cmd"),
                    b"@echo off\r\necho 11.0.0\r\n".to_vec(),
                    0o755,
                ),
            ]
        } else {
            vec![
                (format!("{root}/bin/node"), node.to_vec(), 0o755),
                (format!("{root}/bin/npm"), node.to_vec(), 0o755),
                (format!("{root}/bin/npx"), node.to_vec(), 0o755),
            ]
        };
        match kind {
            ArchiveKind::Zip => {
                let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
                for (path, content, mode) in files {
                    let options = zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored)
                        .unix_permissions(mode);
                    writer.start_file(path, options).unwrap();
                    writer.write_all(&content).unwrap();
                }
                writer.finish().unwrap().into_inner()
            }
            ArchiveKind::TarGz | ArchiveKind::TarXz => {
                let mut builder = tar::Builder::new(Vec::new());
                for (path, content, mode) in files {
                    let mut header = tar::Header::new_gnu();
                    header.set_size(content.len().try_into().unwrap());
                    header.set_mode(mode);
                    header.set_cksum();
                    builder
                        .append_data(&mut header, path, content.as_slice())
                        .unwrap();
                }
                builder.finish().unwrap();
                let tar = builder.into_inner().unwrap();
                if kind == ArchiveKind::TarGz {
                    let mut encoder =
                        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                    encoder.write_all(&tar).unwrap();
                    encoder.finish().unwrap()
                } else {
                    let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 6);
                    encoder.write_all(&tar).unwrap();
                    encoder.finish().unwrap()
                }
            }
        }
    }

    fn fixture_server(
        routes: BTreeMap<String, Vec<u8>>,
        expected_requests: usize,
    ) -> (url::Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
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
                    .unwrap();
                let (status, body) = routes.get(path).map_or_else(
                    || ("404 Not Found", b"not found".as_slice()),
                    |body| ("200 OK", body.as_slice()),
                );
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
                stream.write_all(body).unwrap();
                stream.flush().unwrap();
            }
        });
        (
            url::Url::parse(&format!("http://{address}/dist/")).unwrap(),
            server,
        )
    }
}
