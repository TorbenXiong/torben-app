use std::{fs::OpenOptions, io::Write, path::Path, sync::Arc};

#[cfg(any(unix, test))]
use std::path::PathBuf;

use torben_contracts::{ShellIntegrationState, ShellIntegrationStatus, TorbenError, TorbenResult};

use crate::TorbenPaths;

pub(crate) trait ShellIntegrationBackend: Send + Sync {
    fn recover(&self, _shim_path: &Path) -> TorbenResult<()> {
        Ok(())
    }

    fn status(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus>;
    fn enable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus>;
    fn disable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus>;
}

pub(crate) fn platform_backend(paths: &TorbenPaths) -> Arc<dyn ShellIntegrationBackend> {
    if paths.is_isolated() {
        return Arc::new(IsolatedShellIntegration);
    }
    #[cfg(windows)]
    {
        Arc::new(windows::WindowsShellIntegration::new(
            paths.config_dir().join("shell-integration.json"),
            paths
                .config_dir()
                .join("shell-integration-transaction.json"),
        ))
    }
    #[cfg(unix)]
    {
        match unix::UnixShellIntegration::discover(
            paths
                .config_dir()
                .join("shell-integration-transaction.json"),
        ) {
            Ok(backend) => Arc::new(backend),
            Err(error) => Arc::new(UnavailableShellIntegration { error }),
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = paths;
        Arc::new(UnavailableShellIntegration {
            error: TorbenError::new(
                "shell_integration_unsupported",
                "Shell integration is not supported on this platform.",
            ),
        })
    }
}

#[cfg(not(windows))]
struct UnavailableShellIntegration {
    error: TorbenError,
}

#[cfg(not(windows))]
impl ShellIntegrationBackend for UnavailableShellIntegration {
    fn status(&self, _shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
        Err(self.error.clone())
    }

    fn enable(&self, _shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
        Err(self.error.clone())
    }

    fn disable(&self, _shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
        Err(self.error.clone())
    }
}

struct IsolatedShellIntegration;

impl ShellIntegrationBackend for IsolatedShellIntegration {
    fn status(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
        status(ShellIntegrationState::Disabled, shim_path, Vec::new())
    }

    fn enable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
        Err(isolated_error(shim_path))
    }

    fn disable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
        Err(isolated_error(shim_path))
    }
}

fn isolated_error(shim_path: &Path) -> TorbenError {
    TorbenError::new(
        "shell_integration_isolated",
        "Shell integration is disabled for an isolated Torben data directory.",
    )
    .with_detail("path", shim_path.display().to_string())
}

fn status(
    state: ShellIntegrationState,
    shim_path: &Path,
    targets: Vec<String>,
) -> TorbenResult<ShellIntegrationStatus> {
    Ok(ShellIntegrationStatus {
        state,
        shim_path: path_text(shim_path)?.to_owned(),
        targets,
        new_terminal_required: state != ShellIntegrationState::Disabled,
    })
}

fn path_text(path: &Path) -> TorbenResult<&str> {
    path.to_str().ok_or_else(|| {
        TorbenError::new(
            "shell_path_not_unicode",
            "The shell integration path cannot be represented as Unicode.",
        )
        .with_detail("path", path.to_string_lossy())
    })
}

fn atomic_write(path: &Path, content: &[u8]) -> TorbenResult<()> {
    let parent = path.parent().ok_or_else(|| {
        TorbenError::new(
            "shell_config_path_invalid",
            "A shell integration file has no parent directory.",
        )
        .with_detail("path", path.display().to_string())
    })?;
    std::fs::create_dir_all(parent).map_err(|error| shell_file_error(path, error))?;
    let operation_id = torben_contracts::OperationId::new();
    let pending = path.with_extension(format!("torben-next-{operation_id}"));
    #[cfg(not(unix))]
    let previous = path.with_extension(format!("torben-previous-{operation_id}"));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .map_err(|error| shell_file_error(&pending, error))?;
    if let Err(error) = file.write_all(content).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&pending);
        return Err(shell_file_error(&pending, error));
    }
    drop(file);
    #[cfg(unix)]
    {
        std::fs::rename(&pending, path).map_err(|error| shell_file_error(path, error))?;
        sync_parent(parent)?;
        return Ok(());
    }
    #[cfg(not(unix))]
    {
        if path.exists() {
            std::fs::rename(path, &previous).map_err(|error| shell_file_error(path, error))?;
        }
        if let Err(error) = std::fs::rename(&pending, path) {
            if previous.exists() && !path.exists() {
                let _ = std::fs::rename(&previous, path);
            }
            let _ = std::fs::remove_file(&pending);
            return Err(shell_file_error(path, error));
        }
        let _ = std::fs::remove_file(previous);
        Ok(())
    }
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> TorbenResult<()> {
    std::fs::File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| shell_file_error(parent, error))
}

fn inspect_bounded_regular_file(path: &Path, maximum_bytes: u64) -> TorbenResult<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            if metadata.len() > maximum_bytes {
                return Err(TorbenError::new(
                    "shell_integration_receipt_too_large",
                    "A shell integration receipt exceeds the supported size.",
                )
                .with_detail("path", path.display().to_string())
                .with_detail("maximumBytes", maximum_bytes.to_string())
                .with_detail("actualBytes", metadata.len().to_string()));
            }
            std::fs::read(path)
                .map(Some)
                .map_err(|error| shell_file_error(path, error))
        }
        Ok(_) => Err(TorbenError::new(
            "shell_config_path_conflict",
            "A shell integration receipt is not a regular file.",
        )
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(shell_file_error(path, error)),
    }
}

#[cfg(any(unix, test))]
fn inspect_regular_file(path: &Path) -> TorbenResult<Option<Vec<u8>>> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::read(path)
                .map(Some)
                .map_err(|error| shell_file_error(path, error))
        }
        Ok(_) => Err(TorbenError::new(
            "shell_config_path_conflict",
            "A shell integration target is not a regular file.",
        )
        .with_detail("path", path.display().to_string())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(shell_file_error(path, error)),
    }
}

#[cfg(any(unix, test))]
fn restore_file(path: &Path, original: Option<&[u8]>) -> bool {
    match original {
        Some(content) => atomic_write(path, content).is_ok(),
        None => match std::fs::remove_file(path) {
            Ok(()) => true,
            Err(error) => error.kind() == std::io::ErrorKind::NotFound,
        },
    }
}

fn shell_file_error(path: &Path, error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "shell_integration_io_failed",
        "Could not update shell integration files.",
    )
    .with_detail("path", path.display().to_string())
    .with_detail("reason", error.to_string())
}

#[cfg(any(windows, test))]
fn normalized_windows_path(value: &str) -> String {
    let mut value = value.trim().trim_matches('"').replace('/', "\\");
    while value.len() > 3 && value.ends_with('\\') {
        value.pop();
    }
    value.to_lowercase()
}

#[cfg(any(windows, test))]
fn windows_path_contains(value: &str, entry: &str) -> bool {
    let expected = normalized_windows_path(entry);
    value
        .split(';')
        .any(|candidate| normalized_windows_path(candidate) == expected)
}

#[cfg(any(windows, test))]
fn prepend_windows_path(value: &str, entry: &str) -> String {
    if value.is_empty() {
        entry.to_owned()
    } else {
        format!("{entry};{value}")
    }
}

