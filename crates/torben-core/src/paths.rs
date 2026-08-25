use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

use directories::ProjectDirs;
use torben_contracts::{TorbenError, TorbenResult};

#[derive(Debug, Clone)]
pub struct TorbenPaths {
    data: PathBuf,
    config: PathBuf,
    cache: PathBuf,
    logs: PathBuf,
    app_library: Arc<RwLock<PathBuf>>,
    isolated: bool,
}

impl TorbenPaths {
    pub fn discover() -> TorbenResult<Self> {
        if let Some(override_path) = std::env::var_os("TORBEN_DATA_DIR") {
            let root = PathBuf::from(override_path);
            return Ok(Self::for_test(root));
        }
        let project =
            ProjectDirs::from("io.github", "TorbenXiong", "torben-app").ok_or_else(|| {
                TorbenError::new(
                    "platform_directories_unavailable",
                    "Could not resolve platform data directories.",
                )
            })?;
        let data = project.data_local_dir().to_path_buf();
        Ok(Self {
            app_library: Arc::new(RwLock::new(data.join("apps"))),
            data,
            config: project.config_dir().to_path_buf(),
            cache: project.cache_dir().to_path_buf(),
            logs: project.data_local_dir().join("logs"),
            isolated: false,
        })
    }

    pub fn for_test(root: PathBuf) -> Self {
        let data = root.join("data");
        Self {
            app_library: Arc::new(RwLock::new(data.join("apps"))),
            data,
            config: root.join("config"),
            cache: root.join("cache"),
            logs: root.join("logs"),
            isolated: true,
        }
    }

    pub fn ensure_layout(&self) -> TorbenResult<()> {
        self.ensure_base_layout()?;
        Self::create_directory(&self.app_library())
    }

    pub(crate) fn ensure_base_layout(&self) -> TorbenResult<()> {
        for path in [
            &self.data,
            &self.config,
            &self.cache,
            &self.logs,
            &self.staging_dir(),
            &self.operation_dir(),
            &self.shim_dir(),
            &self.plugin_dir(),
        ] {
            Self::create_directory(path)?;
        }
        Ok(())
    }

    fn create_directory(path: &Path) -> TorbenResult<()> {
        std::fs::create_dir_all(path).map_err(|error| {
            TorbenError::new(
                "directory_create_failed",
                "Could not create a Torben App directory.",
            )
            .with_detail("path", path.display().to_string())
            .with_detail("reason", error.to_string())
        })
    }

    pub fn data_dir(&self) -> &Path {
        &self.data
    }

    pub fn config_dir(&self) -> &Path {
        &self.config
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache
    }

    pub fn log_dir(&self) -> &Path {
        &self.logs
    }

    pub fn state_database(&self) -> PathBuf {
        self.data.join("state.db")
    }

    pub fn app_library(&self) -> PathBuf {
        self.app_library
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn default_app_library(&self) -> PathBuf {
        self.data.join("apps")
    }

    pub(crate) fn set_app_library(&self, path: PathBuf) {
        *self
            .app_library
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = path;
    }

    pub fn app_version_dir(&self, app_id: &str, version: &str) -> PathBuf {
        self.app_library().join(app_id).join(version)
    }

    pub fn download_dir(&self, app_id: &str, version: &str) -> PathBuf {
        self.cache.join("downloads").join(app_id).join(version)
    }

    pub fn staging_dir(&self) -> PathBuf {
        self.data.join("staging")
    }

    pub fn operation_dir(&self) -> PathBuf {
        self.data.join("operations")
    }

    pub fn shim_dir(&self) -> PathBuf {
        self.data.join("tools").join("shims")
    }

    pub fn workspace_lock(&self) -> PathBuf {
        self.data.join("workspace.lock")
    }

    pub fn plugin_dir(&self) -> PathBuf {
        self.data.join("plugins")
    }

    pub fn official_plugin_registry_cache(&self) -> PathBuf {
        self.cache
            .join("plugin-registry")
            .join("official")
            .join("registry.json")
    }

    pub(crate) const fn is_isolated(&self) -> bool {
        self.isolated
    }
}
