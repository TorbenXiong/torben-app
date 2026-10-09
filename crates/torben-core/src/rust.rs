use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
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
    node::sha256_file_checked,
    operation::{CancellationProbe, OperationJournal},
};

const RUST_RELEASES_URL: &str = "https://api.github.com/repos/rust-lang/rust/tags?per_page=100";
const RUST_DIST_BASE: &str = "https://static.rust-lang.org/dist/";
const RUST_TARGET: &str = "x86_64-pc-windows-msvc";
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct RustProvider {
    client: reqwest::Client,
    releases_url: Url,
    dist_base: Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RustDistribution {
    pub archive_name: String,
    pub archive_url: Url,
    pub checksum: String,
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    name: String,
}

impl RustProvider {
    pub fn official() -> TorbenResult<Self> {
        let client = crate::download::client_builder()
            .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            releases_url: Url::parse(RUST_RELEASES_URL).map_err(url_error)?,
            dist_base: Url::parse(RUST_DIST_BASE).map_err(url_error)?,
        })
    }

    pub async fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        let releases: Vec<GitHubRelease> = self.fetch_json(&self.releases_url).await?;
        let mut versions = releases
            .into_iter()
            .filter_map(|release| {
                let version = release.name.strip_prefix('v').unwrap_or(&release.name);
                let version = ExactVersion::from_str(version).ok()?;
                if !version.as_semver().pre.is_empty() {
                    return None;
                }
                Some(VersionDescriptor {
                    version,
                    lts_name: Some("Rust stable".to_owned()),
                    released_at: String::new(),
                    recommended: false,
                })
            })
            .collect::<Vec<_>>();
        versions.sort_by(|left, right| right.version.cmp(&left.version));
        versions.dedup_by(|left, right| left.version == right.version);
        // Keep the catalog focused on the most useful toolchains while still
        // allowing callers to install any exact version supported by the
        // upstream distribution metadata.
        versions.truncate(3);
        if versions.is_empty() {
            return Err(TorbenError::new(
                "rust_metadata_invalid",
                "The official Rust release catalog contains no stable versions.",
            ));
        }
        if let Some(first) = versions.first_mut() {
            first.recommended = true;
        }
        Ok(versions)
    }

    pub async fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        if let Ok(exact) = ExactVersion::from_str(requested) {
            return self.distribution(&exact).await.map(|_| exact);
        }
        let versions = self.list_versions().await?;
        match requested.trim().to_ascii_lowercase().as_str() {
            "stable" | "latest" | "current" => versions
                .first()
                .map(|item| item.version.clone())
                .ok_or_else(|| version_not_found(requested)),
            _ => Err(version_not_found(requested)),
        }
    }

    pub async fn distribution(&self, version: &ExactVersion) -> TorbenResult<RustDistribution> {
        // The MSI contains tens of thousands of documentation files and Windows
        // Installer spends several minutes materializing them. The official
        // tarball has the same toolchain contents and can be unpacked directly,
        // avoiding the MSI administrative install path.
        let archive_name = format!("rust-{version}-{RUST_TARGET}.tar.gz");
        let manifest_url = self
            .dist_base
            .join(&format!("channel-rust-{version}.toml"))
            .map_err(url_error)?;
        let manifest = self.fetch_text(&manifest_url).await?;
        let (archive_url, checksum) = distribution_from_manifest(&manifest, &archive_name)?;
        Ok(RustDistribution {
            archive_name,
            archive_url,
            checksum,
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
        let distribution = self.validate_install_plan(plan, app_id, version).await?;
        let cancellation = journal.cancellation_probe();
        cancellation.check()?;
        let download_dir = paths.download_dir(app_id.as_str(), &version.to_string());
        std::fs::create_dir_all(&download_dir).map_err(io_error)?;
        let archive_path = download_dir.join(&distribution.archive_name);
        journal.record(
            OperationState::Running,
            "download",
            format!("Downloading Rust {version}"),
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
                "The Rust archive does not match the official channel checksum.",
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
            "Extracting the Rust toolchain into staging",
            Some(0.6),
        )?;
        let archive_for_task = archive_path.clone();
        let staging_for_task = staging.clone();
        let extraction_cancellation = cancellation.clone();
        let extracted = tokio::task::spawn_blocking(move || {
            extract_windows_archive(
                &archive_for_task,
                &staging_for_task,
                &extraction_cancellation,
            )
        })
        .await
        .map_err(|error| {
            TorbenError::new(
                "archive_task_failed",
                "The Rust archive extraction task failed.",
            )
            .with_detail("reason", error.to_string())
        })??;
        cancellation.check()?;
        self.health_check_path(&extracted, version)?;
        cancellation.check()?;
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            return Err(TorbenError::new(
                "install_path_exists",
                "The Rust version is already installed.",
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
        if !matches!(command, "rustc" | "cargo" | "rustdoc" | "rustfmt") {
            return Err(TorbenError::new(
                "unsupported_command",
                "Rust does not expose this command.",
            )
            .with_detail("command", command));
        }
        let path = install_path.join("bin").join(if cfg!(windows) {
            format!("{command}.exe")
        } else {
            command.to_owned()
        });
        if path.is_file() {
            Ok(path)
        } else {
            Err(TorbenError::new(
                "managed_command_missing",
                "A managed Rust command is missing.",
            )
            .with_detail("path", path.display().to_string()))
        }
    }

    pub fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let mut records = Vec::new();
        let mut seen = BTreeSet::new();
        let executable = if cfg!(windows) { "rustc.exe" } else { "rustc" };
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            let candidate = directory.join(executable);
            let Ok(canonical) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) || !seen.insert(canonical.clone()) {
                continue;
            }
            let output = std::process::Command::new(&canonical)
                .arg("--version")
                .output();
            let Ok(output) = output else { continue };
            let Some(version) = parse_rustc_version(&String::from_utf8_lossy(&output.stdout))
            else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("rust")?,
                version,
                source_id: SourceId::new("rust.external")?,
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

    async fn validate_install_plan(
        &self,
        plan: &InstallPlan,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<RustDistribution> {
        if app_id.as_str() != "rust"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new("rust.official")?
        {
            return Err(invalid_plan("identity or source owner"));
        }
        let expected = self.distribution(version).await?;
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
            || executable != "rustc"
            || arguments.as_slice() != ["--version"]
            || expected_output != &format!("rustc {version}")
            || commands.as_slice() != ["rustc", "cargo", "rustdoc", "rustfmt"]
        {
            return Err(invalid_plan("official distribution details"));
        }
        Ok(expected)
    }

    fn health_check_path(&self, install_path: &Path, version: &ExactVersion) -> TorbenResult<()> {
        let rustc = self.command_path(install_path, "rustc")?;
        let output = std::process::Command::new(rustc)
            .arg("--version")
            .output()
            .map_err(io_error)?;
        let actual =
            parse_rustc_version(&String::from_utf8_lossy(&output.stdout)).ok_or_else(|| {
                TorbenError::new(
                    "health_check_output_invalid",
                    "rustc returned invalid version output.",
                )
            })?;
        if actual != *version {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The Rust version does not match the requested version.",
            ));
        }
        for command in ["cargo", "rustdoc", "rustfmt"] {
            let _ = self.command_path(install_path, command)?;
        }
        Ok(())
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(&self, url: &Url) -> TorbenResult<T> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(network_error)?;
        validate_origin(&response, url)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND
            && let Some(version) = url
                .path_segments()
                .and_then(|mut segments| segments.next_back())
                .and_then(|name| name.strip_prefix("channel-rust-"))
                .and_then(|name| name.strip_suffix(".toml"))
        {
            return Err(version_not_found(version));
        }
        let bytes = response
            .error_for_status()
            .map_err(network_error)?
            .bytes()
            .await
            .map_err(network_error)?;
        if bytes.len() as u64 > MAX_METADATA_BYTES {
            return Err(TorbenError::new(
                "rust_metadata_too_large",
                "Rust metadata is too large.",
            ));
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            TorbenError::new("rust_metadata_invalid", "Rust metadata is invalid.")
                .with_detail("reason", error.to_string())
        })
    }

    async fn fetch_text(&self, url: &Url) -> TorbenResult<String> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(network_error)?;
        validate_origin(&response, url)?;
        let bytes = response
            .error_for_status()
            .map_err(network_error)?
            .bytes()
            .await
            .map_err(network_error)?;
        if bytes.len() as u64 > MAX_METADATA_BYTES {
            return Err(TorbenError::new(
                "rust_metadata_too_large",
                "Rust metadata is too large.",
            ));
        }
        String::from_utf8(bytes.to_vec()).map_err(|error| {
            TorbenError::new("rust_metadata_invalid", "Rust metadata is not UTF-8.")
                .with_detail("reason", error.to_string())
        })
    }

    async fn download(
        &self,
        url: &Url,
        official: &Url,
        destination: &Path,
        cancellation: &CancellationProbe,
    ) -> TorbenResult<()> {
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(network_error)?;
        if !crate::download::mirror_response(official, url, response.url()) {
            validate_origin(&response, official)?;
        }
        let response = response.error_for_status().map_err(network_error)?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_ARCHIVE_BYTES)
        {
            return Err(TorbenError::new(
                "rust_archive_too_large",
                "The Rust archive is too large.",
            ));
        }
        let partial = destination.with_extension("partial");
        let mut file = crate::download::open_partial(&partial, false).map_err(io_error)?;
        let mut stream = response.bytes_stream();
        let mut total = 0u64;
        let mut transfer = crate::download::TransferRate::new();
        while let Some(chunk) = stream.next().await {
            cancellation.check()?;
            let chunk = chunk.map_err(network_error)?;
            transfer.record(chunk.len(), "rust_network_error")?;
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_ARCHIVE_BYTES {
                return Err(TorbenError::new(
                    "rust_archive_too_large",
                    "The Rust archive is too large.",
                ));
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
        std::fs::rename(partial, destination).map_err(io_error)
    }
}