#[cfg(any(windows, test))]
fn remove_first_windows_path(value: &str, entry: &str) -> String {
    let expected = normalized_windows_path(entry);
    let mut removed = false;
    value
        .split(';')
        .filter(|candidate| {
            if !removed && normalized_windows_path(candidate) == expected {
                removed = true;
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

#[cfg(any(windows, test))]
fn should_delete_windows_path(value: &str, path_existed: bool) -> bool {
    value.is_empty() && !path_existed
}

#[cfg(any(unix, test))]
const BLOCK_START: &str = "# >>> Torben App managed shell integration >>>";
#[cfg(any(unix, test))]
const BLOCK_END: &str = "# <<< Torben App managed shell integration <<<";

#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum ProfileKind {
    Posix,
    Fish,
}

#[cfg(any(unix, test))]
fn managed_block(kind: ProfileKind, shim_path: &str) -> String {
    let quoted = shell_single_quote(shim_path);
    let command = match kind {
        ProfileKind::Posix => format!(
            "case \":$PATH:\" in\n  *:{quoted}:*) ;;\n  *) export PATH={quoted}:\"$PATH\" ;;\nesac"
        ),
        ProfileKind::Fish => {
            format!("if not contains -- {quoted} $PATH\n  set -gx PATH {quoted} $PATH\nend")
        }
    };
    format!("{BLOCK_START}\n{command}\n{BLOCK_END}\n")
}

#[cfg(any(unix, test))]
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ManagedBlockState {
    Missing,
    Exact,
    Outdated,
}

#[cfg(any(unix, test))]
fn managed_block_state(content: &str, expected: &str) -> TorbenResult<ManagedBlockState> {
    let range = managed_block_range(content)?;
    Ok(match range {
        None => ManagedBlockState::Missing,
        Some((start, end)) if &content[start..end] == expected => ManagedBlockState::Exact,
        Some(_) => ManagedBlockState::Outdated,
    })
}

#[cfg(any(unix, test))]
fn upsert_managed_block(content: &str, expected: &str) -> TorbenResult<String> {
    if let Some((start, end)) = managed_block_range(content)? {
        let mut result = String::with_capacity(content.len() - (end - start) + expected.len());
        result.push_str(&content[..start]);
        result.push_str(expected);
        result.push_str(&content[end..]);
        return Ok(result);
    }
    let mut result = content.to_owned();
    if !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    result.push_str(expected);
    Ok(result)
}

#[cfg(any(unix, test))]
fn remove_managed_block(content: &str) -> TorbenResult<String> {
    let Some((start, end)) = managed_block_range(content)? else {
        return Ok(content.to_owned());
    };
    let mut result = String::with_capacity(content.len() - (end - start));
    result.push_str(&content[..start]);
    result.push_str(&content[end..]);
    Ok(result)
}

#[cfg(any(unix, test))]
fn managed_block_range(content: &str) -> TorbenResult<Option<(usize, usize)>> {
    let starts = content.match_indices(BLOCK_START).collect::<Vec<_>>();
    let ends = content.match_indices(BLOCK_END).collect::<Vec<_>>();
    if starts.is_empty() && ends.is_empty() {
        return Ok(None);
    }
    if starts.len() != 1 || ends.len() != 1 || ends[0].0 <= starts[0].0 {
        return Err(TorbenError::new(
            "shell_profile_conflict",
            "A shell profile contains malformed Torben App management markers.",
        ));
    }
    let start = starts[0].0;
    let mut end = ends[0].0 + BLOCK_END.len();
    if content.as_bytes().get(end) == Some(&b'\r') {
        end += 1;
    }
    if content.as_bytes().get(end) == Some(&b'\n') {
        end += 1;
    }
    Ok(Some((start, end)))
}

#[cfg(any(unix, test))]
#[derive(Debug)]
struct FileChange {
    path: PathBuf,
    kind: ProfileKind,
    original: Option<Vec<u8>>,
    updated: Vec<u8>,
}

#[cfg(any(unix, test))]
fn apply_file_changes(changes: &[FileChange]) -> TorbenResult<()> {
    let mut committed = Vec::<usize>::new();
    for (index, change) in changes.iter().enumerate() {
        if change.original.as_deref() == Some(change.updated.as_slice()) {
            continue;
        }
        if let Err(error) = atomic_write(&change.path, &change.updated) {
            let rollback_complete =
                committed
                    .iter()
                    .rev()
                    .fold(true, |complete, committed_index| {
                        let committed_change = &changes[*committed_index];
                        let restored = restore_file(
                            &committed_change.path,
                            committed_change.original.as_deref(),
                        );
                        complete && restored
                    });
            return Err(error.with_detail("rollbackComplete", rollback_complete.to_string()));
        }
        committed.push(index);
    }
    Ok(())
}

#[cfg(any(unix, test))]
const FILE_TRANSACTION_RECEIPT_MAX_BYTES: u64 = 32 * 1024;

#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum FileTransactionAction {
    Enable,
    Disable,
}

#[cfg(any(unix, test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum FileTransactionPhase {
    Preparing,
    Prepared,
}

#[cfg(any(unix, test))]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileTransactionEntry {
    path: PathBuf,
    kind: ProfileKind,
    original_sha256: Option<String>,
    updated_sha256: String,
    backup_path: Option<PathBuf>,
}

#[cfg(any(unix, test))]
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FileTransactionReceipt {
    schema_version: u32,
    operation_id: torben_contracts::OperationId,
    action: FileTransactionAction,
    phase: FileTransactionPhase,
    shim_path: String,
    transaction_dir: PathBuf,
    entries: Vec<FileTransactionEntry>,
}

#[cfg(any(unix, test))]
fn execute_file_transaction(
    receipt_path: &Path,
    shim_path: &str,
    action: FileTransactionAction,
    changes: &[FileChange],
) -> TorbenResult<()> {
    let operation_id = torben_contracts::OperationId::new();
    let parent = receipt_path.parent().ok_or_else(|| {
        TorbenError::new(
            "shell_config_path_invalid",
            "The shell integration transaction has no parent directory.",
        )
        .with_detail("path", receipt_path.display().to_string())
    })?;
    std::fs::create_dir_all(parent).map_err(|error| shell_file_error(parent, error))?;
    let transaction_dir = parent.join(format!("shell-integration-{operation_id}"));
    let entries = changes
        .iter()
        .enumerate()
        .map(|(index, change)| FileTransactionEntry {
            path: change.path.clone(),
            kind: change.kind,
            original_sha256: change.original.as_deref().map(sha256_bytes),
            updated_sha256: sha256_bytes(&change.updated),
            backup_path: change
                .original
                .as_ref()
                .map(|_| transaction_dir.join(format!("{index}.original"))),
        })
        .collect();
    let mut receipt = FileTransactionReceipt {
        schema_version: 1,
        operation_id,
        action,
        phase: FileTransactionPhase::Preparing,
        shim_path: shim_path.to_owned(),
        transaction_dir: transaction_dir.clone(),
        entries,
    };
    write_file_transaction_receipt(receipt_path, &receipt)?;
    let prepared = (|| {
        std::fs::create_dir(&transaction_dir)
            .map_err(|error| shell_file_error(&transaction_dir, error))?;
        for (change, entry) in changes.iter().zip(&receipt.entries) {
            if let (Some(original), Some(backup_path)) = (&change.original, &entry.backup_path) {
                atomic_write(backup_path, original)?;
            }
        }
        receipt.phase = FileTransactionPhase::Prepared;
        write_file_transaction_receipt(receipt_path, &receipt)
    })();
    if let Err(error) = prepared {
        let rollback_complete = recover_file_transaction(
            receipt_path,
            shim_path,
            &changes
                .iter()
                .map(|change| (change.path.clone(), change.kind))
                .collect::<Vec<_>>(),
        )
        .is_ok();
        return Err(error.with_detail("rollbackComplete", rollback_complete.to_string()));
    }
    if let Err(error) = apply_file_changes(changes) {
        let rollback_complete = recover_file_transaction(
            receipt_path,
            shim_path,
            &changes
                .iter()
                .map(|change| (change.path.clone(), change.kind))
                .collect::<Vec<_>>(),
        )
        .is_ok();
        return Err(error.with_detail("rollbackComplete", rollback_complete.to_string()));
    }
    cleanup_file_transaction(receipt_path, &receipt)
}

#[cfg(any(unix, test))]
fn recover_file_transaction(
    receipt_path: &Path,
    expected_shim_path: &str,
    expected_profiles: &[(PathBuf, ProfileKind)],
) -> TorbenResult<()> {
    let Some(receipt) = read_file_transaction_receipt(receipt_path)? else {
        return Ok(());
    };
    validate_file_transaction(
        receipt_path,
        &receipt,
        expected_shim_path,
        expected_profiles,
    )?;
    let current = receipt
        .entries
        .iter()
        .map(|entry| inspect_regular_file(&entry.path))
        .collect::<TorbenResult<Vec<_>>>()?;
    let matches_original = current
        .iter()
        .zip(&receipt.entries)
        .map(|(content, entry)| {
            content_state_matches(content.as_deref(), entry.original_sha256.as_deref())
        })
        .collect::<Vec<_>>();
    let matches_updated = current
        .iter()
        .zip(&receipt.entries)
        .map(|(content, entry)| {
            content
                .as_deref()
                .is_some_and(|content| sha256_bytes(content) == entry.updated_sha256)
        })
        .collect::<Vec<_>>();
    if receipt.phase == FileTransactionPhase::Preparing {
        if !matches_original.iter().all(|matches| *matches) {
            return Err(file_transaction_conflict(receipt_path));
        }
        if !receipt.transaction_dir.exists() {
            return remove_file_if_exists(receipt_path);
        }
        validate_preparing_artifacts(receipt_path, &receipt)?;
        return cleanup_file_transaction(receipt_path, &receipt);
    }
    if prepared_backup_evidence_complete(receipt_path, &receipt)? {
        validate_prepared_backup_evidence(receipt_path, &receipt, expected_shim_path)?;
    }
    if matches_updated.iter().all(|matches| *matches) {
        return cleanup_file_transaction(receipt_path, &receipt);
    }
    if matches_original
        .iter()
        .zip(&matches_updated)
        .any(|(original, updated)| !original && !updated)
    {
        return Err(file_transaction_conflict(receipt_path));
    }
    validate_prepared_backup_evidence(receipt_path, &receipt, expected_shim_path)?;
    for ((entry, original), updated) in receipt
        .entries
        .iter()
        .zip(&matches_original)
        .zip(&matches_updated)
    {
        if *original || !updated {
            continue;
        }
        match (&entry.original_sha256, &entry.backup_path) {
            (Some(expected_hash), Some(backup_path)) => {
                let Some(backup) = inspect_regular_file(backup_path)? else {
                    return Err(file_transaction_conflict(receipt_path));
                };
                if sha256_bytes(&backup) != *expected_hash {
                    return Err(file_transaction_conflict(receipt_path));
                }
                atomic_write(&entry.path, &backup)?;
            }
            (None, None) => match std::fs::remove_file(&entry.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(shell_file_error(&entry.path, error)),
            },
            _ => return Err(file_transaction_conflict(receipt_path)),
        }
    }
    for entry in &receipt.entries {
        let content = inspect_regular_file(&entry.path)?;
        if !content_state_matches(content.as_deref(), entry.original_sha256.as_deref()) {
            return Err(file_transaction_conflict(receipt_path));
        }
    }
    cleanup_file_transaction(receipt_path, &receipt)
}

