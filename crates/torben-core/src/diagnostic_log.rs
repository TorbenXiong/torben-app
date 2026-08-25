use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs4::FileExt;
use serde::Serialize;
use torben_contracts::{
    AppId, ExactVersion, OperationEvent, OperationKind, OperationState, PluginId,
};

const LOG_FILE_NAME: &str = "torben.jsonl";
const LOG_BACKUP_FILE_NAME: &str = "torben.jsonl.1";
const LOG_LOCK_FILE_NAME: &str = "diagnostic.lock";
const MAX_LOG_BYTES: u64 = 5 * 1024 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LifecycleEntry<'a> {
    timestamp: String,
    level: &'static str,
    event: &'a str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OperationEntry<'a> {
    timestamp: &'a str,
    level: &'static str,
    event: &'static str,
    operation_id: String,
    sequence: u64,
    kind: OperationKind,
    state: OperationState,
    phase: &'a str,
    progress: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    plugin_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

pub(crate) fn record_core_started(log_dir: &Path) -> std::io::Result<()> {
    append_entry(
        log_dir,
        &LifecycleEntry {
            timestamp: unix_timestamp(),
            level: "info",
            event: "core_started",
        },
    )
}

pub(crate) fn record_operation(
    log_dir: &Path,
    kind: OperationKind,
    app_id: Option<&AppId>,
    plugin_id: Option<&PluginId>,
    version: Option<&ExactVersion>,
    operation: &OperationEvent,
) -> std::io::Result<()> {
    let level = match operation.state {
        OperationState::Failed => "error",
        OperationState::Cancelling | OperationState::RolledBack => "warn",
        _ => "info",
    };
    append_entry(
        log_dir,
        &OperationEntry {
            timestamp: &operation.timestamp,
            level,
            event: "operation_state",
            operation_id: operation.operation_id.to_string(),
            sequence: operation.sequence,
            kind,
            state: operation.state,
            phase: &operation.phase,
            progress: operation.progress,
            app_id: app_id.map(ToString::to_string),
            plugin_id: plugin_id.map(ToString::to_string),
            version: version.map(ToString::to_string),
        },
    )
}

pub(crate) fn probe(log_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(log_dir)?;
    let lock = acquire_lock(log_dir)?;
    let path = log_dir.join(LOG_FILE_NAME);
    let result = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|file| file.sync_all())
        .map(|()| path);
    let _ = FileExt::unlock(&lock);
    result
}

fn append_entry(log_dir: &Path, entry: &impl Serialize) -> std::io::Result<()> {
    append_entry_with_limit(log_dir, entry, MAX_LOG_BYTES)
}

fn append_entry_with_limit(
    log_dir: &Path,
    entry: &impl Serialize,
    max_bytes: u64,
) -> std::io::Result<()> {
    std::fs::create_dir_all(log_dir)?;
    let mut encoded = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
    encoded.push(b'\n');

    let lock = acquire_lock(log_dir)?;
    let result = append_locked(log_dir, &encoded, max_bytes);
    let _ = FileExt::unlock(&lock);
    result
}

fn append_locked(log_dir: &Path, encoded: &[u8], max_bytes: u64) -> std::io::Result<()> {
    let path = log_dir.join(LOG_FILE_NAME);
    let current_bytes = path.metadata().map_or(0, |metadata| metadata.len());
    if current_bytes > 0 && current_bytes.saturating_add(encoded.len() as u64) > max_bytes {
        let backup = log_dir.join(LOG_BACKUP_FILE_NAME);
        remove_if_exists(&backup)?;
        std::fs::rename(&path, backup)?;
    }

    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(encoded)?;
    file.flush()
}

fn acquire_lock(log_dir: &Path) -> std::io::Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(log_dir.join(LOG_LOCK_FILE_NAME))?;
    FileExt::lock(&lock)?;
    Ok(lock)
}

fn remove_if_exists(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn unix_timestamp() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tempfile::tempdir;
    use torben_contracts::{
        AppId, ExactVersion, OperationEvent, OperationId, OperationKind, OperationState,
    };

    use super::{LOG_BACKUP_FILE_NAME, LOG_FILE_NAME, append_entry_with_limit, record_operation};

    #[test]
    fn operation_log_excludes_free_form_message() {
        let root = tempdir().unwrap();
        let log_dir = root.path().join("logs");
        let app_id = AppId::new("node").unwrap();
        let version = ExactVersion::from_str("22.0.0").unwrap();
        let operation = OperationEvent {
            operation_id: OperationId::new(),
            sequence: 2,
            state: OperationState::Failed,
            phase: "health_check".to_owned(),
            message: "secret-token-must-not-be-logged".to_owned(),
            progress: Some(0.8),
            timestamp: "123".to_owned(),
        };

        record_operation(
            &log_dir,
            OperationKind::Install,
            Some(&app_id),
            None,
            Some(&version),
            &operation,
        )
        .unwrap();

        let content = std::fs::read_to_string(log_dir.join(LOG_FILE_NAME)).unwrap();
        assert!(content.contains("\"event\":\"operation_state\""));
        assert!(content.contains("\"state\":\"failed\""));
        assert!(content.contains("\"appId\":\"node\""));
        assert!(!content.contains("secret-token-must-not-be-logged"));
        assert!(!content.contains("\"message\""));
    }

    #[test]
    fn rotates_to_one_bounded_backup() {
        let root = tempdir().unwrap();
        let log_dir = root.path().join("logs");
        let first = super::LifecycleEntry {
            timestamp: "1".to_owned(),
            level: "info",
            event: "first",
        };
        let second = super::LifecycleEntry {
            timestamp: "2".to_owned(),
            level: "info",
            event: "second",
        };

        append_entry_with_limit(&log_dir, &first, 1).unwrap();
        append_entry_with_limit(&log_dir, &second, 1).unwrap();

        let active = std::fs::read_to_string(log_dir.join(LOG_FILE_NAME)).unwrap();
        let backup = std::fs::read_to_string(log_dir.join(LOG_BACKUP_FILE_NAME)).unwrap();
        assert!(active.contains("second"));
        assert!(backup.contains("first"));
    }
}