pub(crate) fn configure_command_environment(
    command: &mut std::process::Command,
    data_root: &Path,
    bin: &Path,
) -> TorbenResult<()> {
    let cargo_home = data_root.join("home");
    let temp = data_root.join("temp");
    for directory in [cargo_home.clone(), cargo_home.join("bin"), temp.clone()] {
        std::fs::create_dir_all(directory).map_err(io_error)?;
    }
    command.env("CARGO_HOME", &cargo_home);
    command.env("CARGO_REGISTRIES_CRATES_IO_PROTOCOL", "sparse");
    command.env("CARGO_NET_GIT_FETCH_WITH_CLI", "false");
    command.env("RUST_BACKTRACE", "1");
    command.env("TEMP", &temp);
    command.env("TMP", &temp);
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        [bin.to_path_buf(), cargo_home.join("bin")]
            .into_iter()
            .chain(std::env::split_paths(&inherited)),
    )
    .map_err(|error| {
        TorbenError::new(
            "rust_environment_failed",
            "Could not prepare the managed Rust PATH.",
        )
        .with_detail("reason", error.to_string())
    })?;
    command.env("PATH", path);
    Ok(())
}

fn extract_windows_archive(
    archive: &Path,
    staging: &Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<PathBuf> {
    if !cfg!(windows) {
        return Err(TorbenError::new(
            "rust_platform_unsupported",
            "Managed Rust installation currently supports Windows only.",
        ));
    }
    cancellation.check()?;
    std::fs::create_dir_all(staging).map_err(io_error)?;
    let root = crate::node::extract_archive(
        archive,
        crate::node::ArchiveKind::TarGz,
        staging,
        cancellation,
    )?;
    // Rust's tar installer stores each component under its own directory;
    // unpacking alone does not produce the bin/lib layout used by the MSI.
    let components = std::fs::read_to_string(root.join("components")).map_err(io_error)?;
    let prefix = staging.join("toolchain");
    std::fs::create_dir(&prefix).map_err(io_error)?;
    let mut seen = BTreeSet::new();
    for component in components.lines() {
        if component.is_empty()
            || !component
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || !seen.insert(component)
        {
            return Err(TorbenError::new(
                "rust_archive_layout_invalid",
                "The Rust archive contains an invalid component list.",
            ));
        }
        let component_path = root.join(component);
        if !component_path
            .symlink_metadata()
            .map_err(io_error)?
            .is_dir()
        {
            return Err(TorbenError::new(
                "rust_archive_layout_invalid",
                "A Rust component directory is missing.",
            ));
        }
        for entry in std::fs::read_dir(&component_path).map_err(io_error)? {
            cancellation.check()?;
            let entry = entry.map_err(io_error)?;
            if entry.file_name() != "manifest.in" {
                merge_component_payload(
                    &entry.path(),
                    &prefix.join(entry.file_name()),
                    cancellation,
                )?;
            }
        }
    }
    if !prefix.join("bin/rustc.exe").is_file() {
        return Err(TorbenError::new(
            "rust_archive_layout_invalid",
            "The Rust archive does not contain the compiler.",
        ));
    }
    Ok(prefix)
}

fn merge_component_payload(
    source: &Path,
    destination: &Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<()> {
    cancellation.check()?;
    let metadata = source.symlink_metadata().map_err(io_error)?;
    if metadata.file_type().is_symlink() || !(metadata.is_dir() || metadata.is_file()) {
        return Err(TorbenError::new(
            "archive_path_unsafe",
            "The Rust component contains a linked or special file.",
        ));
    }
    if !destination.exists() {
        return std::fs::rename(source, destination).map_err(io_error);
    }
    if !metadata.is_dir() || !destination.is_dir() {
        return Err(TorbenError::new(
            "rust_archive_layout_invalid",
            "Rust components contain conflicting payload paths.",
        ));
    }
    for entry in std::fs::read_dir(source).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        merge_component_payload(
            &entry.path(),
            &destination.join(entry.file_name()),
            cancellation,
        )?;
    }
    std::fs::remove_dir(source).map_err(io_error)
}

fn distribution_from_manifest(manifest: &str, archive_name: &str) -> TorbenResult<(Url, String)> {
    let lines = manifest.lines().collect::<Vec<_>>();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.contains(archive_name)
            && (trimmed.starts_with("gz_url") || trimmed.starts_with("url"))
        {
            let archive_url = trimmed
                .split_once('"')
                .and_then(|(_, value)| value.split_once('"').map(|(url, _)| url))
                .and_then(|value| Url::parse(value).ok());
            for candidate in lines.iter().skip(index + 1).take(4) {
                if let Some(value) = candidate
                    .trim()
                    .strip_prefix("gz_hash = \"")
                    .or_else(|| candidate.trim().strip_prefix("hash = \""))
                    .or_else(|| candidate.trim().strip_prefix("hash-sha256 = \""))
                    .and_then(|value| value.strip_suffix('"'))
                    && value.len() == 64
                    && value.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    let Some(archive_url) = archive_url.clone() else {
                        continue;
                    };
                    if archive_url.host_str() != Some("static.rust-lang.org") {
                        return Err(TorbenError::new(
                            "rust_metadata_invalid",
                            "The Rust channel manifest points outside the official distribution host.",
                        ));
                    }
                    return Ok((archive_url, value.to_ascii_lowercase()));
                }
            }
        }
    }
    Err(TorbenError::new(
        "rust_checksum_missing",
        "The Rust channel manifest does not contain the Windows archive checksum.",
    )
    .with_detail("archive", archive_name))
}

