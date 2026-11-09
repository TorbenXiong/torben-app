use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use tokio::io::AsyncWriteExt;
use torben_contracts::{
    AppId, ExactVersion, InstallRecord, InstallScope, OperationState, SourceId, TorbenError,
    TorbenResult, VersionDescriptor,
    plugin::{InstallPlan, InstallStep},
};
use url::Url;

use crate::{
    TorbenPaths,
    download::verified,
    node::sha256_file_checked,
    operation::{CancellationProbe, OperationJournal},
};

const VERSION: &str = "4.2.8";
const ARCHIVE_NAME: &str = "cmake-4.2.8-windows-x86_64.msi";
const ARCHIVE_URL: &str =
    "https://github.com/Kitware/CMake/releases/download/v4.2.8/cmake-4.2.8-windows-x86_64.msi";
const ARCHIVE_SHA256: &str = "3659257c5cac08b3b7b8d3dcce3643cf689429e1603baec37bdc401df97e9b3f";

#[derive(Debug, Clone)]
pub struct CMakeProvider {
    client: reqwest::Client,
    archive_url: Url,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CMakeDistribution {
    pub archive_name: String,
    pub archive_url: Url,
    pub checksum: String,
}

impl CMakeProvider {
    pub fn official() -> TorbenResult<Self> {
        let client = crate::download::client_builder()
            .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
            .https_only(true)
            .build()
            .map_err(network_error)?;
        Ok(Self {
            client,
            archive_url: Url::parse(ARCHIVE_URL).map_err(url_error)?,
        })
    }

    pub fn list_versions(&self) -> TorbenResult<Vec<VersionDescriptor>> {
        ensure_platform()?;
        Ok(vec![VersionDescriptor {
            version: ExactVersion::from_str(VERSION).map_err(version_error)?,
            lts_name: None,
            released_at: "2026-09-04T00:00:00Z".to_owned(),
            recommended: true,
        }])
    }

    pub fn resolve_version(&self, requested: &str) -> TorbenResult<ExactVersion> {
        let version = ExactVersion::from_str(VERSION).map_err(version_error)?;
        if requested.trim().eq_ignore_ascii_case("latest")
            || requested.trim().eq_ignore_ascii_case("stable")
            || requested.trim() == VERSION
            || requested.trim() == "4.2.8"
        {
            Ok(version)
        } else {
            Err(version_not_found(requested))
        }
    }

    pub fn distribution(&self, version: &ExactVersion) -> TorbenResult<CMakeDistribution> {
        let expected = ExactVersion::from_str(VERSION).map_err(version_error)?;
        if version != &expected {
            return Err(version_not_found(&version.to_string()));
        }
        Ok(CMakeDistribution {
            archive_name: ARCHIVE_NAME.to_owned(),
            archive_url: self.archive_url.clone(),
            checksum: ARCHIVE_SHA256.to_owned(),
        })
    }

