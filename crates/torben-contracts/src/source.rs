use std::{collections::BTreeMap, fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::plugin::{InstallPlan, UninstallPlan};
use crate::{AppId, ExactVersion, InstallRecord, SourceId, TorbenError, TorbenResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAdapterKind {
    Winget,
    Homebrew,
    Apt,
    Dnf,
}

impl SourceAdapterKind {
    pub const ALL: [Self; 4] = [Self::Winget, Self::Homebrew, Self::Apt, Self::Dnf];
}

impl fmt::Display for SourceAdapterKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Winget => "winget",
            Self::Homebrew => "homebrew",
            Self::Apt => "apt",
            Self::Dnf => "dnf",
        })
    }
}

impl FromStr for SourceAdapterKind {
    type Err = TorbenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "winget" => Ok(Self::Winget),
            "homebrew" | "brew" => Ok(Self::Homebrew),
            "apt" | "apt-get" => Ok(Self::Apt),
            "dnf" | "dnf5" => Ok(Self::Dnf),
            _ => Err(TorbenError::new(
                "source_adapter_invalid",
                "Expected winget, homebrew, apt, or dnf.",
            )
            .with_detail("value", value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePackageKind {
    Native,
    Formula,
    Cask,
}

impl fmt::Display for SourcePackageKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Native => "native",
            Self::Formula => "formula",
            Self::Cask => "cask",
        })
    }
}

impl FromStr for SourcePackageKind {
    type Err = TorbenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "native" | "package" => Ok(Self::Native),
            "formula" => Ok(Self::Formula),
            "cask" => Ok(Self::Cask),
            _ => Err(TorbenError::new(
                "source_package_kind_invalid",
                "Expected native, formula, or cask.",
            )
            .with_detail("value", value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAction {
    Install,
    Uninstall,
}

impl fmt::Display for SourceAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Install => "install",
            Self::Uninstall => "uninstall",
        })
    }
}

