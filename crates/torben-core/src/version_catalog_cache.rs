use std::{
    collections::BTreeSet,
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use torben_contracts::{AppId, TorbenError, TorbenResult, VersionDescriptor};

const CACHE_SCHEMA_VERSION: u32 = 1;
const MAX_CACHE_BYTES: u64 = 256 * 1024;
const MAX_CACHED_VERSIONS: usize = 128;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CachedVersionCatalog {
    schema_version: u32,
    app_id: AppId,
    refreshed_at: u64,
    versions: Vec<VersionDescriptor>,
}

pub(crate) fn load(
    path: &Path,
    expected_app_id: &AppId,
) -> TorbenResult<Option<Vec<VersionDescriptor>>> {
    Ok(read(path, expected_app_id)?.map(|catalog| catalog.versions))
}

pub(crate) fn refresh_due(path: &Path, expected_app_id: &AppId, interval: Duration) -> bool {
    let Ok(Some(catalog)) = read(path, expected_app_id) else {
        return true;
    };
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return true;
    };
    let age = now.as_secs().checked_sub(catalog.refreshed_at);
    age.is_none_or(|seconds| seconds >= interval.as_secs())
}

pub(crate) fn save(
    path: &Path,
    app_id: &AppId,
    versions: &[VersionDescriptor],
) -> TorbenResult<()> {
    validate_versions(versions)?;
    let refreshed_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| cache_error("Could not read the system clock.", error))?
        .as_secs();
    let catalog = CachedVersionCatalog {
        schema_version: CACHE_SCHEMA_VERSION,
        app_id: app_id.clone(),
        refreshed_at,
        versions: versions.to_vec(),
    };
    let bytes = serde_json::to_vec(&catalog).map_err(|error| {
        TorbenError::internal("Could not serialize a version catalog cache.")
            .with_detail("reason", error.to_string())
    })?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CACHE_BYTES {
        return Err(TorbenError::new(
            "version_catalog_cache_too_large",
            "The version catalog cache is too large.",
        ));
    }
    commit(path, &bytes)
}

