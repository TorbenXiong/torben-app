//! Transport mirrors never change a provider's source owner or integrity authority.
use std::{
    future::Future,
    path::Path,
    sync::OnceLock,
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use serde::Deserialize;
use torben_contracts::{TorbenError, TorbenResult};
use url::Url;

use crate::{node::sha256_file_checked, operation::CancellationProbe, process};

pub(crate) struct TransferRate {
    started: Instant,
    bytes: u64,
}

impl TransferRate {
    pub(crate) fn new() -> Self {
        Self {
            started: Instant::now(),
            bytes: 0,
        }
    }

    pub(crate) fn record(&mut self, bytes: usize, network_code: &str) -> TorbenResult<()> {
        self.record_at(bytes as u64, Instant::now(), network_code)
    }

    fn record_at(&mut self, bytes: u64, now: Instant, network_code: &str) -> TorbenResult<()> {
        self.bytes = self.bytes.saturating_add(bytes);
        let elapsed = now.duration_since(self.started);
        if elapsed >= Duration::from_secs(15) {
            if self.bytes / elapsed.as_secs() < 16 * 1024 {
                return Err(TorbenError::new(
                    network_code,
                    "The download source is too slow; another source will be tried.",
                )
                .with_detail("reason", "download_throughput_too_low"));
            }
            self.started = now;
            self.bytes = 0;
        }
        Ok(())
    }
}

pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(20))
}

pub(crate) fn open_partial(path: &Path, append: bool) -> std::io::Result<tokio::fs::File> {
    // Tokio opens files on the blocking pool. Dropping a cancelled source
    // future cannot stop that open, which can recreate a partial after Core
    // rolls it back. Open before yielding; streaming writes remain async.
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .map(tokio::fs::File::from_std)
}

#[derive(Deserialize)]
struct GithubRelease {
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    id: u64,
    browser_download_url: String,
}

