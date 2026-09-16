use std::{
    fs::{File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use torben_contracts::{
    AppId, BackupDatabaseInstanceRequest, CreateDatabaseInstanceRequest, DatabaseBackup,
    DatabaseEngine, DatabaseInstance, DatabaseInstanceName, DatabaseInstanceState,
    DatabaseInstanceTarget, DeleteDatabaseInstanceRequest, ExactVersion, InstallRecord,
    InstallScope, RestoreDatabaseInstanceRequest, TorbenError, TorbenResult,
};

use crate::{
    StateStore, TorbenCore, TorbenPaths, validate_selected_installation,
    workspace_lock::WorkspaceLock,
};

const START_TIMEOUT: Duration = Duration::from_secs(15);
const STOP_TIMEOUT: Duration = Duration::from_secs(15);
const INSTANCE_RECEIPT_SCHEMA_VERSION: u32 = 1;
const INSTANCE_RECEIPT_MAX_BYTES: u64 = 16 * 1024;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DatabaseInstanceReceipt {
    schema_version: u32,
    engine: DatabaseEngine,
    name: DatabaseInstanceName,
    runtime_version: ExactVersion,
    port: u16,
    data_path: String,
    created_at: String,
}

impl TorbenCore {
    pub fn database_instances(
        &self,
        engine: Option<DatabaseEngine>,
    ) -> TorbenResult<Vec<DatabaseInstance>> {
        self.store
            .list_database_instances(engine)?
            .into_iter()
            .map(|instance| self.probe_database_instance(instance))
            .collect()
    }

    pub fn database_instance_status(
        &self,
        target: DatabaseInstanceTarget,
    ) -> TorbenResult<DatabaseInstance> {
        let instance = self.load_database_instance(target.engine, &target.name)?;
        self.probe_database_instance(instance)
    }

    pub fn create_database_instance(
        &self,
        request: CreateDatabaseInstanceRequest,
    ) -> TorbenResult<DatabaseInstance> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        if self
            .store
            .get_database_instance(request.engine, &request.name)?
            .is_some()
        {
            return Err(instance_conflict(request.engine, &request.name));
        }
        let runtime = self.database_runtime(request.engine, request.runtime_version.as_ref())?;
        let port = request
            .port
            .unwrap_or_else(|| request.engine.default_port());
        if port == 0 {
            return Err(TorbenError::new(
                "database_instance_port_invalid",
                "A database instance port must be between 1 and 65535.",
            ));
        }
        if self
            .store
            .list_database_instances(None)?
            .iter()
            .any(|instance| instance.port == port)
        {
            return Err(TorbenError::new(
                "database_instance_conflict",
                "The requested network port is already assigned to another managed instance.",
            )
            .with_detail("port", port.to_string()));
        }

        let root = self.database_instance_root(request.engine, &request.name);
        if root.exists() {
            return Err(instance_conflict(request.engine, &request.name)
                .with_detail("path", root.display().to_string()));
        }
        let staging = self.paths.staging_dir().join(format!(
            "database-create-{}-{}-{}",
            request.engine,
            request.name,
            unique_suffix()
        ));
        create_instance_layout(&staging)?;
        let instance = DatabaseInstance {
            engine: request.engine,
            name: request.name,
            runtime_version: runtime.version.clone(),
            port,
            data_path: root.join("data").display().to_string(),
            created_at: timestamp(),
            state: DatabaseInstanceState::Stopped,
            pid: None,
        };
        if let Err(error) = write_instance_receipt(&staging, &instance) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        let initialize_result = self.initialize_database(request.engine, &runtime, &staging);
        if let Err(error) = initialize_result {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(error);
        }
        let parent = root.parent().ok_or_else(|| {
            TorbenError::internal("The database instance directory has no parent.")
        })?;
        std::fs::create_dir_all(parent).map_err(instance_io_error)?;
        std::fs::rename(&staging, &root).map_err(instance_io_error)?;
        if let Err(error) = write_instance_config(request.engine, &root, port, &runtime) {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
        if let Err(error) = self.store.add_database_instance(&instance) {
            let _ = std::fs::remove_dir_all(&root);
            return Err(error);
        }
        Ok(instance)
    }

    pub fn start_database_instance(
        &self,
        target: DatabaseInstanceTarget,
    ) -> TorbenResult<DatabaseInstance> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let instance = self.load_and_validate_database_instance(target.engine, &target.name)?;
        let current = self.probe_database_instance(instance.clone())?;
        match current.state {
            DatabaseInstanceState::Running => return Ok(current),
            DatabaseInstanceState::Stale => {
                return Err(TorbenError::new(
                    "database_instance_stale",
                    "The instance has stale process state and cannot be started safely.",
                )
                .with_detail("engine", target.engine.to_string())
                .with_detail("name", target.name.to_string())
                .with_remediation(
                    "Verify that no server is using the instance data, remove the stale PID file, then retry.",
                ));
            }
            DatabaseInstanceState::Stopped => {}
        }
        let root = database_instance_root_from_record(&instance)?;
        write_instance_config(
            instance.engine,
            &root,
            instance.port,
            &self.database_runtime(instance.engine, Some(&instance.runtime_version))?,
        )?;
        match instance.engine {
            DatabaseEngine::Mysql => {
                let mut command = self.database_command(&instance, "mysqld")?;
                command.arg(format!(
                    "--defaults-file={}",
                    native_config_path(&root.join("config/my.ini"))
                ));
                let pid = spawn_server(command, &root.join("logs/mysql-server.log"), &root)?;
                write_pid(&root.join("run/pid"), pid)?;
            }
            DatabaseEngine::Redis => {
                let mut command = self.database_command(&instance, "redis-server")?;
                command.arg(root.join("config/redis.conf"));
                let pid = spawn_server(command, &root.join("logs/redis-server.log"), &root)?;
                write_pid(&root.join("run/pid"), pid)?;
            }
            DatabaseEngine::Postgresql => {
                let mut command = self.database_command(&instance, "pg_ctl")?;
                command
                    .arg("-D")
                    .arg(root.join("data"))
                    .arg("-l")
                    .arg(root.join("logs/postgresql.log"))
                    .arg("-o")
                    .arg(format!("-p {} -h 127.0.0.1", instance.port))
                    .args(["start", "-w", "-t", "15"]);
                run_checked(&mut command, "database_instance_start_failed")?;
            }
        }
        self.wait_for_database_state(&instance, true, START_TIMEOUT)
    }

    pub fn stop_database_instance(
        &self,
        target: DatabaseInstanceTarget,
    ) -> TorbenResult<DatabaseInstance> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let instance = self.load_and_validate_database_instance(target.engine, &target.name)?;
        let current = self.probe_database_instance(instance.clone())?;
        if current.state == DatabaseInstanceState::Stopped {
            return Ok(current);
        }
        if current.state == DatabaseInstanceState::Stale {
            return Err(TorbenError::new(
                "database_instance_stale",
                "The instance is not healthy, so Torben App will not terminate a PID it cannot verify.",
            )
            .with_detail("engine", target.engine.to_string())
            .with_detail("name", target.name.to_string()));
        }
        let root = database_instance_root_from_record(&instance)?;
        match instance.engine {
            DatabaseEngine::Mysql => {
                let mut command = self.database_command(&instance, "mysqladmin")?;
                command.args([
                    "--no-defaults",
                    "--protocol=tcp",
                    "--host=127.0.0.1",
                    &format!("--port={}", instance.port),
                    "--user=root",
                    "shutdown",
                ]);
                run_checked(&mut command, "database_instance_stop_failed")?;
            }
            DatabaseEngine::Redis => {
                let mut command = self.database_command(&instance, "redis-cli")?;
                command.args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &instance.port.to_string(),
                    "shutdown",
                ]);
                run_checked(&mut command, "database_instance_stop_failed")?;
            }
            DatabaseEngine::Postgresql => {
                let mut command = self.database_command(&instance, "pg_ctl")?;
                command
                    .arg("-D")
                    .arg(root.join("data"))
                    .args(["stop", "-m", "fast", "-w", "-t", "15"]);
                run_checked(&mut command, "database_instance_stop_failed")?;
            }
        }
        let stopped = self.wait_for_database_state(&instance, false, STOP_TIMEOUT)?;
        let _ = std::fs::remove_file(root.join("run/pid"));
        Ok(stopped)
    }

    pub fn backup_database_instance(
        &self,
        request: BackupDatabaseInstanceRequest,
    ) -> TorbenResult<DatabaseBackup> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let instance = self.load_and_validate_database_instance(request.engine, &request.name)?;
        self.backup_database_instance_inner(&instance, request.destination.as_deref())
    }

    pub fn restore_database_instance(
        &self,
        request: RestoreDatabaseInstanceRequest,
    ) -> TorbenResult<DatabaseInstance> {
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let instance = self.load_and_validate_database_instance(request.engine, &request.name)?;
        let source = validate_restore_source(&request.source)?;
        let current = self.probe_database_instance(instance.clone())?;
        match instance.engine {
            DatabaseEngine::Redis => {
                if current.state != DatabaseInstanceState::Stopped {
                    return Err(state_required(&instance, "stopped", current.state));
                }
                Self::restore_redis(&instance, &source)?;
            }
            DatabaseEngine::Mysql | DatabaseEngine::Postgresql => {
                if current.state != DatabaseInstanceState::Running {
                    return Err(state_required(&instance, "running", current.state));
                }
                let recovery = self.backup_database_instance_inner(&instance, None)?;
                if let Err(mut error) = self.restore_sql(&instance, &source) {
                    error
                        .details
                        .insert("recoveryBackup".to_owned(), recovery.path);
                    error.remediation = Some(
                        "The import may be partial. Inspect the instance and use the recovery backup before retrying."
                            .to_owned(),
                    );
                    return Err(error);
                }
            }
        }
        self.probe_database_instance(instance)
    }

    pub fn delete_database_instance(
        &self,
        request: DeleteDatabaseInstanceRequest,
    ) -> TorbenResult<()> {
        if !request.confirm {
            return Err(TorbenError::new(
                "database_instance_delete_confirmation_required",
                "Deleting an instance requires explicit confirmation.",
            ));
        }
        let _lock = WorkspaceLock::acquire(self.paths.workspace_lock())?;
        let instance = self.load_and_validate_database_instance(request.engine, &request.name)?;
        let current = self.probe_database_instance(instance.clone())?;
        if current.state != DatabaseInstanceState::Stopped {
            return Err(state_required(&instance, "stopped", current.state));
        }
        let root = database_instance_root_from_record(&instance)?;
        let staged = self.paths.staging_dir().join(format!(
            "database-delete-{}-{}-{}",
            instance.engine,
            instance.name,
            unique_suffix()
        ));
        std::fs::rename(&root, &staged).map_err(instance_io_error)?;
        if let Err(error) = self
            .store
            .remove_database_instance(instance.engine, &instance.name)
        {
            let _ = std::fs::rename(&staged, &root);
            return Err(error);
        }
        std::fs::remove_dir_all(&staged).map_err(|error| {
            instance_io_error(error).with_remediation(format!(
                "The instance record was removed, but its staged data remains at {} and can be removed manually.",
                staged.display()
            ))
        })
    }

    fn initialize_database(
        &self,
        engine: DatabaseEngine,
        runtime: &InstallRecord,
        root: &Path,
    ) -> TorbenResult<()> {
        let placeholder = DatabaseInstance {
            engine,
            name: DatabaseInstanceName::new("initializing")?,
            runtime_version: runtime.version.clone(),
            port: engine.default_port(),
            data_path: root.join("data").display().to_string(),
            created_at: String::new(),
            state: DatabaseInstanceState::Stopped,
            pid: None,
        };
        match engine {
            DatabaseEngine::Mysql => {
                let mut command = self.database_command(&placeholder, "mysqld")?;
                command
                    .arg("--initialize-insecure")
                    .arg(format!(
                        "--basedir={}",
                        native_config_path(Path::new(&runtime.install_path))
                    ))
                    .arg(format!(
                        "--datadir={}",
                        native_config_path(&root.join("data"))
                    ));
                run_checked(&mut command, "database_instance_initialize_failed")
            }
            DatabaseEngine::Redis => Ok(()),
            DatabaseEngine::Postgresql => {
                let mut command = self.database_command(&placeholder, "initdb")?;
                command.arg("-D").arg(root.join("data")).args([
                    "--encoding=UTF8",
                    "--auth=trust",
                    "--username=postgres",
                ]);
                run_checked(&mut command, "database_instance_initialize_failed")
            }
        }
    }

    fn database_runtime(
        &self,
        engine: DatabaseEngine,
        requested: Option<&ExactVersion>,
    ) -> TorbenResult<InstallRecord> {
        let app_id = AppId::new(engine.as_str())?;
        let version = match requested {
            Some(version) => version.clone(),
            None => self.store.selected_version(&app_id)?.ok_or_else(|| {
                TorbenError::new(
                    "database_runtime_not_selected",
                    "Select an installed database runtime version before creating an instance.",
                )
                .with_detail("engine", engine.to_string())
            })?,
        };
        let record = self
            .store
            .get_installation(&app_id, &version)?
            .ok_or_else(|| {
                TorbenError::new(
                    "database_runtime_not_installed",
                    "The database runtime version required by this instance is not installed.",
                )
                .with_detail("engine", engine.to_string())
                .with_detail("version", version.to_string())
            })?;
        if record.scope != InstallScope::Managed {
            return Err(TorbenError::new(
                "database_runtime_not_managed",
                "Database instances require a Torben-managed runtime installation.",
            ));
        }
        validate_selected_installation(&self.paths, &record)?;
        Ok(record)
    }

    fn database_command(
        &self,
        instance: &DatabaseInstance,
        command: &str,
    ) -> TorbenResult<Command> {
        let runtime = self.database_runtime(instance.engine, Some(&instance.runtime_version))?;
        let install_path = Path::new(&runtime.install_path);
        let executable = match instance.engine {
            DatabaseEngine::Mysql => self.mysql.command_path(install_path, command),
            DatabaseEngine::Redis => self.redis.command_path(install_path, command),
            DatabaseEngine::Postgresql => self.postgresql.command_path(install_path, command),
        }?;
        let mut process = Command::new(&executable);
        let bin = executable.parent().ok_or_else(|| {
            TorbenError::internal("The managed database command has no parent directory.")
        })?;
        match instance.engine {
            DatabaseEngine::Mysql => crate::mysql::configure_command_environment(
                &mut process,
                &self.paths.mysql_data_dir(),
                bin,
            )?,
            DatabaseEngine::Redis => crate::redis::configure_command_environment(
                &mut process,
                &self.paths.redis_data_dir(),
                bin,
            )?,
            DatabaseEngine::Postgresql => crate::postgresql::configure_command_environment(
                &mut process,
                &self.paths.postgresql_data_dir(),
                bin,
            )?,
        }
        Ok(process)
    }

    fn load_database_instance(
        &self,
        engine: DatabaseEngine,
        name: &DatabaseInstanceName,
    ) -> TorbenResult<DatabaseInstance> {
        self.store
            .get_database_instance(engine, name)?
            .ok_or_else(|| instance_not_found(engine, name))
    }

    fn load_and_validate_database_instance(
        &self,
        engine: DatabaseEngine,
        name: &DatabaseInstanceName,
    ) -> TorbenResult<DatabaseInstance> {
        let instance = self.load_database_instance(engine, name)?;
        let expected = self.database_instance_root(engine, name).join("data");
        if Path::new(&instance.data_path) != expected {
            return Err(TorbenError::new(
                "database_instance_path_invalid",
                "The persisted instance data path is outside its managed location.",
            )
            .with_detail("recordedPath", instance.data_path)
            .with_detail("expectedPath", expected.display().to_string()));
        }
        let root = expected.parent().expect("data path always has a parent");
        let metadata = root.symlink_metadata().map_err(instance_io_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(TorbenError::new(
                "database_instance_path_invalid",
                "The managed instance path is not a plain directory.",
            )
            .with_detail("path", root.display().to_string()));
        }
        Ok(instance)
    }

    fn probe_database_instance(
        &self,
        mut instance: DatabaseInstance,
    ) -> TorbenResult<DatabaseInstance> {
        let root = database_instance_root_from_record(&instance)?;
        let pid = read_instance_pid(instance.engine, &root).filter(|pid| process_is_active(*pid));
        let healthy = self.database_health_check(&instance).unwrap_or(false);
        instance.pid = pid;
        instance.state = if healthy {
            DatabaseInstanceState::Running
        } else if pid.is_some() {
            DatabaseInstanceState::Stale
        } else {
            DatabaseInstanceState::Stopped
        };
        Ok(instance)
    }

    fn database_health_check(&self, instance: &DatabaseInstance) -> TorbenResult<bool> {
        let mut command = match instance.engine {
            DatabaseEngine::Mysql => {
                let mut command = self.database_command(instance, "mysqladmin")?;
                command.args([
                    "--no-defaults",
                    "--protocol=tcp",
                    "--host=127.0.0.1",
                    &format!("--port={}", instance.port),
                    "--connect-timeout=2",
                    "--user=root",
                    "ping",
                ]);
                command
            }
            DatabaseEngine::Redis => {
                let mut command = self.database_command(instance, "redis-cli")?;
                command.args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &instance.port.to_string(),
                    "-t",
                    "2",
                    "ping",
                ]);
                command
            }
            DatabaseEngine::Postgresql => {
                let mut command = self.database_command(instance, "pg_isready")?;
                command.args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &instance.port.to_string(),
                    "-t",
                    "2",
                ]);
                command
            }
        };
        command.stdout(Stdio::null()).stderr(Stdio::null());
        command
            .status()
            .map(|status| status.success())
            .map_err(instance_io_error)
    }

    fn wait_for_database_state(
        &self,
        instance: &DatabaseInstance,
        running: bool,
        timeout: Duration,
    ) -> TorbenResult<DatabaseInstance> {
        let started = std::time::Instant::now();
        while started.elapsed() < timeout {
            let status = self.probe_database_instance(instance.clone())?;
            if (running && status.state == DatabaseInstanceState::Running)
                || (!running && status.state == DatabaseInstanceState::Stopped)
            {
                return Ok(status);
            }
            thread::sleep(Duration::from_millis(200));
        }
        Err(TorbenError::new(
            if running {
                "database_instance_start_timeout"
            } else {
                "database_instance_stop_timeout"
            },
            "The database instance did not reach the requested state before the timeout.",
        )
        .with_detail("engine", instance.engine.to_string())
        .with_detail("name", instance.name.to_string()))
    }

    fn backup_database_instance_inner(
        &self,
        instance: &DatabaseInstance,
        requested_destination: Option<&str>,
    ) -> TorbenResult<DatabaseBackup> {
        let status = self.probe_database_instance(instance.clone())?;
        if status.state != DatabaseInstanceState::Running {
            return Err(state_required(instance, "running", status.state));
        }
        let root = database_instance_root_from_record(instance)?;
        let extension = if instance.engine == DatabaseEngine::Redis {
            "rdb"
        } else {
            "sql"
        };
        let destination = backup_destination(instance, &root, requested_destination, extension)?;
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(instance_io_error)?;
        }
        match instance.engine {
            DatabaseEngine::Mysql => {
                let mut command = self.database_command(instance, "mysqldump")?;
                command.args([
                    "--no-defaults",
                    "--protocol=tcp",
                    "--host=127.0.0.1",
                    &format!("--port={}", instance.port),
                    "--user=root",
                    "--all-databases",
                    "--routines",
                    "--events",
                ]);
                run_to_file(
                    &mut command,
                    &destination,
                    "database_instance_backup_failed",
                )?;
            }
            DatabaseEngine::Redis => {
                let mut command = self.database_command(instance, "redis-cli")?;
                command.args(["-h", "127.0.0.1", "-p", &instance.port.to_string(), "save"]);
                run_checked(&mut command, "database_instance_backup_failed")?;
                let source = root.join("data/dump.rdb");
                if !source.is_file() {
                    return Err(TorbenError::new(
                        "database_instance_backup_failed",
                        "Redis completed SAVE but the managed RDB file is missing.",
                    )
                    .with_detail("path", source.display().to_string()));
                }
                copy_new(&source, &destination)?;
            }
            DatabaseEngine::Postgresql => {
                let mut command = self.database_command(instance, "pg_dumpall")?;
                command.args([
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &instance.port.to_string(),
                    "-U",
                    "postgres",
                ]);
                run_to_file(
                    &mut command,
                    &destination,
                    "database_instance_backup_failed",
                )?;
            }
        }
        Ok(DatabaseBackup {
            engine: instance.engine,
            instance_name: instance.name.clone(),
            path: destination.display().to_string(),
            created_at: timestamp(),
        })
    }

    fn restore_sql(&self, instance: &DatabaseInstance, source: &Path) -> TorbenResult<()> {
        let mut command = match instance.engine {
            DatabaseEngine::Mysql => {
                let mut command = self.database_command(instance, "mysql")?;
                command.args([
                    "--no-defaults",
                    "--protocol=tcp",
                    "--host=127.0.0.1",
                    &format!("--port={}", instance.port),
                    "--user=root",
                ]);
                command
            }
            DatabaseEngine::Postgresql => {
                let mut command = self.database_command(instance, "psql")?;
                command.args([
                    "-X",
                    "--set=ON_ERROR_STOP=on",
                    "-h",
                    "127.0.0.1",
                    "-p",
                    &instance.port.to_string(),
                    "-U",
                    "postgres",
                    "-d",
                    "postgres",
                ]);
                command
            }
            DatabaseEngine::Redis => unreachable!("Redis restore uses file replacement"),
        };
        command.stdin(Stdio::from(File::open(source).map_err(instance_io_error)?));
        run_checked(&mut command, "database_instance_restore_failed")
    }

    fn restore_redis(instance: &DatabaseInstance, source: &Path) -> TorbenResult<()> {
        let root = database_instance_root_from_record(instance)?;
        let destination = root.join("data/dump.rdb");
        if source == destination {
            return Err(TorbenError::new(
                "database_restore_source_invalid",
                "The Redis restore source cannot be the active instance data file.",
            )
            .with_detail("path", source.display().to_string()));
        }
        let rollback = root.join("temp/dump-before-restore.rdb");
        if rollback.exists() {
            std::fs::remove_file(&rollback).map_err(instance_io_error)?;
        }
        if destination.is_file() {
            std::fs::rename(&destination, &rollback).map_err(instance_io_error)?;
        }
        if let Err(error) = std::fs::copy(source, &destination).map_err(instance_io_error) {
            if rollback.is_file() {
                let _ = std::fs::rename(&rollback, &destination);
            }
            return Err(error);
        }
        if rollback.is_file() {
            std::fs::remove_file(rollback).map_err(instance_io_error)?;
        }
        Ok(())
    }

    fn database_instance_root(
        &self,
        engine: DatabaseEngine,
        name: &DatabaseInstanceName,
    ) -> PathBuf {
        database_instance_root_for(&self.paths, engine, name)
    }
}