fn read(path: &Path, expected_app_id: &AppId) -> TorbenResult<Option<CachedVersionCatalog>> {
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(cache_error(
                "Could not inspect a version catalog cache.",
                error,
            ));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > MAX_CACHE_BYTES
    {
        return Err(TorbenError::new(
            "version_catalog_cache_invalid",
            "The version catalog cache is not a valid regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    let bytes = std::fs::read(path)
        .map_err(|error| cache_error("Could not read a version catalog cache.", error))?;
    let catalog: CachedVersionCatalog = serde_json::from_slice(&bytes).map_err(|error| {
        TorbenError::new(
            "version_catalog_cache_invalid",
            "The version catalog cache contains invalid data.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    if catalog.schema_version != CACHE_SCHEMA_VERSION || catalog.app_id != *expected_app_id {
        return Err(TorbenError::new(
            "version_catalog_cache_invalid",
            "The version catalog cache identity does not match the requested application.",
        )
        .with_detail("path", path.display().to_string()));
    }
    validate_versions(&catalog.versions)?;
    Ok(Some(catalog))
}

fn validate_versions(versions: &[VersionDescriptor]) -> TorbenResult<()> {
    if versions.is_empty() || versions.len() > MAX_CACHED_VERSIONS {
        return Err(TorbenError::new(
            "version_catalog_cache_invalid",
            "The version catalog cache contains an invalid number of versions.",
        ));
    }
    let mut unique = BTreeSet::new();
    if versions.iter().any(|version| {
        version.released_at.trim().is_empty() || !unique.insert(version.version.clone())
    }) {
        return Err(TorbenError::new(
            "version_catalog_cache_invalid",
            "The version catalog cache contains duplicate or incomplete versions.",
        ));
    }
    Ok(())
}

fn commit(destination: &Path, bytes: &[u8]) -> TorbenResult<()> {
    let parent = destination.parent().ok_or_else(|| {
        TorbenError::new(
            "version_catalog_cache_path_invalid",
            "The version catalog cache has no parent directory.",
        )
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        cache_error(
            "Could not create the version catalog cache directory.",
            error,
        )
    })?;
    let metadata = parent.symlink_metadata().map_err(|error| {
        cache_error(
            "Could not inspect the version catalog cache directory.",
            error,
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TorbenError::new(
            "version_catalog_cache_path_invalid",
            "The version catalog cache directory is not a regular directory.",
        ));
    }
    let next = sibling_path(destination, "next")?;
    let previous = sibling_path(destination, "previous")?;
    remove_regular_file(&next)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&next)
        .map_err(|error| cache_error("Could not stage a version catalog cache.", error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| cache_error("Could not persist a version catalog cache.", error))?;
    drop(file);

    let had_destination = destination.exists();
    if had_destination {
        remove_regular_file(&previous)?;
        std::fs::rename(destination, &previous).map_err(|error| {
            cache_error("Could not stage the previous version catalog cache.", error)
        })?;
    }
    if let Err(error) = std::fs::rename(&next, destination) {
        if had_destination {
            let _ = std::fs::rename(&previous, destination);
        }
        let _ = std::fs::remove_file(&next);
        return Err(cache_error(
            "Could not commit the version catalog cache.",
            error,
        ));
    }
    if had_destination {
        let _ = remove_regular_file(&previous);
    }
    Ok(())
}

fn sibling_path(path: &Path, suffix: &str) -> TorbenResult<PathBuf> {
    let name = path.file_name().ok_or_else(|| {
        TorbenError::new(
            "version_catalog_cache_path_invalid",
            "The version catalog cache has no file name.",
        )
    })?;
    let mut sibling = name.to_os_string();
    sibling.push(format!(".{suffix}"));
    Ok(path.with_file_name(sibling))
}

fn remove_regular_file(path: &Path) -> TorbenResult<()> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            Err(TorbenError::new(
                "version_catalog_cache_invalid",
                "A version catalog cache transaction path is not a regular file.",
            )
            .with_detail("path", path.display().to_string()))
        }
        Ok(_) => std::fs::remove_file(path)
            .map_err(|error| cache_error("Could not remove a version catalog cache file.", error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(cache_error(
            "Could not inspect a version catalog cache file.",
            error,
        )),
    }
}

fn cache_error(message: &str, error: impl std::fmt::Display) -> TorbenError {
    TorbenError::new("version_catalog_cache_io_failed", message)
        .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, time::Duration};

    use tempfile::tempdir;
    use torben_contracts::{AppId, ExactVersion, VersionDescriptor};

    use super::{load, refresh_due, save};

    fn version(value: &str) -> VersionDescriptor {
        VersionDescriptor {
            version: ExactVersion::from_str(value).unwrap(),
            lts_name: Some(format!("Java {} LTS", value.split('.').next().unwrap())),
            released_at: "2026-08-19T00:00:00Z".to_owned(),
            recommended: false,
        }
    }

    #[test]
    fn cache_round_trips_and_is_fresh_for_one_day() {
        let root = tempdir().unwrap();
        let path = root.path().join("versions/temurin.json");
        let app_id = AppId::new("temurin").unwrap();
        let versions = vec![version("25.0.4+101.0.LTS"), version("21.0.12+101.0.LTS")];

        assert!(refresh_due(&path, &app_id, Duration::from_hours(24)));
        save(&path, &app_id, &versions).unwrap();
        assert_eq!(load(&path, &app_id).unwrap().unwrap(), versions);
        assert!(!refresh_due(&path, &app_id, Duration::from_hours(24)));
    }

    #[test]
    fn corrupt_cache_is_rejected_and_marked_due() {
        let root = tempdir().unwrap();
        let path = root.path().join("temurin.json");
        std::fs::write(&path, b"not json").unwrap();
        let app_id = AppId::new("temurin").unwrap();

        assert!(load(&path, &app_id).is_err());
        assert!(refresh_due(&path, &app_id, Duration::from_hours(24)));
    }
}