/// An official API route can reach GitHub's asset CDN without navigating
/// github.com. The caller must still verify the provider's pinned checksum.
pub(crate) async fn github_asset_url(
    client: &reqwest::Client,
    official: &Url,
) -> TorbenResult<Url> {
    let parts: Vec<_> = official.path().split('/').collect();
    let ["", owner, repository, "releases", "download", tag, _] = parts.as_slice() else {
        return Err(TorbenError::internal("Invalid GitHub release asset path."));
    };
    if official.scheme() != "https" || official.host_str() != Some("github.com") {
        return Err(TorbenError::internal(
            "Invalid GitHub release asset origin.",
        ));
    }
    let api = Url::parse(&format!(
        "https://api.github.com/repos/{owner}/{repository}/releases/tags/{tag}"
    ))
    .map_err(|error| TorbenError::internal(error.to_string()))?;
    let response = client
        .get(api.clone())
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|error| {
            TorbenError::new(
                "network_error",
                "The official GitHub asset API is unavailable.",
            )
            .with_detail("reason", error.to_string())
        })?;
    if response.url() != &api
        || response
            .content_length()
            .is_some_and(|size| size > 4 * 1024 * 1024)
    {
        return Err(TorbenError::new(
            "unexpected_download_origin",
            "The GitHub asset API returned an unexpected response.",
        ));
    }
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            TorbenError::new(
                "network_error",
                "The GitHub asset API response was interrupted.",
            )
            .with_detail("reason", error.to_string())
        })?;
        if bytes.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
            return Err(TorbenError::new(
                "network_error",
                "The GitHub asset API response is too large.",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    github_asset_from_json(&bytes, official, owner, repository)
}

fn github_asset_from_json(
    bytes: &[u8],
    official: &Url,
    owner: &str,
    repository: &str,
) -> TorbenResult<Url> {
    let release: GithubRelease = serde_json::from_slice(bytes).map_err(|error| {
        TorbenError::new("network_error", "The GitHub asset API response is invalid.")
            .with_detail("reason", error.to_string())
    })?;
    let asset = release
        .assets
        .into_iter()
        .find(|asset| Url::parse(&asset.browser_download_url).ok().as_ref() == Some(official))
        .ok_or_else(|| {
            TorbenError::new(
                "network_error",
                "The exact GitHub release asset was not found.",
            )
        })?;
    Url::parse(&format!(
        "https://api.github.com/repos/{owner}/{repository}/releases/assets/{}",
        asset.id
    ))
    .map_err(|error| TorbenError::internal(error.to_string()))
}

pub(crate) fn china_region() -> bool {
    static WINDOWS_CHINA: OnceLock<bool> = OnceLock::new();
    if let Ok(region) = std::env::var("TORBEN_REGION") {
        return chinese_locale(&region);
    }
    if ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .any(|value| chinese_locale(&value))
    {
        return true;
    }
    *WINDOWS_CHINA.get_or_init(|| {
        cfg!(windows)
            && process::command("reg.exe")
                .args([
                    "query",
                    r"HKCU\Control Panel\International",
                    "/v",
                    "LocaleName",
                ])
                .output()
                .is_ok_and(|output| {
                    output.status.success()
                        && String::from_utf8_lossy(&output.stdout)
                            .split_whitespace()
                            .any(chinese_locale)
                })
    })
}

fn chinese_locale(value: &str) -> bool {
    let value = value.trim().to_ascii_lowercase().replace('_', "-");
    matches!(value.as_str(), "cn" | "china")
        || value.split(['.', '@', ':']).any(|part| part == "zh-cn")
}

// The upstream URL is case-sensitive; changing its suffix would request a
// different resource on the mirror, so comparisons intentionally stay exact.
#[allow(clippy::case_sensitive_file_extension_comparisons)]
fn mirrors(official: &Url) -> Vec<Url> {
    if official.scheme() != "https"
        || official.port().is_some()
        || !official.username().is_empty()
        || official.password().is_some()
        || official.query().is_some()
        || official.fragment().is_some()
    {
        return Vec::new();
    }
    let mut result = Vec::new();
    let mut map = |prefix: &str, bases: &[&str]| {
        if let Some(path) = official.path().strip_prefix(prefix) {
            for base in bases {
                if let Ok(url) = Url::parse(base).and_then(|base| base.join(path)) {
                    result.push(url);
                }
            }
        }
    };
    match official.host_str() {
        // Mirror indexes can lag by months while returning HTTP 200. Resolve
        // versions from the small official index; mirror signed artifacts only.
        Some("nodejs.org") if official.path() != "/dist/index.json" => map(
            "/dist/",
            &[
                "https://mirrors.tuna.tsinghua.edu.cn/nodejs-release/",
                "https://mirrors.huaweicloud.com/nodejs/",
            ],
        ),
        Some("www.python.org") => map(
            "/ftp/python/",
            &[
                "https://mirrors.huaweicloud.com/python/",
                "https://mirror.nju.edu.cn/python/",
            ],
        ),
        Some("static.rust-lang.org") => {
            // Rust mirror manifests can rewrite URLs and hashes. Only mirror
            // archives, whose hashes still come from the official manifest.
            if official.path().ends_with(".msi")
                || official.path().ends_with(".tar.xz")
                || official.path().ends_with(".tar.gz")
            {
                map(
                    "/dist/",
                    &[
                        "https://mirrors.tuna.tsinghua.edu.cn/rustup/dist/",
                        "https://mirrors.ustc.edu.cn/rust-static/dist/",
                    ],
                );
            }
        }
        Some("cdn.mysql.com") => map(
            "/Downloads/",
            &[
                "https://mirrors.huaweicloud.com/mysql/Downloads/",
                "https://repo.huaweicloud.com/mysql/Downloads/",
            ],
        ),
        Some("github.com") => {
            let parts: Vec<_> = official.path().split('/').collect();
            if let ["", "adoptium", repository, "releases", "download", _, name] = parts.as_slice()
                && let Some(feature) = repository
                    .strip_prefix("temurin")
                    .and_then(|value| value.strip_suffix("-binaries"))
                && feature.chars().all(|character| character.is_ascii_digit())
                && !feature.is_empty()
                && name.starts_with("OpenJDK")
                && name.contains("-jdk_x64_windows_hotspot_")
                && name.ends_with(".zip")
            {
                for base in [
                    "https://mirrors.tuna.tsinghua.edu.cn/Adoptium/",
                    "https://mirror.nju.edu.cn/adoptium/",
                ] {
                    if let Ok(url) = Url::parse(&format!("{base}{feature}/jdk/x64/windows/{name}"))
                    {
                        result.push(url);
                    }
                }
            }
        }
        _ => {}
    }
    result
}

pub(crate) fn candidates(official: &Url) -> Vec<Url> {
    let mut urls = if china_region() {
        mirrors(official)
    } else {
        Vec::new()
    };
    urls.push(official.clone());
    urls
}

/// Require the exact mapped resource, not merely any URL on a mirror host.
pub(crate) fn mirror_response(official: &Url, requested: &Url, response: &Url) -> bool {
    china_region() && mirrors(official).contains(requested) && requested == response
}

pub(crate) async fn try_sources<T, F, Fut>(
    urls: Vec<Url>,
    cancellation: Option<&CancellationProbe>,
    mut attempt: F,
) -> TorbenResult<T>
where
    F: FnMut(Url) -> Fut,
    Fut: Future<Output = TorbenResult<T>>,
{
    let mut last_error = None;
    for url in urls {
        let future = attempt(url);
        tokio::pin!(future);
        let result = loop {
            if let Some(cancellation) = cancellation {
                cancellation.check()?;
            }
            tokio::select! {
                result = &mut future => break result,
                () = tokio::time::sleep(Duration::from_millis(100)), if cancellation.is_some() => {}
            }
        };
        match result {
            Ok(value) => return Ok(value),
            Err(error) if retryable(&error) => {
                tracing::warn!(code = %error.code, "Download source failed; trying the next source");
                last_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }
    Err(last_error.unwrap_or_else(|| TorbenError::internal("No download source was available.")))
}

fn retryable(error: &TorbenError) -> bool {
    error.code.contains("network_error")
        || matches!(
            error.code.as_str(),
            "archive_hash_mismatch"
                | "archive_size_mismatch"
                | "unexpected_download_origin"
                | "checksum_signature_invalid"
                | "checksum_signature_missing"
        )
}

pub(crate) async fn verified<T, F, Fut>(
    official: &Url,
    destination: &Path,
    checksum: &str,
    cancellation: Option<&CancellationProbe>,
    mut attempt: F,
) -> TorbenResult<T>
where
    F: FnMut(Url) -> Fut,
    Fut: Future<Output = TorbenResult<T>>,
{
    let mut urls = candidates(official);
    // A transient failure of the last/only official endpoint gets one retry.
    urls.push(official.clone());
    let result = try_sources(urls, cancellation, |url| {
        let future = attempt(url);
        async move {
            let result = async {
                let value = future.await?;
                let actual = sha256_file_checked(destination, cancellation)?;
                if actual != checksum {
                    return Err(TorbenError::new(
                        "archive_hash_mismatch",
                        "The downloaded archive does not match its authoritative SHA-256 checksum.",
                    )
                    .with_detail("expected", checksum)
                    .with_detail("actual", actual));
                }
                Ok(value)
            }
            .await;
            if result.is_err() {
                // A partial range from one mirror must never be appended to
                // bytes from a different source. In-source resume stays local.
                remove_partial(destination).await?;
            }
            result
        }
    })
    .await;
    // try_sources can cancel by dropping the attempt future, bypassing its
    // error branch. Clean up again after that future and its handles are gone.
    if result.is_err() {
        remove_partial(destination).await?;
    }
    result
}

async fn remove_partial(destination: &Path) -> TorbenResult<()> {
    match tokio::fs::remove_file(destination.with_extension("partial")).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(
            TorbenError::internal("Could not clean up a partial download.")
                .with_detail("reason", error.to_string()),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn partial_file_is_visible_before_yielding_and_preserves_resume_bytes() {
        use tokio::io::AsyncWriteExt;
        let root = tempfile::tempdir().unwrap();
        let partial = root.path().join("runtime.partial");
        let mut file = open_partial(&partial, false).unwrap();
        assert!(partial.is_file());
        file.write_all(b"first").await.unwrap();
        file.flush().await.unwrap();
        drop(file);
        let mut file = open_partial(&partial, true).unwrap();
        file.write_all(b"second").await.unwrap();
        file.flush().await.unwrap();
        drop(file);
        assert_eq!(std::fs::read(&partial).unwrap(), b"firstsecond");
        drop(open_partial(&partial, false).unwrap());
        assert!(std::fs::read(&partial).unwrap().is_empty());
        std::fs::remove_file(partial).unwrap();
    }

    #[tokio::test]
    async fn verified_cancellation_removes_the_attempt_partial() {
        use crate::{StateStore, TorbenPaths, operation::OperationJournal};
        use std::sync::Arc;
        use torben_contracts::{AppId, OperationKind};
        let root = tempfile::tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &AppId::new("node").unwrap(),
            None,
        )
        .unwrap();
        let cancellation = journal.cancellation_probe();
        let destination = root.path().join("runtime.zip");
        let partial = destination.with_extension("partial");
        let official = Url::parse("http://127.0.0.1:1/runtime.zip").unwrap();
        let result = verified(
            &official,
            &destination,
            "unused",
            Some(&cancellation),
            |_| async {
                let _file = open_partial(&partial, false).unwrap();
                OperationJournal::request_cancellation(&paths, &store, journal.operation_id())
                    .unwrap();
                std::future::pending::<TorbenResult<()>>().await
            },
        )
        .await;
        assert_eq!(result.unwrap_err().code, "operation_cancelled");
        assert!(!partial.exists());
    }

    #[tokio::test]
    async fn corrupt_archive_retries_and_discards_partial_bytes_between_sources() {
        use sha2::Digest;
        let root = tempfile::tempdir().unwrap();
        let destination = root.path().join("runtime.zip");
        let partial = destination.with_extension("partial");
        let official = Url::parse("http://127.0.0.1:1/runtime.zip").unwrap();
        let checksum = hex::encode(sha2::Sha256::digest(b"valid archive"));
        let mut attempts = 0;
        verified(&official, &destination, &checksum, None, |_| {
            attempts += 1;
            let attempt = attempts;
            let destination = &destination;
            let partial = &partial;
            async move {
                if attempt == 1 {
                    std::fs::write(partial, b"untrusted partial").unwrap();
                    std::fs::write(destination, b"corrupt archive").unwrap();
                } else {
                    assert!(!partial.exists());
                    std::fs::write(destination, b"valid archive").unwrap();
                }
                Ok(())
            }
        })
        .await
        .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!(std::fs::read(destination).unwrap(), b"valid archive");
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_pending_source_without_trying_another() {
        use crate::{StateStore, TorbenPaths, operation::OperationJournal};
        use std::sync::Arc;
        use torben_contracts::{AppId, OperationKind};
        let root = tempfile::tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &AppId::new("node").unwrap(),
            None,
        )
        .unwrap();
        let cancellation = journal.cancellation_probe();
        let urls = vec![
            Url::parse("http://127.0.0.1:1/first").unwrap(),
            Url::parse("http://127.0.0.1:1/second").unwrap(),
        ];
        let mut attempts = 0;
        let future = try_sources(urls, Some(&cancellation), |_| {
            attempts += 1;
            std::future::pending::<TorbenResult<()>>()
        });
        let cancel = async {
            tokio::time::sleep(Duration::from_millis(20)).await;
            OperationJournal::request_cancellation(&paths, &store, journal.operation_id()).unwrap();
        };
        let (result, ()) = tokio::join!(future, cancel);
        assert_eq!(result.unwrap_err().code, "operation_cancelled");
        assert_eq!(attempts, 1);
    }

    #[test]
    fn github_api_route_requires_the_exact_pinned_asset() {
        let official = Url::parse("https://github.com/redis-windows/redis-windows/releases/download/8.8.0/Redis-8.8.0-Windows-x64-msys2.zip").unwrap();
        let bytes = serde_json::to_vec(
            &serde_json::json!({"assets": [{"id": 123, "browser_download_url": official}]}),
        )
        .unwrap();
        assert_eq!(
            github_asset_from_json(&bytes, &official, "redis-windows", "redis-windows")
                .unwrap()
                .as_str(),
            "https://api.github.com/repos/redis-windows/redis-windows/releases/assets/123"
        );
        let bytes = serde_json::to_vec(&serde_json::json!({"assets": [{"id": 123, "browser_download_url": "https://github.com/another/repo/releases/download/8.8.0/Redis.zip"}]})).unwrap();
        assert!(
            github_asset_from_json(&bytes, &official, "redis-windows", "redis-windows").is_err()
        );
    }

    #[test]
    fn slow_transfer_triggers_failover_without_rejecting_small_fast_files() {
        let mut transfer = TransferRate::new();
        let start = transfer.started;
        transfer
            .record_at(10, start + Duration::from_secs(1), "network_error")
            .unwrap();
        let error = transfer
            .record_at(100, start + Duration::from_secs(15), "network_error")
            .unwrap_err();
        assert_eq!(
            error.details.get("reason").map(String::as_str),
            Some("download_throughput_too_low")
        );
        let mut transfer = TransferRate::new();
        transfer
            .record_at(
                1024 * 1024,
                transfer.started + Duration::from_secs(15),
                "network_error",
            )
            .unwrap();
    }

    #[test]
    fn region_detection_handles_windows_and_explicit_overrides() {
        for locale in ["CN", "china", "zh-CN", "zh_CN.UTF-8", "zh-CN:en"] {
            assert!(chinese_locale(locale));
        }
        for locale in ["global", "official", "en-US", "zh-TW", "en_CN", "notchina"] {
            assert!(!chinese_locale(locale));
        }
    }

    #[test]
    fn maps_archives_without_mirroring_rust_metadata_or_other_sources() {
        for (official, expected) in [
            (
                "https://nodejs.org/dist/v24.19.0/node-v24.19.0-win-x64.zip",
                "https://mirrors.tuna.tsinghua.edu.cn/nodejs-release/v24.19.0/node-v24.19.0-win-x64.zip",
            ),
            (
                "https://www.python.org/ftp/python/3.14.7/python-3.14.7-amd64.zip",
                "https://mirrors.huaweicloud.com/python/3.14.7/python-3.14.7-amd64.zip",
            ),
            (
                "https://static.rust-lang.org/dist/rust-1.98.0-x86_64-pc-windows-msvc.msi",
                "https://mirrors.tuna.tsinghua.edu.cn/rustup/dist/rust-1.98.0-x86_64-pc-windows-msvc.msi",
            ),
            (
                "https://cdn.mysql.com/Downloads/MySQL-8.4/mysql-8.4.11-winx64.zip",
                "https://mirrors.huaweicloud.com/mysql/Downloads/MySQL-8.4/mysql-8.4.11-winx64.zip",
            ),
            (
                "https://github.com/adoptium/temurin21-binaries/releases/download/jdk-21.0.12%2B7/OpenJDK21U-jdk_x64_windows_hotspot_21.0.12_7.zip",
                "https://mirrors.tuna.tsinghua.edu.cn/Adoptium/21/jdk/x64/windows/OpenJDK21U-jdk_x64_windows_hotspot_21.0.12_7.zip",
            ),
        ] {
            let urls = mirrors(&Url::parse(official).unwrap());
            assert_eq!(urls.len(), 2);
            assert_eq!(urls[0].as_str(), expected);
        }
        for official in [
            "https://nodejs.org/dist/index.json",
            "https://static.rust-lang.org/dist/channel-rust-1.98.0.toml",
            "https://www.python.org/api/v2/downloads/release/",
            "https://github.com/redis-windows/redis-windows/releases/download/8.8.0/Redis-8.8.0-Windows-x64-msys2.zip",
            "https://nodejs.org:8443/dist/index.json",
            "https://nodejs.org/dist/index.json?redirect=1",
            "http://127.0.0.1:1234/dist/index.json",
        ] {
            assert!(mirrors(&Url::parse(official).unwrap()).is_empty());
        }
    }

    #[tokio::test]
    async fn failover_keeps_the_last_error_and_does_not_retry_filesystem_errors() {
        let urls = vec![
            Url::parse("http://127.0.0.1:1/first").unwrap(),
            Url::parse("http://127.0.0.1:1/second").unwrap(),
        ];
        let value = try_sources(urls.clone(), None, |url| async move {
            if url.path() == "/first" {
                Err(TorbenError::new("network_error", "fixture"))
            } else {
                Ok("fallback")
            }
        })
        .await
        .unwrap();
        assert_eq!(value, "fallback");
        let error = try_sources(urls, None, |_| async {
            Err::<(), _>(TorbenError::new("node_io_failed", "fixture"))
        })
        .await
        .unwrap_err();
        assert_eq!(error.code, "node_io_failed");
    }
}