#[cfg(any(unix, test))]
fn validate_preparing_artifacts(
    receipt_path: &Path,
    receipt: &FileTransactionReceipt,
) -> TorbenResult<()> {
    let metadata = std::fs::symlink_metadata(&receipt.transaction_dir)
        .map_err(|error| shell_file_error(&receipt.transaction_dir, error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(file_transaction_conflict(receipt_path));
    }
    let expected = receipt
        .entries
        .iter()
        .filter_map(|entry| {
            entry
                .backup_path
                .as_ref()
                .zip(entry.original_sha256.as_ref())
        })
        .collect::<Vec<_>>();
    for entry in std::fs::read_dir(&receipt.transaction_dir)
        .map_err(|error| shell_file_error(&receipt.transaction_dir, error))?
    {
        let entry = entry.map_err(|error| shell_file_error(&receipt.transaction_dir, error))?;
        let path = entry.path();
        let Some((_, expected_hash)) = expected
            .iter()
            .find(|(expected_path, _)| expected_path.as_path() == path.as_path())
        else {
            return Err(file_transaction_conflict(receipt_path));
        };
        let Some(content) = inspect_regular_file(&path)? else {
            return Err(file_transaction_conflict(receipt_path));
        };
        if sha256_bytes(&content) != expected_hash.as_str() {
            return Err(file_transaction_conflict(receipt_path));
        }
    }
    Ok(())
}

#[cfg(any(unix, test))]
fn prepared_backup_evidence_complete(
    receipt_path: &Path,
    receipt: &FileTransactionReceipt,
) -> TorbenResult<bool> {
    for entry in &receipt.entries {
        let Some(backup_path) = &entry.backup_path else {
            continue;
        };
        match std::fs::symlink_metadata(backup_path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => return Err(file_transaction_conflict(receipt_path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(shell_file_error(backup_path, error)),
        }
    }
    Ok(true)
}

#[cfg(any(unix, test))]
fn write_file_transaction_receipt(
    receipt_path: &Path,
    receipt: &FileTransactionReceipt,
) -> TorbenResult<()> {
    let content = serde_json::to_vec_pretty(receipt).map_err(|error| {
        TorbenError::internal("Could not serialize the shell integration transaction receipt.")
            .with_detail("reason", error.to_string())
    })?;
    if content.len() as u64 > FILE_TRANSACTION_RECEIPT_MAX_BYTES {
        return Err(TorbenError::new(
            "shell_integration_receipt_too_large",
            "The shell integration transaction receipt exceeds the supported size.",
        )
        .with_detail("actualBytes", content.len().to_string())
        .with_detail(
            "maximumBytes",
            FILE_TRANSACTION_RECEIPT_MAX_BYTES.to_string(),
        ));
    }
    atomic_write(receipt_path, &content)
}

#[cfg(any(unix, test))]
fn read_file_transaction_receipt(
    receipt_path: &Path,
) -> TorbenResult<Option<FileTransactionReceipt>> {
    let Some(content) =
        inspect_bounded_regular_file(receipt_path, FILE_TRANSACTION_RECEIPT_MAX_BYTES)?
    else {
        return Ok(None);
    };
    serde_json::from_slice(&content).map(Some).map_err(|error| {
        TorbenError::new(
            "shell_integration_transaction_invalid",
            "The shell integration transaction receipt is invalid.",
        )
        .with_detail("path", receipt_path.display().to_string())
        .with_detail("reason", error.to_string())
    })
}

#[cfg(any(unix, test))]
fn validate_file_transaction(
    receipt_path: &Path,
    receipt: &FileTransactionReceipt,
    expected_shim_path: &str,
    expected_profiles: &[(PathBuf, ProfileKind)],
) -> TorbenResult<()> {
    let expected_parent = receipt_path
        .parent()
        .ok_or_else(|| file_transaction_conflict(receipt_path))?;
    let expected_transaction_dir =
        expected_parent.join(format!("shell-integration-{}", receipt.operation_id));
    if receipt.schema_version != 1
        || receipt.shim_path != expected_shim_path
        || receipt.transaction_dir != expected_transaction_dir
        || receipt.entries.len() != expected_profiles.len()
    {
        return Err(file_transaction_conflict(receipt_path));
    }
    for (index, (entry, (expected_path, expected_kind))) in
        receipt.entries.iter().zip(expected_profiles).enumerate()
    {
        let expected_backup = entry
            .original_sha256
            .as_ref()
            .map(|_| receipt.transaction_dir.join(format!("{index}.original")));
        if entry.path != *expected_path
            || entry.kind != *expected_kind
            || entry.backup_path != expected_backup
            || !valid_sha256(&entry.updated_sha256)
            || entry
                .original_sha256
                .as_ref()
                .is_some_and(|hash| !valid_sha256(hash))
        {
            return Err(file_transaction_conflict(receipt_path));
        }
    }
    Ok(())
}

#[cfg(any(unix, test))]
fn validate_prepared_backup_evidence(
    receipt_path: &Path,
    receipt: &FileTransactionReceipt,
    expected_shim_path: &str,
) -> TorbenResult<()> {
    if receipt.phase != FileTransactionPhase::Prepared {
        return Err(file_transaction_conflict(receipt_path));
    }
    for entry in &receipt.entries {
        let original = match &entry.backup_path {
            Some(backup_path) => inspect_regular_file(backup_path)?
                .ok_or_else(|| file_transaction_conflict(receipt_path))?,
            None => Vec::new(),
        };
        if let Some(expected_hash) = &entry.original_sha256
            && sha256_bytes(&original) != *expected_hash
        {
            return Err(file_transaction_conflict(receipt_path));
        }
        let original_text =
            String::from_utf8(original).map_err(|_| file_transaction_conflict(receipt_path))?;
        let updated = match receipt.action {
            FileTransactionAction::Enable => upsert_managed_block(
                &original_text,
                &managed_block(entry.kind, expected_shim_path),
            )?,
            FileTransactionAction::Disable => remove_managed_block(&original_text)?,
        };
        if sha256_bytes(updated.as_bytes()) != entry.updated_sha256 {
            return Err(file_transaction_conflict(receipt_path));
        }
    }
    Ok(())
}

#[cfg(any(unix, test))]
fn cleanup_file_transaction(
    receipt_path: &Path,
    receipt: &FileTransactionReceipt,
) -> TorbenResult<()> {
    for entry in &receipt.entries {
        let Some(backup_path) = &entry.backup_path else {
            continue;
        };
        match std::fs::symlink_metadata(backup_path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                std::fs::remove_file(backup_path)
                    .map_err(|error| shell_file_error(backup_path, error))?;
            }
            Ok(_) => return Err(file_transaction_conflict(receipt_path)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(shell_file_error(backup_path, error)),
        }
    }
    match std::fs::remove_dir(&receipt.transaction_dir) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(shell_file_error(&receipt.transaction_dir, error)),
    }
    match std::fs::remove_file(receipt_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(shell_file_error(receipt_path, error)),
    }
}

#[cfg(any(unix, test))]
fn remove_file_if_exists(path: &Path) -> TorbenResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(shell_file_error(path, error)),
    }
}

#[cfg(any(unix, test))]
fn content_state_matches(content: Option<&[u8]>, expected_hash: Option<&str>) -> bool {
    match (content, expected_hash) {
        (Some(content), Some(expected_hash)) => sha256_bytes(content) == expected_hash,
        (None, None) => true,
        _ => false,
    }
}

#[cfg(any(unix, test))]
fn sha256_bytes(content: &[u8]) -> String {
    use sha2::Digest;

    hex::encode(sha2::Sha256::digest(content))
}

#[cfg(any(unix, test))]
fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(any(unix, test))]
fn file_transaction_conflict(path: &Path) -> TorbenError {
    TorbenError::new(
        "shell_integration_recovery_conflict",
        "Shell integration files changed outside the recorded transaction and were preserved.",
    )
    .with_detail("path", path.display().to_string())
    .with_remediation(
        "Inspect the shell profiles and transaction receipt before retrying Torben App.",
    )
}

#[cfg(windows)]
mod windows {
    use std::{path::Path, process::Command};

    use serde::{Deserialize, Serialize};

    use torben_contracts::OperationId;

    use super::{
        ShellIntegrationBackend, ShellIntegrationState, ShellIntegrationStatus, TorbenError,
        TorbenResult, atomic_write, inspect_bounded_regular_file, path_text, prepend_windows_path,
        remove_first_windows_path, should_delete_windows_path, status, windows_path_contains,
    };