fn engine_data_root(paths: &TorbenPaths, engine: DatabaseEngine) -> PathBuf {
    match engine {
        DatabaseEngine::Mysql => paths.mysql_data_dir(),
        DatabaseEngine::Redis => paths.redis_data_dir(),
        DatabaseEngine::Postgresql => paths.postgresql_data_dir(),
    }
}

fn database_instance_root_for(
    paths: &TorbenPaths,
    engine: DatabaseEngine,
    name: &DatabaseInstanceName,
) -> PathBuf {
    engine_data_root(paths, engine)
        .join("instances")
        .join(name.as_str())
}

pub(crate) fn recover_database_instance_mutations(
    paths: &TorbenPaths,
    store: &StateStore,
) -> TorbenResult<()> {
    for engine in [
        DatabaseEngine::Mysql,
        DatabaseEngine::Redis,
        DatabaseEngine::Postgresql,
    ] {
        let instances_root = engine_data_root(paths, engine).join("instances");
        let Ok(entries) = std::fs::read_dir(&instances_root) else {
            continue;
        };
        for entry in entries {
            let entry = entry.map_err(instance_io_error)?;
            let root = entry.path();
            let metadata = entry.metadata().map_err(instance_io_error)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                continue;
            }
            let Some(name) = entry
                .file_name()
                .to_str()
                .and_then(|value| DatabaseInstanceName::new(value.to_owned()).ok())
            else {
                continue;
            };
            let Some(instance) = read_instance_receipt(&root)? else {
                continue;
            };
            validate_instance_receipt(paths, &instance, engine, &name)?;
            if store.get_database_instance(engine, &name)?.is_none() {
                store.add_database_instance(&instance)?;
            }
        }
    }

    let Ok(entries) = std::fs::read_dir(paths.staging_dir()) else {
        return Ok(());
    };
    for entry in entries {
        let entry = entry.map_err(instance_io_error)?;
        let staged = entry.path();
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let is_create = file_name.starts_with("database-create-");
        let is_delete = file_name.starts_with("database-delete-");
        if !is_create && !is_delete {
            continue;
        }
        let metadata = entry.metadata().map_err(instance_io_error)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            continue;
        }
        let Some(instance) = read_instance_receipt(&staged)? else {
            continue;
        };
        validate_instance_receipt(paths, &instance, instance.engine, &instance.name)?;
        let final_root = database_instance_root_for(paths, instance.engine, &instance.name);
        let recorded = store
            .get_database_instance(instance.engine, &instance.name)?
            .is_some();
        if is_delete && recorded && !final_root.exists() {
            std::fs::rename(&staged, final_root).map_err(instance_io_error)?;
        } else if (is_delete && !recorded) || (is_create && !recorded && !final_root.exists()) {
            std::fs::remove_dir_all(staged).map_err(instance_io_error)?;
        }
    }
    Ok(())
}

