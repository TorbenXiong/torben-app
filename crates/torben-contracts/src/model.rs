use std::{fmt, str::FromStr};

use semver::Version;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{TorbenError, TorbenResult};

macro_rules! identifier {
    ($name:ident, $label:literal) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a validated identifier.
            ///
            /// # Errors
            ///
            /// Returns an error when the value is empty, too long, or contains unsupported
            /// characters.
            pub fn new(value: impl Into<String>) -> TorbenResult<Self> {
                let value = value.into();
                let valid = !value.is_empty()
                    && value.len() <= 128
                    && value.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'.' | b'-' | b'_')
                    });
                if valid {
                    Ok(Self(value))
                } else {
                    Err(TorbenError::new(
                        "invalid_identifier",
                        concat!(
                            $label,
                            " must contain only lowercase ASCII letters, digits, '.', '-' or '_'."
                        ),
                    )
                    .with_detail("value", value))
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = TorbenError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

identifier!(AppId, "AppId");
identifier!(PluginId, "PluginId");
identifier!(SourceId, "SourceId");

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExactVersion(Version);

impl ExactVersion {
    pub fn new(version: Version) -> Self {
        Self(version)
    }

    pub fn as_semver(&self) -> &Version {
        &self.0
    }
}

impl fmt::Display for ExactVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for ExactVersion {
    type Err = TorbenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Version::parse(value.trim_start_matches('v'))
            .map(Self)
            .map_err(|error| {
                TorbenError::new("invalid_version", "Expected an exact semantic version.")
                    .with_detail("value", value)
                    .with_detail("reason", error.to_string())
            })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallScope {
    Managed,
    External,
    PackageManager,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallSource {
    pub id: SourceId,
    pub display_name: String,
    pub managed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallRecord {
    pub app_id: AppId,
    pub version: ExactVersion,
    pub source_id: SourceId,
    pub scope: InstallScope,
    pub install_path: String,
    pub installed_at: String,
    pub health: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectionRecord {
    pub app_id: AppId,
    pub version: ExactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationDescriptor {
    pub id: AppId,
    pub display_name: String,
    pub summary: String,
    pub categories: Vec<String>,
    pub capabilities: Vec<String>,
    pub sources: Vec<InstallSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionDescriptor {
    pub version: ExactVersion,
    pub lts_name: Option<String>,
    pub released_at: String,
    pub recommended: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OperationId(Uuid);

impl OperationId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for OperationId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for OperationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for OperationId {
    type Err = TorbenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(value).map(Self).map_err(|error| {
            TorbenError::new(
                "invalid_operation_id",
                "Expected a valid operation identifier.",
            )
            .with_detail("value", value)
            .with_detail("reason", error.to_string())
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Install,
    Select,
    Uninstall,
    SourceInstall,
    SourceUninstall,
    SourceMigrate,
    Migrate,
    PluginInstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    Running,
    Cancelling,
    Succeeded,
    Failed,
    RolledBack,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationEvent {
    pub operation_id: OperationId,
    pub sequence: u64,
    pub state: OperationState,
    pub phase: String,
    pub message: String,
    pub progress: Option<f32>,
    pub timestamp: String,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::{AppId, ExactVersion, OperationId};

    #[test]
    fn identifiers_reject_path_characters() {
        assert!(AppId::new("node.js").is_ok());
        assert!(AppId::new("../node").is_err());
        assert!(AppId::new("Node").is_err());
    }

    #[test]
    fn exact_version_accepts_node_prefix() {
        assert_eq!(
            ExactVersion::from_str("v24.19.0").unwrap().to_string(),
            "24.19.0"
        );
        assert!(ExactVersion::from_str("lts").is_err());
    }

    #[test]
    fn operation_id_round_trips_and_rejects_invalid_values() {
        let operation_id = OperationId::new();
        assert_eq!(
            OperationId::from_str(&operation_id.to_string()).unwrap(),
            operation_id
        );
        assert_eq!(
            OperationId::from_str("not-an-operation").unwrap_err().code,
            "invalid_operation_id"
        );
    }
}