    const TARGET: &str = "HKCU\\Environment\\Path";
    const OWNERSHIP_RECEIPT_MAX_BYTES: u64 = 16 * 1024;
    const TRANSACTION_RECEIPT_MAX_BYTES: u64 = 128 * 1024;
    const READ_SCRIPT: &str = r"$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false); $exists=$false; $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$false); if($null -eq $key){$value='';$expandedValue='';$kind='ExpandString'}else{try{$kind=$key.GetValueKind('Path').ToString();$value=[string]$key.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames);$expandedValue=[string]$key.GetValue('Path','');$exists=$true}catch{$value='';$expandedValue='';$kind='ExpandString'}finally{$key.Dispose()}}; [Console]::Out.Write((ConvertTo-Json -Compress @{value=$value;expandedValue=$expandedValue;kind=$kind;exists=$exists}))";
    const WRITE_SCRIPT: &str = r#"$ErrorActionPreference='Stop'; $key=[Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment'); try{$kind=[Microsoft.Win32.RegistryValueKind]::$env:TORBEN_SHELL_PATH_KIND;$key.SetValue('Path',$env:TORBEN_SHELL_PATH_VALUE,$kind)}finally{$key.Dispose()}; try{Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class TorbenEnvironmentBroadcast { [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result); }'; $result=[UIntPtr]::Zero; [void][TorbenEnvironmentBroadcast]::SendMessageTimeout([IntPtr]0xffff,0x1a,[UIntPtr]::Zero,'Environment',2,5000,[ref]$result)}catch{}"#;
    const DELETE_SCRIPT: &str = r#"$ErrorActionPreference='Stop'; $key=[Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment',$true); if($null -ne $key){try{$key.DeleteValue('Path',$false)}finally{$key.Dispose()}}; try{Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class TorbenEnvironmentDeleteBroadcast { [DllImport("user32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint Msg, UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result); }'; $result=[UIntPtr]::Zero; [void][TorbenEnvironmentDeleteBroadcast]::SendMessageTimeout([IntPtr]0xffff,0x1a,[UIntPtr]::Zero,'Environment',2,5000,[ref]$result)}catch{}"#;