impl FromStr for SourceAction {
    type Err = TorbenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "install" => Ok(Self::Install),
            "uninstall" | "remove" => Ok(Self::Uninstall),
            _ => Err(
                TorbenError::new("source_action_invalid", "Expected install or uninstall.")
                    .with_detail("value", value),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageCoordinate(String);

impl PackageCoordinate {
    /// Creates a package-manager coordinate that is safe to pass as one process argument.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty, option-like, path-traversing, or unsupported value.
    pub fn new(value: impl Into<String>) -> TorbenResult<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 256
            && !value.starts_with('-')
            && !value.contains("//")
            && !value
                .split('/')
                .any(|segment| matches!(segment, "" | "." | ".."))
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(byte, b'.' | b'-' | b'_' | b'+' | b'@' | b'/' | b':')
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(TorbenError::new(
                "package_coordinate_invalid",
                "The package coordinate contains unsupported or unsafe characters.",
            )
            .with_detail("value", value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for PackageCoordinate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for PackageCoordinate {
    type Err = TorbenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourcePackageVersion(String);

impl SourcePackageVersion {
    /// Preserves a package manager's raw version without interpreting it as `SemVer`.
    ///
    /// # Errors
    ///
    /// Returns an error for empty, oversized, option-like, padded, or control-character input.
    pub fn new(value: impl Into<String>) -> TorbenResult<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 256
            && !value.starts_with('-')
            && value.trim() == value
            && value.chars().all(|character| !character.is_control());
        if valid {
            Ok(Self(value))
        } else {
            Err(TorbenError::new(
                "source_package_version_invalid",
                "The source package version is empty, too long, option-like, padded, or contains control characters.",
            )
            .with_detail("value", value))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SourcePackageVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for SourcePackageVersion {
    type Err = TorbenError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAdapterAvailability {
    Unsupported,
    Missing,
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceAdapterStatus {
    pub adapter: SourceAdapterKind,
    pub source_id: SourceId,
    pub availability: SourceAdapterAvailability,
    pub executable: Option<String>,
    pub version: Option<String>,
    pub supports_exact_version: bool,
    pub requires_elevation: bool,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePackageState {
    pub adapter: SourceAdapterKind,
    pub source_id: SourceId,
    pub coordinate: PackageCoordinate,
    pub package_kind: SourcePackageKind,
    pub installed: bool,
    pub installed_version: Option<SourcePackageVersion>,
    pub architecture: Option<String>,
    pub manager_owned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceOperationPlan {
    pub action: SourceAction,
    pub adapter: SourceAdapterKind,
    pub source_id: SourceId,
    pub coordinate: PackageCoordinate,
    pub package_kind: SourcePackageKind,
    pub package_version: Option<SourcePackageVersion>,
    pub executable: String,
    pub preview_arguments: Vec<String>,
    pub execute_arguments: Vec<String>,
    #[serde(default)]
    pub execution_identity: Option<String>,
    pub environment: BTreeMap<String, String>,
    pub requires_elevation: bool,
    pub exact_version_guaranteed: bool,
    pub mutates_system: bool,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceExecutionRequest {
    pub app_id: AppId,
    pub app_version: ExactVersion,
    pub action: SourceAction,
    pub adapter: SourceAdapterKind,
    pub coordinate: PackageCoordinate,
    pub package_kind: SourcePackageKind,
    pub package_version: Option<SourcePackageVersion>,
    pub executable_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_execution_identity: Option<String>,
    pub accept_system_changes: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceExecutionOutcome {
    OwnershipCommitted,
    OwnershipRemoved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceExecutionResult {
    pub operation_id: crate::OperationId,
    pub plan: SourceOperationPlan,
    pub before: SourcePackageState,
    pub after: SourcePackageState,
    pub outcome: SourceExecutionOutcome,
    pub installation: Option<PackageInstallationRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMigrationRequest {
    pub app_id: AppId,
    pub app_version: ExactVersion,
    pub target_adapter: SourceAdapterKind,
    pub target_coordinate: PackageCoordinate,
    pub target_package_kind: SourcePackageKind,
    pub target_package_version: Option<SourcePackageVersion>,
    pub target_executable_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_plan_token: Option<String>,
    pub accept_system_changes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMigrationPlan {
    pub app_id: AppId,
    pub app_version: ExactVersion,
    pub current_owner: PackageInstallationRecord,
    pub current_state: SourcePackageState,
    pub target_state: SourcePackageState,
    pub uninstall_current: SourceOperationPlan,
    pub install_target: SourceOperationPlan,
    pub cleanup_target: SourceOperationPlan,
    pub restore_current: SourceOperationPlan,
    pub target_executable_path: String,
    pub approval_token: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMigrationResult {
    pub operation_id: crate::OperationId,
    pub plan: SourceMigrationPlan,
    pub installation: PackageInstallationRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedToPackageMigrationPlan {
    pub app_id: AppId,
    pub app_version: ExactVersion,
    pub current_installation: InstallRecord,
    pub uninstall_current: UninstallPlan,
    pub target_state: SourcePackageState,
    pub install_target: SourceOperationPlan,
    pub cleanup_target: SourceOperationPlan,
    pub target_executable_path: String,
    pub approval_token: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedToPackageMigrationResult {
    pub operation_id: crate::OperationId,
    pub plan: ManagedToPackageMigrationPlan,
    pub installation: PackageInstallationRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageToManagedMigrationRequest {
    pub app_id: AppId,
    pub app_version: ExactVersion,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_plan_token: Option<String>,
    pub accept_system_changes: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageToManagedMigrationPlan {
    pub app_id: AppId,
    pub app_version: ExactVersion,
    pub current_owner: PackageInstallationRecord,
    pub current_state: SourcePackageState,
    pub uninstall_current: SourceOperationPlan,
    pub restore_current: SourceOperationPlan,
    pub install_managed: InstallPlan,
    pub managed_target_path: String,
    pub approval_token: String,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageToManagedMigrationResult {
    pub operation_id: crate::OperationId,
    pub plan: PackageToManagedMigrationPlan,
    pub installation: InstallRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageInstallationRecord {
    pub app_id: AppId,
    pub app_version: ExactVersion,
    pub source_id: SourceId,
    pub adapter: SourceAdapterKind,
    pub coordinate: PackageCoordinate,
    pub package_kind: SourcePackageKind,
    pub package_version: SourcePackageVersion,
    pub architecture: String,
    pub executable_path: String,
    pub owned_by_torben: bool,
    pub installed_at: String,
    pub health: String,
}

#[cfg(test)]
mod tests {
    use super::{PackageCoordinate, SourcePackageVersion};

    #[test]
    fn package_coordinate_rejects_options_and_path_traversal() {
        assert!(PackageCoordinate::new("Microsoft.VisualStudioCode").is_ok());
        assert!(PackageCoordinate::new("homebrew/core/node@24").is_ok());
        assert!(PackageCoordinate::new("--force").is_err());
        assert!(PackageCoordinate::new("tap/../formula").is_err());
    }

    #[test]
    fn source_versions_preserve_non_semver_package_syntax() {
        assert!(SourcePackageVersion::new("1:20.11.1+dfsg-2~deb12u1").is_ok());
        assert!(SourcePackageVersion::new("3.12.11-2.fc42").is_ok());
        assert!(SourcePackageVersion::new("--force").is_err());
        assert!(SourcePackageVersion::new("bad\nversion").is_err());
    }
}
