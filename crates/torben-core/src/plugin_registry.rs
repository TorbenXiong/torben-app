use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;
use torben_contracts::{
    OperationId, TorbenError, TorbenResult,
    plugin::{PluginRegistry, PluginRegistryStatus},
};
use torben_plugin_host::{RegistryPluginSelection, RegistryVerifier, VerifiedRegistryPlugin};
use url::Url;

use crate::operation::CancellationProbe;

const MAX_REGISTRY_BYTES: u64 = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;
const MAX_PLUGIN_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;

pub(crate) fn official_url(value: &str) -> TorbenResult<Url> {
    let url = Url::parse(value).map_err(|error| {
        TorbenError::new(
            "official_registry_url_invalid",
            "The built-in official plugin registry URL is invalid.",
        )
        .with_detail("reason", error.to_string())
    })?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(TorbenError::new(
            "official_registry_url_invalid",
            "The official plugin registry must use an HTTPS URL without credentials, a query, or a fragment.",
        ));
    }
    Ok(url)
}

pub(crate) fn official_client() -> TorbenResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(format!("Torben-App/{}", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(network_error)
}

#[cfg(test)]
pub(crate) fn fixture_url(value: &str) -> TorbenResult<Url> {
    let url = Url::parse(value)
        .map_err(|error| TorbenError::new("fixture_registry_url_invalid", error.to_string()))?;
    if url.scheme() != "http" || url.host_str().is_none_or(|host| host != "127.0.0.1") {
        return Err(TorbenError::new(
            "fixture_registry_url_invalid",
            "A fixture registry must use loopback HTTP.",
        ));
    }
    Ok(url)
}

#[cfg(test)]
pub(crate) fn fixture_client() -> TorbenResult<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(network_error)
}

pub(crate) async fn refresh<F>(
    client: &reqwest::Client,
    source_url: &Url,
    cache_path: &Path,
    verify: F,
) -> TorbenResult<PluginRegistry>
where
    F: Fn(&[u8]) -> TorbenResult<PluginRegistry>,
{
    let bytes = fetch_limited(client, source_url).await?;
    accept_snapshot(cache_path, &bytes, verify)
}

pub(crate) async fn download_package(
    client: &reqwest::Client,
    registry_url: &Url,
    registry_cache_path: &Path,
    selection: &RegistryPluginSelection,
    verifier: &RegistryVerifier,
    operation_id: OperationId,
    cancellation: &CancellationProbe,
) -> TorbenResult<VerifiedRegistryPlugin> {
    download_package_files(
        client,
        registry_url,
        registry_cache_path,
        selection,
        operation_id,
        cancellation,
        |bytes| verifier.verify_manifest_bytes(selection, bytes),
        |manifest_path| {
            verifier
                .verify_selected_package(selection, manifest_path)
                .map(|_| ())
        },
    )
    .await?;
    verifier.verify(
        registry_cache_path,
        &selection.entry.plugin_id,
        Some(&selection.entry.version),
    )
}