    #[derive(Debug, Clone, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct UserPathValue {
        value: String,
        expanded_value: String,
        kind: String,
        exists: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Receipt {
        schema_version: u32,
        shim_path: String,
        #[serde(default = "default_path_existed")]
        path_existed: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum TransactionAction {
        Enable,
        Disable,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RegistryPathState {
        exists: bool,
        kind: String,
        value: String,
    }

    impl From<&UserPathValue> for RegistryPathState {
        fn from(value: &UserPathValue) -> Self {
            Self {
                exists: value.exists,
                kind: value.kind.clone(),
                value: value.value.clone(),
            }
        }
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct TransactionReceipt {
        schema_version: u32,
        operation_id: OperationId,
        action: TransactionAction,
        shim_path: String,
        original_path: RegistryPathState,
        updated_path: RegistryPathState,
        original_ownership: Option<Receipt>,
        updated_ownership: Option<Receipt>,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum RecoveryDecision {
        CleanupCommitted,
        RollBack,
    }

    const fn default_path_existed() -> bool {
        // Receipts written before this field existed cannot prove that Torben created
        // the registry value. Preserve an empty value instead of deleting user state.
        true
    }

    pub(super) struct WindowsShellIntegration {
        receipt_path: std::path::PathBuf,
        transaction_path: std::path::PathBuf,
    }

    impl WindowsShellIntegration {
        pub(super) const fn new(
            receipt_path: std::path::PathBuf,
            transaction_path: std::path::PathBuf,
        ) -> Self {
            Self {
                receipt_path,
                transaction_path,
            }
        }

        fn read_receipt(&self) -> TorbenResult<Option<Receipt>> {
            let Some(content) =
                inspect_bounded_regular_file(&self.receipt_path, OWNERSHIP_RECEIPT_MAX_BYTES)?
            else {
                return Ok(None);
            };
            let receipt: Receipt = serde_json::from_slice(&content).map_err(|error| {
                TorbenError::new(
                    "shell_integration_receipt_invalid",
                    "The shell integration ownership receipt is invalid.",
                )
                .with_detail("path", self.receipt_path.display().to_string())
                .with_detail("reason", error.to_string())
            })?;
            if receipt.schema_version != 1 {
                return Err(TorbenError::new(
                    "shell_integration_receipt_incompatible",
                    "The shell integration ownership receipt has an unsupported version.",
                )
                .with_detail("schemaVersion", receipt.schema_version.to_string()));
            }
            Ok(Some(receipt))
        }

        fn write_receipt(&self, shim_path: &str, path_existed: bool) -> TorbenResult<()> {
            let content = serde_json::to_vec_pretty(&Receipt {
                schema_version: 1,
                shim_path: shim_path.to_owned(),
                path_existed,
            })
            .map_err(|error| {
                TorbenError::internal("Could not serialize the shell integration receipt.")
                    .with_detail("reason", error.to_string())
            })?;
            atomic_write(&self.receipt_path, &content)
        }

        fn remove_receipt(&self) -> TorbenResult<()> {
            match std::fs::remove_file(&self.receipt_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(TorbenError::new(
                    "shell_integration_receipt_remove_failed",
                    "Could not remove the shell integration ownership receipt.",
                )
                .with_detail("path", self.receipt_path.display().to_string())
                .with_detail("reason", error.to_string())),
            }
        }

        fn write_transaction(&self, receipt: &TransactionReceipt) -> TorbenResult<()> {
            let content = serde_json::to_vec_pretty(receipt).map_err(|error| {
                TorbenError::internal("Could not serialize the shell integration transaction.")
                    .with_detail("reason", error.to_string())
            })?;
            if content.len() as u64 > TRANSACTION_RECEIPT_MAX_BYTES {
                return Err(TorbenError::new(
                    "shell_integration_receipt_too_large",
                    "The user PATH is too large to protect with a shell integration transaction.",
                )
                .with_detail("actualBytes", content.len().to_string())
                .with_detail("maximumBytes", TRANSACTION_RECEIPT_MAX_BYTES.to_string()));
            }
            atomic_write(&self.transaction_path, &content)
        }

        fn read_transaction(&self) -> TorbenResult<Option<TransactionReceipt>> {
            let Some(content) = inspect_bounded_regular_file(
                &self.transaction_path,
                TRANSACTION_RECEIPT_MAX_BYTES,
            )?
            else {
                return Ok(None);
            };
            let receipt =
                serde_json::from_slice::<TransactionReceipt>(&content).map_err(|error| {
                    TorbenError::new(
                        "shell_integration_transaction_invalid",
                        "The shell integration transaction receipt is invalid.",
                    )
                    .with_detail("path", self.transaction_path.display().to_string())
                    .with_detail("reason", error.to_string())
                })?;
            if receipt.schema_version != 1 {
                return Err(TorbenError::new(
                    "shell_integration_transaction_incompatible",
                    "The shell integration transaction receipt has an unsupported version.",
                )
                .with_detail("schemaVersion", receipt.schema_version.to_string()));
            }
            Ok(Some(receipt))
        }

        fn remove_transaction(&self) -> TorbenResult<()> {
            remove_receipt_file(&self.transaction_path, "transaction")
        }

        fn apply_ownership(&self, receipt: Option<&Receipt>) -> TorbenResult<()> {
            match receipt {
                Some(receipt) => {
                    // The transaction receipt already contains both ownership states. Removing
                    // the old receipt first avoids the non-atomic Windows replace gap; recovery
                    // treats the temporary absence as a rollback-only intermediate state.
                    self.remove_receipt()?;
                    self.write_receipt(&receipt.shim_path, receipt.path_existed)
                }
                None => self.remove_receipt(),
            }
        }

        fn recover_transaction(&self, expected_shim_path: &str) -> TorbenResult<()> {
            let Some(transaction) = self.read_transaction()? else {
                return Ok(());
            };
            validate_transaction(&transaction, expected_shim_path)?;
            let current_path = read_user_path()?;
            let current_path = RegistryPathState::from(&current_path);
            let current_ownership = self.read_receipt()?;
            match recovery_decision(&transaction, &current_path, current_ownership.as_ref())? {
                RecoveryDecision::CleanupCommitted => return self.remove_transaction(),
                RecoveryDecision::RollBack => {}
            }
            apply_registry_path(&transaction.original_path)?;
            self.apply_ownership(transaction.original_ownership.as_ref())?;
            self.remove_transaction()
        }

        fn execute_transaction(&self, transaction: TransactionReceipt) -> TorbenResult<()> {
            self.write_transaction(&transaction)?;
            let result = apply_registry_path(&transaction.updated_path)
                .and_then(|()| self.apply_ownership(transaction.updated_ownership.as_ref()))
                .and_then(|()| self.remove_transaction());
            if let Err(error) = result {
                let rollback_complete = self.recover_transaction(&transaction.shim_path).is_ok();
                return Err(error.with_detail("rollbackComplete", rollback_complete.to_string()));
            }
            Ok(())
        }
    }

    impl ShellIntegrationBackend for WindowsShellIntegration {
        fn recover(&self, shim_path: &Path) -> TorbenResult<()> {
            self.recover_transaction(path_text(shim_path)?)
        }

        fn status(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
            let shim_path = path_text(shim_path)?;
            let user_path = read_user_path()?;
            let receipt = self.read_receipt()?;
            let state = match receipt {
                Some(receipt) if receipt.shim_path == shim_path => {
                    if windows_path_contains(&user_path.value, shim_path) {
                        ShellIntegrationState::Managed
                    } else {
                        ShellIntegrationState::Outdated
                    }
                }
                Some(receipt) => {
                    if windows_path_contains(&user_path.value, &receipt.shim_path) {
                        ShellIntegrationState::Outdated
                    } else if windows_path_contains(&user_path.value, shim_path) {
                        ShellIntegrationState::External
                    } else {
                        ShellIntegrationState::Outdated
                    }
                }
                None if windows_path_contains(&user_path.expanded_value, shim_path) => {
                    ShellIntegrationState::External
                }
                None => ShellIntegrationState::Disabled,
            };
            status(state, Path::new(shim_path), vec![TARGET.to_owned()])
        }

        fn enable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
            let shim_path = path_text(shim_path)?;
            let user_path = read_user_path()?;
            let receipt = self.read_receipt()?;
            if receipt.is_none() && windows_path_contains(&user_path.expanded_value, shim_path) {
                return status(
                    ShellIntegrationState::External,
                    Path::new(shim_path),
                    vec![TARGET.to_owned()],
                );
            }
            let mut updated = user_path.value.clone();
            let path_existed = receipt
                .as_ref()
                .map_or(user_path.exists, |receipt| receipt.path_existed);
            if let Some(receipt) = &receipt {
                updated = remove_first_windows_path(&updated, &receipt.shim_path);
            }
            if !windows_path_contains(&updated, shim_path) {
                updated = prepend_windows_path(&updated, shim_path);
            }
            let original_path = RegistryPathState::from(&user_path);
            let updated_path = RegistryPathState {
                exists: true,
                kind: user_path.kind.clone(),
                value: updated,
            };
            self.execute_transaction(TransactionReceipt {
                schema_version: 1,
                operation_id: OperationId::new(),
                action: TransactionAction::Enable,
                shim_path: shim_path.to_owned(),
                original_path,
                updated_path,
                original_ownership: receipt,
                updated_ownership: Some(Receipt {
                    schema_version: 1,
                    shim_path: shim_path.to_owned(),
                    path_existed,
                }),
            })?;
            status(
                ShellIntegrationState::Managed,
                Path::new(shim_path),
                vec![TARGET.to_owned()],
            )
        }

        fn disable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
            let shim_path = path_text(shim_path)?;
            let user_path = read_user_path()?;
            let Some(receipt) = self.read_receipt()? else {
                if windows_path_contains(&user_path.value, shim_path) {
                    return Err(TorbenError::new(
                        "shell_integration_not_managed",
                        "The shim path was not added by Torben App and will not be removed.",
                    )
                    .with_detail("path", shim_path));
                }
                return status(
                    ShellIntegrationState::Disabled,
                    Path::new(shim_path),
                    vec![TARGET.to_owned()],
                );
            };
            let updated = remove_first_windows_path(&user_path.value, &receipt.shim_path);
            let original_path = RegistryPathState::from(&user_path);
            let delete_path = should_delete_windows_path(&updated, receipt.path_existed);
            let updated_path = RegistryPathState {
                exists: !delete_path,
                kind: if delete_path {
                    "ExpandString".to_owned()
                } else {
                    user_path.kind.clone()
                },
                value: updated.clone(),
            };
            self.execute_transaction(TransactionReceipt {
                schema_version: 1,
                operation_id: OperationId::new(),
                action: TransactionAction::Disable,
                shim_path: shim_path.to_owned(),
                original_path,
                updated_path,
                original_ownership: Some(receipt),
                updated_ownership: None,
            })?;
            let state = if windows_path_contains(&updated, shim_path) {
                ShellIntegrationState::External
            } else {
                ShellIntegrationState::Disabled
            };
            status(state, Path::new(shim_path), vec![TARGET.to_owned()])
        }
    }

    fn validate_transaction(
        transaction: &TransactionReceipt,
        expected_shim_path: &str,
    ) -> TorbenResult<()> {
        if transaction.shim_path != expected_shim_path
            || !matches!(
                transaction.original_path.kind.as_str(),
                "String" | "ExpandString"
            )
            || !matches!(
                transaction.updated_path.kind.as_str(),
                "String" | "ExpandString"
            )
        {
            return Err(transaction_conflict(Path::new(expected_shim_path)));
        }
        let expected_updated_value = match transaction.action {
            TransactionAction::Enable => {
                let mut value = transaction.original_path.value.clone();
                if let Some(receipt) = &transaction.original_ownership {
                    value = remove_first_windows_path(&value, &receipt.shim_path);
                }
                if !windows_path_contains(&value, expected_shim_path) {
                    value = prepend_windows_path(&value, expected_shim_path);
                }
                value
            }
            TransactionAction::Disable => {
                let Some(receipt) = &transaction.original_ownership else {
                    return Err(transaction_conflict(Path::new(expected_shim_path)));
                };
                remove_first_windows_path(&transaction.original_path.value, &receipt.shim_path)
            }
        };
        let original_ownership_valid = transaction
            .original_ownership
            .as_ref()
            .is_none_or(|receipt| receipt.schema_version == 1 && !receipt.shim_path.is_empty());
        let expected_path_existed = transaction
            .original_ownership
            .as_ref()
            .map_or(transaction.original_path.exists, |receipt| {
                receipt.path_existed
            });
        let ownership_valid = match transaction.action {
            TransactionAction::Enable => {
                transaction
                    .updated_ownership
                    .as_ref()
                    .is_some_and(|receipt| {
                        receipt.schema_version == 1
                            && receipt.shim_path == expected_shim_path
                            && receipt.path_existed == expected_path_existed
                    })
            }
            TransactionAction::Disable => transaction.updated_ownership.is_none(),
        };
        let expected_exists = match transaction.action {
            TransactionAction::Enable => true,
            TransactionAction::Disable => !should_delete_windows_path(
                &expected_updated_value,
                transaction
                    .original_ownership
                    .as_ref()
                    .is_none_or(|receipt| receipt.path_existed),
            ),
        };
        if !original_ownership_valid
            || !ownership_valid
            || transaction.updated_path.value != expected_updated_value
            || transaction.updated_path.exists != expected_exists
            || transaction.updated_path.kind
                != if expected_exists {
                    transaction.original_path.kind.clone()
                } else {
                    "ExpandString".to_owned()
                }
        {
            return Err(transaction_conflict(Path::new(expected_shim_path)));
        }
        Ok(())
    }

    fn matches_known_state<T: PartialEq>(current: T, original: T, updated: T) -> bool {
        current == original || current == updated
    }

    fn recovery_decision(
        transaction: &TransactionReceipt,
        current_path: &RegistryPathState,
        current_ownership: Option<&Receipt>,
    ) -> TorbenResult<RecoveryDecision> {
        if *current_path == transaction.updated_path
            && current_ownership == transaction.updated_ownership.as_ref()
        {
            return Ok(RecoveryDecision::CleanupCommitted);
        }
        let ownership_is_replace_gap = current_ownership.is_none()
            && transaction.original_ownership.is_some()
            && transaction.updated_ownership.is_some();
        if matches_known_state(
            current_path,
            &transaction.original_path,
            &transaction.updated_path,
        ) && (matches_known_state(
            current_ownership,
            transaction.original_ownership.as_ref(),
            transaction.updated_ownership.as_ref(),
        ) || ownership_is_replace_gap)
        {
            Ok(RecoveryDecision::RollBack)
        } else {
            Err(transaction_conflict(Path::new(&transaction.shim_path)))
        }
    }

    fn apply_registry_path(path: &RegistryPathState) -> TorbenResult<()> {
        if path.exists {
            write_user_path(&path.value, &path.kind)
        } else {
            delete_user_path()
        }
    }

    fn remove_receipt_file(path: &Path, kind: &str) -> TorbenResult<()> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(TorbenError::new(
                "shell_integration_receipt_remove_failed",
                "Could not remove a shell integration receipt.",
            )
            .with_detail("path", path.display().to_string())
            .with_detail("receiptKind", kind)
            .with_detail("reason", error.to_string())),
        }
    }

    fn transaction_conflict(path: &Path) -> TorbenError {
        TorbenError::new(
            "shell_integration_recovery_conflict",
            "Shell integration changed outside the recorded transaction and was preserved.",
        )
        .with_detail("path", path.display().to_string())
        .with_remediation(
            "Inspect the user PATH and shell integration receipts before retrying Torben App.",
        )
    }