fn create_instance_layout(root: &Path) -> TorbenResult<()> {
    for directory in ["data", "config", "logs", "temp", "run", "backups"] {
        std::fs::create_dir_all(root.join(directory)).map_err(instance_io_error)?;
    }
    Ok(())
}

fn write_instance_receipt(root: &Path, instance: &DatabaseInstance) -> TorbenResult<()> {
    let receipt = DatabaseInstanceReceipt {
        schema_version: INSTANCE_RECEIPT_SCHEMA_VERSION,
        engine: instance.engine,
        name: instance.name.clone(),
        runtime_version: instance.runtime_version.clone(),
        port: instance.port,
        data_path: instance.data_path.clone(),
        created_at: instance.created_at.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&receipt).map_err(|error| {
        TorbenError::new(
            "database_instance_receipt_invalid",
            "The database instance recovery receipt could not be serialized.",
        )
        .with_detail("reason", error.to_string())
    })?;
    write_managed_file(&root.join("run/instance.json"), &bytes)
}

fn read_instance_receipt(root: &Path) -> TorbenResult<Option<DatabaseInstance>> {
    let path = root.join("run/instance.json");
    let metadata = match path.symlink_metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(instance_io_error(error)),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > INSTANCE_RECEIPT_MAX_BYTES
    {
        return Err(TorbenError::new(
            "database_instance_receipt_invalid",
            "The database instance recovery receipt is not a bounded regular file.",
        )
        .with_detail("path", path.display().to_string()));
    }
    let bytes = std::fs::read(&path).map_err(instance_io_error)?;
    let receipt: DatabaseInstanceReceipt = serde_json::from_slice(&bytes).map_err(|error| {
        TorbenError::new(
            "database_instance_receipt_invalid",
            "The database instance recovery receipt is invalid.",
        )
        .with_detail("path", path.display().to_string())
        .with_detail("reason", error.to_string())
    })?;
    if receipt.schema_version != INSTANCE_RECEIPT_SCHEMA_VERSION {
        return Err(TorbenError::new(
            "database_instance_receipt_invalid",
            "The database instance recovery receipt schema is unsupported.",
        )
        .with_detail("schemaVersion", receipt.schema_version.to_string()));
    }
    Ok(Some(DatabaseInstance {
        engine: receipt.engine,
        name: receipt.name,
        runtime_version: receipt.runtime_version,
        port: receipt.port,
        data_path: receipt.data_path,
        created_at: receipt.created_at,
        state: DatabaseInstanceState::Stopped,
        pid: None,
    }))
}