#[allow(clippy::too_many_arguments)]
async fn download_package_files<VerifyManifest, VerifyStaged>(
    client: &reqwest::Client,
    registry_url: &Url,
    registry_cache_path: &Path,
    selection: &RegistryPluginSelection,
    operation_id: OperationId,
    cancellation: &CancellationProbe,
    verify_manifest: VerifyManifest,
    verify_staged: VerifyStaged,
) -> TorbenResult<PathBuf>
where
    VerifyManifest: Fn(&[u8]) -> TorbenResult<torben_contracts::plugin::PluginManifest>,
    VerifyStaged: Fn(&Path) -> TorbenResult<()>,
{
    let registry_root = registry_cache_path.parent().ok_or_else(|| {
        TorbenError::new(
            "plugin_registry_cache_path_invalid",
            "The official plugin registry cache has no parent directory.",
        )
    })?;
    let manifest_relative = Path::new(&selection.entry.manifest_path);
    let package_relative = manifest_relative
        .parent()
        .filter(|path| !path.as_os_str().is_empty() && path != &Path::new("."));
    let package_relative = package_relative.ok_or_else(|| {
        TorbenError::new(
            "plugin_registry_package_layout_invalid",
            "An official plugin manifest must be inside a version-specific package directory.",
        )
    })?;
    let manifest_name = manifest_relative.file_name().ok_or_else(|| {
        TorbenError::new(
            "plugin_registry_package_layout_invalid",
            "The official plugin manifest path has no file name.",
        )
    })?;
    let manifest_url = join_same_origin(registry_url, &selection.entry.manifest_path)?;
    cancellation.check()?;
    let manifest_bytes = fetch_bytes_limited(
        client,
        registry_url,
        &manifest_url,
        MAX_MANIFEST_BYTES,
        "plugin_manifest_too_large",
        "The official plugin manifest exceeds the maximum allowed size.",
        cancellation,
    )
    .await?;
    let manifest = verify_manifest(&manifest_bytes)?;
    let target_name = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let target = manifest
        .targets
        .iter()
        .find(|target| target.target == target_name)
        .ok_or_else(|| {
            TorbenError::new(
                "plugin_target_missing",
                "The plugin does not support this platform target.",
            )
        })?;
    let executable_url = join_same_origin(&manifest_url, &target.executable)?;
    ensure_relative_directory_tree(
        registry_root,
        package_relative.parent().unwrap_or_else(|| Path::new("")),
    )?;
    let final_package = registry_root.join(package_relative);
    let staging = registry_root.join(format!(".package-{operation_id}"));
    if staging.exists() {
        remove_regular_directory(&staging)?;
    }
    std::fs::create_dir_all(&staging).map_err(cache_io_error)?;
    let result = async {
        cancellation.check()?;
        let staged_manifest = staging.join(manifest_name);
        if let Some(parent) = staged_manifest.parent() {
            std::fs::create_dir_all(parent).map_err(cache_io_error)?;
        }
        write_synced_file(&staged_manifest, &manifest_bytes)?;
        let staged_executable = staging.join(&target.executable);
        if let Some(parent) = staged_executable.parent() {
            std::fs::create_dir_all(parent).map_err(cache_io_error)?;
        }
        download_file_limited(
            client,
            registry_url,
            &executable_url,
            &staged_executable,
            MAX_PLUGIN_EXECUTABLE_BYTES,
            cancellation,
        )
        .await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = std::fs::metadata(&staged_executable)
                .map_err(cache_io_error)?
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&staged_executable, permissions).map_err(cache_io_error)?;
        }
        cancellation.check()?;
        verify_staged(&staged_manifest)?;
        commit_package_directory(&staging, &final_package)?;
        cancellation.check()?;
        Ok(final_package.join(manifest_name))
    }
    .await;
    if result.is_err() && staging.exists() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

fn join_same_origin(base: &Url, relative: &str) -> TorbenResult<Url> {
    let joined = base.join(relative).map_err(|error| {
        TorbenError::new(
            "plugin_registry_asset_url_invalid",
            "An official plugin registry asset URL is invalid.",
        )
        .with_detail("reason", error.to_string())
    })?;
    if joined.scheme() != base.scheme()
        || joined.host_str() != base.host_str()
        || joined.port_or_known_default() != base.port_or_known_default()
    {
        return Err(TorbenError::new(
            "plugin_registry_asset_origin_changed",
            "An official plugin registry asset changed network origin.",
        ));
    }
    Ok(joined)
}