    fn powershell() -> TorbenResult<std::path::PathBuf> {
        let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
            TorbenError::new(
                "windows_system_root_unavailable",
                "Could not locate the Windows system directory.",
            )
        })?;
        let path = std::path::PathBuf::from(system_root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        if !path.is_file() {
            return Err(TorbenError::new(
                "windows_powershell_unavailable",
                "Windows PowerShell is required to update the user PATH safely.",
            )
            .with_detail("path", path.display().to_string()));
        }
        Ok(path)
    }

    fn read_user_path() -> TorbenResult<UserPathValue> {
        let output = Command::new(powershell()?)
            .args([
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                READ_SCRIPT,
            ])
            .output()
            .map_err(shell_command_error)?;
        if !output.status.success() {
            return Err(shell_command_failure(&output));
        }
        serde_json::from_slice(&output.stdout).map_err(|error| {
            TorbenError::new(
                "shell_integration_response_invalid",
                "Windows returned an invalid user PATH response.",
            )
            .with_detail("reason", error.to_string())
        })
    }

    fn write_user_path(value: &str, kind: &str) -> TorbenResult<()> {
        if !matches!(kind, "String" | "ExpandString") {
            return Err(TorbenError::new(
                "shell_path_registry_type_unsupported",
                "The user PATH registry value has an unsupported type.",
            )
            .with_detail("kind", kind));
        }
        run_write_command(WRITE_SCRIPT, Some((value, kind)))
    }

    fn delete_user_path() -> TorbenResult<()> {
        run_write_command(DELETE_SCRIPT, None)
    }

    fn run_write_command(script: &str, environment: Option<(&str, &str)>) -> TorbenResult<()> {
        let mut command = Command::new(powershell()?);
        command.args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ]);
        if let Some((value, kind)) = environment {
            command
                .env("TORBEN_SHELL_PATH_VALUE", value)
                .env("TORBEN_SHELL_PATH_KIND", kind);
        }
        let output = command.output().map_err(shell_command_error)?;
        if output.status.success() {
            Ok(())
        } else {
            Err(shell_command_failure(&output))
        }
    }

    fn shell_command_error(error: std::io::Error) -> TorbenError {
        TorbenError::new(
            "shell_integration_command_failed",
            "Could not run the Windows user environment integration command.",
        )
        .with_detail("reason", error.to_string())
    }

    fn shell_command_failure(output: &std::process::Output) -> TorbenError {
        TorbenError::new(
            "shell_integration_command_failed",
            "Windows rejected the user environment integration change.",
        )
        .with_detail("exitCode", output.status.code().unwrap_or(-1).to_string())
        .with_detail("reason", String::from_utf8_lossy(&output.stderr).trim())
        .with_remediation(
            "Retry without elevation; if the user Environment key is policy-managed, contact the device administrator.",
        )
    }

    #[cfg(test)]
    mod tests {
        use super::{
            OperationId, Receipt, RecoveryDecision, RegistryPathState, TransactionAction,
            TransactionReceipt, recovery_decision, validate_transaction,
        };

        #[test]
        fn enable_transaction_preserves_the_original_path_ownership() {
            let shim = r"C:\Torben\shims";
            let transaction = TransactionReceipt {
                schema_version: 1,
                operation_id: OperationId::new(),
                action: TransactionAction::Enable,
                shim_path: shim.to_owned(),
                original_path: RegistryPathState {
                    exists: true,
                    kind: "ExpandString".to_owned(),
                    value: format!(r"{shim};C:\Tools"),
                },
                updated_path: RegistryPathState {
                    exists: true,
                    kind: "ExpandString".to_owned(),
                    value: format!(r"{shim};C:\Tools"),
                },
                original_ownership: Some(Receipt {
                    schema_version: 1,
                    shim_path: shim.to_owned(),
                    path_existed: false,
                }),
                updated_ownership: Some(Receipt {
                    schema_version: 1,
                    shim_path: shim.to_owned(),
                    path_existed: false,
                }),
            };

            validate_transaction(&transaction, shim).unwrap();
        }

        #[test]
        fn disable_transaction_normalizes_a_deleted_registry_value() {
            let shim = r"C:\Torben\shims";
            let transaction = TransactionReceipt {
                schema_version: 1,
                operation_id: OperationId::new(),
                action: TransactionAction::Disable,
                shim_path: shim.to_owned(),
                original_path: RegistryPathState {
                    exists: true,
                    kind: "String".to_owned(),
                    value: shim.to_owned(),
                },
                updated_path: RegistryPathState {
                    exists: false,
                    kind: "ExpandString".to_owned(),
                    value: String::new(),
                },
                original_ownership: Some(Receipt {
                    schema_version: 1,
                    shim_path: shim.to_owned(),
                    path_existed: false,
                }),
                updated_ownership: None,
            };

            validate_transaction(&transaction, shim).unwrap();
        }

        #[test]
        fn windows_transaction_rejects_an_altered_path_plan() {
            let shim = r"C:\Torben\shims";
            let transaction = TransactionReceipt {
                schema_version: 1,
                operation_id: OperationId::new(),
                action: TransactionAction::Enable,
                shim_path: shim.to_owned(),
                original_path: RegistryPathState {
                    exists: true,
                    kind: "String".to_owned(),
                    value: r"C:\Tools".to_owned(),
                },
                updated_path: RegistryPathState {
                    exists: true,
                    kind: "String".to_owned(),
                    value: format!(r"{shim};C:\Unexpected"),
                },
                original_ownership: None,
                updated_ownership: Some(Receipt {
                    schema_version: 1,
                    shim_path: shim.to_owned(),
                    path_existed: true,
                }),
            };

            let error = validate_transaction(&transaction, shim).unwrap_err();

            assert_eq!(error.code, "shell_integration_recovery_conflict");
        }

        #[test]
        fn windows_recovery_cleans_only_a_fully_applied_transaction() {
            let transaction = enable_transaction();

            let decision = recovery_decision(
                &transaction,
                &transaction.updated_path,
                transaction.updated_ownership.as_ref(),
            )
            .unwrap();

            assert_eq!(decision, RecoveryDecision::CleanupCommitted);
        }

        #[test]
        fn windows_recovery_rolls_back_a_single_sided_path_commit() {
            let transaction = enable_transaction();

            let decision = recovery_decision(
                &transaction,
                &transaction.updated_path,
                transaction.original_ownership.as_ref(),
            )
            .unwrap();

            assert_eq!(decision, RecoveryDecision::RollBack);
        }

        #[test]
        fn windows_recovery_treats_missing_replacement_ownership_as_rollback_only() {
            let mut transaction = enable_transaction();
            transaction.original_ownership = Some(Receipt {
                schema_version: 1,
                shim_path: r"C:\OldTorben\shims".to_owned(),
                path_existed: true,
            });

            let decision =
                recovery_decision(&transaction, &transaction.updated_path, None).unwrap();

            assert_eq!(decision, RecoveryDecision::RollBack);
        }

        #[test]
        fn windows_recovery_preserves_an_external_registry_change() {
            let transaction = enable_transaction();
            let external = RegistryPathState {
                exists: true,
                kind: "String".to_owned(),
                value: r"C:\External".to_owned(),
            };

            let error = recovery_decision(
                &transaction,
                &external,
                transaction.original_ownership.as_ref(),
            )
            .unwrap_err();

            assert_eq!(error.code, "shell_integration_recovery_conflict");
        }

        fn enable_transaction() -> TransactionReceipt {
            let shim = r"C:\Torben\shims";
            TransactionReceipt {
                schema_version: 1,
                operation_id: OperationId::new(),
                action: TransactionAction::Enable,
                shim_path: shim.to_owned(),
                original_path: RegistryPathState {
                    exists: true,
                    kind: "String".to_owned(),
                    value: r"C:\Tools".to_owned(),
                },
                updated_path: RegistryPathState {
                    exists: true,
                    kind: "String".to_owned(),
                    value: format!(r"{shim};C:\Tools"),
                },
                original_ownership: None,
                updated_ownership: Some(Receipt {
                    schema_version: 1,
                    shim_path: shim.to_owned(),
                    path_existed: true,
                }),
            }
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::path::{Path, PathBuf};

    use directories::BaseDirs;

    use super::{
        FileChange, FileTransactionAction, ManagedBlockState, ProfileKind, ShellIntegrationBackend,
        ShellIntegrationState, ShellIntegrationStatus, TorbenError, TorbenResult,
        execute_file_transaction, inspect_regular_file, managed_block, managed_block_state,
        path_text, recover_file_transaction, remove_managed_block, status, upsert_managed_block,
    };

    #[derive(Debug)]
    struct ProfileTarget {
        path: PathBuf,
        kind: ProfileKind,
    }

    pub(super) struct UnixShellIntegration {
        profiles: Vec<ProfileTarget>,
        transaction_path: PathBuf,
    }

    impl UnixShellIntegration {
        pub(super) fn discover(transaction_path: PathBuf) -> TorbenResult<Self> {
            let base = BaseDirs::new().ok_or_else(|| {
                TorbenError::new(
                    "home_directory_unavailable",
                    "Could not resolve the user home directory for shell integration.",
                )
            })?;
            let home = base.home_dir();
            let fish_config = std::env::var_os("XDG_CONFIG_HOME")
                .map_or_else(|| home.join(".config"), PathBuf::from)
                .join("fish")
                .join("conf.d")
                .join("torben.fish");
            Ok(Self {
                profiles: vec![
                    ProfileTarget {
                        path: home.join(".profile"),
                        kind: ProfileKind::Posix,
                    },
                    ProfileTarget {
                        path: home.join(".zprofile"),
                        kind: ProfileKind::Posix,
                    },
                    ProfileTarget {
                        path: fish_config,
                        kind: ProfileKind::Fish,
                    },
                ],
                transaction_path,
            })
        }

        fn inspect(&self, shim_path: &str) -> TorbenResult<Vec<ManagedBlockState>> {
            self.profiles
                .iter()
                .map(|profile| {
                    let content = inspect_regular_file(&profile.path)?.unwrap_or_default();
                    let content = String::from_utf8(content).map_err(|error| {
                        TorbenError::new(
                            "shell_profile_not_utf8",
                            "A shell profile is not valid UTF-8 and will not be modified.",
                        )
                        .with_detail("path", profile.path.display().to_string())
                        .with_detail("reason", error.to_string())
                    })?;
                    managed_block_state(&content, &managed_block(profile.kind, shim_path))
                })
                .collect()
        }

        fn targets(&self) -> Vec<String> {
            self.profiles
                .iter()
                .map(|profile| profile.path.display().to_string())
                .collect()
        }

        fn changes(&self, shim_path: &str, enable: bool) -> TorbenResult<Vec<FileChange>> {
            self.profiles
                .iter()
                .map(|profile| {
                    let original = inspect_regular_file(&profile.path)?;
                    let content = String::from_utf8(original.clone().unwrap_or_default()).map_err(
                        |error| {
                            TorbenError::new(
                                "shell_profile_not_utf8",
                                "A shell profile is not valid UTF-8 and will not be modified.",
                            )
                            .with_detail("path", profile.path.display().to_string())
                            .with_detail("reason", error.to_string())
                        },
                    )?;
                    let updated = if enable {
                        upsert_managed_block(&content, &managed_block(profile.kind, shim_path))?
                    } else {
                        remove_managed_block(&content)?
                    };
                    Ok(FileChange {
                        path: profile.path.clone(),
                        kind: profile.kind,
                        original,
                        updated: updated.into_bytes(),
                    })
                })
                .collect()
        }

        fn expected_profiles(&self) -> Vec<(PathBuf, ProfileKind)> {
            self.profiles
                .iter()
                .map(|profile| (profile.path.clone(), profile.kind))
                .collect()
        }

        fn recover_transaction(&self, shim_path: &str) -> TorbenResult<()> {
            recover_file_transaction(&self.transaction_path, shim_path, &self.expected_profiles())
        }
    }

    impl ShellIntegrationBackend for UnixShellIntegration {
        fn recover(&self, shim_path: &Path) -> TorbenResult<()> {
            let shim_path = path_text(shim_path)?;
            validate_unix_path(shim_path)?;
            self.recover_transaction(shim_path)
        }

        fn status(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
            let shim_path_text = path_text(shim_path)?;
            validate_unix_path(shim_path_text)?;
            let states = self.inspect(shim_path_text)?;
            let state = if states
                .iter()
                .all(|state| *state == ManagedBlockState::Exact)
            {
                ShellIntegrationState::Managed
            } else if states
                .iter()
                .any(|state| *state != ManagedBlockState::Missing)
            {
                ShellIntegrationState::Outdated
            } else if std::env::var_os("PATH")
                .is_some_and(|value| std::env::split_paths(&value).any(|path| path == shim_path))
            {
                ShellIntegrationState::External
            } else {
                ShellIntegrationState::Disabled
            };
            status(state, shim_path, self.targets())
        }

        fn enable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
            let shim_path_text = path_text(shim_path)?;
            validate_unix_path(shim_path_text)?;
            self.recover_transaction(shim_path_text)?;
            let states = self.inspect(shim_path_text)?;
            if states
                .iter()
                .all(|state| *state == ManagedBlockState::Missing)
                && std::env::var_os("PATH").is_some_and(|value| {
                    std::env::split_paths(&value).any(|path| path == shim_path)
                })
            {
                return status(ShellIntegrationState::External, shim_path, self.targets());
            }
            execute_file_transaction(
                &self.transaction_path,
                shim_path_text,
                FileTransactionAction::Enable,
                &self.changes(shim_path_text, true)?,
            )?;
            status(ShellIntegrationState::Managed, shim_path, self.targets())
        }

        fn disable(&self, shim_path: &Path) -> TorbenResult<ShellIntegrationStatus> {
            let shim_path_text = path_text(shim_path)?;
            validate_unix_path(shim_path_text)?;
            self.recover_transaction(shim_path_text)?;
            let states = self.inspect(shim_path_text)?;
            if states
                .iter()
                .all(|state| *state == ManagedBlockState::Missing)
            {
                if std::env::var_os("PATH").is_some_and(|value| {
                    std::env::split_paths(&value).any(|path| path == shim_path)
                }) {
                    return Err(TorbenError::new(
                        "shell_integration_not_managed",
                        "The shim path was not added by Torben App and will not be removed.",
                    )
                    .with_detail("path", shim_path_text));
                }
                return status(ShellIntegrationState::Disabled, shim_path, self.targets());
            }
            execute_file_transaction(
                &self.transaction_path,
                shim_path_text,
                FileTransactionAction::Disable,
                &self.changes(shim_path_text, false)?,
            )?;
            status(ShellIntegrationState::Disabled, shim_path, self.targets())
        }
    }

    fn validate_unix_path(shim_path: &str) -> TorbenResult<()> {
        if shim_path.contains(':') || shim_path.contains('\n') || shim_path.contains('\r') {
            return Err(TorbenError::new(
                "shell_path_unsupported",
                "The shim path contains characters that cannot be represented safely in PATH.",
            )
            .with_detail("path", shim_path));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        BLOCK_END, BLOCK_START, FileChange, FileTransactionAction, FileTransactionEntry,
        FileTransactionPhase, FileTransactionReceipt, ProfileKind, apply_file_changes,
        atomic_write, execute_file_transaction, managed_block, managed_block_state,
        prepend_windows_path, recover_file_transaction, remove_first_windows_path,
        remove_managed_block, sha256_bytes, should_delete_windows_path, upsert_managed_block,
        windows_path_contains, write_file_transaction_receipt,
    };

    #[test]
    fn windows_path_changes_are_idempotent_and_preserve_unrelated_entries() {
        let shim = r"C:\Users\Test\Torben\shims";
        let original = r"C:\Windows;C:\Tools;";
        let enabled = prepend_windows_path(original, shim);

        assert!(windows_path_contains(
            &enabled,
            r"c:/users/test/torben/shims/"
        ));
        assert_eq!(remove_first_windows_path(&enabled, shim), original);
        assert_eq!(
            remove_first_windows_path(&format!("{shim};{shim};C:\\Tools"), shim),
            format!("{shim};C:\\Tools")
        );
    }

    #[test]
    fn windows_path_comparison_handles_non_ascii_case() {
        assert!(windows_path_contains(
            r"C:\Users\JÖRG\Torben\shims",
            r"c:\users\jörg\torben\shims"
        ));
    }

    #[test]
    fn windows_path_value_is_deleted_only_when_torben_created_it_and_no_entries_remain() {
        assert!(should_delete_windows_path("", false));
        assert!(!should_delete_windows_path("", true));
        assert!(!should_delete_windows_path(r"C:\Tools", false));
    }

    #[test]
    fn profile_block_round_trips_without_changing_user_content() {
        let original = "export EDITOR=vim\n";
        let block = managed_block(ProfileKind::Posix, "/Users/torben/app shims");
        let fish_block = managed_block(ProfileKind::Fish, "/Users/torben/app shims");
        let enabled = upsert_managed_block(original, &block).unwrap();

        assert_eq!(
            managed_block_state(&enabled, &block).unwrap(),
            super::ManagedBlockState::Exact
        );
        assert!(enabled.contains("'/Users/torben/app shims'"));
        assert!(fish_block.contains("set -gx PATH '/Users/torben/app shims' $PATH"));
        assert!(block.contains("case \":$PATH:\" in"));
        assert_eq!(remove_managed_block(&enabled).unwrap(), original);
    }

    #[test]
    fn malformed_profile_markers_fail_closed() {
        let malformed = format!("{BLOCK_START}\nexport PATH=/tmp:$PATH\n");
        let error = remove_managed_block(&malformed).unwrap_err();

        assert_eq!(error.code, "shell_profile_conflict");
        assert!(!malformed.contains(BLOCK_END));
    }

    #[test]
    fn multi_file_failure_rolls_back_prior_profile_changes() {
        let root = tempdir().unwrap();
        let first = root.path().join("first.profile");
        std::fs::write(&first, b"original").unwrap();
        let blocked_parent = root.path().join("not-a-directory");
        std::fs::write(&blocked_parent, b"conflict").unwrap();
        let changes = vec![
            FileChange {
                path: first.clone(),
                kind: ProfileKind::Posix,
                original: Some(b"original".to_vec()),
                updated: b"updated".to_vec(),
            },
            FileChange {
                path: blocked_parent.join("profile"),
                kind: ProfileKind::Posix,
                original: None,
                updated: b"unreachable".to_vec(),
            },
        ];

        let error = apply_file_changes(&changes).unwrap_err();

        assert_eq!(
            error.details.get("rollbackComplete").map(String::as_str),
            Some("true")
        );
        assert_eq!(std::fs::read(first).unwrap(), b"original");
    }

    #[test]
    fn file_transaction_commits_all_profiles_and_cleans_evidence() {
        let root = tempdir().unwrap();
        let transaction_path = root.path().join("shell-transaction.json");
        let first = root.path().join("profile");
        let second = root.path().join("zprofile");
        std::fs::write(&first, b"export EDITOR=vim\n").unwrap();
        let shim = "/opt/torben/shims";
        let changes = file_changes_for_test(
            &[
                (first.clone(), ProfileKind::Posix),
                (second.clone(), ProfileKind::Posix),
            ],
            shim,
            FileTransactionAction::Enable,
        );

        execute_file_transaction(
            &transaction_path,
            shim,
            FileTransactionAction::Enable,
            &changes,
        )
        .unwrap();

        assert!(!transaction_path.exists());
        assert!(
            String::from_utf8(std::fs::read(first).unwrap())
                .unwrap()
                .contains(BLOCK_START)
        );
        assert!(
            String::from_utf8(std::fs::read(second).unwrap())
                .unwrap()
                .contains(BLOCK_START)
        );
    }

    #[test]
    fn startup_rolls_back_a_receipt_bound_partial_profile_commit() {
        let root = tempdir().unwrap();
        let transaction_path = root.path().join("shell-transaction.json");
        let first = root.path().join("profile");
        let second = root.path().join("zprofile");
        std::fs::write(&first, b"first original\n").unwrap();
        std::fs::write(&second, b"second original\n").unwrap();
        let shim = "/opt/torben/shims";
        let profiles = vec![
            (first.clone(), ProfileKind::Posix),
            (second.clone(), ProfileKind::Posix),
        ];
        let changes = file_changes_for_test(&profiles, shim, FileTransactionAction::Enable);
        let receipt = write_prepared_transaction_for_test(
            &transaction_path,
            shim,
            FileTransactionAction::Enable,
            &changes,
        );
        atomic_write(&first, &changes[0].updated).unwrap();

        recover_file_transaction(&transaction_path, shim, &profiles).unwrap();

        assert_eq!(std::fs::read(first).unwrap(), b"first original\n");
        assert_eq!(std::fs::read(second).unwrap(), b"second original\n");
        assert!(!transaction_path.exists());
        assert!(!receipt.transaction_dir.exists());
    }

    #[test]
    fn startup_cleans_a_receipt_bound_interrupted_profile_preparation() {
        let root = tempdir().unwrap();
        let transaction_path = root.path().join("shell-transaction.json");
        let profile = root.path().join("profile");
        std::fs::write(&profile, b"original\n").unwrap();
        let shim = "/opt/torben/shims";
        let profiles = vec![(profile.clone(), ProfileKind::Posix)];
        let changes = file_changes_for_test(&profiles, shim, FileTransactionAction::Enable);
        let mut receipt = write_prepared_transaction_for_test(
            &transaction_path,
            shim,
            FileTransactionAction::Enable,
            &changes,
        );
        receipt.phase = FileTransactionPhase::Preparing;
        write_file_transaction_receipt(&transaction_path, &receipt).unwrap();

        recover_file_transaction(&transaction_path, shim, &profiles).unwrap();

        assert_eq!(std::fs::read(profile).unwrap(), b"original\n");
        assert!(!transaction_path.exists());
        assert!(!receipt.transaction_dir.exists());
    }

    #[test]
    fn startup_finishes_receipt_only_cleanup_after_profile_commit() {
        let root = tempdir().unwrap();
        let transaction_path = root.path().join("shell-transaction.json");
        let profile = root.path().join("profile");
        std::fs::write(&profile, b"original\n").unwrap();
        let shim = "/opt/torben/shims";
        let profiles = vec![(profile.clone(), ProfileKind::Posix)];
        let changes = file_changes_for_test(&profiles, shim, FileTransactionAction::Enable);
        let receipt = write_prepared_transaction_for_test(
            &transaction_path,
            shim,
            FileTransactionAction::Enable,
            &changes,
        );
        atomic_write(&profile, &changes[0].updated).unwrap();
        for entry in &receipt.entries {
            if let Some(backup) = &entry.backup_path {
                std::fs::remove_file(backup).unwrap();
            }
        }
        std::fs::remove_dir(&receipt.transaction_dir).unwrap();

        recover_file_transaction(&transaction_path, shim, &profiles).unwrap();

        assert_eq!(std::fs::read(profile).unwrap(), changes[0].updated);
        assert!(!transaction_path.exists());
    }

    #[test]
    fn startup_preserves_profiles_changed_outside_the_transaction() {
        let root = tempdir().unwrap();
        let transaction_path = root.path().join("shell-transaction.json");
        let profile = root.path().join("profile");
        std::fs::write(&profile, b"original\n").unwrap();
        let shim = "/opt/torben/shims";
        let profiles = vec![(profile.clone(), ProfileKind::Posix)];
        let changes = file_changes_for_test(&profiles, shim, FileTransactionAction::Enable);
        let receipt = write_prepared_transaction_for_test(
            &transaction_path,
            shim,
            FileTransactionAction::Enable,
            &changes,
        );
        std::fs::write(&profile, b"external change\n").unwrap();

        let error = recover_file_transaction(&transaction_path, shim, &profiles).unwrap_err();

        assert_eq!(error.code, "shell_integration_recovery_conflict");
        assert_eq!(std::fs::read(profile).unwrap(), b"external change\n");
        assert!(transaction_path.exists());
        assert!(receipt.transaction_dir.exists());
    }

    #[test]
    fn startup_preserves_a_partial_commit_with_an_altered_backup() {
        let root = tempdir().unwrap();
        let transaction_path = root.path().join("shell-transaction.json");
        let profile = root.path().join("profile");
        std::fs::write(&profile, b"original\n").unwrap();
        let shim = "/opt/torben/shims";
        let profiles = vec![(profile.clone(), ProfileKind::Posix)];
        let changes = file_changes_for_test(&profiles, shim, FileTransactionAction::Enable);
        let receipt = write_prepared_transaction_for_test(
            &transaction_path,
            shim,
            FileTransactionAction::Enable,
            &changes,
        );
        atomic_write(&profile, &changes[0].updated).unwrap();
        std::fs::write(
            receipt.entries[0].backup_path.as_ref().unwrap(),
            b"altered backup\n",
        )
        .unwrap();

        let error = recover_file_transaction(&transaction_path, shim, &profiles).unwrap_err();

        assert_eq!(error.code, "shell_integration_recovery_conflict");
        assert_eq!(std::fs::read(profile).unwrap(), changes[0].updated);
        assert!(transaction_path.exists());
        assert!(receipt.transaction_dir.exists());
    }

    #[test]
    fn startup_rejects_a_transaction_bound_to_another_profile() {
        let root = tempdir().unwrap();
        let transaction_path = root.path().join("shell-transaction.json");
        let profile = root.path().join("profile");
        std::fs::write(&profile, b"original\n").unwrap();
        let shim = "/opt/torben/shims";
        let profiles = vec![(profile.clone(), ProfileKind::Posix)];
        let changes = file_changes_for_test(&profiles, shim, FileTransactionAction::Enable);
        let receipt = write_prepared_transaction_for_test(
            &transaction_path,
            shim,
            FileTransactionAction::Enable,
            &changes,
        );
        let other_profiles = vec![(root.path().join("other"), ProfileKind::Posix)];

        let error = recover_file_transaction(&transaction_path, shim, &other_profiles).unwrap_err();

        assert_eq!(error.code, "shell_integration_recovery_conflict");
        assert!(transaction_path.exists());
        assert!(receipt.transaction_dir.exists());
        assert_eq!(std::fs::read(profile).unwrap(), b"original\n");
    }

    fn file_changes_for_test(
        profiles: &[(std::path::PathBuf, ProfileKind)],
        shim: &str,
        action: FileTransactionAction,
    ) -> Vec<FileChange> {
        profiles
            .iter()
            .map(|(path, kind)| {
                let original = super::inspect_regular_file(path).unwrap();
                let text = String::from_utf8(original.clone().unwrap_or_default()).unwrap();
                let updated = match action {
                    FileTransactionAction::Enable => {
                        upsert_managed_block(&text, &managed_block(*kind, shim)).unwrap()
                    }
                    FileTransactionAction::Disable => remove_managed_block(&text).unwrap(),
                };
                FileChange {
                    path: path.clone(),
                    kind: *kind,
                    original,
                    updated: updated.into_bytes(),
                }
            })
            .collect()
    }

    fn write_prepared_transaction_for_test(
        transaction_path: &std::path::Path,
        shim: &str,
        action: FileTransactionAction,
        changes: &[FileChange],
    ) -> FileTransactionReceipt {
        let operation_id = torben_contracts::OperationId::new();
        let transaction_dir = transaction_path
            .parent()
            .unwrap()
            .join(format!("shell-integration-{operation_id}"));
        std::fs::create_dir(&transaction_dir).unwrap();
        let entries = changes
            .iter()
            .enumerate()
            .map(|(index, change)| {
                let backup_path = change.original.as_ref().map(|original| {
                    let path = transaction_dir.join(format!("{index}.original"));
                    atomic_write(&path, original).unwrap();
                    path
                });
                FileTransactionEntry {
                    path: change.path.clone(),
                    kind: change.kind,
                    original_sha256: change.original.as_deref().map(sha256_bytes),
                    updated_sha256: sha256_bytes(&change.updated),
                    backup_path,
                }
            })
            .collect();
        let receipt = FileTransactionReceipt {
            schema_version: 1,
            operation_id,
            action,
            phase: FileTransactionPhase::Prepared,
            shim_path: shim.to_owned(),
            transaction_dir,
            entries,
        };
        write_file_transaction_receipt(transaction_path, &receipt).unwrap();
        receipt
    }
}