fn validate_instance_receipt(
    paths: &TorbenPaths,
    instance: &DatabaseInstance,
    engine: DatabaseEngine,
    name: &DatabaseInstanceName,
) -> TorbenResult<()> {
    let expected = database_instance_root_for(paths, engine, name).join("data");
    if instance.engine != engine
        || &instance.name != name
        || Path::new(&instance.data_path) != expected
    {
        return Err(TorbenError::new(
            "database_instance_receipt_invalid",
            "The database instance recovery receipt does not match its managed path.",
        )
        .with_detail("expectedPath", expected.display().to_string())
        .with_detail("recordedPath", instance.data_path.clone()));
    }
    Ok(())
}

fn write_instance_config(
    engine: DatabaseEngine,
    root: &Path,
    port: u16,
    runtime: &InstallRecord,
) -> TorbenResult<()> {
    match engine {
        DatabaseEngine::Mysql => {
            let contents = format!(
                "[mysqld]\nbasedir={}\ndatadir={}\nport={}\nbind-address=127.0.0.1\npid-file={}\nlog-error={}\ntmpdir={}\nsecure-file-priv={}\n",
                native_config_path(Path::new(&runtime.install_path)),
                native_config_path(&root.join("data")),
                port,
                native_config_path(&root.join("run/mysql.pid")),
                native_config_path(&root.join("logs/mysql-error.log")),
                native_config_path(&root.join("temp")),
                native_config_path(&root.join("data")),
            );
            write_managed_file(&root.join("config/my.ini"), contents.as_bytes())
        }
        DatabaseEngine::Redis => {
            let contents = format!(
                "bind 127.0.0.1\nprotected-mode yes\nport {}\ndir \"{}\"\ndbfilename dump.rdb\nappendonly no\nsave 60 1\npidfile \"{}\"\nlogfile \"{}\"\n",
                port,
                native_config_path(&root.join("data")),
                native_config_path(&root.join("run/redis.pid")),
                native_config_path(&root.join("logs/redis.log")),
            );
            write_managed_file(&root.join("config/redis.conf"), contents.as_bytes())
        }
        DatabaseEngine::Postgresql => Ok(()),
    }
}

