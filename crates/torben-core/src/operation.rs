use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use serde::{Deserialize, Serialize};
use torben_contracts::{
    AppId, ExactVersion, ManagedToPackageMigrationPlan, OperationEvent, OperationId, OperationKind,
    OperationState, PackageToManagedMigrationPlan, PluginId, SourceAction, SourceAdapterKind,
    SourceExecutionRequest, SourceMigrationPlan, SourcePackageKind, SourcePackageVersion,
    TorbenError, TorbenResult,
};

use crate::{StateStore, TorbenPaths};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct JournalFile {
    operation_id: OperationId,
    kind: OperationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_id: Option<AppId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plugin_id: Option<PluginId>,
    version: Option<ExactVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    migration: Option<MigrationSubject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source: Option<SourceOperationSubject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_migration: Option<SourceMigrationSubject>,
    events: Vec<OperationEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MigrationSubject {
    pub(crate) source: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) staging: PathBuf,
    #[serde(default)]
    pub(crate) target_existed: bool,
    #[serde(default)]
    pub(crate) target_committed: bool,
    #[serde(default)]
    pub(crate) source_cleanup_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceOperationSubject {
    pub(crate) action: SourceAction,
    pub(crate) adapter: SourceAdapterKind,
    pub(crate) coordinate: torben_contracts::PackageCoordinate,
    pub(crate) package_kind: SourcePackageKind,
    pub(crate) package_version: Option<SourcePackageVersion>,
    pub(crate) executable_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) approved_execution_identity: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum SourceMigrationSubject {
    PackageToPackage(Box<SourceMigrationPlan>),
    ManagedToPackage(Box<ManagedToPackageMigrationPlan>),
    PackageToManaged(Box<PackageToManagedMigrationPlan>),
}

pub struct OperationJournal {
    path: PathBuf,
    log_dir: PathBuf,
    journal: JournalFile,
    store: Arc<StateStore>,
}

#[derive(Default)]
struct JournalSubject {
    app_id: Option<AppId>,
    plugin_id: Option<PluginId>,
    version: Option<ExactVersion>,
    migration: Option<MigrationSubject>,
    source: Option<SourceOperationSubject>,
    source_migration: Option<SourceMigrationSubject>,
}

#[derive(Clone)]
pub(crate) struct CancellationProbe {
    operation_id: OperationId,
    path: PathBuf,
}

impl CancellationProbe {
    pub(crate) fn check(&self) -> TorbenResult<()> {
        if self.path.exists() {
            Err(TorbenError::new(
                "operation_cancelled",
                "The operation was cancelled by the user.",
            )
            .with_detail("operationId", self.operation_id.to_string())
            .with_remediation("Review rollback status before starting another operation."))
        } else {
            Ok(())
        }
    }
}

impl OperationJournal {
    pub fn start(
        paths: &TorbenPaths,
        store: Arc<StateStore>,
        kind: OperationKind,
        app_id: &AppId,
        version: Option<&ExactVersion>,
    ) -> TorbenResult<Self> {
        Self::start_with_subject(
            paths,
            store,
            kind,
            JournalSubject {
                app_id: Some(app_id.clone()),
                version: version.cloned(),
                ..JournalSubject::default()
            },
        )
    }

    pub fn start_plugin(
        paths: &TorbenPaths,
        store: Arc<StateStore>,
        plugin_id: &PluginId,
        version: &ExactVersion,
    ) -> TorbenResult<Self> {
        Self::start_with_subject(
            paths,
            store,
            OperationKind::PluginInstall,
            JournalSubject {
                plugin_id: Some(plugin_id.clone()),
                version: Some(version.clone()),
                ..JournalSubject::default()
            },
        )
    }

    pub(crate) fn start_migration(
        paths: &TorbenPaths,
        store: Arc<StateStore>,
        source: PathBuf,
        target: PathBuf,
        staging: PathBuf,
        target_existed: bool,
    ) -> TorbenResult<Self> {
        Self::start_with_subject(
            paths,
            store,
            OperationKind::Migrate,
            JournalSubject {
                migration: Some(MigrationSubject {
                    source,
                    target,
                    staging,
                    target_existed,
                    target_committed: false,
                    source_cleanup_pending: false,
                }),
                ..JournalSubject::default()
            },
        )
    }

    pub(crate) fn start_source(
        paths: &TorbenPaths,
        store: Arc<StateStore>,
        request: &SourceExecutionRequest,
    ) -> TorbenResult<Self> {
        let kind = match request.action {
            SourceAction::Install => OperationKind::SourceInstall,
            SourceAction::Uninstall => OperationKind::SourceUninstall,
        };
        Self::start_with_subject(
            paths,
            store,
            kind,
            JournalSubject {
                app_id: Some(request.app_id.clone()),
                version: Some(request.app_version.clone()),
                source: Some(SourceOperationSubject {
                    action: request.action,
                    adapter: request.adapter,
                    coordinate: request.coordinate.clone(),
                    package_kind: request.package_kind,
                    package_version: request.package_version.clone(),
                    executable_path: request.executable_path.clone(),
                    approved_execution_identity: request.approved_execution_identity.clone(),
                }),
                ..JournalSubject::default()
            },
        )
    }

    pub(crate) fn start_source_migration(
        paths: &TorbenPaths,
        store: Arc<StateStore>,
        plan: &SourceMigrationPlan,
    ) -> TorbenResult<Self> {
        Self::start_with_subject(
            paths,
            store,
            OperationKind::SourceMigrate,
            JournalSubject {
                app_id: Some(plan.app_id.clone()),
                version: Some(plan.app_version.clone()),
                source_migration: Some(SourceMigrationSubject::PackageToPackage(Box::new(
                    plan.clone(),
                ))),
                ..JournalSubject::default()
            },
        )
    }

    pub(crate) fn start_managed_to_package_migration(
        paths: &TorbenPaths,
        store: Arc<StateStore>,
        plan: &ManagedToPackageMigrationPlan,
    ) -> TorbenResult<Self> {
        Self::start_with_subject(
            paths,
            store,
            OperationKind::SourceMigrate,
            JournalSubject {
                app_id: Some(plan.app_id.clone()),
                version: Some(plan.app_version.clone()),
                source_migration: Some(SourceMigrationSubject::ManagedToPackage(Box::new(
                    plan.clone(),
                ))),
                ..JournalSubject::default()
            },
        )
    }

    pub(crate) fn start_package_to_managed_migration(
        paths: &TorbenPaths,
        store: Arc<StateStore>,
        plan: &PackageToManagedMigrationPlan,
    ) -> TorbenResult<Self> {
        Self::start_with_subject(
            paths,
            store,
            OperationKind::SourceMigrate,
            JournalSubject {
                app_id: Some(plan.app_id.clone()),
                version: Some(plan.app_version.clone()),
                source_migration: Some(SourceMigrationSubject::PackageToManaged(Box::new(
                    plan.clone(),
                ))),
                ..JournalSubject::default()
            },
        )
    }

    fn start_with_subject(
        paths: &TorbenPaths,
        store: Arc<StateStore>,
        kind: OperationKind,
        subject: JournalSubject,
    ) -> TorbenResult<Self> {
        let operation_id = OperationId::new();
        let path = paths.operation_dir().join(format!("{operation_id}.json"));
        let mut this = Self {
            path,
            log_dir: paths.log_dir().to_path_buf(),
            store,
            journal: JournalFile {
                operation_id,
                kind,
                app_id: subject.app_id,
                plugin_id: subject.plugin_id,
                version: subject.version,
                migration: subject.migration,
                source: subject.source,
                source_migration: subject.source_migration,
                events: Vec::new(),
            },
        };
        this.record(
            OperationState::Running,
            "prepare",
            "Operation started",
            Some(0.0),
        )?;
        Ok(this)
    }

    pub fn operation_id(&self) -> OperationId {
        self.journal.operation_id
    }

    pub(crate) const fn kind(&self) -> OperationKind {
        self.journal.kind
    }

    pub(crate) fn latest_phase(&self) -> Option<&str> {
        self.journal.events.last().map(|event| event.phase.as_str())
    }

    pub(crate) fn app_id(&self) -> Option<&AppId> {
        self.journal.app_id.as_ref()
    }

    pub(crate) fn plugin_id(&self) -> Option<&PluginId> {
        self.journal.plugin_id.as_ref()
    }

    pub(crate) fn version(&self) -> Option<&ExactVersion> {
        self.journal.version.as_ref()
    }

    pub(crate) fn migration(&self) -> Option<&MigrationSubject> {
        self.journal.migration.as_ref()
    }

    pub(crate) fn set_migration_source_cleanup_pending(
        &mut self,
        pending: bool,
    ) -> TorbenResult<()> {
        let migration = self.journal.migration.as_mut().ok_or_else(|| {
            TorbenError::new(
                "operation_journal_invalid",
                "The operation journal has no managed-library migration subject.",
            )
        })?;
        migration.source_cleanup_pending = pending;
        self.persist()
    }

    pub(crate) fn set_migration_target_committed(&mut self) -> TorbenResult<()> {
        let migration = self.journal.migration.as_mut().ok_or_else(|| {
            TorbenError::new(
                "operation_journal_invalid",
                "The operation journal has no managed-library migration subject.",
            )
        })?;
        migration.target_committed = true;
        self.persist()
    }

    pub(crate) fn source(&self) -> Option<&SourceOperationSubject> {
        self.journal.source.as_ref()
    }

    pub(crate) fn source_migration(&self) -> Option<&SourceMigrationSubject> {
        self.journal.source_migration.as_ref()
    }

    pub(crate) fn fail_reconciled(&mut self, error: &TorbenError) -> TorbenResult<()> {
        self.fail(error)?;
        self.record(
            OperationState::Failed,
            "reconcile",
            "External package-manager state was inspected; ownership was not changed",
            None,
        )?;
        self.cleanup_cancellation_marker();
        Ok(())
    }

    pub(crate) fn fail_reconciliation_required(&mut self, error: &TorbenError) -> TorbenResult<()> {
        self.fail(error)?;
        self.cleanup_cancellation_marker();
        Ok(())
    }

    pub(crate) fn set_version(&mut self, version: &ExactVersion) -> TorbenResult<()> {
        if let Some(existing) = &self.journal.version
            && existing != version
        {
            return Err(TorbenError::new(
                "operation_version_conflict",
                "The operation exact version cannot be changed after resolution.",
            )
            .with_detail("existingVersion", existing.to_string())
            .with_detail("requestedVersion", version.to_string()));
        }
        self.journal.version = Some(version.clone());
        self.persist()
    }

    pub fn record(
        &mut self,
        state: OperationState,
        phase: impl Into<String>,
        message: impl Into<String>,
        progress: Option<f32>,
    ) -> TorbenResult<()> {
        let event = OperationEvent {
            operation_id: self.journal.operation_id,
            sequence: self.journal.events.len() as u64,
            state,
            phase: phase.into(),
            message: message.into(),
            progress,
            timestamp: timestamp(),
        };
        self.journal.events.push(event);
        self.persist()
    }

    pub fn succeed(&mut self, message: impl Into<String>) -> TorbenResult<()> {
        self.record(OperationState::Succeeded, "complete", message, Some(1.0))?;
        self.cleanup_cancellation_marker();
        Ok(())
    }

    pub(crate) fn fail(&mut self, error: &TorbenError) -> TorbenResult<()> {
        self.record(
            OperationState::Failed,
            "failed",
            format!("{}: {}", error.code, error.message),
            None,
        )
    }

    pub fn fail_and_rollback(&mut self, error: &TorbenError) -> TorbenResult<()> {
        self.fail(error)?;
        self.record(
            OperationState::RolledBack,
            "rollback",
            "Operation rolled back",
            None,
        )?;
        self.cleanup_cancellation_marker();
        Ok(())
    }

    pub(crate) fn recover_rollback(&mut self, message: impl Into<String>) -> TorbenResult<()> {
        self.record(OperationState::Failed, "recovery", message, None)?;
        self.record(
            OperationState::RolledBack,
            "rollback",
            "Interrupted operation rolled back during startup recovery",
            None,
        )?;
        self.cleanup_cancellation_marker();
        Ok(())
    }

    pub(crate) fn cancellation_probe(&self) -> CancellationProbe {
        CancellationProbe {
            operation_id: self.operation_id(),
            path: cancellation_marker(&self.path),
        }
    }

    pub(crate) fn acknowledge_cancellation(&mut self) -> TorbenResult<()> {
        self.record(
            OperationState::Cancelling,
            "cancel",
            "Cancellation requested; rolling back the active operation",
            None,
        )
    }

    pub(crate) fn request_cancellation(
        paths: &TorbenPaths,
        store: &StateStore,
        operation_id: OperationId,
    ) -> TorbenResult<()> {
        let content = store.get_operation_journal(operation_id)?.ok_or_else(|| {
            TorbenError::new(
                "operation_not_found",
                "The requested operation was not found.",
            )
            .with_detail("operationId", operation_id.to_string())
        })?;
        let journal = read_projected_journal(&content)?;
        let latest = journal.events.last().ok_or_else(|| {
            TorbenError::new(
                "operation_journal_invalid",
                "The requested operation has no state events.",
            )
        })?;
        if !matches!(
            journal.kind,
            OperationKind::Install | OperationKind::PluginInstall
        ) || !matches!(
            latest.state,
            OperationState::Running | OperationState::Cancelling
        ) {
            cleanup_cancellation_artifacts(paths, operation_id);
            return Err(TorbenError::new(
                "operation_not_cancellable",
                "The requested operation cannot be cancelled in its current state.",
            )
            .with_detail("operationId", operation_id.to_string())
            .with_detail("kind", format!("{:?}", journal.kind).to_ascii_lowercase())
            .with_detail("state", format!("{:?}", latest.state).to_ascii_lowercase()));
        }
        let marker = paths
            .operation_dir()
            .join(format!("{operation_id}.json.cancel"));
        if marker.exists() {
            return Ok(());
        }
        let pending = persistence_artifact(&marker, "next");
        write_synced(&pending, operation_id.to_string().as_bytes())?;
        match std::fs::rename(&pending, &marker) {
            Ok(()) => Ok(()),
            Err(_) if marker.exists() => {
                let _ = std::fs::remove_file(pending);
                Ok(())
            }
            Err(error) => Err(io_error(error)),
        }
    }

    pub(crate) fn interrupted(
        paths: &TorbenPaths,
        store: Arc<StateStore>,
    ) -> TorbenResult<Vec<Self>> {
        repair_persistence_artifacts(paths)?;
        let mut journals = read_journals(paths, store)?
            .into_iter()
            .filter(Self::requires_recovery)
            .collect::<Vec<_>>();
        journals.sort_by(|left, right| left.path.cmp(&right.path));
        Ok(journals)
    }

    pub fn list(store: &StateStore) -> TorbenResult<Vec<OperationEvent>> {
        let mut events = Vec::new();
        for content in store.list_operation_journals()? {
            events.extend(read_projected_journal(&content)?.events);
        }
        events.sort_by(|left, right| right.timestamp.cmp(&left.timestamp));
        Ok(events)
    }

    fn persist(&self) -> TorbenResult<()> {
        let content = serde_json::to_string_pretty(&self.journal).map_err(|error| {
            TorbenError::new(
                "operation_journal_serialize_failed",
                "Could not serialize an operation journal.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let pending = persistence_artifact(&self.path, "next");
        let previous = persistence_artifact(&self.path, "previous");
        write_synced(&pending, content.as_bytes())?;

        if previous.exists() {
            std::fs::remove_file(&previous).map_err(io_error)?;
        }
        if self.path.exists() {
            std::fs::rename(&self.path, &previous).map_err(io_error)?;
        }
        if let Err(error) = std::fs::rename(&pending, &self.path) {
            if previous.exists() && !self.path.exists() {
                let _ = std::fs::rename(&previous, &self.path);
            }
            return Err(io_error(error));
        }
        let _ = std::fs::remove_file(previous);
        self.project(&content)?;
        if let Some(event) = self.journal.events.last() {
            let _ = crate::diagnostic_log::record_operation(
                &self.log_dir,
                self.journal.kind,
                self.journal.app_id.as_ref(),
                self.journal.plugin_id.as_ref(),
                self.journal.version.as_ref(),
                event,
            );
        }
        Ok(())
    }

    fn is_terminal(&self) -> bool {
        self.journal.events.last().is_some_and(|event| {
            matches!(
                event.state,
                OperationState::Succeeded | OperationState::RolledBack
            ) || (event.state == OperationState::Failed
                && matches!(
                    self.journal.kind,
                    OperationKind::SourceInstall
                        | OperationKind::SourceUninstall
                        | OperationKind::SourceMigrate
                ))
        })
    }

    fn requires_recovery(&self) -> bool {
        !self.is_terminal()
            || (self.journal.kind == OperationKind::Migrate
                && self
                    .journal
                    .migration
                    .as_ref()
                    .is_some_and(|migration| migration.source_cleanup_pending))
    }

    fn project(&self, content: &str) -> TorbenResult<()> {
        let latest = self.journal.events.last().ok_or_else(|| {
            TorbenError::new(
                "operation_journal_invalid",
                "An operation journal has no events.",
            )
            .with_detail("path", self.path.display().to_string())
        })?;
        self.store.upsert_operation_journal(
            self.journal.operation_id,
            self.journal.kind,
            latest.state,
            content,
            &latest.timestamp,
        )
    }

    fn cleanup_cancellation_marker(&self) {
        cleanup_cancellation_artifacts_from_path(&self.path);
    }
}

fn read_journals(
    paths: &TorbenPaths,
    store: Arc<StateStore>,
) -> TorbenResult<Vec<OperationJournal>> {
    let mut journals = Vec::new();
    for entry in std::fs::read_dir(paths.operation_dir()).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        if entry.path().extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let path = entry.path();
        let journal = OperationJournal {
            journal: read_journal_file(&path)?,
            path,
            log_dir: paths.log_dir().to_path_buf(),
            store: Arc::clone(&store),
        };
        let content = serde_json::to_string_pretty(&journal.journal).map_err(|error| {
            TorbenError::new(
                "operation_journal_serialize_failed",
                "Could not serialize an operation journal.",
            )
            .with_detail("reason", error.to_string())
        })?;
        journal.project(&content)?;
        if journal.is_terminal() {
            journal.cleanup_cancellation_marker();
        }
        journals.push(journal);
    }
    Ok(journals)
}

fn read_projected_journal(content: &str) -> TorbenResult<JournalFile> {
    let journal: JournalFile = serde_json::from_str(content).map_err(|error| {
        TorbenError::new(
            "operation_state_invalid",
            "A projected operation journal is invalid.",
        )
        .with_detail("reason", error.to_string())
    })?;
    validate_journal_subject(&journal, "SQLite operations table")?;
    Ok(journal)
}

fn read_journal_file(path: &Path) -> TorbenResult<JournalFile> {
    let content = std::fs::read(path).map_err(io_error)?;
    let journal: JournalFile = serde_json::from_slice(&content).map_err(|error| {
        TorbenError::new(
            "operation_journal_invalid",
            "An operation journal is invalid.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    validate_journal_subject(&journal, &path.display().to_string())?;
    Ok(journal)
}

fn validate_journal_subject(journal: &JournalFile, location: &str) -> TorbenResult<()> {
    let valid_subject = match journal.kind {
        OperationKind::PluginInstall => journal.plugin_id.is_some() && journal.app_id.is_none(),
        OperationKind::Migrate => {
            journal.app_id.is_none()
                && journal.plugin_id.is_none()
                && journal.migration.is_some()
                && journal.source.is_none()
                && journal.source_migration.is_none()
        }
        OperationKind::SourceInstall | OperationKind::SourceUninstall => {
            journal.app_id.is_some()
                && journal.plugin_id.is_none()
                && journal.migration.is_none()
                && journal.source_migration.is_none()
                && journal.source.as_ref().is_some_and(|source| {
                    matches!(
                        (journal.kind, source.action),
                        (OperationKind::SourceInstall, SourceAction::Install)
                            | (OperationKind::SourceUninstall, SourceAction::Uninstall)
                    )
                })
        }
        OperationKind::SourceMigrate => {
            journal.app_id.is_some()
                && journal.plugin_id.is_none()
                && journal.migration.is_none()
                && journal.source.is_none()
                && journal.source_migration.as_ref().is_some_and(|migration| {
                    let (app_id, version) = match migration {
                        SourceMigrationSubject::PackageToPackage(plan) => {
                            (&plan.app_id, &plan.app_version)
                        }
                        SourceMigrationSubject::ManagedToPackage(plan) => {
                            (&plan.app_id, &plan.app_version)
                        }
                        SourceMigrationSubject::PackageToManaged(plan) => {
                            (&plan.app_id, &plan.app_version)
                        }
                    };
                    journal.app_id.as_ref() == Some(app_id)
                        && journal.version.as_ref() == Some(version)
                })
        }
        _ => {
            journal.app_id.is_some()
                && journal.plugin_id.is_none()
                && journal.migration.is_none()
                && journal.source.is_none()
                && journal.source_migration.is_none()
        }
    };
    if !valid_subject {
        return Err(TorbenError::new(
            "operation_journal_invalid",
            "An operation journal has an invalid subject.",
        )
        .with_detail("location", location));
    }
    if journal.events.is_empty()
        || journal.events.iter().enumerate().any(|(sequence, event)| {
            event.operation_id != journal.operation_id || event.sequence != sequence as u64
        })
    {
        return Err(TorbenError::new(
            "operation_journal_invalid",
            "An operation journal has an invalid event sequence.",
        )
        .with_detail("location", location));
    }
    Ok(())
}

fn repair_persistence_artifacts(paths: &TorbenPaths) -> TorbenResult<()> {
    let mut canonical_paths = Vec::new();
    for entry in std::fs::read_dir(paths.operation_dir()).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if name.ends_with(".json.next") || name.ends_with(".json.previous") {
            let canonical = path.with_extension("");
            if !canonical_paths.contains(&canonical) {
                canonical_paths.push(canonical);
            }
        }
    }

    for canonical in canonical_paths {
        let pending = persistence_artifact(&canonical, "next");
        let previous = persistence_artifact(&canonical, "previous");
        if canonical.exists() {
            read_journal_file(&canonical)?;
            remove_if_exists(&pending)?;
            remove_if_exists(&previous)?;
            continue;
        }

        let source = if pending.exists() && read_journal_file(&pending).is_ok() {
            &pending
        } else if previous.exists() && read_journal_file(&previous).is_ok() {
            &previous
        } else {
            return Err(TorbenError::new(
                "operation_journal_invalid",
                "Interrupted journal persistence left no valid operation journal.",
            )
            .with_detail("path", canonical.display().to_string()));
        };
        std::fs::rename(source, &canonical).map_err(io_error)?;
        remove_if_exists(&pending)?;
        remove_if_exists(&previous)?;
    }
    Ok(())
}

fn persistence_artifact(path: &Path, suffix: &str) -> PathBuf {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map_or_else(|| suffix.to_owned(), |value| format!("{value}.{suffix}"));
    path.with_extension(extension)
}

fn cancellation_marker(journal_path: &Path) -> PathBuf {
    persistence_artifact(journal_path, "cancel")
}

fn cleanup_cancellation_artifacts(paths: &TorbenPaths, operation_id: OperationId) {
    let journal = paths.operation_dir().join(format!("{operation_id}.json"));
    cleanup_cancellation_artifacts_from_path(&journal);
}

fn cleanup_cancellation_artifacts_from_path(journal_path: &Path) {
    let marker = cancellation_marker(journal_path);
    let pending = persistence_artifact(&marker, "next");
    let _ = std::fs::remove_file(marker);
    let _ = std::fs::remove_file(pending);
}

fn write_synced(path: &Path, content: &[u8]) -> TorbenResult<()> {
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .map_err(io_error)?;
    file.write_all(content).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

fn remove_if_exists(path: &Path) -> TorbenResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error(error)),
    }
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or_else(
            |_| "0".to_owned(),
            |duration| duration.as_secs().to_string(),
        )
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "operation_journal_io_failed",
        "Could not access an operation journal.",
    )
    .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::{str::FromStr, sync::Arc};
    use tempfile::tempdir;

    use torben_contracts::{
        AppId, ExactVersion, OperationKind, OperationState, PluginId, TorbenError,
    };

    use crate::{StateStore, TorbenPaths};

    use super::{OperationJournal, SourceMigrationSubject, persistence_artifact};

    fn operation_store(paths: &TorbenPaths) -> Arc<StateStore> {
        Arc::new(StateStore::open(paths.state_database()).unwrap())
    }

    #[test]
    fn repairs_an_interrupted_journal_file_replacement() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let app_id = AppId::new("node").unwrap();
        let journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Install,
            &app_id,
            None,
        )
        .unwrap();
        let canonical = journal.path.clone();
        drop(journal);
        let pending = persistence_artifact(&canonical, "next");
        let previous = persistence_artifact(&canonical, "previous");
        std::fs::copy(&canonical, &pending).unwrap();
        std::fs::rename(&canonical, &previous).unwrap();

        let interrupted = OperationJournal::interrupted(&paths, operation_store(&paths)).unwrap();

        assert_eq!(interrupted.len(), 1);
        assert!(canonical.is_file());
        assert!(!pending.exists());
        assert!(!previous.exists());
    }

    #[test]
    fn failed_journal_remains_available_for_startup_recovery() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let app_id = AppId::new("node").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Uninstall,
            &app_id,
            None,
        )
        .unwrap();
        journal
            .fail(&TorbenError::new(
                "fixture_failure",
                "Recovery is still required.",
            ))
            .unwrap();

        let interrupted = OperationJournal::interrupted(&paths, operation_store(&paths)).unwrap();

        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].operation_id(), journal.operation_id());
    }

    #[test]
    fn persists_terminal_operation_state() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let app_id = AppId::new("node").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            operation_store(&paths),
            OperationKind::Install,
            &app_id,
            None,
        )
        .unwrap();
        journal
            .record(
                OperationState::Running,
                "verify",
                "Verifying fixture",
                Some(0.5),
            )
            .unwrap();
        journal.succeed("Committed").unwrap();

        let events = OperationJournal::list(&operation_store(&paths)).unwrap();
        assert_eq!(events.len(), 3);
        assert!(
            events
                .iter()
                .any(|event| event.state == OperationState::Succeeded)
        );
    }

    #[test]
    fn diagnostic_log_failure_does_not_fail_the_durable_journal() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = operation_store(&paths);
        let app_id = AppId::new("node").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            None,
        )
        .unwrap();
        let blocker = root.path().join("not-a-directory");
        std::fs::write(&blocker, b"fixture").unwrap();
        journal.log_dir = blocker.join("logs");

        journal
            .record(
                OperationState::Running,
                "download",
                "Durable event still commits",
                Some(0.25),
            )
            .unwrap();

        let events = OperationJournal::list(&store).unwrap();
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| event.phase == "download"));
    }

    #[test]
    fn locks_the_exact_version_after_resolution() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = operation_store(&paths);
        let app_id = AppId::new("node").unwrap();
        let resolved = ExactVersion::from_str("24.19.0").unwrap();
        let conflicting = ExactVersion::from_str("22.22.0").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            None,
        )
        .unwrap();
        let operation_id = journal.operation_id();

        journal.set_version(&resolved).unwrap();
        journal.set_version(&resolved).unwrap();
        let error = journal.set_version(&conflicting).unwrap_err();
        drop(journal);

        assert_eq!(error.code, "operation_version_conflict");
        assert_eq!(
            error.details.get("existingVersion").map(String::as_str),
            Some("24.19.0")
        );
        let interrupted = OperationJournal::interrupted(&paths, store).unwrap();
        let persisted = interrupted
            .iter()
            .find(|journal| journal.operation_id() == operation_id)
            .unwrap();
        assert_eq!(persisted.version(), Some(&resolved));
    }

    #[test]
    fn persists_a_plugin_subject_without_an_application_sentinel() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let plugin_id = PluginId::new("dev.example.fixture").unwrap();
        let version = ExactVersion::from_str("1.2.3").unwrap();
        let journal =
            OperationJournal::start_plugin(&paths, operation_store(&paths), &plugin_id, &version)
                .unwrap();

        let interrupted = OperationJournal::interrupted(&paths, operation_store(&paths)).unwrap();

        assert_eq!(interrupted.len(), 1);
        assert_eq!(interrupted[0].operation_id(), journal.operation_id());
        assert_eq!(interrupted[0].plugin_id(), Some(&plugin_id));
        assert_eq!(interrupted[0].app_id(), None);
    }

    #[test]
    fn reads_legacy_untagged_package_to_package_migration_subject() {
        let operation = |action: &str, adapter: &str, source_id: &str, package: &str| {
            serde_json::json!({
                "action": action,
                "adapter": adapter,
                "sourceId": source_id,
                "coordinate": package,
                "packageKind": "native",
                "packageVersion": "1.0.0-1",
                "executable": adapter,
                "previewArguments": [action, package],
                "executeArguments": [action, package],
                "executionIdentity": null,
                "environment": {},
                "requiresElevation": true,
                "exactVersionGuaranteed": true,
                "mutatesSystem": true,
                "warnings": []
            })
        };
        let state = |adapter: &str, source_id: &str, package: &str, installed: bool| {
            serde_json::json!({
                "adapter": adapter,
                "sourceId": source_id,
                "coordinate": package,
                "packageKind": "native",
                "installed": installed,
                "installedVersion": installed.then_some("1.0.0-1"),
                "architecture": installed.then_some("x86_64"),
                "managerOwned": installed
            })
        };
        let value = serde_json::json!({
            "appId": "vscode",
            "appVersion": "1.0.0",
            "currentOwner": {
                "appId": "vscode",
                "appVersion": "1.0.0",
                "sourceId": "source.apt",
                "adapter": "apt",
                "coordinate": "code",
                "packageKind": "native",
                "packageVersion": "1.0.0-1",
                "architecture": "x86_64",
                "executablePath": "/usr/bin/code",
                "ownedByTorben": true,
                "installedAt": "fixture",
                "health": "healthy"
            },
            "currentState": state("apt", "source.apt", "code", true),
            "targetState": state("dnf", "source.dnf", "code", false),
            "uninstallCurrent": operation("uninstall", "apt", "source.apt", "code"),
            "installTarget": operation("install", "dnf", "source.dnf", "code"),
            "cleanupTarget": operation("uninstall", "dnf", "source.dnf", "code"),
            "restoreCurrent": operation("install", "apt", "source.apt", "code"),
            "targetExecutablePath": "/usr/bin/code",
            "approvalToken": "fixture-token",
            "warnings": []
        });

        let subject: SourceMigrationSubject = serde_json::from_value(value).unwrap();

        assert!(matches!(
            subject,
            SourceMigrationSubject::PackageToPackage(_)
        ));
    }

    #[test]
    fn rejects_a_journal_with_both_application_and_plugin_subjects() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let plugin_id = PluginId::new("dev.example.fixture").unwrap();
        let version = ExactVersion::from_str("1.2.3").unwrap();
        let journal =
            OperationJournal::start_plugin(&paths, operation_store(&paths), &plugin_id, &version)
                .unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal.path).unwrap()).unwrap();
        value["appId"] = serde_json::Value::String("node".to_owned());
        std::fs::write(&journal.path, serde_json::to_vec(&value).unwrap()).unwrap();

        let error = OperationJournal::interrupted(&paths, operation_store(&paths))
            .err()
            .unwrap();

        assert_eq!(error.code, "operation_journal_invalid");
        assert!(error.message.contains("invalid subject"));
    }

    #[test]
    fn task_history_remains_in_sqlite_after_a_terminal_file_is_archived() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = operation_store(&paths);
        let app_id = AppId::new("node").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            None,
        )
        .unwrap();
        journal.succeed("Committed").unwrap();
        std::fs::remove_file(&journal.path).unwrap();

        let events = OperationJournal::list(&store).unwrap();

        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .any(|event| event.state == OperationState::Succeeded)
        );
    }

    #[test]
    fn startup_scan_rebuilds_a_missing_sqlite_projection_from_the_file_journal() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = operation_store(&paths);
        let app_id = AppId::new("node").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            None,
        )
        .unwrap();
        journal.succeed("Committed").unwrap();
        rusqlite::Connection::open(paths.state_database())
            .unwrap()
            .execute("DELETE FROM operations", [])
            .unwrap();
        assert!(store.list_operation_journals().unwrap().is_empty());

        OperationJournal::interrupted(&paths, Arc::clone(&store)).unwrap();
        let events = OperationJournal::list(&store).unwrap();

        assert_eq!(events.len(), 2);
        assert_eq!(store.list_operation_journals().unwrap().len(), 1);
    }

    #[test]
    fn cancellation_request_is_cross_connection_idempotent_and_worker_owned() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let worker_store = operation_store(&paths);
        let requester_store = operation_store(&paths);
        let app_id = AppId::new("node").unwrap();
        let mut journal = OperationJournal::start(
            &paths,
            Arc::clone(&worker_store),
            OperationKind::Install,
            &app_id,
            None,
        )
        .unwrap();
        let operation_id = journal.operation_id();

        OperationJournal::request_cancellation(&paths, &requester_store, operation_id).unwrap();
        OperationJournal::request_cancellation(&paths, &requester_store, operation_id).unwrap();

        let cancelled = journal.cancellation_probe().check().unwrap_err();
        assert_eq!(cancelled.code, "operation_cancelled");
        assert_eq!(OperationJournal::list(&worker_store).unwrap().len(), 1);
        journal.acknowledge_cancellation().unwrap();
        journal.fail_and_rollback(&cancelled).unwrap();
        let mut events = OperationJournal::list(&worker_store)
            .unwrap()
            .into_iter()
            .filter(|event| event.operation_id == operation_id)
            .collect::<Vec<_>>();
        events.sort_by_key(|event| event.sequence);
        let states = events
            .into_iter()
            .map(|event| event.state)
            .collect::<Vec<_>>();
        assert_eq!(
            states,
            [
                OperationState::Running,
                OperationState::Cancelling,
                OperationState::Failed,
                OperationState::RolledBack
            ]
        );
        assert!(journal.cancellation_probe().check().is_ok());
    }

    #[test]
    fn terminal_and_short_operations_reject_cancellation() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        paths.ensure_layout().unwrap();
        let store = operation_store(&paths);
        let app_id = AppId::new("node").unwrap();
        let mut completed = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Install,
            &app_id,
            None,
        )
        .unwrap();
        completed.succeed("Committed").unwrap();
        let terminal_error =
            OperationJournal::request_cancellation(&paths, &store, completed.operation_id())
                .unwrap_err();
        assert_eq!(terminal_error.code, "operation_not_cancellable");

        let selection = OperationJournal::start(
            &paths,
            Arc::clone(&store),
            OperationKind::Select,
            &app_id,
            None,
        )
        .unwrap();
        let short_error =
            OperationJournal::request_cancellation(&paths, &store, selection.operation_id())
                .unwrap_err();
        assert_eq!(short_error.code, "operation_not_cancellable");
    }
}
