use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    str::FromStr,
    sync::Arc,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use torben_contracts::{
    ManagedLibraryMigrationResult, ManagedLibraryStatus, OperationId, OperationState, TorbenError,
    TorbenResult,
};
use walkdir::WalkDir;

use crate::{
    StateStore, TorbenPaths,
    operation::{MigrationSubject, OperationJournal},
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileEntry {
    relative: PathBuf,
    size: u64,
    sha256: [u8; 32],
}

const MIGRATION_RECEIPT_SCHEMA_VERSION: u32 = 1;
const MIGRATION_RECEIPT_MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct MigrationReceipt {
    schema_version: u32,
    operation_id: OperationId,
    source: PathBuf,
    target: PathBuf,
    staging: PathBuf,
    target_existed: bool,
}

pub(crate) fn status(paths: &TorbenPaths) -> TorbenResult<ManagedLibraryStatus> {
    let path = paths.app_library();
    let bytes_used = manifest(&path)?.iter().map(|entry| entry.size).sum();
    Ok(ManagedLibraryStatus {
        custom: path != paths.default_app_library(),
        path: path.display().to_string(),
        default_path: paths.default_app_library().display().to_string(),
        bytes_used,
    })
}

#[allow(clippy::too_many_lines)]
pub(crate) fn migrate(
    paths: &TorbenPaths,
    store: Arc<StateStore>,
    target: &Path,
) -> TorbenResult<ManagedLibraryMigrationResult> {
    validate_target(target)?;
    let configured_source = paths.app_library();
    let source = canonical_existing_directory(&configured_source)?;
    let target = normalized_target(target)?;
    if source == target || target.starts_with(&source) || source.starts_with(&target) {
        return Err(TorbenError::new(
            "managed_library_path_conflict",
            "The source and destination application libraries overlap.",
        ));
    }
    let target_existed = target.exists();
    if target_existed && target.read_dir().map_err(io_error)?.next().is_some() {
        return Err(TorbenError::new(
            "managed_library_target_not_empty",
            "The destination application library must be empty.",
        )
        .with_detail("path", target.display().to_string()));
    }
    let parent = target.parent().ok_or_else(|| invalid_target(&target))?;
    std::fs::create_dir_all(parent).map_err(io_error)?;
    let staging = parent.join(format!(".torben-library-staging-{}", OperationId::new()));
    let mut journal = OperationJournal::start_migration(
        paths,
        Arc::clone(&store),
        source.clone(),
        target.clone(),
        staging.clone(),
        target_existed,
    )?;
    let mut state_committed = false;
    let mut target_committed = false;
    let result = (|| {
        write_migration_receipt(paths, &journal)?;
        let expected = manifest(&source)?;
        let bytes: u64 = expected.iter().map(|entry| entry.size).sum();
        let available = fs4::available_space(parent).map_err(io_error)?;
        if available < bytes {
            return Err(TorbenError::new(
                "managed_library_space_insufficient",
                "The destination does not have enough available space.",
            )
            .with_detail("requiredBytes", bytes.to_string())
            .with_detail("availableBytes", available.to_string()));
        }
        journal.record(
            OperationState::Running,
            "copy",
            "Copying the managed application library",
            Some(0.2),
        )?;
        copy_tree(&source, &staging)?;
        journal.record(
            OperationState::Running,
            "verify",
            "Verifying the copied application library",
            Some(0.65),
        )?;
        if manifest(&staging)? != expected {
            return Err(TorbenError::new(
                "managed_library_verification_failed",
                "The copied application library does not match the source.",
            ));
        }
        if target_existed {
            std::fs::remove_dir(&target).map_err(io_error)?;
        }
        std::fs::rename(&staging, &target).map_err(io_error)?;
        target_committed = true;
        journal.set_migration_target_committed()?;
        journal.record(
            OperationState::Running,
            "commit",
            "Switching the active managed application library",
            Some(0.85),
        )?;
        store.commit_managed_library_migration(&configured_source, &target)?;
        state_committed = true;
        paths.set_app_library(target.clone());
        let source_cleanup_pending = std::fs::remove_dir_all(&source).is_err();
        if source_cleanup_pending {
            journal.set_migration_source_cleanup_pending(true)?;
            journal.succeed(
                "Managed application library migration committed; old-source cleanup is pending",
            )?;
        } else {
            journal.succeed("Managed application library migration committed")?;
            remove_migration_receipt(paths, journal.operation_id());
        }
        Ok(ManagedLibraryMigrationResult {
            previous_path: source.display().to_string(),
            current_path: target.display().to_string(),
            bytes_copied: bytes,
            source_cleanup_pending,
        })
    })();
    if state_committed && result.is_err() {
        return result;
    }
    if let Err(error) = &result {
        let staging_removed = !staging.exists() || std::fs::remove_dir_all(&staging).is_ok();
        let target_restored =
            rollback_uncommitted_target(&target, target_existed, target_committed);
        if !staging_removed || !target_restored {
            let pending = TorbenError::new(
                "managed_library_rollback_pending",
                "The managed-library migration failed and its filesystem cleanup is incomplete.",
            )
            .with_detail("causeCode", error.code.clone())
            .with_detail("stagingCleanupComplete", staging_removed.to_string())
            .with_detail("targetRestoreComplete", target_restored.to_string())
            .with_remediation(
                "Restart Torben App to resume recovery before retrying the library migration.",
            );
            journal.fail(&pending)?;
            return Err(pending);
        }
        journal.fail_and_rollback(
            &error
                .clone()
                .with_detail("stagingRollbackComplete", staging_removed.to_string()),
        )?;
        remove_migration_receipt(paths, journal.operation_id());
    }
    result
}

pub(crate) fn recover(
    paths: &TorbenPaths,
    store: &StateStore,
    journal: &mut OperationJournal,
) -> TorbenResult<()> {
    let migration = journal.migration().cloned().ok_or_else(|| {
        TorbenError::new(
            "operation_journal_invalid",
            "The library migration journal has no migration subject.",
        )
    })?;
    validate_migration_recovery_path_shape(&migration)?;
    let receipt_path = migration_receipt_path(paths, journal.operation_id());
    if !receipt_path.exists()
        && journal.latest_phase() == Some("prepare")
        && !migration.target_committed
        && !migration.source_cleanup_pending
        && store.managed_library_path()?.as_ref() != Some(&migration.target)
        && pre_receipt_state_is_untouched(
            &migration.source,
            &migration.target,
            &migration.staging,
            migration.target_existed,
        )
    {
        journal
            .recover_rollback("Managed library migration stopped before its receipt was created")?;
        return Ok(());
    }
    validate_migration_receipt(paths, journal.operation_id(), &migration)?;
    let MigrationSubject {
        source,
        target,
        staging,
        target_existed,
        target_committed,
        source_cleanup_pending,
    } = migration;
    if store.managed_library_path()?.as_ref() == Some(&target) {
        if !target.is_dir() {
            return Err(TorbenError::new(
                "managed_library_recovery_failed",
                "The committed managed application library is missing.",
            ));
        }
        paths.set_app_library(target);
        remove_directory_if_present(&staging)?;
        remove_directory_if_present(&source)?;
        if source_cleanup_pending {
            journal.set_migration_source_cleanup_pending(false)?;
        }
        journal.succeed("Recovered committed managed library migration")?;
        remove_migration_receipt(paths, journal.operation_id());
        Ok(())
    } else {
        let staging_removed = !staging.exists() || std::fs::remove_dir_all(staging).is_ok();
        let target_restored =
            rollback_uncommitted_target(&target, target_existed, target_committed);
        if !staging_removed || !target_restored {
            return Err(TorbenError::new(
                "managed_library_recovery_failed",
                "An interrupted managed library copy could not be removed safely.",
            ));
        }
        journal.recover_rollback("Interrupted managed library migration was not committed")?;
        remove_migration_receipt(paths, journal.operation_id());
        Ok(())
    }
}

fn rollback_uncommitted_target(
    target: &Path,
    target_existed: bool,
    target_committed: bool,
) -> bool {
    if target_committed {
        if target.exists() && std::fs::remove_dir_all(target).is_err() {
            return false;
        }
        return !target_existed || std::fs::create_dir(target).is_ok();
    }
    if target_existed {
        if target.exists() {
            return target
                .symlink_metadata()
                .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_dir())
                && target
                    .read_dir()
                    .is_ok_and(|mut entries| entries.next().is_none());
        }
        return std::fs::create_dir(target).is_ok();
    }
    !target.exists()
}