async fn fetch_bytes_limited(
    client: &reqwest::Client,
    configured_url: &Url,
    url: &Url,
    maximum: u64,
    too_large_code: &str,
    too_large_message: &str,
    cancellation: &CancellationProbe,
) -> TorbenResult<Vec<u8>> {
    cancellation.check()?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(network_error)?;
    validate_asset_response(&response, configured_url)?;
    if response
        .content_length()
        .is_some_and(|length| length > maximum)
    {
        return Err(resource_too_large(
            too_large_code,
            too_large_message,
            maximum,
        ));
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await.transpose().map_err(network_error)? {
        cancellation.check()?;
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(|| resource_too_large(too_large_code, too_large_message, maximum))?;
        if u64::try_from(next_len).unwrap_or(u64::MAX) > maximum {
            return Err(resource_too_large(
                too_large_code,
                too_large_message,
                maximum,
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn download_file_limited(
    client: &reqwest::Client,
    configured_url: &Url,
    url: &Url,
    destination: &Path,
    maximum: u64,
    cancellation: &CancellationProbe,
) -> TorbenResult<()> {
    cancellation.check()?;
    let response = client
        .get(url.clone())
        .send()
        .await
        .map_err(network_error)?;
    validate_asset_response(&response, configured_url)?;
    if response
        .content_length()
        .is_some_and(|length| length > maximum)
    {
        return Err(resource_too_large(
            "plugin_executable_too_large",
            "The official plugin executable exceeds the maximum allowed size.",
            maximum,
        ));
    }
    let partial = sibling_path(destination, "partial")?;
    remove_regular_cache_file(&partial)?;
    let mut file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&partial)
        .await
        .map_err(cache_io_error)?;
    let mut received = 0_u64;
    let mut stream = response.bytes_stream();
    let result = async {
        while let Some(chunk) = stream.next().await.transpose().map_err(network_error)? {
            cancellation.check()?;
            received = received
                .checked_add(u64::try_from(chunk.len()).unwrap_or(u64::MAX))
                .ok_or_else(|| {
                    resource_too_large(
                        "plugin_executable_too_large",
                        "The official plugin executable exceeds the maximum allowed size.",
                        maximum,
                    )
                })?;
            if received > maximum {
                return Err(resource_too_large(
                    "plugin_executable_too_large",
                    "The official plugin executable exceeds the maximum allowed size.",
                    maximum,
                ));
            }
            file.write_all(&chunk).await.map_err(cache_io_error)?;
        }
        file.flush().await.map_err(cache_io_error)?;
        file.sync_all().await.map_err(cache_io_error)?;
        drop(file);
        std::fs::rename(&partial, destination).map_err(cache_io_error)
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&partial).await;
    }
    result
}

fn validate_asset_response(response: &reqwest::Response, configured_url: &Url) -> TorbenResult<()> {
    if response.url().scheme() != configured_url.scheme()
        || response.url().host_str() != configured_url.host_str()
        || response.url().port_or_known_default() != configured_url.port_or_known_default()
    {
        return Err(TorbenError::new(
            "plugin_registry_asset_origin_changed",
            "An official plugin registry asset request changed network origin.",
        ));
    }
    if !response.status().is_success() {
        return Err(TorbenError::new(
            "plugin_registry_asset_http_error",
            "An official plugin registry asset request returned an unsuccessful status.",
        )
        .with_detail("status", response.status().to_string()));
    }
    Ok(())
}

fn write_synced_file(path: &Path, bytes: &[u8]) -> TorbenResult<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(cache_io_error)?;
    file.write_all(bytes).map_err(cache_io_error)?;
    file.sync_all().map_err(cache_io_error)
}

fn commit_package_directory(staging: &Path, destination: &Path) -> TorbenResult<()> {
    let parent = destination.parent().ok_or_else(|| {
        TorbenError::new(
            "plugin_registry_package_layout_invalid",
            "The official plugin package cache has no parent directory.",
        )
    })?;
    std::fs::create_dir_all(parent).map_err(cache_io_error)?;
    let previous = sibling_path(destination, "previous")?;
    remove_regular_directory_if_exists(&previous)?;
    let had_destination = destination.exists();
    if had_destination {
        let metadata = destination.symlink_metadata().map_err(cache_io_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(TorbenError::new(
                "plugin_registry_cache_invalid",
                "The official plugin package cache is not a regular directory.",
            ));
        }
        std::fs::rename(destination, &previous).map_err(cache_io_error)?;
    }
    if let Err(error) = std::fs::rename(staging, destination) {
        if had_destination {
            let _ = std::fs::rename(&previous, destination);
        }
        return Err(cache_io_error(error));
    }
    if had_destination {
        let _ = std::fs::remove_dir_all(previous);
    }
    Ok(())
}

fn ensure_relative_directory_tree(root: &Path, relative: &Path) -> TorbenResult<()> {
    std::fs::create_dir_all(root).map_err(cache_io_error)?;
    let root_metadata = root.symlink_metadata().map_err(cache_io_error)?;
    if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
        return Err(TorbenError::new(
            "plugin_registry_cache_invalid",
            "The official plugin registry cache root is not a regular directory.",
        ));
    }
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err(TorbenError::new(
                "plugin_registry_package_layout_invalid",
                "The official plugin package path is unsafe.",
            ));
        };
        current.push(component);
        match current.symlink_metadata() {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                return Err(TorbenError::new(
                    "plugin_registry_cache_invalid",
                    "An official plugin package cache parent is not a regular directory.",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&current).map_err(cache_io_error)?;
            }
            Err(error) => return Err(cache_io_error(error)),
        }
    }
    Ok(())
}