fn parse_rustc_version(output: &str) -> Option<ExactVersion> {
    output
        .split_whitespace()
        .nth(1)
        .and_then(|value| ExactVersion::from_str(value).ok())
}

fn validate_origin(response: &reqwest::Response, expected: &Url) -> TorbenResult<()> {
    if response.url().scheme() != expected.scheme()
        || response.url().host_str() != expected.host_str()
    {
        return Err(TorbenError::new(
            "unexpected_download_origin",
            "Rust metadata redirected outside the official origin.",
        ));
    }
    Ok(())
}

fn invalid_plan(field: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The Rust plugin returned an invalid installation plan.",
    )
    .with_detail("field", field)
}
fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested Rust version was not found.",
    )
    .with_detail("requested", requested)
}
fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new(
        "rust_network_error",
        "A Rust metadata or archive request failed.",
    )
    .with_detail("reason", error.to_string())
}
fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new("rust_url_invalid", "The Rust provider URL is invalid.")
        .with_detail("reason", error.to_string())
}
fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "rust_io_failed",
        "A Rust managed filesystem operation failed.",
    )
    .with_detail("reason", error.to_string())
}
fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| String::new(), |value| value.as_secs().to_string())
}

#[cfg(all(test, windows))]
mod tests {
    use std::{io::Write, sync::Arc};