fn migration_receipt_path(paths: &TorbenPaths, operation_id: OperationId) -> PathBuf {
    paths
        .operation_dir()
        .join(format!("{operation_id}.library-migration.receipt"))
}

fn receipt_from_journal(journal: &OperationJournal) -> TorbenResult<MigrationReceipt> {
    let migration = journal.migration().ok_or_else(|| {
        TorbenError::new(
            "operation_journal_invalid",
            "The library migration journal has no migration subject.",
        )
    })?;
    Ok(MigrationReceipt {
        schema_version: MIGRATION_RECEIPT_SCHEMA_VERSION,
        operation_id: journal.operation_id(),
        source: migration.source.clone(),
        target: migration.target.clone(),
        staging: migration.staging.clone(),
        target_existed: migration.target_existed,
    })
}

fn write_migration_receipt(paths: &TorbenPaths, journal: &OperationJournal) -> TorbenResult<()> {
    let receipt = receipt_from_journal(journal)?;
    let content = serde_json::to_vec(&receipt).map_err(|error| {
        migration_receipt_error("Could not serialize the managed-library migration receipt.")
            .with_detail("reason", error.to_string())
    })?;
    let path = migration_receipt_path(paths, journal.operation_id());
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| {
            migration_receipt_error("Could not create the managed-library migration receipt.")
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
    file.write_all(&content).map_err(|error| {
        migration_receipt_error("Could not write the managed-library migration receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    file.sync_all().map_err(|error| {
        migration_receipt_error("Could not sync the managed-library migration receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })
}

fn read_migration_receipt(
    paths: &TorbenPaths,
    operation_id: OperationId,
) -> TorbenResult<MigrationReceipt> {
    let path = migration_receipt_path(paths, operation_id);
    let metadata = path.symlink_metadata().map_err(|error| {
        migration_receipt_error("The managed-library migration receipt is unavailable.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MIGRATION_RECEIPT_MAX_BYTES
    {
        return Err(migration_receipt_error(
            "The managed-library migration receipt is not a bounded regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    let content = std::fs::read(&path).map_err(|error| {
        migration_receipt_error("Could not read the managed-library migration receipt.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })?;
    serde_json::from_slice(&content).map_err(|error| {
        migration_receipt_error("The managed-library migration receipt is invalid.")
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
    })
}

fn validate_migration_recovery_path_shape(migration: &MigrationSubject) -> TorbenResult<()> {
    for path in [&migration.source, &migration.target, &migration.staging] {
        if !is_safe_recorded_path(path) {
            return Err(migration_recovery_path_error(path));
        }
    }
    if migration.source == migration.target
        || migration.target.starts_with(&migration.source)
        || migration.source.starts_with(&migration.target)
        || migration.staging == migration.source
        || migration.staging.starts_with(&migration.source)
        || migration.source.starts_with(&migration.staging)
        || migration.staging == migration.target
        || migration.staging.starts_with(&migration.target)
        || migration.target.starts_with(&migration.staging)
        || migration.staging.parent() != migration.target.parent()
    {
        return Err(migration_recovery_path_error(&migration.staging));
    }
    let Some(staging_name) = migration.staging.file_name().and_then(|name| name.to_str()) else {
        return Err(migration_recovery_path_error(&migration.staging));
    };
    let Some(staging_id) = staging_name.strip_prefix(".torben-library-staging-") else {
        return Err(migration_recovery_path_error(&migration.staging));
    };
    if OperationId::from_str(staging_id).is_err() {
        return Err(migration_recovery_path_error(&migration.staging));
    }
    Ok(())
}

fn validate_migration_receipt(
    paths: &TorbenPaths,
    operation_id: OperationId,
    migration: &MigrationSubject,
) -> TorbenResult<()> {
    let expected = MigrationReceipt {
        schema_version: MIGRATION_RECEIPT_SCHEMA_VERSION,
        operation_id,
        source: migration.source.clone(),
        target: migration.target.clone(),
        staging: migration.staging.clone(),
        target_existed: migration.target_existed,
    };
    if read_migration_receipt(paths, operation_id)? != expected {
        return Err(migration_receipt_error(
            "The managed-library migration journal does not match its durable receipt.",
        ));
    }
    Ok(())
}

fn pre_receipt_state_is_untouched(
    source: &Path,
    target: &Path,
    staging: &Path,
    target_existed: bool,
) -> bool {
    if !source
        .symlink_metadata()
        .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_dir())
        || staging.exists()
    {
        return false;
    }
    if target_existed {
        target
            .symlink_metadata()
            .is_ok_and(|metadata| !metadata.file_type().is_symlink() && metadata.is_dir())
            && target
                .read_dir()
                .is_ok_and(|mut entries| entries.next().is_none())
    } else {
        !target.exists()
    }
}

fn is_safe_recorded_path(path: &Path) -> bool {
    path.is_absolute()
        && path.to_str().is_some()
        && path.file_name().is_some()
        && !path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
}

fn migration_recovery_path_error(path: &Path) -> TorbenError {
    TorbenError::new(
        "managed_library_recovery_path_invalid",
        "A managed-library recovery path is unsafe or outside the recorded transaction.",
    )
    .with_detail("path", path.display().to_string())
    .with_remediation(
        "Inspect the operation journal and its migration receipt before retrying startup.",
    )
}

fn migration_receipt_error(message: &str) -> TorbenError {
    TorbenError::new("managed_library_recovery_receipt_invalid", message).with_remediation(
        "Inspect the operation journal and its migration receipt before retrying startup.",
    )
}

fn remove_migration_receipt(paths: &TorbenPaths, operation_id: OperationId) {
    let _ = std::fs::remove_file(migration_receipt_path(paths, operation_id));
}

fn remove_directory_if_present(path: &Path) -> TorbenResult<()> {
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(io_error)?;
    }
    Ok(())
}

fn validate_target(target: &Path) -> TorbenResult<()> {
    if !target.is_absolute() || target.as_os_str().is_empty() {
        return Err(invalid_target(target));
    }
    if target.to_str().is_none() {
        return Err(invalid_target(target));
    }
    if target
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err(invalid_target(target));
    }
    Ok(())
}

fn normalized_target(target: &Path) -> TorbenResult<PathBuf> {
    if target.exists() {
        canonical_existing_directory(target)
    } else {
        let parent = target.parent().ok_or_else(|| invalid_target(target))?;
        let parent = clean_canonical_path(std::fs::canonicalize(parent).map_err(io_error)?);
        Ok(parent.join(target.file_name().ok_or_else(|| invalid_target(target))?))
    }
}

fn canonical_existing_directory(path: &Path) -> TorbenResult<PathBuf> {
    if path
        .symlink_metadata()
        .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Err(invalid_target(path));
    }
    std::fs::canonicalize(path)
        .map(clean_canonical_path)
        .map_err(io_error)
}

#[cfg(windows)]
fn clean_canonical_path(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path
    }
}

#[cfg(not(windows))]
const fn clean_canonical_path(path: PathBuf) -> PathBuf {
    path
}

fn manifest(root: &Path) -> TorbenResult<Vec<FileEntry>> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry.map_err(|error| io_error(std::io::Error::other(error)))?;
        if entry.file_type().is_symlink() {
            return Err(TorbenError::new(
                "managed_library_symlink_unsupported",
                "Managed application library migration does not follow symbolic links.",
            )
            .with_detail("path", entry.path().display().to_string()));
        }
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(root).map_err(|error| {
                TorbenError::internal("Could not resolve a managed library file.")
                    .with_detail("reason", error.to_string())
            })?;
            let size = entry
                .metadata()
                .map_err(|error| io_error(error.into()))?
                .len();
            files.push(FileEntry {
                relative: relative.to_path_buf(),
                size,
                sha256: sha256(entry.path())?,
            });
        }
    }
    files.sort_by(|left, right| left.relative.cmp(&right.relative));
    Ok(files)
}

fn copy_tree(source: &Path, target: &Path) -> TorbenResult<()> {
    std::fs::create_dir(target).map_err(io_error)?;
    for entry in WalkDir::new(source).min_depth(1).follow_links(false) {
        let entry = entry.map_err(|error| io_error(std::io::Error::other(error)))?;
        let relative = entry.path().strip_prefix(source).map_err(|error| {
            TorbenError::internal("Could not resolve a managed library file.")
                .with_detail("reason", error.to_string())
        })?;
        let destination = target.join(relative);
        if entry.file_type().is_dir() {
            std::fs::create_dir(&destination).map_err(io_error)?;
        } else if entry.file_type().is_file() {
            std::fs::copy(entry.path(), destination).map_err(io_error)?;
        } else {
            return Err(TorbenError::new(
                "managed_library_entry_unsupported",
                "The managed application library contains an unsupported entry.",
            ));
        }
    }
    Ok(())
}

fn sha256(path: &Path) -> TorbenResult<[u8; 32]> {
    let mut file = File::open(path).map_err(io_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn invalid_target(path: &Path) -> TorbenError {
    TorbenError::new(
        "managed_library_path_invalid",
        "Choose an absolute, regular directory for the managed application library.",
    )
    .with_detail("path", path.display().to_string())
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "managed_library_io_failed",
        "Could not migrate the managed application library.",
    )
    .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};

    use serde_json::json;
    use tempfile::tempdir;
    use torben_contracts::{
        AppId, ExactVersion, InstallRecord, InstallScope, OperationId, OperationState, SourceId,
    };

    use super::{migrate, recover, write_migration_receipt};
    use crate::{StateStore, TorbenCore, TorbenPaths, operation::OperationJournal};

    fn record(install_path: std::path::PathBuf) -> InstallRecord {
        InstallRecord {
            app_id: AppId::new("node").unwrap(),
            version: ExactVersion::from_str("24.19.0").unwrap(),
            source_id: SourceId::new("node.official").unwrap(),
            scope: InstallScope::Managed,
            install_path: install_path.display().to_string(),
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        }
    }

    #[test]
    fn migration_copies_verifies_switches_state_and_removes_source() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        let install_path = paths.app_version_dir("node", "24.19.0");
        std::fs::create_dir_all(&install_path).unwrap();
        std::fs::write(install_path.join("node.bin"), b"managed node fixture").unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let original = record(install_path);
        store.add_installation(&original).unwrap();
        let target = root.path().join("migrated-apps");
        std::fs::create_dir(&target).unwrap();

        let result = migrate(&paths, Arc::clone(&store), &target).unwrap();

        assert_eq!(paths.app_library(), target);
        assert!(!std::path::Path::new(&result.previous_path).exists());
        assert_eq!(result.bytes_copied, 20);
        assert!(!result.source_cleanup_pending);
        let migrated = store
            .get_installation(&original.app_id, &original.version)
            .unwrap()
            .unwrap();
        assert!(std::path::Path::new(&migrated.install_path).starts_with(&target));
        assert_eq!(
            std::fs::read(std::path::Path::new(&migrated.install_path).join("node.bin")).unwrap(),
            b"managed node fixture"
        );
        let events = OperationJournal::list(&store).unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.state == OperationState::Succeeded)
        );
    }

    #[test]
    fn state_commit_failure_removes_the_copy_and_preserves_the_source() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        std::fs::write(paths.app_library().join("fixture.bin"), b"fixture").unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let outside = root.path().join("outside").join("node");
        std::fs::create_dir_all(&outside).unwrap();
        let invalid = record(outside);
        store.add_installation(&invalid).unwrap();
        let source = paths.app_library();
        let target = root.path().join("migrated-apps");

        let error = migrate(&paths, Arc::clone(&store), &target).unwrap_err();

        assert_eq!(error.code, "managed_install_path_invalid");
        assert!(source.join("fixture.bin").is_file());
        assert!(!target.exists());
        assert!(store.managed_library_path().unwrap().is_none());
        let events = OperationJournal::list(&store).unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.state == OperationState::RolledBack)
        );
    }

    #[test]
    fn recovery_removes_an_uncommitted_copy_and_preserves_the_source() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        let source = paths.app_library();
        std::fs::write(source.join("source.bin"), b"source").unwrap();
        let target = root.path().join("target");
        let staging = root
            .path()
            .join(format!(".torben-library-staging-{}", OperationId::new()));
        std::fs::create_dir(&target).unwrap();
        std::fs::create_dir(&staging).unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let mut journal = OperationJournal::start_migration(
            &paths,
            Arc::clone(&store),
            source.clone(),
            target.clone(),
            staging.clone(),
            true,
        )
        .unwrap();
        write_migration_receipt(&paths, &journal).unwrap();

        recover(&paths, &store, &mut journal).unwrap();

        assert!(source.join("source.bin").is_file());
        assert!(target.is_dir());
        assert!(target.read_dir().unwrap().next().is_none());
        assert!(!staging.exists());
        assert!(
            OperationJournal::list(&store)
                .unwrap()
                .iter()
                .any(|event| event.state == OperationState::RolledBack)
        );
    }

    #[test]
    fn recovery_safely_closes_a_journal_created_before_its_receipt() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        let source = paths.app_library();
        std::fs::write(source.join("source.bin"), b"source").unwrap();
        let target = root.path().join("target");
        let staging = root
            .path()
            .join(format!(".torben-library-staging-{}", OperationId::new()));
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let mut journal = OperationJournal::start_migration(
            &paths,
            Arc::clone(&store),
            source.clone(),
            target.clone(),
            staging,
            false,
        )
        .unwrap();

        recover(&paths, &store, &mut journal).unwrap();

        assert!(source.join("source.bin").is_file());
        assert!(!target.exists());
        assert!(
            OperationJournal::list(&store)
                .unwrap()
                .iter()
                .any(|event| event.state == OperationState::RolledBack)
        );
    }

    #[test]
    fn recovery_removes_a_transaction_owned_target_before_rolling_back() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        let source = paths.app_library();
        std::fs::write(source.join("source.bin"), b"source").unwrap();
        let target = root.path().join("target");
        let staging = root
            .path()
            .join(format!(".torben-library-staging-{}", OperationId::new()));
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("copied.bin"), b"copy").unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let mut journal = OperationJournal::start_migration(
            &paths,
            Arc::clone(&store),
            source.clone(),
            target.clone(),
            staging,
            false,
        )
        .unwrap();
        write_migration_receipt(&paths, &journal).unwrap();
        journal.set_migration_target_committed().unwrap();

        recover(&paths, &store, &mut journal).unwrap();

        assert!(source.join("source.bin").is_file());
        assert!(!target.exists());
        assert!(
            OperationJournal::list(&store)
                .unwrap()
                .iter()
                .any(|event| event.state == OperationState::RolledBack)
        );
    }

    #[test]
    fn recovery_preserves_an_unowned_nonempty_target_and_fails_closed() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        let source = paths.app_library();
        std::fs::write(source.join("source.bin"), b"source").unwrap();
        let target = root.path().join("external-target");
        let staging = root
            .path()
            .join(format!(".torben-library-staging-{}", OperationId::new()));
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("external.bin"), b"external").unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let mut journal = OperationJournal::start_migration(
            &paths,
            Arc::clone(&store),
            source.clone(),
            target.clone(),
            staging,
            false,
        )
        .unwrap();
        write_migration_receipt(&paths, &journal).unwrap();

        let error = recover(&paths, &store, &mut journal).unwrap_err();

        assert_eq!(error.code, "managed_library_recovery_failed");
        assert!(source.join("source.bin").is_file());
        assert_eq!(
            std::fs::read(target.join("external.bin")).unwrap(),
            b"external"
        );
    }

    #[test]
    fn recovery_finishes_cleanup_after_the_state_commit() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        let source = paths.app_library();
        std::fs::write(source.join("old.bin"), b"old").unwrap();
        let target = root.path().join("target");
        let staging = root
            .path()
            .join(format!(".torben-library-staging-{}", OperationId::new()));
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("new.bin"), b"new").unwrap();
        std::fs::create_dir(&staging).unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let mut journal = OperationJournal::start_migration(
            &paths,
            Arc::clone(&store),
            source.clone(),
            target.clone(),
            staging.clone(),
            false,
        )
        .unwrap();
        write_migration_receipt(&paths, &journal).unwrap();
        store
            .commit_managed_library_migration(&source, &target)
            .unwrap();

        recover(&paths, &store, &mut journal).unwrap();

        assert_eq!(paths.app_library(), target);
        assert!(!source.exists());
        assert!(!staging.exists());
        assert!(
            OperationJournal::list(&store)
                .unwrap()
                .iter()
                .any(|event| event.state == OperationState::Succeeded)
        );
    }

    #[test]
    fn terminal_cleanup_pending_migration_is_retried_during_startup_recovery() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        let source = paths.app_library();
        std::fs::write(source.join("old.bin"), b"old").unwrap();
        let target = root.path().join("target");
        let staging = root
            .path()
            .join(format!(".torben-library-staging-{}", OperationId::new()));
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("new.bin"), b"new").unwrap();
        std::fs::create_dir(&staging).unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let mut journal = OperationJournal::start_migration(
            &paths,
            Arc::clone(&store),
            source.clone(),
            target.clone(),
            staging.clone(),
            false,
        )
        .unwrap();
        write_migration_receipt(&paths, &journal).unwrap();
        let operation_id = journal.operation_id();
        store
            .commit_managed_library_migration(&source, &target)
            .unwrap();
        journal.set_migration_source_cleanup_pending(true).unwrap();
        journal
            .succeed("Committed with old-source cleanup pending")
            .unwrap();
        drop(journal);

        let mut interrupted = OperationJournal::interrupted(&paths, Arc::clone(&store)).unwrap();
        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].operation_id(), operation_id);
        recover(&paths, &store, &mut interrupted[0]).unwrap();

        assert_eq!(paths.app_library(), target);
        assert!(!source.exists());
        assert!(!staging.exists());
        assert!(
            OperationJournal::interrupted(&paths, Arc::clone(&store))
                .unwrap()
                .is_empty()
        );
        let succeeded = OperationJournal::list(&store)
            .unwrap()
            .into_iter()
            .filter(|event| {
                event.operation_id == operation_id && event.state == OperationState::Succeeded
            })
            .count();
        assert_eq!(succeeded, 2);
    }

    #[test]
    fn recovery_rejects_a_tampered_source_path_before_deleting_any_directory() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        let source = paths.app_library();
        std::fs::write(source.join("old.bin"), b"old").unwrap();
        let protected = root.path().join("must-not-delete");
        std::fs::create_dir(&protected).unwrap();
        std::fs::write(protected.join("protected.bin"), b"protected").unwrap();
        let target = root.path().join("target");
        let staging = root
            .path()
            .join(format!(".torben-library-staging-{}", OperationId::new()));
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("new.bin"), b"new").unwrap();
        std::fs::create_dir(&staging).unwrap();
        let store = Arc::new(StateStore::open(paths.state_database()).unwrap());
        let mut journal = OperationJournal::start_migration(
            &paths,
            Arc::clone(&store),
            source.clone(),
            target.clone(),
            staging,
            false,
        )
        .unwrap();
        let operation_id = journal.operation_id();
        write_migration_receipt(&paths, &journal).unwrap();
        store
            .commit_managed_library_migration(&source, &target)
            .unwrap();
        journal.set_migration_source_cleanup_pending(true).unwrap();
        journal
            .succeed("Committed with old-source cleanup pending")
            .unwrap();
        drop(journal);

        let journal_path = paths.operation_dir().join(format!("{operation_id}.json"));
        let mut content: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        content["migration"]["source"] = json!(protected);
        std::fs::write(&journal_path, serde_json::to_vec_pretty(&content).unwrap()).unwrap();

        let mut interrupted = OperationJournal::interrupted(&paths, Arc::clone(&store)).unwrap();
        let error = recover(&paths, &store, &mut interrupted[0]).unwrap_err();

        assert_eq!(error.code, "managed_library_recovery_receipt_invalid");
        assert!(protected.join("protected.bin").is_file());
        assert!(source.join("old.bin").is_file());
        assert!(target.join("new.bin").is_file());
    }

    #[test]
    fn startup_rejects_a_missing_configured_library_instead_of_creating_it() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().join("torben"));
        paths.ensure_layout().unwrap();
        let source = paths.app_library();
        let target = root.path().join("missing-target");
        std::fs::create_dir(&target).unwrap();
        {
            let store = StateStore::open(paths.state_database()).unwrap();
            store
                .commit_managed_library_migration(&source, &target)
                .unwrap();
        }
        std::fs::remove_dir(&target).unwrap();

        let Err(error) = TorbenCore::open(paths) else {
            panic!("missing configured library unexpectedly opened");
        };

        assert_eq!(error.code, "managed_library_unavailable");
        assert!(!target.exists());
    }
}