fn remove_regular_directory_if_exists(path: &Path) -> TorbenResult<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(TorbenError::new(
                "plugin_registry_cache_invalid",
                "A plugin package cache transaction path is not a regular directory.",
            ))
        }
        Ok(_) => std::fs::remove_dir_all(path).map_err(cache_io_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(cache_io_error(error)),
    }
}

fn remove_regular_directory(path: &Path) -> TorbenResult<()> {
    remove_regular_directory_if_exists(path)
}

pub(crate) fn load<F>(cache_path: &Path, verify: F) -> TorbenResult<Option<PluginRegistry>>
where
    F: Fn(&[u8]) -> TorbenResult<PluginRegistry>,
{
    read_cached(cache_path, &verify)
}

fn accept_snapshot<F>(cache_path: &Path, bytes: &[u8], verify: F) -> TorbenResult<PluginRegistry>
where
    F: Fn(&[u8]) -> TorbenResult<PluginRegistry>,
{
    let incoming = verify(bytes)?;
    if let Some(cached) = read_cached(cache_path, &verify)? {
        if incoming.sequence < cached.sequence {
            return Err(TorbenError::new(
                "plugin_registry_rollback_detected",
                "The downloaded plugin registry is older than the trusted cached snapshot.",
            )
            .with_detail("cachedSequence", cached.sequence.to_string())
            .with_detail("incomingSequence", incoming.sequence.to_string()));
        }
        if incoming.sequence == cached.sequence {
            if incoming != cached {
                return Err(TorbenError::new(
                    "plugin_registry_sequence_conflict",
                    "The downloaded plugin registry changed without advancing its sequence.",
                )
                .with_detail("sequence", incoming.sequence.to_string()));
            }
            return Ok(cached);
        }
    }
    commit_cache(cache_path, bytes)?;
    Ok(incoming)
}

fn read_cached<F>(cache_path: &Path, verify: &F) -> TorbenResult<Option<PluginRegistry>>
where
    F: Fn(&[u8]) -> TorbenResult<PluginRegistry>,
{
    let metadata = match cache_path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(cache_io_error(error)),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TorbenError::new(
            "plugin_registry_cache_invalid",
            "The official plugin registry cache is not a regular file.",
        )
        .with_detail("path", cache_path.display().to_string()));
    }
    let bytes = std::fs::read(cache_path).map_err(cache_io_error)?;
    verify(&bytes).map(Some).map_err(|error| {
        TorbenError::new(
            "plugin_registry_cache_invalid",
            "The cached official plugin registry failed trust verification.",
        )
        .with_detail("reasonCode", error.code)
        .with_detail("reason", error.message)
        .with_remediation(
            "Inspect the cache for local corruption before removing it and refreshing again.",
        )
    })
}

async fn fetch_limited(client: &reqwest::Client, source_url: &Url) -> TorbenResult<Vec<u8>> {
    let response = client
        .get(source_url.clone())
        .send()
        .await
        .map_err(network_error)?;
    if response.url().scheme() != source_url.scheme()
        || response.url().host_str() != source_url.host_str()
        || response.url().port_or_known_default() != source_url.port_or_known_default()
    {
        return Err(TorbenError::new(
            "plugin_registry_origin_changed",
            "The official plugin registry request changed network origin.",
        )
        .with_detail("url", response.url().to_string()));
    }
    if !response.status().is_success() {
        return Err(TorbenError::new(
            "plugin_registry_http_error",
            "The official plugin registry request returned an unsuccessful status.",
        )
        .with_detail("status", response.status().to_string()));
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_REGISTRY_BYTES)
    {
        return Err(registry_too_large());
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await.transpose().map_err(network_error)? {
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or_else(registry_too_large)?;
        if u64::try_from(next_len).unwrap_or(u64::MAX) > MAX_REGISTRY_BYTES {
            return Err(registry_too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn commit_cache(destination: &Path, bytes: &[u8]) -> TorbenResult<()> {
    let parent = destination.parent().ok_or_else(|| {
        TorbenError::new(
            "plugin_registry_cache_path_invalid",
            "The official plugin registry cache has no parent directory.",
        )
    })?;
    std::fs::create_dir_all(parent).map_err(cache_io_error)?;
    let parent_metadata = parent.symlink_metadata().map_err(cache_io_error)?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(TorbenError::new(
            "plugin_registry_cache_invalid",
            "The official plugin registry cache directory is not a regular directory.",
        ));
    }
    let next = sibling_path(destination, "next")?;
    let previous = sibling_path(destination, "previous")?;
    remove_regular_cache_file(&next)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&next)
        .map_err(cache_io_error)?;
    file.write_all(bytes).map_err(cache_io_error)?;
    file.sync_all().map_err(cache_io_error)?;
    drop(file);

    let had_destination = destination.exists();
    if had_destination {
        remove_regular_cache_file(&previous)?;
        std::fs::rename(destination, &previous).map_err(cache_io_error)?;
    }
    if let Err(error) = std::fs::rename(&next, destination) {
        if had_destination {
            let _ = std::fs::rename(&previous, destination);
        }
        let _ = std::fs::remove_file(&next);
        return Err(cache_io_error(error));
    }
    if had_destination {
        let _ = remove_regular_cache_file(&previous);
    }
    Ok(())
}

fn sibling_path(path: &Path, suffix: &str) -> TorbenResult<PathBuf> {
    let file_name = path.file_name().ok_or_else(|| {
        TorbenError::new(
            "plugin_registry_cache_path_invalid",
            "The official plugin registry cache has no file name.",
        )
    })?;
    let mut name = file_name.to_os_string();
    name.push(format!(".{suffix}"));
    Ok(path.with_file_name(name))
}

fn remove_regular_cache_file(path: &Path) -> TorbenResult<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(TorbenError::new(
                "plugin_registry_cache_invalid",
                "A registry cache transaction path is not a regular file.",
            )
            .with_detail("path", path.display().to_string()))
        }
        Ok(_) => std::fs::remove_file(path).map_err(cache_io_error),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(cache_io_error(error)),
    }
}