    pub fn install_plan(
        &self,
        app_id: &AppId,
        version: &ExactVersion,
        source_id: &SourceId,
        target: &str,
    ) -> TorbenResult<InstallPlan> {
        ensure_platform()?;
        if app_id.as_str() != "cmake"
            || source_id != &SourceId::new("cmake.official")?
            || target != current_target()
        {
            return Err(invalid_plan("identity, source owner, or target"));
        }
        let distribution = self.distribution(version)?;
        Ok(InstallPlan {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: source_id.clone(),
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
                    executable: "cmake".to_owned(),
                    arguments: vec!["--version".to_owned()],
                    expected_output: "cmake version".to_owned(),
                },
                InstallStep::CreateShims {
                    commands: vec!["cmake".to_owned()],
                },
            ],
            metadata: std::collections::BTreeMap::from([("target".to_owned(), target.to_owned())]),
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
            "Downloading CMake",
            Some(0.2),
        )?;
        if !archive_path.is_file()
            || sha256_file_checked(&archive_path, Some(&cancellation))? != distribution.checksum
        {
            let official_url = distribution.archive_url.clone();
            let destination = archive_path.clone();
            verified(
                &official_url,
                &destination,
                &distribution.checksum,
                Some(&cancellation),
                |url| {
                    let official_url = official_url.clone();
                    let destination = destination.clone();
                    let cancellation = cancellation.clone();
                    async move {
                        self.download(&url, &official_url, &destination, &cancellation)
                            .await
                    }
                },
            )
            .await?;
        }
        let staging =
            paths.managed_install_staging_dir(app_id.as_str(), &journal.operation_id().to_string());
        let payload = staging.join("payload");
        std::fs::create_dir_all(&payload).map_err(io_error)?;
        journal.record(
            OperationState::Running,
            "extract",
            "Extracting the CMake MSI into staging",
            Some(0.6),
        )?;
        extract_msi(&archive_path, &payload, &cancellation)?;
        let extracted_root = locate_install_root(&payload)?;
        self.health_check_path(&extracted_root, version)?;
        cancellation.check()?;
        let final_path = paths.app_version_dir(app_id.as_str(), &version.to_string());
        if final_path.exists() {
            return Err(TorbenError::new(
                "install_path_exists",
                "The CMake version is already installed.",
            ));
        }
        if let Some(parent) = final_path.parent() {
            std::fs::create_dir_all(parent).map_err(io_error)?;
        }
        std::fs::rename(extracted_root, &final_path).map_err(io_error)?;
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
        if command != "cmake" {
            return Err(TorbenError::new(
                "unsupported_command",
                "CMake exposes only the cmake command.",
            )
            .with_detail("command", command));
        }
        let path = install_path.join("cmake.exe");
        if path.is_file() {
            Ok(path)
        } else {
            Err(TorbenError::new(
                "managed_command_missing",
                "The managed cmake.exe is missing.",
            )
            .with_detail("path", path.display().to_string()))
        }
    }

