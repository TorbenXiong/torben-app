use std::{path::PathBuf, str::FromStr, sync::Mutex};

use rusqlite::{Connection, OptionalExtension, params};
use torben_contracts::{
    AppId, ExactVersion, InstallRecord, InstallScope, OperationId, OperationKind, OperationState,
    PackageCoordinate, PackageInstallationRecord, PluginId, SelectionRecord, SourceAdapterKind,
    SourceId, SourcePackageKind, SourcePackageVersion, TorbenError, TorbenResult, UserSettings,
    plugin::PluginOrigin,
};

const USER_SETTINGS_KEY: &str = "user_preferences";
const MANAGED_LIBRARY_KEY: &str = "managed_library_path";
const CURRENT_SCHEMA_VERSION: i64 = 3;

pub struct StateStore {
    connection: Mutex<Connection>,
}

fn validate_schema_version(connection: &Connection) -> TorbenResult<()> {
    let unsupported = connection
        .query_row(
            "SELECT version FROM schema_migrations
             WHERE version < 1 OR version > ?1
             ORDER BY version DESC LIMIT 1",
            [CURRENT_SCHEMA_VERSION],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(database_error)?;
    let Some(version) = unsupported else {
        return Ok(());
    };
    if version > CURRENT_SCHEMA_VERSION {
        return Err(TorbenError::new(
            "database_schema_newer",
            "The state database was created by a newer Torben App schema.",
        )
        .with_detail("databaseVersion", version.to_string())
        .with_detail("supportedVersion", CURRENT_SCHEMA_VERSION.to_string())
        .with_remediation(
            "Open this data directory with a compatible newer Torben App version, or restore a compatible backup.",
        ));
    }
    Err(TorbenError::new(
        "database_schema_invalid",
        "The state database contains an invalid migration version.",
    )
    .with_detail("databaseVersion", version.to_string())
    .with_detail("supportedVersion", CURRENT_SCHEMA_VERSION.to_string())
    .with_remediation("Inspect or restore the state database before retrying."))
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRecord {
    pub id: PluginId,
    pub version: ExactVersion,
    pub enabled: bool,
    pub manifest_json: String,
    pub origin: PluginOrigin,
}

impl StateStore {
    pub fn open(path: PathBuf) -> TorbenResult<Self> {
        let connection = Connection::open(&path).map_err(|error| {
            TorbenError::new("database_open_failed", "Could not open the state database.")
                .with_detail("path", path.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA journal_mode = WAL;
                 CREATE TABLE IF NOT EXISTS schema_migrations (
                   version INTEGER PRIMARY KEY,
                   applied_at TEXT NOT NULL
                 );",
            )
            .map_err(database_error)?;
        validate_schema_version(&connection)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS installations (
                   app_id TEXT NOT NULL,
                   version TEXT NOT NULL,
                   source_id TEXT NOT NULL,
                   scope TEXT NOT NULL,
                   install_path TEXT NOT NULL,
                   installed_at TEXT NOT NULL,
                   health TEXT NOT NULL,
                   PRIMARY KEY (app_id, version)
                 );
                 CREATE TABLE IF NOT EXISTS selections (
                   app_id TEXT PRIMARY KEY,
                   version TEXT NOT NULL,
                   FOREIGN KEY (app_id, version) REFERENCES installations(app_id, version)
                 );
                 CREATE TABLE IF NOT EXISTS sources (
                   id TEXT PRIMARY KEY,
                   display_name TEXT NOT NULL,
                   managed INTEGER NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS plugins (
                   id TEXT PRIMARY KEY,
                   version TEXT NOT NULL,
                   enabled INTEGER NOT NULL,
                   manifest_json TEXT NOT NULL,
                   origin TEXT NOT NULL DEFAULT 'sideloaded'
                 );
                 CREATE TABLE IF NOT EXISTS operations (
                   id TEXT PRIMARY KEY,
                   kind TEXT NOT NULL,
                   state TEXT NOT NULL,
                   journal_json TEXT NOT NULL,
                   updated_at TEXT NOT NULL
                 );
                 CREATE TABLE IF NOT EXISTS settings (
                   key TEXT PRIMARY KEY,
                   value_json TEXT NOT NULL
                 );
                 INSERT OR IGNORE INTO schema_migrations(version, applied_at)
                   VALUES (1, datetime('now'));
                  INSERT OR IGNORE INTO sources(id, display_name, managed)
                   VALUES ('node.official', 'Official archive', 1);
                  INSERT OR IGNORE INTO sources(id, display_name, managed)
                   VALUES ('temurin.official', 'Eclipse Temurin official archive', 1);
                  INSERT OR IGNORE INTO sources(id, display_name, managed)
                   VALUES ('python.official', 'Official Python distribution', 1);",
            )
            .map_err(database_error)?;
        let has_plugin_origin = {
            let mut statement = connection
                .prepare("PRAGMA table_info(plugins)")
                .map_err(database_error)?;
            let columns = statement
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(database_error)?;
            columns
                .collect::<Result<Vec<_>, _>>()
                .map_err(database_error)?
                .iter()
                .any(|column| column == "origin")
        };
        if !has_plugin_origin {
            connection
                .execute_batch(
                    "ALTER TABLE plugins ADD COLUMN origin TEXT NOT NULL DEFAULT 'sideloaded';",
                )
                .map_err(database_error)?;
        }
        connection
            .execute(
                "INSERT OR IGNORE INTO schema_migrations(version, applied_at)
                   VALUES (2, datetime('now'))",
                [],
            )
            .map_err(database_error)?;
        apply_package_installation_migration(&connection)?;
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn add_installation(&self, record: &InstallRecord) -> TorbenResult<()> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO installations
                 (app_id, version, source_id, scope, install_path, installed_at, health)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(app_id, version) DO UPDATE SET
                   source_id=excluded.source_id,
                   scope=excluded.scope,
                   install_path=excluded.install_path,
                   installed_at=excluded.installed_at,
                   health=excluded.health",
                params![
                    record.app_id.as_str(),
                    record.version.to_string(),
                    record.source_id.as_str(),
                    scope_name(record.scope),
                    record.install_path,
                    record.installed_at,
                    record.health,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn get_installation(
        &self,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<Option<InstallRecord>> {
        let connection = self.lock()?;
        let raw = connection
            .query_row(
                "SELECT app_id, version, source_id, scope, install_path, installed_at, health
                 FROM installations WHERE app_id=?1 AND version=?2",
                params![app_id.as_str(), version.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?;
        raw.map(parse_installation).transpose()
    }

    pub fn list_installations(&self) -> TorbenResult<Vec<InstallRecord>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT app_id, version, source_id, scope, install_path, installed_at, health
                 FROM installations ORDER BY app_id, version DESC",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                ))
            })
            .map_err(database_error)?;
        rows.map(|row| row.map_err(database_error).and_then(parse_installation))
            .collect()
    }

    pub fn remove_installation(&self, app_id: &AppId, version: &ExactVersion) -> TorbenResult<()> {
        let connection = self.lock()?;
        connection
            .execute(
                "DELETE FROM installations WHERE app_id=?1 AND version=?2",
                params![app_id.as_str(), version.to_string()],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn upsert_package_installation(
        &self,
        record: &PackageInstallationRecord,
    ) -> TorbenResult<()> {
        let connection = self.lock()?;
        validate_package_installation_owner(&connection, record)?;
        connection
            .execute(
                "INSERT INTO package_installations
                 (app_id, app_version, source_id, adapter, package_name, package_kind,
                  package_version, architecture, executable_path, owned_by_torben,
                  installed_at, health)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(app_id, app_version) DO UPDATE SET
                   source_id=excluded.source_id,
                   adapter=excluded.adapter,
                   package_name=excluded.package_name,
                   package_kind=excluded.package_kind,
                   package_version=excluded.package_version,
                   architecture=excluded.architecture,
                   executable_path=excluded.executable_path,
                   owned_by_torben=excluded.owned_by_torben,
                   installed_at=excluded.installed_at,
                   health=excluded.health",
                params![
                    record.app_id.as_str(),
                    record.app_version.to_string(),
                    record.source_id.as_str(),
                    source_adapter_name(record.adapter),
                    record.coordinate.as_str(),
                    source_package_kind_name(record.package_kind),
                    record.package_version.as_str(),
                    record.architecture,
                    record.executable_path,
                    i32::from(record.owned_by_torben),
                    record.installed_at,
                    record.health,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn commit_package_installation(
        &self,
        installation: &InstallRecord,
        package: &PackageInstallationRecord,
    ) -> TorbenResult<()> {
        if installation.app_id != package.app_id
            || installation.version != package.app_version
            || installation.source_id != package.source_id
            || installation.scope != InstallScope::PackageManager
        {
            return Err(TorbenError::new(
                "package_installation_parent_mismatch",
                "Package ownership does not match its installation record.",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(database_error)?;
        let existing = transaction
            .query_row(
                "SELECT source_id, scope FROM installations WHERE app_id=?1 AND version=?2",
                params![
                    installation.app_id.as_str(),
                    installation.version.to_string()
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()
            .map_err(database_error)?;
        if let Some((source_id, scope)) = existing
            && (source_id != installation.source_id.as_str()
                || scope != scope_name(InstallScope::PackageManager))
        {
            return Err(TorbenError::new(
                "installation_source_conflict",
                "The application version is already owned by another source.",
            )
            .with_detail("appId", installation.app_id.to_string())
            .with_detail("version", installation.version.to_string())
            .with_detail("existingSourceId", source_id)
            .with_detail("existingScope", scope));
        }
        transaction
            .execute(
                "INSERT INTO installations
                 (app_id, version, source_id, scope, install_path, installed_at, health)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(app_id, version) DO UPDATE SET
                   install_path=excluded.install_path,
                   installed_at=excluded.installed_at,
                   health=excluded.health",
                params![
                    installation.app_id.as_str(),
                    installation.version.to_string(),
                    installation.source_id.as_str(),
                    scope_name(installation.scope),
                    installation.install_path,
                    installation.installed_at,
                    installation.health,
                ],
            )
            .map_err(database_error)?;
        validate_package_installation_owner(&transaction, package)?;
        transaction
            .execute(
                "INSERT INTO package_installations
                 (app_id, app_version, source_id, adapter, package_name, package_kind,
                  package_version, architecture, executable_path, owned_by_torben,
                  installed_at, health)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                 ON CONFLICT(app_id, app_version) DO UPDATE SET
                   source_id=excluded.source_id,
                   adapter=excluded.adapter,
                   package_name=excluded.package_name,
                   package_kind=excluded.package_kind,
                   package_version=excluded.package_version,
                   architecture=excluded.architecture,
                   executable_path=excluded.executable_path,
                   owned_by_torben=excluded.owned_by_torben,
                   installed_at=excluded.installed_at,
                   health=excluded.health",
                params![
                    package.app_id.as_str(),
                    package.app_version.to_string(),
                    package.source_id.as_str(),
                    source_adapter_name(package.adapter),
                    package.coordinate.as_str(),
                    source_package_kind_name(package.package_kind),
                    package.package_version.as_str(),
                    package.architecture,
                    package.executable_path,
                    i32::from(package.owned_by_torben),
                    package.installed_at,
                    package.health,
                ],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    pub fn replace_package_installation(
        &self,
        current: &PackageInstallationRecord,
        installation: &InstallRecord,
        package: &PackageInstallationRecord,
    ) -> TorbenResult<()> {
        if current.app_id != installation.app_id
            || current.app_version != installation.version
            || installation.app_id != package.app_id
            || installation.version != package.app_version
            || installation.source_id != package.source_id
            || installation.scope != InstallScope::PackageManager
        {
            return Err(TorbenError::new(
                "package_installation_parent_mismatch",
                "Package migration records do not describe one application version.",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(database_error)?;
        let stored = transaction
            .query_row(
                "SELECT source_id, adapter, package_name, package_kind, package_version
                 FROM package_installations WHERE app_id=?1 AND app_version=?2",
                params![current.app_id.as_str(), current.app_version.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?;
        let expected = (
            current.source_id.as_str(),
            source_adapter_name(current.adapter),
            current.coordinate.as_str(),
            source_package_kind_name(current.package_kind),
            current.package_version.as_str(),
        );
        if stored.as_ref().map(|row| {
            (
                row.0.as_str(),
                row.1.as_str(),
                row.2.as_str(),
                row.3.as_str(),
                row.4.as_str(),
            )
        }) != Some(expected)
        {
            return Err(TorbenError::new(
                "source_migration_owner_changed",
                "The package source owner changed after the migration plan was reviewed.",
            ));
        }
        let parent_updated = transaction
            .execute(
                "UPDATE installations SET source_id=?1, scope=?2, install_path=?3,
                   installed_at=?4, health=?5
                 WHERE app_id=?6 AND version=?7 AND source_id=?8 AND scope='package_manager'",
                params![
                    installation.source_id.as_str(),
                    scope_name(installation.scope),
                    installation.install_path,
                    installation.installed_at,
                    installation.health,
                    installation.app_id.as_str(),
                    installation.version.to_string(),
                    current.source_id.as_str(),
                ],
            )
            .map_err(database_error)?;
        if parent_updated != 1 {
            return Err(TorbenError::new(
                "source_migration_owner_changed",
                "The installation source owner changed before migration commit.",
            ));
        }
        transaction
            .execute(
                "UPDATE package_installations SET source_id=?1, adapter=?2, package_name=?3,
                   package_kind=?4, package_version=?5, architecture=?6, executable_path=?7,
                   owned_by_torben=?8, installed_at=?9, health=?10
                 WHERE app_id=?11 AND app_version=?12",
                params![
                    package.source_id.as_str(),
                    source_adapter_name(package.adapter),
                    package.coordinate.as_str(),
                    source_package_kind_name(package.package_kind),
                    package.package_version.as_str(),
                    package.architecture,
                    package.executable_path,
                    i32::from(package.owned_by_torben),
                    package.installed_at,
                    package.health,
                    package.app_id.as_str(),
                    package.app_version.to_string(),
                ],
            )
            .map_err(database_error)?;
        validate_package_installation_owner(&transaction, package)?;
        transaction.commit().map_err(database_error)
    }

    pub fn replace_managed_with_package(
        &self,
        current: &InstallRecord,
        installation: &InstallRecord,
        package: &PackageInstallationRecord,
    ) -> TorbenResult<()> {
        if current.scope != InstallScope::Managed
            || current.app_id != installation.app_id
            || current.version != installation.version
            || installation.app_id != package.app_id
            || installation.version != package.app_version
            || installation.source_id != package.source_id
            || installation.scope != InstallScope::PackageManager
        {
            return Err(TorbenError::new(
                "package_installation_parent_mismatch",
                "Managed-to-package migration records do not describe one application version.",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(database_error)?;
        let parent_updated = transaction
            .execute(
                "UPDATE installations SET source_id=?1, scope=?2, install_path=?3,
                   installed_at=?4, health=?5
                 WHERE app_id=?6 AND version=?7 AND source_id=?8 AND scope='managed'",
                params![
                    installation.source_id.as_str(),
                    scope_name(installation.scope),
                    installation.install_path,
                    installation.installed_at,
                    installation.health,
                    installation.app_id.as_str(),
                    installation.version.to_string(),
                    current.source_id.as_str(),
                ],
            )
            .map_err(database_error)?;
        if parent_updated != 1 {
            return Err(TorbenError::new(
                "source_migration_owner_changed",
                "The managed source owner changed before migration commit.",
            ));
        }
        transaction
            .execute(
                "INSERT INTO package_installations
                 (app_id, app_version, source_id, adapter, package_name, package_kind,
                  package_version, architecture, executable_path, owned_by_torben,
                  installed_at, health)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    package.app_id.as_str(),
                    package.app_version.to_string(),
                    package.source_id.as_str(),
                    source_adapter_name(package.adapter),
                    package.coordinate.as_str(),
                    source_package_kind_name(package.package_kind),
                    package.package_version.as_str(),
                    package.architecture,
                    package.executable_path,
                    i32::from(package.owned_by_torben),
                    package.installed_at,
                    package.health,
                ],
            )
            .map_err(database_error)?;
        validate_package_installation_owner(&transaction, package)?;
        transaction.commit().map_err(database_error)
    }

    pub fn replace_package_with_managed(
        &self,
        current: &PackageInstallationRecord,
        managed: &InstallRecord,
    ) -> TorbenResult<()> {
        if managed.scope != InstallScope::Managed
            || current.app_id != managed.app_id
            || current.app_version != managed.version
        {
            return Err(TorbenError::new(
                "package_installation_parent_mismatch",
                "Package-to-managed migration records do not describe one application version.",
            ));
        }
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(database_error)?;
        let stored = transaction
            .query_row(
                "SELECT source_id, adapter, package_name, package_kind, package_version
                 FROM package_installations WHERE app_id=?1 AND app_version=?2",
                params![current.app_id.as_str(), current.app_version.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?;
        let expected = (
            current.source_id.as_str(),
            source_adapter_name(current.adapter),
            current.coordinate.as_str(),
            source_package_kind_name(current.package_kind),
            current.package_version.as_str(),
        );
        if stored.as_ref().map(|row| {
            (
                row.0.as_str(),
                row.1.as_str(),
                row.2.as_str(),
                row.3.as_str(),
                row.4.as_str(),
            )
        }) != Some(expected)
        {
            return Err(TorbenError::new(
                "source_migration_owner_changed",
                "The package source owner changed after the migration plan was reviewed.",
            ));
        }
        transaction
            .execute(
                "DELETE FROM package_installations WHERE app_id=?1 AND app_version=?2",
                params![current.app_id.as_str(), current.app_version.to_string()],
            )
            .map_err(database_error)?;
        let parent_updated = transaction
            .execute(
                "UPDATE installations SET source_id=?1, scope='managed', install_path=?2,
                   installed_at=?3, health=?4
                 WHERE app_id=?5 AND version=?6 AND source_id=?7 AND scope='package_manager'",
                params![
                    managed.source_id.as_str(),
                    managed.install_path,
                    managed.installed_at,
                    managed.health,
                    managed.app_id.as_str(),
                    managed.version.to_string(),
                    current.source_id.as_str(),
                ],
            )
            .map_err(database_error)?;
        if parent_updated != 1 {
            return Err(TorbenError::new(
                "source_migration_owner_changed",
                "The package source owner changed before migration commit.",
            ));
        }
        transaction.commit().map_err(database_error)
    }

    pub fn remove_package_installation(
        &self,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<()> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(database_error)?;
        let removed = transaction
            .execute(
                "DELETE FROM installations
                 WHERE app_id=?1 AND version=?2 AND scope='package_manager'",
                params![app_id.as_str(), version.to_string()],
            )
            .map_err(database_error)?;
        if removed != 1 {
            return Err(TorbenError::new(
                "package_ownership_not_found",
                "No matching Torben-owned package-manager installation was found.",
            )
            .with_detail("appId", app_id.to_string())
            .with_detail("version", version.to_string()));
        }
        transaction.commit().map_err(database_error)
    }

    pub fn package_installation(
        &self,
        app_id: &AppId,
        version: &ExactVersion,
    ) -> TorbenResult<Option<PackageInstallationRecord>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT app_id, app_version, source_id, adapter, package_name, package_kind,
                        package_version, architecture, executable_path, owned_by_torben,
                        installed_at, health
                 FROM package_installations WHERE app_id=?1 AND app_version=?2",
                params![app_id.as_str(), version.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, String>(6)?,
                        row.get::<_, String>(7)?,
                        row.get::<_, String>(8)?,
                        row.get::<_, bool>(9)?,
                        row.get::<_, String>(10)?,
                        row.get::<_, String>(11)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?
            .map(parse_package_installation)
            .transpose()
    }

    pub fn list_package_installations(&self) -> TorbenResult<Vec<PackageInstallationRecord>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT app_id, app_version, source_id, adapter, package_name, package_kind,
                        package_version, architecture, executable_path, owned_by_torben,
                        installed_at, health
                 FROM package_installations ORDER BY app_id, app_version DESC",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, bool>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                ))
            })
            .map_err(database_error)?;
        rows.map(|row| {
            row.map_err(database_error)
                .and_then(parse_package_installation)
        })
        .collect()
    }

    pub fn set_selection(&self, app_id: &AppId, version: &ExactVersion) -> TorbenResult<()> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO selections(app_id, version) VALUES (?1, ?2)
                 ON CONFLICT(app_id) DO UPDATE SET version=excluded.version",
                params![app_id.as_str(), version.to_string()],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn clear_selection(&self, app_id: &AppId) -> TorbenResult<()> {
        let connection = self.lock()?;
        connection
            .execute(
                "DELETE FROM selections WHERE app_id=?1",
                params![app_id.as_str()],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn selected_version(&self, app_id: &AppId) -> TorbenResult<Option<ExactVersion>> {
        let connection = self.lock()?;
        let version = connection
            .query_row(
                "SELECT version FROM selections WHERE app_id=?1",
                params![app_id.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(database_error)?;
        version
            .map(|value| ExactVersion::from_str(&value))
            .transpose()
    }

    pub fn list_selections(&self) -> TorbenResult<Vec<SelectionRecord>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT app_id, version FROM selections ORDER BY app_id")
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(database_error)?;
        rows.map(|row| {
            let (app_id, version) = row.map_err(database_error)?;
            Ok(SelectionRecord {
                app_id: AppId::new(app_id)?,
                version: ExactVersion::from_str(&version)?,
            })
        })
        .collect()
    }

    pub fn upsert_plugin(&self, plugin: &PluginRecord) -> TorbenResult<()> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO plugins(id, version, enabled, manifest_json, origin) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET version=excluded.version, enabled=excluded.enabled, manifest_json=excluded.manifest_json, origin=excluded.origin",
                params![plugin.id.as_str(), plugin.version.to_string(), i32::from(plugin.enabled), plugin.manifest_json, plugin_origin_name(plugin.origin)],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn list_plugins(&self) -> TorbenResult<Vec<PluginRecord>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare("SELECT id, version, enabled, manifest_json, origin FROM plugins ORDER BY id")
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, bool>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(database_error)?;
        rows.map(|row| {
            let (id, version, enabled, manifest_json, origin) = row.map_err(database_error)?;
            parse_plugin_record(id, version, enabled, manifest_json, origin)
        })
        .collect()
    }

    pub fn get_plugin(&self, plugin_id: &PluginId) -> TorbenResult<Option<PluginRecord>> {
        let connection = self.lock()?;
        let raw = connection
            .query_row(
                "SELECT id, version, enabled, manifest_json, origin FROM plugins WHERE id=?1",
                params![plugin_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(database_error)?;
        raw.map(|(id, version, enabled, manifest_json, origin)| {
            parse_plugin_record(id, version, enabled, manifest_json, origin)
        })
        .transpose()
    }

    pub fn set_plugin_enabled(&self, plugin_id: &PluginId, enabled: bool) -> TorbenResult<()> {
        let connection = self.lock()?;
        let changed = connection
            .execute(
                "UPDATE plugins SET enabled=?2 WHERE id=?1",
                params![plugin_id.as_str(), i32::from(enabled)],
            )
            .map_err(database_error)?;
        if changed == 0 {
            return Err(
                TorbenError::new("plugin_not_found", "The plugin is not installed.")
                    .with_detail("pluginId", plugin_id.to_string()),
            );
        }
        Ok(())
    }

    pub fn upsert_operation_journal(
        &self,
        operation_id: OperationId,
        kind: OperationKind,
        state: OperationState,
        journal_json: &str,
        updated_at: &str,
    ) -> TorbenResult<()> {
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO operations(id, kind, state, journal_json, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                   kind=excluded.kind,
                   state=excluded.state,
                   journal_json=excluded.journal_json,
                   updated_at=excluded.updated_at",
                params![
                    operation_id.to_string(),
                    operation_kind_name(kind),
                    operation_state_name(state),
                    journal_json,
                    updated_at,
                ],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn list_operation_journals(&self) -> TorbenResult<Vec<String>> {
        let connection = self.lock()?;
        let mut statement = connection
            .prepare(
                "SELECT journal_json FROM operations
                 ORDER BY CAST(updated_at AS INTEGER) DESC, id DESC",
            )
            .map_err(database_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(database_error)?;
        rows.map(|row| row.map_err(database_error)).collect()
    }

    pub fn get_operation_journal(&self, operation_id: OperationId) -> TorbenResult<Option<String>> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT journal_json FROM operations WHERE id=?1",
                params![operation_id.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(database_error)
    }

    pub fn user_settings(&self) -> TorbenResult<UserSettings> {
        let connection = self.lock()?;
        let value = connection
            .query_row(
                "SELECT value_json FROM settings WHERE key=?1",
                params![USER_SETTINGS_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(database_error)?;
        value.map_or_else(
            || Ok(UserSettings::default()),
            |value| {
                let settings: UserSettings = serde_json::from_str(&value).map_err(|error| {
                    TorbenError::new(
                        "settings_state_invalid",
                        "The saved user settings are invalid.",
                    )
                    .with_detail("key", USER_SETTINGS_KEY)
                    .with_detail("reason", error.to_string())
                    .with_remediation(
                        "Repair or remove the invalid settings row before starting Torben App again.",
                    )
                })?;
                settings.validate().map_err(|error| {
                    TorbenError::new(
                        "settings_state_invalid",
                        "The saved user settings violate an update preference invariant.",
                    )
                    .with_detail("key", USER_SETTINGS_KEY)
                    .with_detail("reason", format!("{}: {}", error.code, error.message))
                    .with_remediation(
                        "Repair or remove the invalid settings row before starting Torben App again.",
                    )
                })?;
                Ok(settings)
            },
        )
    }

    pub fn save_user_settings(&self, settings: &UserSettings) -> TorbenResult<()> {
        settings.validate()?;
        let value = serde_json::to_string(settings).map_err(|error| {
            TorbenError::internal("Could not serialize user settings.")
                .with_detail("reason", error.to_string())
        })?;
        let connection = self.lock()?;
        connection
            .execute(
                "INSERT INTO settings(key, value_json) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json",
                params![USER_SETTINGS_KEY, value],
            )
            .map_err(database_error)?;
        Ok(())
    }

    pub fn managed_library_path(&self) -> TorbenResult<Option<PathBuf>> {
        let connection = self.lock()?;
        let value = connection
            .query_row(
                "SELECT value_json FROM settings WHERE key=?1",
                params![MANAGED_LIBRARY_KEY],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(database_error)?;
        value
            .map(|value| {
                serde_json::from_str::<String>(&value)
                    .map(PathBuf::from)
                    .map_err(|error| {
                        TorbenError::new(
                            "managed_library_state_invalid",
                            "The saved managed application library path is invalid.",
                        )
                        .with_detail("reason", error.to_string())
                    })
            })
            .transpose()
    }

    pub fn commit_managed_library_migration(
        &self,
        source: &std::path::Path,
        target: &std::path::Path,
    ) -> TorbenResult<()> {
        let target_text = target.to_str().ok_or_else(|| {
            TorbenError::new(
                "managed_library_path_invalid",
                "The managed application library path is not valid UTF-8.",
            )
        })?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction().map_err(database_error)?;
        let managed_paths = {
            let mut statement = transaction
                .prepare(
                    "SELECT app_id, version, install_path FROM installations WHERE scope='managed'",
                )
                .map_err(database_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(database_error)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(database_error)?
        };
        for (app_id, version, install_path) in managed_paths {
            let relative = std::path::Path::new(&install_path)
                .strip_prefix(source)
                .map_err(|_| {
                    TorbenError::new(
                        "managed_install_path_invalid",
                        "A managed installation is outside the active application library.",
                    )
                    .with_detail("path", install_path.clone())
                })?;
            let migrated = target.join(relative);
            transaction
                .execute(
                    "UPDATE installations SET install_path=?1 WHERE app_id=?2 AND version=?3",
                    params![migrated.display().to_string(), app_id, version],
                )
                .map_err(database_error)?;
        }
        let value = serde_json::to_string(target_text).map_err(|error| {
            TorbenError::internal("Could not serialize the managed library path.")
                .with_detail("reason", error.to_string())
        })?;
        transaction
            .execute(
                "INSERT INTO settings(key, value_json) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value_json=excluded.value_json",
                params![MANAGED_LIBRARY_KEY, value],
            )
            .map_err(database_error)?;
        transaction.commit().map_err(database_error)
    }

    fn lock(&self) -> TorbenResult<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| {
            TorbenError::new(
                "database_lock_poisoned",
                "The state database lock is unavailable.",
            )
        })
    }
}

type RawInstallation = (String, String, String, String, String, String, String);
type RawPackageInstallation = (
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    String,
    bool,
    String,
    String,
);

fn parse_installation(raw: RawInstallation) -> TorbenResult<InstallRecord> {
    Ok(InstallRecord {
        app_id: AppId::new(raw.0)?,
        version: ExactVersion::from_str(&raw.1)?,
        source_id: SourceId::new(raw.2)?,
        scope: match raw.3.as_str() {
            "managed" => InstallScope::Managed,
            "external" => InstallScope::External,
            "package_manager" => InstallScope::PackageManager,
            value => {
                return Err(
                    TorbenError::new("invalid_database_state", "Unknown install scope.")
                        .with_detail("scope", value),
                );
            }
        },
        install_path: raw.4,
        installed_at: raw.5,
        health: raw.6,
    })
}

fn parse_package_installation(
    raw: RawPackageInstallation,
) -> TorbenResult<PackageInstallationRecord> {
    Ok(PackageInstallationRecord {
        app_id: AppId::new(raw.0)?,
        app_version: ExactVersion::from_str(&raw.1)?,
        source_id: SourceId::new(raw.2)?,
        adapter: SourceAdapterKind::from_str(&raw.3)?,
        coordinate: PackageCoordinate::new(raw.4)?,
        package_kind: SourcePackageKind::from_str(&raw.5)?,
        package_version: SourcePackageVersion::new(raw.6)?,
        architecture: raw.7,
        executable_path: raw.8,
        owned_by_torben: raw.9,
        installed_at: raw.10,
        health: raw.11,
    })
}

fn parse_plugin_record(
    id: String,
    version: String,
    enabled: bool,
    manifest_json: String,
    origin: String,
) -> TorbenResult<PluginRecord> {
    Ok(PluginRecord {
        id: PluginId::new(id)?,
        version: ExactVersion::from_str(&version)?,
        enabled,
        manifest_json,
        origin: parse_plugin_origin(&origin)?,
    })
}

const fn plugin_origin_name(origin: PluginOrigin) -> &'static str {
    match origin {
        PluginOrigin::BuiltIn => "built_in",
        PluginOrigin::OfficialRegistry => "official_registry",
        PluginOrigin::Sideloaded => "sideloaded",
    }
}

fn parse_plugin_origin(value: &str) -> TorbenResult<PluginOrigin> {
    match value {
        "built_in" => Ok(PluginOrigin::BuiltIn),
        "official_registry" => Ok(PluginOrigin::OfficialRegistry),
        "sideloaded" => Ok(PluginOrigin::Sideloaded),
        _ => Err(
            TorbenError::new("invalid_database_state", "Unknown plugin origin.")
                .with_detail("origin", value),
        ),
    }
}

const fn scope_name(scope: InstallScope) -> &'static str {
    match scope {
        InstallScope::Managed => "managed",
        InstallScope::External => "external",
        InstallScope::PackageManager => "package_manager",
    }
}

const fn operation_kind_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Install => "install",
        OperationKind::Select => "select",
        OperationKind::Uninstall => "uninstall",
        OperationKind::SourceInstall => "source_install",
        OperationKind::SourceUninstall => "source_uninstall",
        OperationKind::SourceMigrate => "source_migrate",
        OperationKind::Migrate => "migrate",
        OperationKind::PluginInstall => "plugin_install",
    }
}

const fn source_adapter_name(adapter: SourceAdapterKind) -> &'static str {
    match adapter {
        SourceAdapterKind::Winget => "winget",
        SourceAdapterKind::Homebrew => "homebrew",
        SourceAdapterKind::Apt => "apt",
        SourceAdapterKind::Dnf => "dnf",
    }
}

const fn source_package_kind_name(kind: SourcePackageKind) -> &'static str {
    match kind {
        SourcePackageKind::Native => "native",
        SourcePackageKind::Formula => "formula",
        SourcePackageKind::Cask => "cask",
    }
}

const fn operation_state_name(state: OperationState) -> &'static str {
    match state {
        OperationState::Pending => "pending",
        OperationState::Running => "running",
        OperationState::Cancelling => "cancelling",
        OperationState::Succeeded => "succeeded",
        OperationState::Failed => "failed",
        OperationState::RolledBack => "rolled_back",
    }
}

fn validate_package_installation_owner(
    connection: &Connection,
    record: &PackageInstallationRecord,
) -> TorbenResult<()> {
    let expected_source = format!("source.{}", source_adapter_name(record.adapter));
    if record.source_id.as_str() != expected_source {
        return Err(TorbenError::new(
            "package_source_owner_mismatch",
            "The package adapter does not match its immutable source owner.",
        )
        .with_detail("adapter", source_adapter_name(record.adapter))
        .with_detail("expectedSourceId", expected_source)
        .with_detail("actualSourceId", record.source_id.to_string()));
    }
    let parent = connection
        .query_row(
            "SELECT source_id, scope FROM installations WHERE app_id=?1 AND version=?2",
            params![record.app_id.as_str(), record.app_version.to_string()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(database_error)?;
    let Some((parent_source, parent_scope)) = parent else {
        return Err(TorbenError::new(
            "package_installation_parent_missing",
            "Package ownership requires a matching installation record.",
        ));
    };
    if parent_scope != scope_name(InstallScope::PackageManager)
        || parent_source != record.source_id.as_str()
    {
        return Err(TorbenError::new(
            "package_installation_parent_mismatch",
            "Package ownership does not match the installation scope and source.",
        )
        .with_detail("parentScope", parent_scope)
        .with_detail("parentSourceId", parent_source)
        .with_detail("packageSourceId", record.source_id.to_string()));
    }
    Ok(())
}

fn apply_package_installation_migration(connection: &Connection) -> TorbenResult<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS package_installations (
               app_id TEXT NOT NULL,
               app_version TEXT NOT NULL,
               source_id TEXT NOT NULL,
               adapter TEXT NOT NULL,
               package_name TEXT NOT NULL,
               package_kind TEXT NOT NULL,
               package_version TEXT NOT NULL,
               architecture TEXT NOT NULL,
               executable_path TEXT NOT NULL,
               owned_by_torben INTEGER NOT NULL,
               installed_at TEXT NOT NULL,
               health TEXT NOT NULL,
               PRIMARY KEY (app_id, app_version),
               FOREIGN KEY (app_id, app_version)
                 REFERENCES installations(app_id, version) ON DELETE CASCADE
             );
             INSERT OR IGNORE INTO schema_migrations(version, applied_at)
               VALUES (3, datetime('now'));",
        )
        .map_err(database_error)?;
    Ok(())
}

fn database_error(error: rusqlite::Error) -> TorbenError {
    TorbenError::new("database_error", "The state database operation failed.")
        .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tempfile::tempdir;
    use torben_contracts::{
        AppId, ExactVersion, InstallRecord, InstallScope, LanguagePreference, PackageCoordinate,
        PackageInstallationRecord, SourceAdapterKind, SourceId, SourcePackageKind,
        SourcePackageVersion, ThemePreference, UpdatePreferences, UserSettings,
        plugin::PluginOrigin,
    };

    use super::{CURRENT_SCHEMA_VERSION, StateStore, USER_SETTINGS_KEY};

    #[test]
    fn fresh_database_records_every_embedded_migration() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        drop(StateStore::open(path.clone()).unwrap());
        let connection = rusqlite::Connection::open(path).unwrap();
        let mut statement = connection
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap();
        let versions = statement
            .query_map([], |row| row.get::<_, i64>(0))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(versions, [1, 2, CURRENT_SCHEMA_VERSION]);
    }

    #[test]
    fn repairs_the_preexisting_schema_two_receipt_gap() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        drop(StateStore::open(path.clone()).unwrap());
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute("DELETE FROM schema_migrations WHERE version=2", [])
            .unwrap();
        drop(connection);

        drop(StateStore::open(path.clone()).unwrap());

        let connection = rusqlite::Connection::open(path).unwrap();
        let recorded = connection
            .query_row(
                "SELECT COUNT(*) FROM schema_migrations WHERE version=2",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(recorded, 1);
    }

    #[test]
    fn rejects_a_database_from_a_newer_schema_before_creating_application_tables() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE schema_migrations (
                   version INTEGER PRIMARY KEY,
                   applied_at TEXT NOT NULL
                 );
                 INSERT INTO schema_migrations(version, applied_at)
                   VALUES (4, datetime('now'));",
            )
            .unwrap();
        drop(connection);

        let Err(error) = StateStore::open(path.clone()) else {
            panic!("a newer database schema must fail closed");
        };

        assert_eq!(error.code, "database_schema_newer");
        assert_eq!(error.details["databaseVersion"], "4");
        assert_eq!(
            error.details["supportedVersion"],
            CURRENT_SCHEMA_VERSION.to_string()
        );
        assert!(error.remediation.is_some());
        let connection = rusqlite::Connection::open(path).unwrap();
        let application_tables = connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type='table' AND name IN ('installations', 'plugins', 'operations')",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(application_tables, 0);
    }

    #[test]
    fn persists_installation_and_selection() {
        let directory = tempdir().unwrap();
        let store = StateStore::open(directory.path().join("state.db")).unwrap();
        let app_id = AppId::new("node").unwrap();
        let version = ExactVersion::from_str("24.19.0").unwrap();
        store
            .add_installation(&InstallRecord {
                app_id: app_id.clone(),
                version: version.clone(),
                source_id: SourceId::new("node.official").unwrap(),
                scope: InstallScope::Managed,
                install_path: "test".to_owned(),
                installed_at: "0".to_owned(),
                health: "healthy".to_owned(),
            })
            .unwrap();
        store.set_selection(&app_id, &version).unwrap();
        assert_eq!(store.selected_version(&app_id).unwrap(), Some(version));
    }

    #[test]
    fn migration_three_persists_package_manager_ownership_and_raw_version() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        let store = StateStore::open(path.clone()).unwrap();
        let app_id = AppId::new("node").unwrap();
        let app_version = ExactVersion::from_str("20.11.1").unwrap();
        store
            .add_installation(&InstallRecord {
                app_id: app_id.clone(),
                version: app_version.clone(),
                source_id: SourceId::new("source.apt").unwrap(),
                scope: InstallScope::PackageManager,
                install_path: "/usr/bin/node".to_owned(),
                installed_at: "fixture".to_owned(),
                health: "healthy".to_owned(),
            })
            .unwrap();
        let package = PackageInstallationRecord {
            app_id: app_id.clone(),
            app_version: app_version.clone(),
            source_id: SourceId::new("source.apt").unwrap(),
            adapter: SourceAdapterKind::Apt,
            coordinate: PackageCoordinate::new("nodejs").unwrap(),
            package_kind: SourcePackageKind::Native,
            package_version: SourcePackageVersion::new("1:20.11.1+dfsg-2~deb12u1").unwrap(),
            architecture: "amd64".to_owned(),
            executable_path: "/usr/bin/node".to_owned(),
            owned_by_torben: true,
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        };
        store.upsert_package_installation(&package).unwrap();
        drop(store);

        let reopened = StateStore::open(path).unwrap();
        assert_eq!(
            reopened
                .package_installation(&app_id, &app_version)
                .unwrap(),
            Some(package)
        );
        assert_eq!(reopened.list_package_installations().unwrap().len(), 1);
        reopened.remove_installation(&app_id, &app_version).unwrap();
        assert!(reopened.list_package_installations().unwrap().is_empty());
    }

    #[test]
    fn package_ownership_requires_matching_scope_source_and_adapter() {
        let directory = tempdir().unwrap();
        let store = StateStore::open(directory.path().join("state.db")).unwrap();
        let app_id = AppId::new("node").unwrap();
        let app_version = ExactVersion::from_str("20.11.1").unwrap();
        let source_id = SourceId::new("source.apt").unwrap();
        let mut installation = InstallRecord {
            app_id: app_id.clone(),
            version: app_version.clone(),
            source_id: source_id.clone(),
            scope: InstallScope::Managed,
            install_path: "/usr/bin/node".to_owned(),
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        };
        store.add_installation(&installation).unwrap();
        let mut package = PackageInstallationRecord {
            app_id,
            app_version,
            source_id,
            adapter: SourceAdapterKind::Apt,
            coordinate: PackageCoordinate::new("nodejs").unwrap(),
            package_kind: SourcePackageKind::Native,
            package_version: SourcePackageVersion::new("1:20.11.1+dfsg-2~deb12u1").unwrap(),
            architecture: "amd64".to_owned(),
            executable_path: "/usr/bin/node".to_owned(),
            owned_by_torben: true,
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        };

        assert_eq!(
            store
                .upsert_package_installation(&package)
                .unwrap_err()
                .code,
            "package_installation_parent_mismatch"
        );
        installation.scope = InstallScope::PackageManager;
        store.add_installation(&installation).unwrap();
        package.adapter = SourceAdapterKind::Dnf;
        assert_eq!(
            store
                .upsert_package_installation(&package)
                .unwrap_err()
                .code,
            "package_source_owner_mismatch"
        );
    }

    #[test]
    fn atomically_replaces_managed_ownership_with_package_ownership() {
        let directory = tempdir().unwrap();
        let store = StateStore::open(directory.path().join("state.db")).unwrap();
        let app_id = AppId::new("vscode").unwrap();
        let version = ExactVersion::from_str("1.134.0").unwrap();
        let managed = InstallRecord {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: SourceId::new("vscode.official").unwrap(),
            scope: InstallScope::Managed,
            install_path: "managed/vscode/1.134.0".to_owned(),
            installed_at: "before".to_owned(),
            health: "healthy".to_owned(),
        };
        store.add_installation(&managed).unwrap();
        let source_id = SourceId::new("source.dnf").unwrap();
        let package_parent = InstallRecord {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: source_id.clone(),
            scope: InstallScope::PackageManager,
            install_path: "/usr/bin/code".to_owned(),
            installed_at: "after".to_owned(),
            health: "healthy".to_owned(),
        };
        let package = PackageInstallationRecord {
            app_id: app_id.clone(),
            app_version: version.clone(),
            source_id,
            adapter: SourceAdapterKind::Dnf,
            coordinate: PackageCoordinate::new("code").unwrap(),
            package_kind: SourcePackageKind::Native,
            package_version: SourcePackageVersion::new("1.134.0-1.fc42").unwrap(),
            architecture: "x86_64".to_owned(),
            executable_path: "/usr/bin/code".to_owned(),
            owned_by_torben: true,
            installed_at: "after".to_owned(),
            health: "healthy".to_owned(),
        };

        store
            .replace_managed_with_package(&managed, &package_parent, &package)
            .unwrap();

        assert_eq!(
            store.get_installation(&app_id, &version).unwrap(),
            Some(package_parent)
        );
        assert_eq!(
            store.package_installation(&app_id, &version).unwrap(),
            Some(package)
        );
    }

    #[test]
    fn atomically_replaces_package_ownership_with_managed_ownership() {
        let directory = tempdir().unwrap();
        let store = StateStore::open(directory.path().join("state.db")).unwrap();
        let app_id = AppId::new("vscode").unwrap();
        let version = ExactVersion::from_str("1.134.0").unwrap();
        let source_id = SourceId::new("source.dnf").unwrap();
        let package_parent = InstallRecord {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: source_id.clone(),
            scope: InstallScope::PackageManager,
            install_path: "/usr/bin".to_owned(),
            installed_at: "before".to_owned(),
            health: "healthy".to_owned(),
        };
        let package = PackageInstallationRecord {
            app_id: app_id.clone(),
            app_version: version.clone(),
            source_id,
            adapter: SourceAdapterKind::Dnf,
            coordinate: PackageCoordinate::new("code").unwrap(),
            package_kind: SourcePackageKind::Native,
            package_version: SourcePackageVersion::new("1.134.0-1.fc42").unwrap(),
            architecture: "x86_64".to_owned(),
            executable_path: "/usr/bin/code".to_owned(),
            owned_by_torben: true,
            installed_at: "before".to_owned(),
            health: "healthy".to_owned(),
        };
        store
            .commit_package_installation(&package_parent, &package)
            .unwrap();
        let managed = InstallRecord {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: SourceId::new("vscode.official").unwrap(),
            scope: InstallScope::Managed,
            install_path: "managed/vscode/1.134.0".to_owned(),
            installed_at: "after".to_owned(),
            health: "healthy".to_owned(),
        };

        store
            .replace_package_with_managed(&package, &managed)
            .unwrap();

        assert_eq!(
            store.get_installation(&app_id, &version).unwrap(),
            Some(managed)
        );
        assert!(
            store
                .package_installation(&app_id, &version)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_selection_for_missing_installation() {
        let directory = tempdir().unwrap();
        let store = StateStore::open(directory.path().join("state.db")).unwrap();
        let app_id = AppId::new("node").unwrap();
        let version = ExactVersion::from_str("24.19.0").unwrap();
        assert!(store.set_selection(&app_id, &version).is_err());
    }

    #[test]
    fn persists_user_settings_and_supplies_defaults() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        let store = StateStore::open(path.clone()).unwrap();
        assert_eq!(store.user_settings().unwrap(), UserSettings::default());
        let settings = UserSettings {
            theme: ThemePreference::Light,
            language: LanguagePreference::SimplifiedChinese,
            updates: UpdatePreferences::default(),
        };

        store.save_user_settings(&settings).unwrap();
        drop(store);

        let reopened = StateStore::open(path).unwrap();
        assert_eq!(reopened.user_settings().unwrap(), settings);
    }

    #[test]
    fn rejects_invalid_persisted_user_settings() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        drop(StateStore::open(path.clone()).unwrap());
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute(
                "INSERT INTO settings(key, value_json) VALUES (?1, ?2)",
                [USER_SETTINGS_KEY, r#"{"theme":"sepia","language":"en"}"#],
            )
            .unwrap();
        let store = StateStore::open(path).unwrap();

        let error = store.user_settings().unwrap_err();

        assert_eq!(error.code, "settings_state_invalid");
        assert_eq!(
            error.details.get("key").map(String::as_str),
            Some(USER_SETTINGS_KEY)
        );
    }

    #[test]
    fn migrates_legacy_plugin_rows_to_explicit_sideloaded_origin() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("state.db");
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE plugins (
                   id TEXT PRIMARY KEY,
                   version TEXT NOT NULL,
                   enabled INTEGER NOT NULL,
                   manifest_json TEXT NOT NULL
                 );
                 INSERT INTO plugins(id, version, enabled, manifest_json)
                   VALUES ('dev.example.legacy', '1.2.3', 1, '{}');",
            )
            .unwrap();
        drop(connection);

        let store = StateStore::open(path).unwrap();
        let records = store.list_plugins().unwrap();

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].origin, PluginOrigin::Sideloaded);
    }
}