pub(crate) fn status(
    configured: bool,
    source_url: Option<&str>,
    cache_path: &Path,
    registry: Option<&PluginRegistry>,
) -> PluginRegistryStatus {
    PluginRegistryStatus {
        configured,
        source_url: source_url.map(str::to_owned),
        cache_path: cache_path.display().to_string(),
        sequence: registry.map(|registry| registry.sequence),
        generated_at: registry.map(|registry| registry.generated_at.clone()),
    }
}

fn registry_too_large() -> TorbenError {
    TorbenError::new(
        "plugin_registry_too_large",
        "The official plugin registry exceeds the maximum allowed size.",
    )
    .with_detail("maximumBytes", MAX_REGISTRY_BYTES.to_string())
}

fn resource_too_large(code: &str, message: &str, maximum: u64) -> TorbenError {
    TorbenError::new(code, message).with_detail("maximumBytes", maximum.to_string())
}

fn network_error(error: reqwest::Error) -> TorbenError {
    TorbenError::new(
        "plugin_registry_network_error",
        "The official plugin registry request failed.",
    )
    .with_detail("reason", error.to_string())
}

pub(crate) fn cache_io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "plugin_registry_cache_io_failed",
        "The official plugin registry cache operation failed.",
    )
    .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        io::{Read, Write},
        net::TcpListener,
        str::FromStr,
        thread,
    };

    use sha2::Digest;
    use tempfile::tempdir;
    use torben_contracts::{
        ExactVersion, PluginId,
        plugin::{
            PLUGIN_PROTOCOL_VERSION, PLUGIN_REGISTRY_SCHEMA_VERSION, PluginManifest,
            PluginPermissions, PluginRegistryEntry, PluginRegistryPublisher, PluginTarget,
        },
    };
    use torben_plugin_host::RegistryPluginSelection;

    use crate::{StateStore, TorbenPaths, operation::OperationJournal};

    use super::*;

    #[test]
    fn official_source_requires_https_without_credentials() {
        assert!(official_url("https://plugins.example/registry.json").is_ok());
        assert_eq!(
            official_url("http://plugins.example/registry.json")
                .unwrap_err()
                .code,
            "official_registry_url_invalid"
        );
        assert!(official_url("https://user@plugins.example/registry.json").is_err());
    }

    #[test]
    fn newer_snapshot_commits_and_rollback_is_rejected() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("registry.json");
        let newer = registry(2, "newer");
        let older = registry(1, "older");
        let verify = |bytes: &[u8]| serde_json::from_slice(bytes).map_err(json_error);

        accept_snapshot(&cache, &serde_json::to_vec(&newer).unwrap(), verify).unwrap();
        let error =
            accept_snapshot(&cache, &serde_json::to_vec(&older).unwrap(), verify).unwrap_err();

        assert_eq!(error.code, "plugin_registry_rollback_detected");
        assert_eq!(
            serde_json::from_slice::<PluginRegistry>(&std::fs::read(cache).unwrap()).unwrap(),
            newer
        );
    }

    #[test]
    fn same_sequence_with_different_content_is_rejected() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("registry.json");
        let first = registry(3, "first");
        let conflicting = registry(3, "conflicting");
        let verify = |bytes: &[u8]| serde_json::from_slice(bytes).map_err(json_error);
        accept_snapshot(&cache, &serde_json::to_vec(&first).unwrap(), verify).unwrap();

        let error = accept_snapshot(&cache, &serde_json::to_vec(&conflicting).unwrap(), verify)
            .unwrap_err();

        assert_eq!(error.code, "plugin_registry_sequence_conflict");
    }

    #[tokio::test]
    async fn refresh_fetches_and_commits_a_bounded_snapshot() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("registry.json");
        let expected = registry(4, "network");
        let body = serde_json::to_vec(&expected).unwrap();
        let (url, server) = serve_once(body);
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();

        let actual = refresh(&client, &url, &cache, |bytes| {
            serde_json::from_slice(bytes).map_err(json_error)
        })
        .await
        .unwrap();
        server.join().unwrap();

        assert_eq!(actual, expected);
        assert!(cache.is_file());
    }

    #[tokio::test]
    async fn oversized_registry_is_rejected_before_cache_mutation() {
        let directory = tempdir().unwrap();
        let cache = directory.path().join("registry.json");
        let (url, server) = serve_declared_length(MAX_REGISTRY_BYTES + 1);
        let client = reqwest::Client::new();

        let error = refresh(&client, &url, &cache, |_| {
            unreachable!("oversized content must fail before verification")
        })
        .await
        .unwrap_err();
        server.join().unwrap();

        assert_eq!(error.code, "plugin_registry_too_large");
        assert!(!cache.exists());
    }

    #[tokio::test]
    async fn package_files_download_to_cache_before_the_install_transaction() {
        let directory = tempdir().unwrap();
        let paths = TorbenPaths::for_test(directory.path().join("workspace"));
        paths.ensure_layout().unwrap();
        let plugin_id = PluginId::new("app.example.fixture").unwrap();
        let version = ExactVersion::from_str("1.2.3").unwrap();
        let executable_path = if cfg!(windows) {
            "bin/plugin.exe"
        } else {
            "bin/plugin"
        };
        let executable_bytes = b"downloaded plugin fixture".to_vec();
        let manifest = PluginManifest {
            id: plugin_id.clone(),
            display_name: "Fixture".to_owned(),
            version: version.clone(),
            protocol_version: PLUGIN_PROTOCOL_VERSION,
            minimum_host_version: ExactVersion::from_str("0.1.0").unwrap(),
            publisher: "Fixture Publisher".to_owned(),
            capabilities: Vec::new(),
            permissions: PluginPermissions::default(),
            targets: vec![PluginTarget {
                target: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                executable: executable_path.to_owned(),
                sha256: hex::encode(sha2::Sha256::digest(&executable_bytes)),
            }],
            signature: None,
            revoked: false,
        };
        let manifest_bytes = serde_json::to_vec(&manifest).unwrap();
        let manifest_path = "packages/fixture/1.2.3/plugin.json";
        let selection = RegistryPluginSelection {
            entry: PluginRegistryEntry {
                plugin_id: plugin_id.clone(),
                version: version.clone(),
                publisher_id: "fixture.publisher".to_owned(),
                manifest_path: manifest_path.to_owned(),
                manifest_sha256: hex::encode(sha2::Sha256::digest(&manifest_bytes)),
                revoked: false,
            },
            publisher: PluginRegistryPublisher {
                id: "fixture.publisher".to_owned(),
                display_name: "Fixture Publisher".to_owned(),
                public_key: "fixture".to_owned(),
                revoked: false,
            },
        };
        let executable_route = format!("/packages/fixture/1.2.3/{executable_path}");
        let (registry_url, server) = serve_routes(BTreeMap::from([
            (format!("/{manifest_path}"), manifest_bytes.clone()),
            (executable_route, executable_bytes.clone()),
        ]));
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let store = std::sync::Arc::new(StateStore::open(paths.state_database()).unwrap());
        let journal = OperationJournal::start_plugin(&paths, store, &plugin_id, &version).unwrap();
        let cancellation = journal.cancellation_probe();
        let registry_cache = paths.official_plugin_registry_cache();

        let installed_manifest = download_package_files(
            &client,
            &registry_url,
            &registry_cache,
            &selection,
            journal.operation_id(),
            &cancellation,
            |bytes| serde_json::from_slice(bytes).map_err(json_error),
            |path| {
                let staged: PluginManifest =
                    serde_json::from_slice(&std::fs::read(path).map_err(cache_io_error)?)
                        .map_err(json_error)?;
                let executable = path.parent().unwrap().join(&staged.targets[0].executable);
                if std::fs::read(executable).map_err(cache_io_error)? != executable_bytes {
                    return Err(TorbenError::new(
                        "fixture_hash_mismatch",
                        "fixture mismatch",
                    ));
                }
                Ok(())
            },
        )
        .await
        .unwrap();
        server.join().unwrap();

        assert_eq!(
            installed_manifest,
            registry_cache
                .parent()
                .unwrap()
                .join("packages/fixture/1.2.3/plugin.json")
        );
        assert!(
            installed_manifest
                .parent()
                .unwrap()
                .join(executable_path)
                .is_file()
        );
    }

    #[tokio::test]
    async fn cancelled_package_download_stops_before_network_or_cache_mutation() {
        let directory = tempdir().unwrap();
        let paths = TorbenPaths::for_test(directory.path().join("workspace"));
        paths.ensure_layout().unwrap();
        let plugin_id = PluginId::new("app.example.cancelled").unwrap();
        let version = ExactVersion::from_str("1.0.0").unwrap();
        let store = std::sync::Arc::new(StateStore::open(paths.state_database()).unwrap());
        let journal = OperationJournal::start_plugin(
            &paths,
            std::sync::Arc::clone(&store),
            &plugin_id,
            &version,
        )
        .unwrap();
        OperationJournal::request_cancellation(&paths, &store, journal.operation_id()).unwrap();
        let cancellation = journal.cancellation_probe();
        let selection = RegistryPluginSelection {
            entry: PluginRegistryEntry {
                plugin_id,
                version,
                publisher_id: "fixture.publisher".to_owned(),
                manifest_path: "packages/cancelled/1.0.0/plugin.json".to_owned(),
                manifest_sha256: "00".repeat(32),
                revoked: false,
            },
            publisher: PluginRegistryPublisher {
                id: "fixture.publisher".to_owned(),
                display_name: "Fixture".to_owned(),
                public_key: "fixture".to_owned(),
                revoked: false,
            },
        };
        let client = reqwest::Client::new();
        let registry_url = Url::parse("http://127.0.0.1:9/registry.json").unwrap();
        let registry_cache = paths.official_plugin_registry_cache();

        let error = download_package_files(
            &client,
            &registry_url,
            &registry_cache,
            &selection,
            journal.operation_id(),
            &cancellation,
            |_| -> TorbenResult<PluginManifest> { unreachable!() },
            |_| Ok(()),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, "operation_cancelled");
        assert!(
            !registry_cache
                .parent()
                .unwrap()
                .join(format!(".package-{}", journal.operation_id()))
                .exists()
        );
    }

    fn registry(sequence: u64, generated_at: &str) -> PluginRegistry {
        PluginRegistry {
            schema_version: PLUGIN_REGISTRY_SCHEMA_VERSION,
            sequence,
            generated_at: generated_at.to_owned(),
            minimum_host_version: ExactVersion::from_str("0.1.0").unwrap(),
            publishers: Vec::new(),
            entries: Vec::new(),
            signature: Some("fixture".to_owned()),
        }
    }

    fn json_error(error: serde_json::Error) -> TorbenError {
        TorbenError::new("fixture_json_invalid", error.to_string())
    }

    fn serve_once(body: Vec<u8>) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .unwrap();
            stream.write_all(&body).unwrap();
        });
        (
            Url::parse(&format!("http://{address}/registry.json")).unwrap(),
            handle,
        )
    }

    fn serve_routes(mut routes: BTreeMap<String, Vec<u8>>) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            while !routes.is_empty() {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap();
                let body = routes.remove(path).unwrap();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });
        (
            Url::parse(&format!("http://{address}/registry.json")).unwrap(),
            handle,
        )
    }

    fn serve_declared_length(length: u64) -> (Url, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).unwrap();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
        });
        (
            Url::parse(&format!("http://{address}/registry.json")).unwrap(),
            handle,
        )
    }
}
