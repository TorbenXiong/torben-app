use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{AppId, PluginId, TorbenError, TorbenResult};

const SUPPORTED_ENVIRONMENT_APPS: [&str; 4] = ["node", "temurin", "python", "rust"];
const MAX_ENVIRONMENT_VARIABLES_PER_APP: usize = 32;
const MAX_ENVIRONMENT_VARIABLE_VALUE_BYTES: usize = 8 * 1024;

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
        let mut seen = BTreeSet::new();
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApplicationEnvironments(pub BTreeMap<AppId, BTreeMap<String, String>>);

impl ApplicationEnvironments {
    /// Returns configured variables for an application.
    pub fn get(&self, app_id: &AppId) -> Option<&BTreeMap<String, String>> {
        self.0.get(app_id)
    }

    /// Validates plugin-scoped process environment variables.
    ///
    /// # Errors
    ///
    /// Returns an error for unsupported applications, unsafe names, reserved variables, or values
    /// that exceed the persisted settings limit.
    pub fn validate(&self) -> TorbenResult<()> {
        for (app_id, variables) in &self.0 {
            if !SUPPORTED_ENVIRONMENT_APPS.contains(&app_id.as_str()) {
                return Err(TorbenError::new(
                    "application_environment_unsupported",
                    "This application does not support plugin-scoped environment variables.",
                )
                .with_detail("appId", app_id.to_string()));
            }
            if variables.len() > MAX_ENVIRONMENT_VARIABLES_PER_APP {
                return Err(TorbenError::new(
                    "application_environment_too_large",
                    "Too many environment variables were configured for one application.",
                )
                .with_detail("appId", app_id.to_string())
                .with_detail("maximum", MAX_ENVIRONMENT_VARIABLES_PER_APP.to_string()));
            }
            let mut names = BTreeSet::new();
            for (name, value) in variables {
                validate_environment_variable_name(name)?;
                let normalized = name.to_ascii_uppercase();
                if !names.insert(normalized.clone()) {
                    return Err(TorbenError::new(
                        "application_environment_duplicate",
                        "Environment variable names must be unique without regard to case.",
                    )
                    .with_detail("appId", app_id.to_string())
                    .with_detail("name", name));
                }
                if reserved_environment_variable(app_id, &normalized) {
                    return Err(TorbenError::new(
                        "application_environment_reserved",
                        "This environment variable is managed by Torben App and cannot be overridden.",
                    )
                    .with_detail("appId", app_id.to_string())
                    .with_detail("name", name));
                }
                if value.len() > MAX_ENVIRONMENT_VARIABLE_VALUE_BYTES || value.contains('\0') {
                    return Err(TorbenError::new(
                        "application_environment_value_invalid",
                        "Environment variable values must be free of null bytes and no larger than 8 KiB.",
                    )
                    .with_detail("appId", app_id.to_string())
                    .with_detail("name", name));
                }
            }
        }
        Ok(())
    }
}

fn validate_environment_variable_name(name: &str) -> TorbenResult<()> {
    let mut bytes = name.bytes();
    let first = bytes.next();
    let valid = name.len() <= 128
        && first.is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(TorbenError::new(
            "application_environment_name_invalid",
            "Environment variable names must use ASCII letters, digits, and underscores and cannot start with a digit.",
        )
        .with_detail("name", name))
    }
}

fn reserved_environment_variable(app_id: &AppId, normalized: &str) -> bool {
    if normalized.starts_with("TORBEN_")
        || matches!(
            normalized,
            "PATH" | "PATHEXT" | "SYSTEMROOT" | "COMSPEC" | "TEMP" | "TMP" | "TMPDIR"
        )
    {
        return true;
    }
    match app_id.as_str() {
        "node" => matches!(
            normalized,
            "NPM_CONFIG_CACHE"
                | "NPM_CONFIG_PREFIX"
                | "NPM_CONFIG_USERCONFIG"
                | "NPM_CONFIG_GLOBALCONFIG"
                | "NPM_PACKAGE_CONFIG_NODE_GYP_DEVDIR"
                | "NODE_REPL_HISTORY"
                | "NODE_COMPILE_CACHE"
                | "PNPM_CONFIG_STORE_DIR"
                | "PNPM_CONFIG_STATE_DIR"
                | "PNPM_CONFIG_CACHE_DIR"
        ),
        "python" => matches!(
            normalized,
            "PIP_CACHE_DIR"
                | "PIP_CONFIG_FILE"
                | "PIP_DISABLE_PIP_VERSION_CHECK"
                | "PYTHONUSERBASE"
        ),
        "rust" => normalized == "CARGO_HOME",
        _ => false,
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
    #[serde(default)]
    pub plugin_order: Vec<PluginId>,
    #[serde(default)]
    pub application_environments: ApplicationEnvironments,
}

impl UserSettings {
    /// Validates settings whose serialized forms require additional invariants.
    ///
    /// # Errors
    ///
    /// Returns an error when update preferences are invalid.
    pub fn validate(&self) -> TorbenResult<()> {
        self.updates.validate()?;
        let mut plugin_ids = BTreeSet::new();
        for plugin_id in &self.plugin_order {
            PluginId::new(plugin_id.as_str())?;
            if !plugin_ids.insert(plugin_id.as_str()) {
                return Err(TorbenError::new(
                    "plugin_order_invalid",
                    "Plugin display order contains a duplicate plugin.",
                )
                .with_detail("pluginId", plugin_id.to_string()));
            }
        }
        self.application_environments.validate()
    }
}

#[cfg(test)]
mod tests {
    use crate::AppId;

    use super::{
        ApplicationEnvironments, LanguagePreference, ThemePreference, UpdatePreferences,
        UserSettings,
    };

    #[test]
    fn settings_use_stable_wire_values() {
        let settings = UserSettings {
            theme: ThemePreference::Dark,
            language: LanguagePreference::SimplifiedChinese,
            updates: UpdatePreferences::default(),
            plugin_order: Vec::new(),
            application_environments: ApplicationEnvironments::default(),
        };

        assert_eq!(
            serde_json::to_string(&settings).unwrap(),
            r#"{"theme":"dark","language":"zh-CN","updates":{"notifyTorbenApp":true,"notifyManagedApps":true,"automaticallyInstallTorbenApp":false,"automaticallyUpdateApps":[]},"pluginOrder":[],"applicationEnvironments":{}}"#
        );
        assert_eq!(
            serde_json::from_str::<UserSettings>(r#"{"theme":"system","language":"en"}"#).unwrap(),
            UserSettings {
                theme: ThemePreference::System,
                language: LanguagePreference::English,
                updates: UpdatePreferences::default(),
                plugin_order: Vec::new(),
                application_environments: ApplicationEnvironments::default(),
            }
        );
    }

    #[test]
    fn application_environments_validate_supported_and_reserved_variables() {
        let node = AppId::new("node").unwrap();
        let mut settings = UserSettings::default();
        settings.application_environments.0.insert(
            node.clone(),
            [("NODE_OPTIONS".to_owned(), "--enable-source-maps".to_owned())]
                .into_iter()
                .collect(),
        );
        assert!(settings.validate().is_ok());

        settings.application_environments.0.insert(
            node,
            [("npm_config_cache".to_owned(), "outside".to_owned())]
                .into_iter()
                .collect(),
        );
        assert_eq!(
            settings.validate().unwrap_err().code,
            "application_environment_reserved"
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
