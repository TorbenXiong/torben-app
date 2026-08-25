use serde::{Deserialize, Serialize};

use crate::{AppId, TorbenError, TorbenResult};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LanguagePreference {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "zh-CN")]
    SimplifiedChinese,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePreferences {
    #[serde(default = "enabled_by_default")]
    pub notify_torben_app: bool,
    #[serde(default = "enabled_by_default")]
    pub notify_managed_apps: bool,
    #[serde(default)]
    pub automatically_install_torben_app: bool,
    #[serde(default)]
    pub automatically_update_apps: Vec<AppId>,
}

impl Default for UpdatePreferences {
    fn default() -> Self {
        Self {
            notify_torben_app: true,
            notify_managed_apps: true,
            automatically_install_torben_app: false,
            automatically_update_apps: Vec::new(),
        }
    }
}

impl UpdatePreferences {
    /// Validates persisted per-application update preferences.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or duplicate application identifier.
    pub fn validate(&self) -> TorbenResult<()> {
        let mut seen = std::collections::BTreeSet::new();
        for app_id in &self.automatically_update_apps {
            AppId::new(app_id.as_str())?;
            if !seen.insert(app_id.as_str()) {
                return Err(TorbenError::new(
                    "update_preferences_invalid",
                    "Automatic application update preferences contain a duplicate application.",
                )
                .with_detail("appId", app_id.to_string()));
            }
        }
        Ok(())
    }
}

const fn enabled_by_default() -> bool {
    true
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSettings {
    pub theme: ThemePreference,
    pub language: LanguagePreference,
    #[serde(default)]
    pub updates: UpdatePreferences,
}

impl UserSettings {
    /// Validates settings whose serialized forms require additional invariants.
    ///
    /// # Errors
    ///
    /// Returns an error when update preferences are invalid.
    pub fn validate(&self) -> TorbenResult<()> {
        self.updates.validate()
    }
}

#[cfg(test)]
mod tests {
    use crate::AppId;

    use super::{LanguagePreference, ThemePreference, UpdatePreferences, UserSettings};

    #[test]
    fn settings_use_stable_wire_values() {
        let settings = UserSettings {
            theme: ThemePreference::Dark,
            language: LanguagePreference::SimplifiedChinese,
            updates: UpdatePreferences::default(),
        };

        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            r#"{"theme":"dark","language":"zh-CN","updates":{"notifyTorbenApp":true,"notifyManagedApps":true,"automaticallyInstallTorbenApp":false,"automaticallyUpdateApps":[]}}"#
        );
        assert_eq!(
            serde_json::from_str::<UserSettings>(r#"{"theme":"system","language":"en"}"#).unwrap(),
            UserSettings {
                theme: ThemePreference::System,
                language: LanguagePreference::English,
                updates: UpdatePreferences::default(),
            }
        );
    }

    #[test]
    fn update_preferences_reject_invalid_and_duplicate_application_ids() {
        let mut preferences = UpdatePreferences::default();
        preferences
            .automatically_update_apps
            .push(AppId::new("node").unwrap());
        preferences
            .automatically_update_apps
            .push(AppId::new("node").unwrap());
        assert_eq!(
            preferences.validate().unwrap_err().code,
            "update_preferences_invalid"
        );

        let settings: UserSettings = serde_json::from_str(
            r#"{"theme":"system","language":"en","updates":{"automaticallyUpdateApps":["../unsafe"]}}"#,
        )
        .unwrap();
        assert_eq!(settings.validate().unwrap_err().code, "invalid_identifier");
    }
}