    use super::{distribution_from_manifest, extract_windows_archive};
    use crate::{StateStore, TorbenPaths, operation::OperationJournal};
    use torben_contracts::{AppId, OperationKind};

    fn fixture(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder.append_data(&mut header, path, *bytes).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&tar).unwrap();
        gzip.finish().unwrap()
    }

    #[test]
    fn official_tar_components_form_one_complete_toolchain() {
        let root = tempfile::tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let journal = OperationJournal::start(
            &paths,
            store,
            OperationKind::Install,
            &AppId::new("rust").unwrap(),
            None,
        )
        .unwrap();
        let archive = root.path().join("rust.tar.gz");
        std::fs::write(
            &archive,
            fixture(&[
                (
                    "rust/components",
                    b"rustc\ncargo\nrust-std-x86_64-pc-windows-msvc\nrustfmt-preview\nrust-docs\n",
                ),
                ("rust/rustc/manifest.in", b"file:bin/rustc.exe\n"),
                ("rust/rustc/bin/rustc.exe", b"compiler"),
                ("rust/rustc/bin/rustdoc.exe", b"rustdoc"),
                ("rust/cargo/bin/cargo.exe", b"cargo"),
                (
                    "rust/rust-std-x86_64-pc-windows-msvc/lib/rustlib/libstd.rlib",
                    b"standard library",
                ),
                ("rust/rustfmt-preview/bin/rustfmt.exe", b"formatter"),
                (
                    "rust/rust-docs/share/doc/rust/html/index.html",
                    b"documentation",
                ),
            ]),
        )
        .unwrap();
        let prefix = extract_windows_archive(
            &archive,
            &root.path().join("extract"),
            &journal.cancellation_probe(),
        )
        .unwrap();
        for path in [
            "bin/rustc.exe",
            "bin/rustdoc.exe",
            "bin/cargo.exe",
            "bin/rustfmt.exe",
            "lib/rustlib/libstd.rlib",
            "share/doc/rust/html/index.html",
        ] {
            assert!(prefix.join(path).is_file(), "missing {path}");
        }
        assert!(!prefix.join("manifest.in").exists());
        OperationJournal::request_cancellation(
            &paths,
            &StateStore::open(paths.state_database()).unwrap(),
            journal.operation_id(),
        )
        .unwrap();
        assert_eq!(
            extract_windows_archive(
                &archive,
                &root.path().join("cancelled"),
                &journal.cancellation_probe()
            )
            .unwrap_err()
            .code,
            "operation_cancelled"
        );
    }

    #[test]
    fn official_gzip_checksum_is_selected_without_using_xz_checksum() {
        let name = "rust-1.99.0-x86_64-pc-windows-msvc.tar.gz";
        let hash = "a".repeat(64);
        let manifest = format!(
            "url = \"https://static.rust-lang.org/dist/2026-10-01/{name}\"\nhash = \"{hash}\"\nxz_hash = \"{}\"\n",
            "b".repeat(64)
        );
        let (url, checksum) = distribution_from_manifest(&manifest, name).unwrap();
        assert!(url.path().ends_with(name));
        assert_eq!(checksum, hash);
    }
}