    pub fn discover_external(&self, managed_root: &Path) -> TorbenResult<Vec<InstallRecord>> {
        let mut records = Vec::new();
        let mut seen = BTreeSet::new();
        for directory in std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()) {
            let candidate = directory.join("cmake.exe");
            let Ok(canonical) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            if canonical.starts_with(managed_root) || !seen.insert(canonical.clone()) {
                continue;
            }
            let output = crate::process::command(&canonical)
                .arg("--version")
                .output();
            let Ok(output) = output else { continue };
            let Some(version) = successful_version(&output) else {
                continue;
            };
            records.push(InstallRecord {
                app_id: AppId::new("cmake")?,
                version,
                source_id: SourceId::new("cmake.external")?,
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
    ) -> TorbenResult<CMakeDistribution> {
        if app_id.as_str() != "cmake"
            || &plan.app_id != app_id
            || &plan.version != version
            || plan.source_id != SourceId::new("cmake.official")?
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
            || executable != "cmake"
            || arguments.as_slice() != ["--version"]
            || expected_output != "cmake version"
            || commands.as_slice() != ["cmake"]
        {
            return Err(invalid_plan("official distribution details"));
        }
        Ok(expected)
    }

    fn health_check_path(&self, install_path: &Path, version: &ExactVersion) -> TorbenResult<()> {
        let output = crate::process::command(self.command_path(install_path, "cmake")?)
            .arg("--version")
            .output()
            .map_err(io_error)?;
        if successful_version(&output).as_ref() != Some(version) {
            return Err(TorbenError::new(
                "health_check_version_mismatch",
                "The cmake executable did not report the requested version.",
            ));
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
        let response = self
            .client
            .get(url.clone())
            .send()
            .await
            .map_err(network_error)?;
        if !download_response_allowed(official, url, response.url()) {
            return Err(TorbenError::new(
                "unexpected_download_origin",
                "The CMake download redirected outside the official release.",
            ));
        }
        let response = response.error_for_status().map_err(network_error)?;
        if response
            .content_length()
            .is_some_and(|size| size > 128 * 1024 * 1024)
        {
            return Err(TorbenError::new(
                "cmake_archive_too_large",
                "The CMake MSI is too large.",
            ));
        }
        let partial = destination.with_extension("partial");
        let mut file = crate::download::open_partial(&partial, false).map_err(io_error)?;
        let mut stream = response.bytes_stream();
        let mut total = 0u64;
        let mut transfer = crate::download::TransferRate::new();
        while let Some(chunk) = futures_util::StreamExt::next(&mut stream).await {
            cancellation.check()?;
            let chunk = chunk.map_err(network_error)?;
            transfer.record(chunk.len(), "cmake_network_error")?;
            total = total.saturating_add(chunk.len() as u64);
            if total > 128 * 1024 * 1024 {
                return Err(TorbenError::new(
                    "cmake_archive_too_large",
                    "The CMake MSI is too large.",
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

fn download_response_allowed(official: &Url, requested: &Url, response: &Url) -> bool {
    crate::download::mirror_response(official, requested, response)
        || (requested == official
            && (response == official
                || (official.scheme() == "https"
                    && official.host_str() == Some("github.com")
                    && response.scheme() == "https"
                    && response.port().is_none()
                    && response.username().is_empty()
                    && response.password().is_none()
                    && matches!(
                        response.host_str(),
                        Some(
                            "objects.githubusercontent.com"
                                | "release-assets.githubusercontent.com"
                        )
                    ))))
}

fn extract_msi(
    archive: &Path,
    payload: &Path,
    cancellation: &CancellationProbe,
) -> TorbenResult<()> {
    cancellation.check()?;
    let status = crate::process::command("msiexec.exe")
        .args([
            "/a",
            &archive.display().to_string(),
            "/qn",
            &format!("TARGETDIR={}", payload.display()),
        ])
        .status()
        .map_err(io_error)?;
    if status.success() {
        Ok(())
    } else {
        Err(
            TorbenError::new("archive_error", "The CMake MSI could not be extracted.")
                .with_detail("status", status.to_string()),
        )
    }
}

fn locate_install_root(payload: &Path) -> TorbenResult<PathBuf> {
    for entry in walkdir::WalkDir::new(payload)
        .follow_links(false)
        .max_depth(5)
    {
        let entry = entry.map_err(|error| io_error_message(&error.to_string()))?;
        if entry.file_type().is_file() && entry.file_name().eq_ignore_ascii_case("cmake.exe") {
            return entry
                .path()
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| io_error_message("The staged CMake executable has no parent."));
        }
    }
    Err(TorbenError::new(
        "managed_command_missing",
        "The CMake MSI did not contain cmake.exe.",
    ))
}

fn successful_version(output: &std::process::Output) -> Option<ExactVersion> {
    if !output.status.success() {
        return None;
    }
    parse_version(&String::from_utf8_lossy(&output.stdout))
}

fn parse_version(output: &str) -> Option<ExactVersion> {
    output.lines().find_map(|line| {
        let line = line.trim();
        let token = line
            .strip_prefix("cmake version ")
            .or_else(|| line.strip_prefix("CMake version "))?
            .split_whitespace()
            .next()?;
        ExactVersion::from_str(token).ok()
    })
}
fn ensure_platform() -> TorbenResult<()> {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok(())
    } else {
        Err(TorbenError::new(
            "cmake_platform_unsupported",
            "Managed CMake currently supports Windows x64 only.",
        ))
    }
}

fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
fn invalid_plan(field: &str) -> TorbenError {
    TorbenError::new(
        "plugin_install_plan_invalid",
        "The CMake plugin returned an invalid installation plan.",
    )
    .with_detail("field", field)
}
fn version_not_found(requested: &str) -> TorbenError {
    TorbenError::new(
        "version_not_found",
        "The requested CMake version was not found.",
    )
    .with_detail("requested", requested)
}
fn version_error(error: TorbenError) -> TorbenError {
    TorbenError::new("version_invalid", "The CMake version is invalid.")
        .with_detail("reason", error.to_string())
}
fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new("cmake_network_error", "The official CMake request failed.")
        .with_detail("reason", error.to_string())
}
fn url_error(error: url::ParseError) -> TorbenError {
    TorbenError::new("cmake_url_invalid", "The official CMake URL is invalid.")
        .with_detail("reason", error.to_string())
}
fn io_error(error: std::io::Error) -> TorbenError {
    io_error_message(&error.to_string())
}
fn io_error_message(reason: &str) -> TorbenError {
    TorbenError::new(
        "cmake_io_failed",
        "A CMake managed filesystem operation failed.",
    )
    .with_detail("reason", reason)
}
fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| String::new(), |value| value.as_secs().to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        str::FromStr,
        sync::Arc,
        thread,
    };

    use torben_contracts::{AppId, ExactVersion, OperationKind, SourceId, plugin::InstallStep};
    use url::Url;

    use crate::{StateStore, TorbenPaths, operation::OperationJournal};

    use super::{
        ARCHIVE_URL, CMakeProvider, current_target, download_response_allowed, locate_install_root,
        parse_version,
    };

    #[cfg(windows)]
    #[test]
    fn external_version_requires_a_successful_health_check() {
        use std::{
            os::windows::process::ExitStatusExt,
            process::{ExitStatus, Output},
        };

        for (status, stdout, expected) in [
            (0, "cmake version 4.2.8", Some("4.2.8".parse().unwrap())),
            (1, "cmake version 4.2.8", None),
            (0, "unrelated command", None),
        ] {
            let output = Output {
                status: ExitStatus::from_raw(status),
                stdout: stdout.as_bytes().to_vec(),
                stderr: Vec::new(),
            };
            assert_eq!(super::successful_version(&output), expected);
        }
    }

    #[test]
    fn accepts_official_response_and_github_asset_cdn_only() {
        let official = Url::parse(ARCHIVE_URL).unwrap();
        assert!(download_response_allowed(&official, &official, &official));
        for url in [
            "https://release-assets.githubusercontent.com/github-production-release-asset/file",
            "https://objects.githubusercontent.com/github-production-release-asset/file",
        ] {
            assert!(download_response_allowed(
                &official,
                &official,
                &Url::parse(url).unwrap()
            ));
        }
        for url in [
            "https://github.com/other/release/file",
            "https://untrusted.example/cmake-4.2.8-windows-x86_64.msi",
            "https://release-assets.githubusercontent.com.evil.example/file",
            "http://release-assets.githubusercontent.com/file",
            "https://release-assets.githubusercontent.com:8443/file",
            "https://user@release-assets.githubusercontent.com/file",
        ] {
            assert!(!download_response_allowed(
                &official,
                &official,
                &Url::parse(url).unwrap()
            ));
        }
    }

    #[tokio::test]
    async fn downloads_and_verifies_the_exact_fixture_resource() {
        use sha2::{Digest, Sha256};

        let body = b"verified CMake fixture";
        let (url, server) = fixture_server("200 OK", body);
        let provider = CMakeProvider {
            client: crate::download::client_builder()
                .no_proxy()
                .build()
                .unwrap(),
            archive_url: url.clone(),
        };
        let root = tempfile::tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        paths.ensure_layout().unwrap();
        let journal = OperationJournal::start(
            &paths,
            Arc::new(StateStore::open(paths.state_database()).unwrap()),
            OperationKind::Install,
            &AppId::new("cmake").unwrap(),
            None,
        )
        .unwrap();
        let cancellation = journal.cancellation_probe();
        let destination = root.path().join("fixture.msi");
        let checksum = hex::encode(Sha256::digest(body));
        crate::download::verified(
            &url,
            &destination,
            &checksum,
            Some(&cancellation),
            |candidate| {
                let cancellation = &cancellation;
                let provider = &provider;
                let destination = &destination;
                let official = &url;
                async move {
                    provider
                        .download(&candidate, official, destination, cancellation)
                        .await
                }
            },
        )
        .await
        .unwrap();
        server.join().unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), body);
        assert!(!destination.with_extension("partial").exists());
    }

    #[tokio::test]
    async fn preserves_http_failure_without_committing_a_download() {
        let (url, server) = fixture_server("404 Not Found", b"missing");
        let provider = CMakeProvider {
            client: crate::download::client_builder()
                .no_proxy()
                .build()
                .unwrap(),
            archive_url: url.clone(),
        };
        let root = tempfile::tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("workspace"));
        paths.ensure_layout().unwrap();
        let journal = OperationJournal::start(
            &paths,
            Arc::new(StateStore::open(paths.state_database()).unwrap()),
            OperationKind::Install,
            &AppId::new("cmake").unwrap(),
            None,
        )
        .unwrap();
        let destination = root.path().join("fixture.msi");
        let error = provider
            .download(&url, &url, &destination, &journal.cancellation_probe())
            .await
            .unwrap_err();
        server.join().unwrap();
        assert_eq!(error.code, "cmake_network_error");
        assert!(error.details["reason"].contains("404"));
        assert!(!destination.exists());
        assert!(!destination.with_extension("partial").exists());
    }

    #[cfg(windows)]
    #[test]
    fn staged_health_check_uses_the_directory_containing_cmake() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("fixture.rs");
        let install_root = root.path().join("payload/Files/CMake");
        std::fs::create_dir_all(&install_root).unwrap();
        let executable = install_root.join("cmake.exe");
        std::fs::write(&source, r#"fn main() { println!("cmake version 4.2.8"); }"#).unwrap();
        let output =
            std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .arg(source)
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let located = locate_install_root(&root.path().join("payload")).unwrap();
        assert_eq!(located, install_root);
        let provider = CMakeProvider::official().unwrap();
        provider
            .health_check_path(&located, &"4.2.8".parse().unwrap())
            .unwrap();
        assert_eq!(
            provider
                .health_check_path(&located, &"26.3.0".parse().unwrap())
                .unwrap_err()
                .code,
            "health_check_version_mismatch"
        );
    }

    fn fixture_server(status: &str, body: &[u8]) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = Url::parse(&format!(
            "http://{}/fixture.msi",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let body = body.to_vec();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                .unwrap();
            let mut request = [0u8; 4096];
            let count = stream.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..count]).starts_with("GET /fixture.msi "));
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(&body).unwrap();
        });
        (url, server)
    }

    #[test]
    fn parses_official_windows_version_output() {
        assert_eq!(
            parse_version("cmake version 4.2.8 : Copyright"),
            Some("4.2.8".parse().unwrap())
        );
        assert_eq!(
            parse_version("  CMake version 4.2.8\r\nCopyright (C) Kitware"),
            Some("4.2.8".parse().unwrap())
        );
    }

    #[test]
    fn rejects_unrelated_command_output() {
        assert_eq!(parse_version("archive tool 4.2.8"), None);
    }

    #[test]
    fn core_generates_the_pinned_install_plan() {
        let provider = CMakeProvider::official().unwrap();
        let plan = provider
            .install_plan(
                &AppId::new("cmake").unwrap(),
                &ExactVersion::from_str("4.2.8").unwrap(),
                &SourceId::new("cmake.official").unwrap(),
                &current_target(),
            )
            .unwrap();
        assert!(matches!(
            plan.steps.get(1),
            Some(InstallStep::VerifySha256 { expected, .. })
                if expected == "3659257c5cac08b3b7b8d3dcce3643cf689429e1603baec37bdc401df97e9b3f"
        ));
        assert!(matches!(
            plan.steps.get(3),
            Some(InstallStep::HealthCheck { executable, arguments, .. })
                if executable == "cmake" && arguments.as_slice() == ["--version"]
        ));
    }
}