fn write_managed_file(path: &Path, contents: &[u8]) -> TorbenResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    let mut file = options.open(path).map_err(instance_io_error)?;
    file.write_all(contents).map_err(instance_io_error)?;
    file.sync_all().map_err(instance_io_error)
}

fn spawn_server(mut command: Command, log_path: &Path, working_dir: &Path) -> TorbenResult<u32> {
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .map_err(instance_io_error)?;
    command
        .current_dir(working_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone().map_err(instance_io_error)?))
        .stderr(Stdio::from(log));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(0x0800_0000 | 0x0000_0200);
    }
    command
        .spawn()
        .map(|child| child.id())
        .map_err(instance_io_error)
}

fn run_checked(command: &mut Command, code: &str) -> TorbenResult<()> {
    let output = command.output().map_err(instance_io_error)?;
    if output.status.success() {
        return Ok(());
    }
    Err(TorbenError::new(code, "A managed database command failed.")
        .with_detail("status", output.status.to_string())
        .with_detail("stderr", bounded_output(&output.stderr))
        .with_detail("stdout", bounded_output(&output.stdout)))
}

fn run_to_file(command: &mut Command, destination: &Path, code: &str) -> TorbenResult<()> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(instance_io_error)?;
    command.stdout(Stdio::from(file));
    if let Err(error) = run_checked(command, code) {
        let _ = std::fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

fn copy_new(source: &Path, destination: &Path) -> TorbenResult<()> {
    let mut from = File::open(source).map_err(instance_io_error)?;
    let mut to = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .map_err(instance_io_error)?;
    std::io::copy(&mut from, &mut to).map_err(instance_io_error)?;
    to.sync_all().map_err(instance_io_error)
}

fn write_pid(path: &Path, pid: u32) -> TorbenResult<()> {
    write_managed_file(path, pid.to_string().as_bytes())
}

fn read_instance_pid(engine: DatabaseEngine, root: &Path) -> Option<u32> {
    let candidates = match engine {
        DatabaseEngine::Mysql => vec![root.join("run/mysql.pid"), root.join("run/pid")],
        DatabaseEngine::Redis => vec![root.join("run/redis.pid"), root.join("run/pid")],
        DatabaseEngine::Postgresql => vec![root.join("data/postmaster.pid")],
    };
    candidates.into_iter().find_map(|path| {
        std::fs::read_to_string(path)
            .ok()?
            .lines()
            .next()?
            .trim()
            .parse()
            .ok()
    })
}

fn backup_destination(
    instance: &DatabaseInstance,
    root: &Path,
    requested: Option<&str>,
    extension: &str,
) -> TorbenResult<PathBuf> {
    let destination = requested.map_or_else(
        || {
            root.join("backups").join(format!(
                "{}-{}.{}",
                instance.name,
                unique_suffix(),
                extension
            ))
        },
        PathBuf::from,
    );
    if !destination.is_absolute() {
        return Err(TorbenError::new(
            "database_backup_path_invalid",
            "An explicit backup destination must be an absolute path.",
        )
        .with_detail("path", destination.display().to_string()));
    }
    if destination.exists() {
        return Err(TorbenError::new(
            "database_backup_path_exists",
            "The backup destination already exists and will not be overwritten.",
        )
        .with_detail("path", destination.display().to_string()));
    }
    Ok(destination)
}

fn validate_restore_source(source: &str) -> TorbenResult<PathBuf> {
    let source = PathBuf::from(source);
    if !source.is_absolute() || !source.is_file() {
        return Err(TorbenError::new(
            "database_restore_source_invalid",
            "The restore source must be an existing absolute file path.",
        )
        .with_detail("path", source.display().to_string()));
    }
    Ok(source)
}

fn database_instance_root_from_record(instance: &DatabaseInstance) -> TorbenResult<PathBuf> {
    Path::new(&instance.data_path)
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            TorbenError::new(
                "database_instance_path_invalid",
                "The managed instance data path has no parent directory.",
            )
        })
}

