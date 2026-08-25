use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{AppId, ExactVersion, InstallRecord};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedUpdateCandidate {
    pub app_id: AppId,
    pub channel: String,
    pub installed_version: ExactVersion,
    pub available_version: ExactVersion,
    pub selected_version: Option<ExactVersion>,
    pub released_at: String,
    pub recommended: bool,
    pub automatic: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedUpdateWarning {
    pub app_id: AppId,
    pub code: String,
    pub message: String,
    pub details: BTreeMap<String, String>,
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedUpdateCheck {
    pub checked_apps: usize,
    pub candidates: Vec<ManagedUpdateCandidate>,
    pub warnings: Vec<ManagedUpdateWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedUpdateResult {
    pub candidate: ManagedUpdateCandidate,
    pub installation: InstallRecord,
    pub selection_updated: bool,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use crate::{AppId, ExactVersion};

    use super::ManagedUpdateCandidate;

    #[test]
    fn managed_update_candidate_uses_stable_camel_case_fields() {
        let candidate = ManagedUpdateCandidate {
            app_id: AppId::new("node").unwrap(),
            channel: "24".to_owned(),
            installed_version: ExactVersion::from_str("24.19.0").unwrap(),
            available_version: ExactVersion::from_str("24.20.1").unwrap(),
            selected_version: Some(ExactVersion::from_str("24.19.0").unwrap()),
            released_at: "2026-08-24T00:00:00Z".to_owned(),
            recommended: true,
            automatic: false,
        };
        let value = serde_json::to_value(candidate).unwrap();
        assert_eq!(value["appId"], "node");
        assert_eq!(value["installedVersion"], "24.19.0");
        assert_eq!(value["availableVersion"], "24.20.1");
        assert_eq!(value["selectedVersion"], "24.19.0");
    }
}
