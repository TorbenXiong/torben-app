use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
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

const GO_RELEASES_URL: &str = "https://go.dev/dl/?mode=json&include=all";
const GO_RELEASES_FALLBACK_URL: &str = "https://golang.google.cn/dl/?mode=json&include=all";
const GO_DIST_BASE: &str = "https://dl.google.com/go/";
const MAX_METADATA_BYTES: u64 = 8 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct GoProvider {
    client: reqwest::Client,
    releases_url: Url,
    releases_fallback_url: Url,
    dist_base: Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoDistribution {
    pub archive_name: String,
    pub archive_url: Url,
    pub checksum: String,
}

#[derive(Debug, Deserialize)]
struct GoRelease {
    version: String,
    stable: bool,
    files: Vec<GoFile>,
}

#[derive(Debug, Deserialize)]
struct GoFile {
    filename: String,
    os: String,
    arch: String,
    version: String,
    sha256: String,
    kind: String,
}

impl GoProvider {
    pub fn official() -> TorbenResult<Self> {
        let client = crate::download::client_builder()
            .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            releases_url: Url::parse(GO_RELEASES_URL).map_err(url_error)?,
            releases_fallback_url: Url::parse(GO_RELEASES_FALLBACK_URL).map_err(url_error)?,
            dist_base: Url::parse(GO_DIST_BASE).map_err(url_error)?,
        })
    }

    pub async fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        ensure_platform()?;
        let releases = self.releases().await?;
        catalog_versions(&releases)
    }

    pub async fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        if let Some(exact) = parse_release_version(requested.trim()) {
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

    pub async fn distribution(&self, version: &ExactVersion) -> TorbenResult<GoDistribution> {
        ensure_platform()?;
        let releases = self.releases().await?;
        distribution_from_releases(&releases, version, &self.dist_base)
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
            format!("Downloading Go {version}"),
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
                "The Go archive does not match the official SHA-256 checksum.",
            )
            .with_detail("expected", distribution.checksum)
            .with_detail("actual", actual));
        }
        let staging =
            paths.managed_install_staging_dir(app_id.as_str(), &journal.operation_id().to_string());
        std::fs::create_dir_all(&staging).map_err(io_error)?;
        journal.record(
            OperationState::Running,
            "extract",
            "Extracting the Go toolchain into staging",
            Some(0.6),
        )?;
        let archive_for_task = archive_path.clone();
        let staging_for_task = staging.clone();
        let extraction_cancellation = cancellation.clone();
        let extracted = tokio::task::spawn_blocking(move || {
            crate::node::extract_archive(
                &archive_for_task,
                crate::node::ArchiveKind::Zip,
                &staging_for_task,
                &extraction_cancellation,
            )
        })
        .await
        .map_err(|error| {
            TorbenError::new(
                "archive_task_failed",
                "The Go archive extraction task failed.",
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
                "The Go version is already installed.",
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
        if !matches!(command, "go" | "gofmt") {
            return Err(TorbenError::new(
                "unsupported_command",
                "Go does not expose this command.",
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
                "A managed Go command is missing.",
            )
            .with_detail("path", path.display().to_string()))
        }
    }

    pub fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let mut records = Vec::new();
        let mut seen = BTreeSet::new();
        let executable = if cfg!(windows) { "go.exe" } else { "go" };
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            let candidate = directory.join(executable);
            let Ok(canonical) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) || !seen.insert(canonical.clone()) {
                continue;
            }
            let output = crate::process::command(&canonical)
                .env("GOTOOLCHAIN", "local")
                .env("GOENV", "off")
                .arg("version")
                .output();
            let Ok(output) = output else { continue };
            let Some(version) = parse_go_version(&String::from_utf8_lossy(&output.stdout)) else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("go")?,
                version,
                source_id: SourceId::new("go.external")?,
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
    ) -> TorbenResult<GoDistribution> {
        if app_id.as_str() != "go"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new("go.official")?
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
            || executable != "go"
            || arguments.as_slice() != ["version"]
            || expected_output != &format!("go version go{version} windows/amd64")
            || commands.as_slice() != ["go", "gofmt"]
        {
            return Err(invalid_plan("official distribution details"));
        }
        Ok(expected)
    }

    fn health_check_path(&self, install_path: &Path, version: &ExactVersion) -> TorbenResult<()> {
        let go = self.command_path(install_path, "go")?;
        let output = crate::process::command(go)
            .env("GOROOT", install_path)
            .env("GOTOOLCHAIN", "local")
            .env("GOENV", "off")
            .arg("version")
            .output()
            .map_err(io_error)?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if !output.status.success()
            || parse_go_version(&stdout).as_ref() != Some(version)
            || !stdout.trim().ends_with(" windows/amd64")
        {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The Go executable did not report the requested Windows x64 version.",
            ));
        }
        self.command_path(install_path, "gofmt")?;
        for tool in ["compile.exe", "link.exe"] {
            if !install_path
                .join("pkg/tool/windows_amd64")
                .join(tool)
                .is_file()
            {
                return Err(TorbenError::new(
                    "managed_command_missing",
                    "A managed Go compiler tool is missing.",
                )
                .with_detail("tool", tool));
            }
        }
        Ok(())
    }

    async fn releases(&self) -> TorbenResult<Vec<GoRelease>> {
        match self.fetch_json(&self.releases_url).await {
            Ok(releases) => Ok(releases),
            Err(error) if error.code == "go_network_error" => {
                self.fetch_json(&self.releases_fallback_url).await
            }
            Err(error) => Err(error),
        }
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(&self, url: &Url) -> TorbenResult<T> {
        let response = self
            .client
            .get(url.clone())
            // Two official endpoints must finish within the host's 30-second RPC deadline.
            .timeout(Duration::from_secs(6))
            .send()
            .await
            .map_err(network_error)?;
        validate_origin(&response, url)?;
        let response = response.error_for_status().map_err(network_error)?;
        if response
            .content_length()
            .is_some_and(|size| size > MAX_METADATA_BYTES)
        {
            return Err(metadata_error("Go metadata is too large."));
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(network_error)?;
            if bytes.len().saturating_add(chunk.len()) as u64 > MAX_METADATA_BYTES {
                return Err(metadata_error("Go metadata is too large."));
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|error| {
            TorbenError::new("go_metadata_invalid", "Go metadata is invalid.")
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
                "go_archive_too_large",
                "The Go archive is too large.",
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
            transfer.record(chunk.len(), "go_network_error")?;
            total = total.saturating_add(chunk.len() as u64);
            if total > MAX_ARCHIVE_BYTES {
                return Err(TorbenError::new(
                    "go_archive_too_large",
                    "The Go archive is too large.",
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
    let gopath = data_root.join("gopath");
    let cache = data_root.join("cache");
    let modules = gopath.join("pkg/mod");
    let tools = gopath.join("bin");
    let temp = data_root.join("temp");
    // Go telemetry uses os.UserConfigDir, not GOTELEMETRYDIR (which is read-only).
    let config = data_root.join("config");
    let telemetry = config.join("go/telemetry");
    for directory in [&cache, &modules, &tools, &temp, &telemetry] {
        std::fs::create_dir_all(directory).map_err(io_error)?;
    }
    std::fs::write(telemetry.join("mode"), b"off\n").map_err(io_error)?;
    command
        .env("GOPATH", &gopath)
        .env("GOMODCACHE", &modules)
        .env("GOBIN", &tools)
        .env("GOCACHE", &cache)
        .env("GOENV", config.join("go/env"))
        .env(
            "GOROOT",
            bin.parent()
                .ok_or_else(|| metadata_error("The Go bin directory has no parent."))?,
        )
        .env("GOTOOLCHAIN", "local")
        .env("APPDATA", &config)
        .env("GOTMPDIR", &temp)
        .env("TEMP", &temp)
        .env("TMP", &temp);
    let inherited = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        [bin.to_path_buf(), tools]
            .into_iter()
            .chain(std::env::split_paths(&inherited)),
    )
    .map_err(|error| {
        TorbenError::new(
            "go_environment_failed",
            "Could not prepare the managed Go PATH.",
        )
        .with_detail("reason", error.to_string())
    })?;
    command.env("PATH", path);
    Ok(())
}

fn ensure_platform() -> TorbenResult<()> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok(())
    } else {
        Err(TorbenError::new(
            "go_platform_unsupported",
            "Managed Go currently supports Windows x64 only.",
        ))
    }
}

fn parse_release_version(value: &str) -> Option<ExactVersion> {
    let value = value.strip_prefix("go").unwrap_or(value);
    let normalized = if value.split('.').count() == 2 {
        format!("{value}.0")
    } else {
        value.to_owned()
    };
    let version = ExactVersion::from_str(&normalized).ok()?;
    (version.as_semver().pre.is_empty() && version.as_semver().build.is_empty()).then_some(version)
}

fn windows_archive(release: &GoRelease) -> TorbenResult<Option<&GoFile>> {
    let mut files = release
        .files
        .iter()
        .filter(|file| file.os == "windows" && file.arch == "amd64" && file.kind == "archive");
    let Some(file) = files.next() else {
        return Ok(None);
    };
    if files.next().is_some()
        || file.version != release.version
        || file.filename != format!("{}.windows-amd64.zip", release.version)
    {
        return Err(metadata_error(
            "The Go Windows archive identity or checksum is invalid.",
        ));
    }
    // The all-releases feed includes historical archives without SHA-256. Those packages
    // cannot be installed safely, but must not prevent discovery of verified modern releases.
    if file.sha256.is_empty() {
        return Ok(None);
    }
    if file.sha256.len() != 64 || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(metadata_error(
            "The Go Windows archive checksum is invalid.",
        ));
    }
    Ok(Some(file))
}

fn catalog_versions(releases: &[GoRelease]) -> TorbenResult<Vec<VersionDescriptor>> {
    let mut versions = Vec::new();
    for release in releases.iter().filter(|release| release.stable) {
        let Some(version) = parse_release_version(&release.version) else {
            continue;
        };
        if windows_archive(release)?.is_some() {
            versions.push(VersionDescriptor {
                version,
                lts_name: None,
                released_at: String::new(),
                recommended: false,
            });
        }
    }
    versions.sort_by(|left, right| right.version.cmp(&left.version));
    // Show the newest patch in each stable minor line; exact patch installs remain supported.
    let mut lines = BTreeSet::new();
    versions.retain(|entry| {
        lines.insert((
            entry.version.as_semver().major,
            entry.version.as_semver().minor,
        ))
    });
    versions.truncate(3);
    let Some(first) = versions.first_mut() else {
        return Err(metadata_error(
            "The Go catalog contains no stable Windows x64 archives.",
        ));
    };
    first.recommended = true;
    Ok(versions)
}

fn distribution_from_releases(
    releases: &[GoRelease],
    version: &ExactVersion,
    base: &Url,
) -> TorbenResult<GoDistribution> {
    for release in releases.iter().filter(|release| release.stable) {
        if parse_release_version(&release.version).as_ref() != Some(version) {
            continue;
        }
        if let Some(file) = windows_archive(release)? {
            return Ok(GoDistribution {
                archive_name: file.filename.clone(),
                archive_url: base.join(&file.filename).map_err(url_error)?,
                checksum: file.sha256.to_ascii_lowercase(),
            });
        }
    }
    Err(version_not_found(&version.to_string()))
}

fn parse_go_version(output: &str) -> Option<ExactVersion> {
    let mut words = output.split_whitespace();
    if words.next()? != "go" || words.next()? != "version" {
        return None;
    }
    parse_release_version(words.next()?.strip_prefix("go")?)
}

fn metadata_error(message: &str) -> TorbenError {
    TorbenError::new("go_metadata_invalid", message)
}

fn validate_origin(response: &reqwest::Response, expected: &Url) -> TorbenResult<()> {
    if response.url().scheme() != expected.scheme()
        || response.url().host_str() != expected.host_str()
        || response.url().port_or_known_default() != expected.port_or_known_default()
    {
        return Err(TorbenError::new(
            "unexpected_download_origin",
            "Go metadata redirected outside the official origin.",
        ));
    }
    Ok(())
}

fn invalid_plan(field: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The Go plugin returned an invalid installation plan.",
    )
    .with_detail("field", field)
}
fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested Go version was not found.",
    )
    .with_detail("requested", requested)
}
fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new(
        "go_network_error",
        "A Go metadata or archive request failed.",
    )
    .with_detail("reason", error.to_string())
    .with_remediation("Check the network connection and retry the Go version catalog.")
}
fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new("go_url_invalid", "The Go provider URL is invalid.")
        .with_detail("reason", error.to_string())
}
fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new("go_io_failed", "A Go managed filesystem operation failed.")
        .with_detail("reason", error.to_string())
}
fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| String::new(), |value| value.as_secs().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn windows_fixture_installs_selects_and_uninstalls_with_a_custom_library() {
        use crate::{TorbenCore, bundled_shim::BundledShim, node_plugin::BundledPlugin};
        use sha2::{Digest, Sha256};
        use std::fmt::Write as _;
        use std::io::{Read, Write};

        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fixture-go.rs");
        let executable = root.path().join("go.exe");
        std::fs::write(
            &source,
            r#"fn main() { println!("go version go1.27.2 windows/amd64"); }"#,
        )
        .unwrap();
        let output =
            std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for name in [
            "go/bin/go.exe",
            "go/bin/gofmt.exe",
            "go/pkg/tool/windows_amd64/compile.exe",
            "go/pkg/tool/windows_amd64/link.exe",
        ] {
            archive
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive
                .write_all(&std::fs::read(&executable).unwrap())
                .unwrap();
        }
        let archive = archive.finish().unwrap().into_inner();
        let checksum = hex::encode(Sha256::digest(&archive));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let version = ExactVersion::from_str("1.27.2").unwrap();
        let metadata = serde_json::to_vec(&[serde_json::json!({
            "version": "go1.27.2", "stable": true,
            "files": [{"filename": "go1.27.2.windows-amd64.zip", "os": "windows", "arch": "amd64", "version": "go1.27.2", "sha256": checksum, "kind": "archive"}]
        })]).unwrap();
        let server = std::thread::spawn(move || {
            for (route, body) in [
                ("/releases", metadata),
                ("/go1.27.2.windows-amd64.zip", archive),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0; 4096];
                let count = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..count]);
                assert_eq!(request.split_whitespace().nth(1), Some(route));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        let library = root.path().join("custom-library");
        let mut core = TorbenCore::open(paths).unwrap();
        core.migrate_managed_library(&library).unwrap();
        core.go = GoProvider {
            client: reqwest::Client::new(),
            releases_url: base.join("releases").unwrap(),
            releases_fallback_url: base.join("fallback").unwrap(),
            dist_base: base.clone(),
        };
        let final_path = core.paths().app_version_dir("go", &version.to_string());
        let plugin = root.path().join("go-plugin.cmd");
        let archive_name = "go1.27.2.windows-amd64.zip";
        let responses = [
            (
                "initialize",
                1,
                serde_json::json!({
                    "protocolVersion": 1, "pluginId": "app.torben.plugin.go", "pluginVersion": env!("CARGO_PKG_VERSION"),
                    "applications": [{"id":"go", "displayName":"Go", "summary":"fixture", "categories":[], "capabilities":[], "sources":[]}]
                }),
            ),
            (
                "version.resolve",
                2,
                serde_json::json!({"requested":"current", "resolved":version}),
            ),
            (
                "uninstall.plan",
                2,
                serde_json::json!({"appId":"go", "version":version, "sourceId":"go.official", "installPath":final_path, "preserveUserData":true}),
            ),
            (
                "install.plan",
                3,
                serde_json::json!({
                    "appId":"go", "version":version, "sourceId":"go.official", "metadata":{},
                    "steps":[
                        {"type":"download", "url":base.join(archive_name).unwrap(), "destination_name":archive_name},
                        {"type":"verify_sha256", "archive_name":archive_name, "expected":checksum},
                        {"type":"extract_archive", "archive_name":archive_name, "strip_components":0},
                        {"type":"health_check", "executable":"go", "arguments":["version"], "expected_output":"go version go1.27.2 windows/amd64"},
                        {"type":"create_shims", "commands":["go","gofmt"]}
                    ]
                }),
            ),
            (
                "health.check",
                2,
                serde_json::json!({"healthy":true, "actualVersion":version, "message":"healthy"}),
            ),
        ];
        let mut script = "@echo off\r\n:loop\r\nset request=\r\nset /p request=\r\nif errorlevel 1 exit /b 0\r\n".to_owned();
        for (method, id, result) in responses {
            let response = serde_json::json!({"jsonrpc":"2.0", "id":id, "result":result});
            write!(
                script,
                "echo %request%| findstr /c:\"{method}\" >nul && (echo {response}& goto loop)\r\n"
            )
            .unwrap();
        }
        script.push_str("exit /b 1\r\n");
        std::fs::write(&plugin, script).unwrap();
        core.go_plugin = BundledPlugin::go_from_executable(plugin);
        core.bundled_shim = BundledShim::from_executable(executable);
        let app_id = AppId::new("go").unwrap();
        let cache = core.paths().go_data_dir().join("cache/fixture");
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(&cache, b"preserved-cache").unwrap();

        let installed = core.install(&app_id, "current").await.unwrap();
        server.join().unwrap();
        assert_eq!(
            core.store.selected_version(&app_id).unwrap(),
            Some(version.clone())
        );
        assert!(Path::new(&installed.install_path).starts_with(&library));
        assert!(core.executable_for(&app_id, "go").unwrap().is_file());
        assert!(core.executable_for(&app_id, "gofmt").unwrap().is_file());
        core.clear_selection(&app_id).unwrap();
        core.uninstall(&app_id, &version).await.unwrap();
        assert!(core.installed().unwrap().is_empty());
        assert!(!final_path.exists());
        assert_eq!(std::fs::read(cache).unwrap(), b"preserved-cache");
    }

    fn release(version: &str) -> GoRelease {
        GoRelease {
            version: version.to_owned(),
            stable: true,
            files: vec![GoFile {
                filename: format!("{version}.windows-amd64.zip"),
                os: "windows".to_owned(),
                arch: "amd64".to_owned(),
                version: version.to_owned(),
                sha256: "a".repeat(64),
                kind: "archive".to_owned(),
            }],
        }
    }

    #[test]
    fn catalog_keeps_latest_patch_per_minor_and_rejects_invalid_archive_metadata() {
        let versions = catalog_versions(&[
            release("go1.26.5"),
            release("go1.27.1"),
            release("go1.27.2"),
        ])
        .unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[0].version.to_string(), "1.27.2");
        assert!(versions[0].recommended);
        assert_eq!(versions[1].version.to_string(), "1.26.5");
        assert!(!versions[1].recommended);
        let mut invalid = release("go1.27.2");
        invalid.files[0].filename = "../go.zip".to_owned();
        assert_eq!(
            windows_archive(&invalid).unwrap_err().code,
            "go_metadata_invalid"
        );
        invalid = release("go1.27.2");
        invalid.files[0].sha256 = "invalid".to_owned();
        assert_eq!(
            windows_archive(&invalid).unwrap_err().code,
            "go_metadata_invalid"
        );
    }

    #[test]
    fn catalog_skips_historical_archives_without_checksums() {
        let mut historical = release("go1.10.8");
        historical.files[0].sha256.clear();

        let versions = catalog_versions(&[historical, release("go1.27.2")]).unwrap();

        assert_eq!(versions.len(), 1);
        assert_eq!(versions[0].version.to_string(), "1.27.2");
        assert!(versions[0].recommended);
    }

    #[test]
    fn managed_environment_keeps_caches_and_telemetry_inside_data_root() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("package-managers/go");
        let bin = root.path().join("apps/go/1.27.2/bin");
        let mut command = std::process::Command::new("go");
        configure_command_environment(&mut command, &data, &bin).unwrap();
        let env = command
            .get_envs()
            .collect::<std::collections::BTreeMap<_, _>>();
        for name in [
            "GOPATH",
            "GOMODCACHE",
            "GOBIN",
            "GOCACHE",
            "GOENV",
            "APPDATA",
            "GOTMPDIR",
            "TEMP",
            "TMP",
        ] {
            assert!(Path::new(env[std::ffi::OsStr::new(name)].unwrap()).starts_with(&data));
        }
        assert_eq!(env[std::ffi::OsStr::new("GOTOOLCHAIN")].unwrap(), "local");
        assert_eq!(
            Path::new(env[std::ffi::OsStr::new("GOROOT")].unwrap()),
            bin.parent().unwrap()
        );
        assert_eq!(
            std::fs::read(data.join("config/go/telemetry/mode")).unwrap(),
            b"off\n"
        );
    }

    #[test]
    fn parses_go_release_versions() {
        assert_eq!(
            parse_release_version("go1.27.2").unwrap().to_string(),
            "1.27.2"
        );
        assert_eq!(parse_release_version("1.27").unwrap().to_string(), "1.27.0");
        assert!(parse_release_version("go1.27.2rc1").is_none());
    }

    #[test]
    fn selects_windows_amd64_archive() {
        let release = GoRelease {
            version: "go1.27.2".to_owned(),
            stable: true,
            files: vec![GoFile {
                filename: "go1.27.2.windows-amd64.zip".to_owned(),
                os: "windows".to_owned(),
                arch: "amd64".to_owned(),
                version: "go1.27.2".to_owned(),
                sha256: "a".repeat(64),
                kind: "archive".to_owned(),
            }],
        };
        let version = parse_release_version("1.27.2").unwrap();
        let base = Url::parse("https://dl.google.com/go/").unwrap();
        let distribution = distribution_from_releases(&[release], &version, &base).unwrap();
        assert_eq!(distribution.archive_name, "go1.27.2.windows-amd64.zip");
        assert_eq!(distribution.checksum, "a".repeat(64));
    }
}