fn native_config_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn bounded_output(bytes: &[u8]) -> String {
    const LIMIT: usize = 8 * 1024;
    String::from_utf8_lossy(&bytes[..bytes.len().min(LIMIT)])
        .trim()
        .to_owned()
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_nanos());
    format!("{nanos}-{}", std::process::id())
}

#[cfg(windows)]
fn process_is_active(pid: u32) -> bool {
    use std::os::windows::process::CommandExt as _;

    let mut command = Command::new("tasklist.exe");
    command.args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"]);
    command.creation_flags(0x0800_0000);
    let Ok(output) = command.output() else {
        return true;
    };
    if !output.status.success() {
        return true;
    }
    let expected = pid.to_string();
    String::from_utf8_lossy(&output.stdout).lines().any(|line| {
        line.split(',')
            .nth(1)
            .is_some_and(|value| value.trim_matches('"').trim() == expected)
    })
}

#[cfg(not(windows))]
fn process_is_active(pid: u32) -> bool {
    Path::new("/proc").join(pid.to_string()).exists()
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or_else(|_| "0".to_owned(), |value| value.as_secs().to_string())
}

fn instance_conflict(engine: DatabaseEngine, name: &DatabaseInstanceName) -> TorbenError {
    TorbenError::new(
        "database_instance_conflict",
        "The database instance already exists.",
    )
    .with_detail("engine", engine.to_string())
    .with_detail("name", name.to_string())
}

