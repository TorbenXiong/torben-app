use std::path::{Path, PathBuf};

use torben_contracts::{TorbenError, TorbenResult};

#[derive(Debug, Clone)]
pub(crate) struct BundledShim {
    candidates: Vec<PathBuf>,
}

impl BundledShim {
    pub(crate) fn discover() -> TorbenResult<Self> {
        let current_executable = std::env::current_exe().map_err(|error| {
            TorbenError::new(
                "host_executable_unavailable",
                "Could not locate the Torben App executable.",
            )
            .with_detail("reason", error.to_string())
        })?;
        let executable_directory = current_executable.parent().ok_or_else(|| {
            TorbenError::new(
                "host_executable_unavailable",
                "The Torben App executable has no parent directory.",
            )
        })?;
        let filename = format!("torben-shim{}", std::env::consts::EXE_SUFFIX);
        let mut candidates = vec![
            executable_directory.join(&filename),
            executable_directory.join("tools").join(&filename),
        ];
        if executable_directory.ends_with("deps")
            && let Some(target_directory) = executable_directory.parent()
        {
            candidates.push(target_directory.join(&filename));
        }
        #[cfg(target_os = "macos")]
        if let Some(contents_directory) = executable_directory.parent() {
            candidates.push(contents_directory.join("Resources").join(&filename));
            candidates.push(
                contents_directory
                    .join("Resources")
                    .join("tools")
                    .join(&filename),
            );
        }
        Ok(Self { candidates })
    }

    pub(crate) fn executable(&self) -> Option<&Path> {
        self.candidates
            .iter()
            .find(|candidate| candidate.is_file())
            .map(PathBuf::as_path)
    }

    pub(crate) fn missing_error(&self) -> TorbenError {
        TorbenError::new(
            "bundled_shim_missing",
            "The bundled Torben command shim is missing.",
        )
        .with_detail(
            "searchedPaths",
            self.candidates
                .iter()
                .map(|candidate| candidate.display().to_string())
                .collect::<Vec<_>>()
                .join(";"),
        )
        .with_remediation("Reinstall Torben App or rebuild the complete workspace.")
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    pub(crate) fn from_executable(executable: PathBuf) -> Self {
        Self {
            candidates: vec![executable],
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::BundledShim;

    #[test]
    fn resolves_an_existing_explicit_shim() {
        let executable = NamedTempFile::new().unwrap();
        let shim = BundledShim::from_executable(executable.path().to_path_buf());

        assert_eq!(shim.executable(), Some(executable.path()));
    }
}
