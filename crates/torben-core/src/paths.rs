use std::{
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
};

#[cfg(not(windows))]
use directories::ProjectDirs;
use torben_contracts::{TorbenError, TorbenResult};

pub const WINDOWS_DATA_ROOT_POINTER_FILE: &str = "TorbenApp.data-root";

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
        #[cfg(windows)]
        {
            let executable = std::env::current_exe().map_err(|error| {
                TorbenError::new(
                    "platform_directories_unavailable",
                    "Could not resolve the Torben App installation directory.",
                )
                .with_detail("reason", error.to_string())
            })?;
            Self::beside_windows_executable(&executable)
        }
        #[cfg(not(windows))]
        Self::platform_directories()
    }

    #[cfg(not(windows))]
    fn platform_directories() -> TorbenResult<Self> {
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

    #[cfg(any(windows, test))]
    fn beside_windows_executable(executable: &Path) -> TorbenResult<Self> {
        let directory = executable.parent().ok_or_else(|| {
            TorbenError::new(
                "platform_directories_unavailable",
                "Could not resolve the Torben App installation directory.",
            )
        })?;
        // Deployed aliases must reopen the same database as the adjacent GUI and CLI.
        let stem = executable
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_ascii_lowercase);
        let is_alias = matches!(
            stem.as_deref(),
            Some(
                "node"
                    | "npm"
                    | "npx"
                    | "java"
                    | "javac"
                    | "python"
                    | "python3"
                    | "pip"
                    | "pip3"
                    | "git"
                    | "code"
                    | "codex"
            )
        );
        let is_shim_directory = ["shims", "tools"].iter().enumerate().all(|(index, name)| {
            directory
                .ancestors()
                .nth(index)
                .and_then(Path::file_name)
                .and_then(|part| part.to_str())
                .is_some_and(|part| part.eq_ignore_ascii_case(name))
        });
        if is_alias && is_shim_directory {
            let data_root = directory.ancestors().nth(2).ok_or_else(|| {
                TorbenError::new(
                    "platform_directories_unavailable",
                    "Could not resolve the command shim's data directory.",
                )
            })?;
            let normalized = match data_root.file_name().and_then(|name| name.to_str()) {
                Some(name) if name.eq_ignore_ascii_case("userData") => data_root
                    .parent()
                    .map_or_else(|| data_root.to_path_buf(), |parent| parent.join("userData")),
                Some(name) if name.eq_ignore_ascii_case("data") => data_root
                    .parent()
                    .map_or_else(|| data_root.to_path_buf(), |parent| parent.join("data")),
                _ => data_root.to_path_buf(),
            };
            return Ok(Self::at_data_root(normalized, false));
        }
        let data_root = Self::windows_data_root_pointer(directory)?
            .unwrap_or_else(|| directory.join("userData"));
        Ok(Self::at_data_root(data_root, false))
    }

    #[cfg(any(windows, test))]
    fn windows_data_root_pointer(root: &Path) -> TorbenResult<Option<PathBuf>> {
        let pointer = root.join(WINDOWS_DATA_ROOT_POINTER_FILE);
        let metadata = match std::fs::symlink_metadata(&pointer) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(TorbenError::new(
                    "data_root_pointer_unavailable",
                    "Could not inspect the Torben App data-directory pointer.",
                )
                .with_detail("path", pointer.display().to_string())
                .with_detail("reason", error.to_string()));
            }
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 16 * 1024 {
            return Err(TorbenError::new(
                "data_root_pointer_invalid",
                "The Torben App data-directory pointer is not a valid regular file.",
            )
            .with_detail("path", pointer.display().to_string()));
        }
        let value = std::fs::read_to_string(&pointer).map_err(|error| {
            TorbenError::new(
                "data_root_pointer_unavailable",
                "Could not read the Torben App data-directory pointer.",
            )
            .with_detail("path", pointer.display().to_string())
            .with_detail("reason", error.to_string())
        })?;
        let value = value.trim();
        let path = PathBuf::from(value);
        if value.is_empty()
            || value.chars().any(char::is_control)
            || value.len() > 4096
            || !path.is_absolute()
        {
            return Err(TorbenError::new(
                "data_root_pointer_invalid",
                "The Torben App data-directory pointer must contain one absolute path.",
            )
            .with_detail("path", pointer.display().to_string()));
        }
        Ok(Some(path))
    }

    pub fn for_test(root: PathBuf) -> Self {
        Self::rooted(root, true)
    }

    fn rooted(root: PathBuf, isolated: bool) -> Self {
        let data = root.join("data");
        Self::at_data_root_with_layout(
            data,
            root.join("config"),
            root.join("cache"),
            root.join("logs"),
            isolated,
        )
    }

    fn at_data_root(data: PathBuf, isolated: bool) -> Self {
        let config = data.join("config");
        let cache = data.join("cache");
        let logs = data.join("logs");
        Self::at_data_root_with_layout(data, config, cache, logs, isolated)
    }

    fn at_data_root_with_layout(
        data: PathBuf,
        config: PathBuf,
        cache: PathBuf,
        logs: PathBuf,
        isolated: bool,
    ) -> Self {
        Self {
            app_library: Arc::new(RwLock::new(data.join("apps"))),
            data,
            config,
            cache,
            logs,
            isolated,
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

    pub(crate) fn version_catalog_cache(&self, app_id: &str) -> PathBuf {
        self.cache
            .join("version-catalogs")
            .join(format!("{app_id}.json"))
    }

    pub(crate) const fn is_isolated(&self) -> bool {
        self.isolated
    }
}

#[cfg(test)]
mod tests {
    use super::TorbenPaths;

    #[test]
    fn installation_directory_named_like_a_shim_directory_is_not_reinterpreted() {
        let base = tempfile::tempdir().unwrap();
        let root = base.path().join("data/tools/shims");
        let paths =
            TorbenPaths::beside_windows_executable(&root.join("torben-desktop.exe")).unwrap();
        assert_eq!(paths.data_dir(), root.join("userData"));
    }

    #[test]
    fn windows_gui_cli_and_deployed_aliases_share_the_installation_layout() {
        let root = tempfile::Builder::new()
            .prefix("Torben 安装 ")
            .tempdir()
            .unwrap();
        for executable in [
            "torben-desktop.exe",
            "torben.exe",
            "torben-shim.exe",
            "userData/tools/shims/node.exe",
            "userData/tools/shims/npm.exe",
            "userData/tools/shims/codex.exe",
            "USERDATA/TOOLS/SHIMS/NODE.EXE",
        ] {
            let paths =
                TorbenPaths::beside_windows_executable(&root.path().join(executable)).unwrap();
            paths.ensure_layout().unwrap();
            assert_eq!(paths.data_dir(), root.path().join("userData"));
            assert_eq!(paths.config_dir(), root.path().join("userData/config"));
            assert_eq!(paths.cache_dir(), root.path().join("userData/cache"));
            assert_eq!(paths.log_dir(), root.path().join("userData/logs"));
            assert_eq!(
                paths.version_catalog_cache("temurin"),
                root.path()
                    .join("userData/cache/version-catalogs/temurin.json")
            );
            assert_eq!(paths.app_library(), root.path().join("userData/apps"));
            assert_eq!(paths.shim_dir(), root.path().join("userData/tools/shims"));
            assert_eq!(paths.plugin_dir(), root.path().join("userData/plugins"));
            assert!(!paths.is_isolated());
        }
    }

    #[test]
    fn unwritable_layout_fails_without_falling_back_to_user_directories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("userData"), b"occupied").unwrap();
        let paths =
            TorbenPaths::beside_windows_executable(&root.path().join("torben.exe")).unwrap();
        assert_eq!(
            paths.ensure_layout().unwrap_err().code,
            "directory_create_failed"
        );
        assert!(!root.path().join("userData/config").exists());
    }

    #[test]
    fn windows_data_root_pointer_overrides_the_sibling_default_for_gui_and_aliases() {
        let root = tempfile::tempdir().unwrap();
        let selected = root.path().join("selected data");
        std::fs::write(
            root.path().join(super::WINDOWS_DATA_ROOT_POINTER_FILE),
            selected.display().to_string(),
        )
        .unwrap();

        let gui =
            TorbenPaths::beside_windows_executable(&root.path().join("TorbenApp.exe")).unwrap();
        let alias =
            TorbenPaths::beside_windows_executable(&selected.join("tools/shims/java.exe")).unwrap();
        assert_eq!(gui.data_dir(), selected);
        assert_eq!(alias.data_dir(), selected);
    }
}