fn instance_not_found(engine: DatabaseEngine, name: &DatabaseInstanceName) -> TorbenError {
    TorbenError::new(
        "database_instance_not_found",
        "The database instance is not managed by Torben App.",
    )
    .with_detail("engine", engine.to_string())
    .with_detail("name", name.to_string())
}

fn state_required(
    instance: &DatabaseInstance,
    required: &str,
    actual: DatabaseInstanceState,
) -> TorbenError {
    TorbenError::new(
        "database_instance_state_invalid",
        "The database instance is not in the state required by this operation.",
    )
    .with_detail("engine", instance.engine.to_string())
    .with_detail("name", instance.name.to_string())
    .with_detail("required", required)
    .with_detail("actual", format!("{actual:?}").to_ascii_lowercase())
}

fn instance_io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "database_instance_io_failed",
        "A managed database instance filesystem or process operation failed.",
    )
    .with_detail("reason", error.to_string())
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tempfile::tempdir;
    use torben_contracts::{
        AppId, CreateDatabaseInstanceRequest, DatabaseEngine, DatabaseInstance,
        DatabaseInstanceName, DatabaseInstanceState, DeleteDatabaseInstanceRequest, ExactVersion,
        InstallRecord, InstallScope, SourceId,
    };

    use crate::{TorbenCore, TorbenPaths};

    use super::{backup_destination, bounded_output, native_config_path};

    fn fixture(root: &Path) -> DatabaseInstance {
        DatabaseInstance {
            engine: DatabaseEngine::Redis,
            name: DatabaseInstanceName::new("local").unwrap(),
            runtime_version: ExactVersion::from_str("8.2.1").unwrap(),
            port: 6379,
            data_path: root.join("data").display().to_string(),
            created_at: "fixture".to_owned(),
            state: DatabaseInstanceState::Stopped,
            pid: None,
        }
    }

    use std::path::Path;

    #[test]
    fn default_backups_stay_inside_the_instance_and_never_overwrite() {
        let root = tempdir().unwrap();
        let instance = fixture(root.path());
        let destination = backup_destination(&instance, root.path(), None, "rdb").unwrap();
        let expected_parent = root.path().join("backups");
        assert_eq!(destination.parent(), Some(expected_parent.as_path()));

        std::fs::create_dir_all(destination.parent().unwrap()).unwrap();
        std::fs::write(&destination, b"existing").unwrap();
        assert!(
            backup_destination(
                &instance,
                root.path(),
                Some(&destination.display().to_string()),
                "rdb"
            )
            .is_err()
        );
    }

    #[test]
    fn diagnostic_output_is_bounded() {
        let output = vec![b'x'; 16 * 1024];
        assert_eq!(bounded_output(&output).len(), 8 * 1024);
    }

    #[test]
    fn managed_config_paths_use_forward_slashes() {
        assert!(!native_config_path(Path::new("C:\\Torben Data\\db")).contains('\\'));
    }

    #[test]
    fn redis_instance_create_list_and_delete_share_the_persisted_core_lifecycle() {
        let root = tempdir().unwrap();
        let paths = TorbenPaths::for_test(root.path().to_path_buf());
        let core = TorbenCore::open(paths.clone()).unwrap();
        let version = ExactVersion::from_str("8.2.1").unwrap();
        let app_id = AppId::new("redis").unwrap();
        let install_path = paths.app_version_dir("redis", &version.to_string());
        std::fs::create_dir_all(&install_path).unwrap();
        core.store
            .add_installation(&InstallRecord {
                app_id: app_id.clone(),
                version: version.clone(),
                source_id: SourceId::new("redis.windows-community").unwrap(),
                scope: InstallScope::Managed,
                install_path: install_path.display().to_string(),
                installed_at: "fixture".to_owned(),
                health: "healthy".to_owned(),
            })
            .unwrap();
        core.store.set_selection(&app_id, &version).unwrap();

        let created = core
            .create_database_instance(CreateDatabaseInstanceRequest {
                engine: DatabaseEngine::Redis,
                name: DatabaseInstanceName::new("local").unwrap(),
                runtime_version: None,
                port: Some(16379),
            })
            .unwrap();

        assert_eq!(created.runtime_version, version);
        assert_eq!(created.state, DatabaseInstanceState::Stopped);
        let instance_root = Path::new(&created.data_path)
            .parent()
            .unwrap()
            .to_path_buf();
        assert!(instance_root.join("config/redis.conf").is_file());
        assert!(instance_root.join("backups").is_dir());
        assert_eq!(
            core.database_instances(Some(DatabaseEngine::Redis))
                .unwrap(),
            std::slice::from_ref(&created)
        );

        core.store
            .remove_database_instance(DatabaseEngine::Redis, &created.name)
            .unwrap();
        drop(core);
        let core = TorbenCore::open(paths.clone()).unwrap();
        assert_eq!(
            core.database_instances(Some(DatabaseEngine::Redis))
                .unwrap(),
            std::slice::from_ref(&created)
        );

        let staged_delete = paths
            .staging_dir()
            .join("database-delete-redis-local-fixture");
        std::fs::rename(&instance_root, &staged_delete).unwrap();
        drop(core);
        let core = TorbenCore::open(paths).unwrap();
        assert!(instance_root.is_dir());

        core.delete_database_instance(DeleteDatabaseInstanceRequest {
            engine: DatabaseEngine::Redis,
            name: created.name,
            confirm: true,
        })
        .unwrap();
        assert!(!instance_root.exists());
        assert!(
            core.database_instances(Some(DatabaseEngine::Redis))
                .unwrap()
                .is_empty()
        );
    }
}
