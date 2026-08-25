use std::{collections::BTreeSet, path::Path, str::FromStr, time::Duration};

use torben_contracts::{
    ExactVersion, PluginId, TorbenError, TorbenResult,
    plugin::{
        InitializeParams, InitializeResult, PLUGIN_PROTOCOL_VERSION, PluginCapability,
        PluginManifest, SchemaAction, SchemaActionKind, SchemaActionParams, SchemaActionResult,
        SchemaField, SchemaFieldKind, SchemaPage, SchemaPageListParams, SchemaPageListResult,
        method,
    },
};
use torben_plugin_host::{PluginClient, PluginVerifier};

use crate::{StateStore, TorbenPaths, node_plugin::current_target};

const PLUGIN_CALL_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PAGES: usize = 16;
const MAX_SECTIONS_PER_PAGE: usize = 16;
const MAX_FIELDS_PER_SECTION: usize = 32;
const MAX_ACTIONS_PER_SECTION: usize = 16;
const MAX_OPTIONS_PER_FIELD: usize = 64;
const MAX_ID_CHARS: usize = 64;
const MAX_LABEL_CHARS: usize = 128;
const MAX_DESCRIPTION_CHARS: usize = 1024;
const MAX_VALUE_CHARS: usize = 4096;

pub(crate) struct InstalledSchemaSession {
    client: PluginClient,
    plugin_id: PluginId,
}

impl InstalledSchemaSession {
    pub(crate) async fn connect(
        paths: &TorbenPaths,
        store: &StateStore,
        plugin_id: &PluginId,
    ) -> TorbenResult<Self> {
        let record = store.get_plugin(plugin_id)?.ok_or_else(|| {
            TorbenError::new("plugin_not_found", "The plugin is not installed.")
                .with_detail("pluginId", plugin_id.to_string())
        })?;
        if !record.enabled {
            return Err(TorbenError::new(
                "plugin_disabled",
                "The plugin must be enabled before its pages can be used.",
            )
            .with_detail("pluginId", plugin_id.to_string()));
        }
        let stored_manifest: PluginManifest =
            serde_json::from_str(&record.manifest_json).map_err(|error| {
                TorbenError::new(
                    "plugin_manifest_state_invalid",
                    "The stored plugin manifest is invalid.",
                )
                .with_detail("reason", error.to_string())
            })?;
        ensure_schema_capability(&stored_manifest)?;
        let package_root = paths
            .plugin_dir()
            .join(plugin_id.as_str())
            .join(record.version.to_string());
        ensure_regular_directory(&package_root)?;
        let manifest_path = package_root.join("plugin.json");
        ensure_regular_file(&manifest_path)?;
        let verified = PluginVerifier::developer_mode().verify(&manifest_path)?;
        ensure_regular_file(&verified.executable)?;
        if verified.manifest != stored_manifest
            || verified.manifest.id != record.id
            || verified.manifest.version != record.version
        {
            return Err(TorbenError::new(
                "plugin_manifest_state_mismatch",
                "The installed plugin package does not match its persisted manifest.",
            ));
        }
        let mut client = PluginClient::spawn(&verified, PLUGIN_CALL_TIMEOUT)?;
        let initialized: InitializeResult = client
            .call(
                method::INITIALIZE,
                &InitializeParams {
                    protocol_version: PLUGIN_PROTOCOL_VERSION,
                    host_version: ExactVersion::from_str(env!("CARGO_PKG_VERSION"))?,
                    target: current_target(),
                    locale: "en-US".to_owned(),
                },
            )
            .await?;
        if initialized.protocol_version != PLUGIN_PROTOCOL_VERSION
            || initialized.plugin_id != record.id
            || initialized.plugin_version != record.version
        {
            return Err(TorbenError::new(
                "plugin_identity_mismatch",
                "The installed plugin process identity does not match its package.",
            ));
        }
        Ok(Self {
            client,
            plugin_id: record.id,
        })
    }

    pub(crate) async fn pages(&mut self) -> TorbenResult<Vec<SchemaPage>> {
        let result: SchemaPageListResult = self
            .client
            .call(
                method::SCHEMA_PAGES,
                &SchemaPageListParams {
                    plugin_id: self.plugin_id.clone(),
                },
            )
            .await?;
        if result.plugin_id != self.plugin_id {
            return Err(TorbenError::new(
                "plugin_response_mismatch",
                "The plugin returned schema pages for a different plugin.",
            ));
        }
        Ok(result.pages)
    }

    pub(crate) async fn action(
        &mut self,
        params: &SchemaActionParams,
    ) -> TorbenResult<SchemaActionResult> {
        self.client.call(method::SCHEMA_ACTION, params).await
    }

    pub(crate) async fn shutdown(self) -> TorbenResult<()> {
        self.client.shutdown().await
    }
}

pub(crate) fn ensure_schema_capability(manifest: &PluginManifest) -> TorbenResult<()> {
    if manifest.capabilities.contains(&PluginCapability::SchemaUi) {
        Ok(())
    } else {
        Err(TorbenError::new(
            "plugin_capability_missing",
            "The plugin does not declare the schema UI capability.",
        )
        .with_detail("pluginId", manifest.id.to_string()))
    }
}

pub(crate) fn validate_pages(pages: &[SchemaPage]) -> TorbenResult<()> {
    if pages.len() > MAX_PAGES {
        return Err(schema_invalid("The plugin returned too many schema pages."));
    }
    let mut page_ids = BTreeSet::new();
    for page in pages {
        validate_id(&page.id, "page")?;
        if !page_ids.insert(page.id.as_str()) {
            return Err(schema_invalid("Schema page identifiers must be unique."));
        }
        validate_text(&page.title, MAX_LABEL_CHARS, "page title")?;
        validate_optional_text(
            page.description.as_deref(),
            MAX_DESCRIPTION_CHARS,
            "page description",
        )?;
        if page.sections.len() > MAX_SECTIONS_PER_PAGE {
            return Err(schema_invalid("A schema page contains too many sections."));
        }
        let mut section_ids = BTreeSet::new();
        let mut field_ids = BTreeSet::new();
        for section in &page.sections {
            validate_id(&section.id, "section")?;
            if !section_ids.insert(section.id.as_str()) {
                return Err(schema_invalid("Schema section identifiers must be unique."));
            }
            validate_optional_text(section.title.as_deref(), MAX_LABEL_CHARS, "section title")?;
            validate_optional_text(
                section.description.as_deref(),
                MAX_DESCRIPTION_CHARS,
                "section description",
            )?;
            if section.fields.len() > MAX_FIELDS_PER_SECTION
                || section.actions.len() > MAX_ACTIONS_PER_SECTION
            {
                return Err(schema_invalid(
                    "A schema section contains too many fields or actions.",
                ));
            }
            let mut action_ids = BTreeSet::new();
            for field in &section.fields {
                validate_field(field)?;
                if !field_ids.insert(field.id.as_str()) {
                    return Err(schema_invalid(
                        "Schema field identifiers must be unique within a page.",
                    ));
                }
            }
            for action in &section.actions {
                validate_action(action)?;
                if !action_ids.insert(action.id.as_str()) {
                    return Err(schema_invalid(
                        "Schema action identifiers must be unique within a section.",
                    ));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn validate_action_request(
    pages: &[SchemaPage],
    params: &SchemaActionParams,
    confirmed: bool,
) -> TorbenResult<()> {
    validate_pages(pages)?;
    let page = pages
        .iter()
        .find(|page| page.id == params.page_id)
        .ok_or_else(|| {
            TorbenError::new(
                "plugin_schema_page_not_found",
                "The schema page was not found.",
            )
        })?;
    let section = page
        .sections
        .iter()
        .find(|section| section.id == params.section_id)
        .ok_or_else(|| {
            TorbenError::new(
                "plugin_schema_section_not_found",
                "The schema section was not found.",
            )
        })?;
    let action = section
        .actions
        .iter()
        .find(|action| action.id == params.action_id)
        .ok_or_else(|| {
            TorbenError::new(
                "plugin_schema_action_not_found",
                "The schema action was not found.",
            )
        })?;
    if !action.enabled {
        return Err(TorbenError::new(
            "plugin_schema_action_disabled",
            "The schema action is currently disabled.",
        ));
    }
    if action.kind == SchemaActionKind::Destructive && !confirmed {
        return Err(TorbenError::new(
            "plugin_schema_confirmation_required",
            "The destructive schema action requires explicit confirmation.",
        ));
    }
    for (field_id, value) in &params.values {
        let field = page
            .sections
            .iter()
            .flat_map(|section| &section.fields)
            .find(|field| field.id == *field_id)
            .ok_or_else(|| {
                TorbenError::new(
                    "plugin_schema_value_invalid",
                    "The action supplied a value for an unknown schema field.",
                )
                .with_detail("fieldId", field_id)
            })?;
        if field.read_only {
            return Err(TorbenError::new(
                "plugin_schema_value_invalid",
                "The action cannot change a read-only schema field.",
            )
            .with_detail("fieldId", field_id));
        }
        validate_field_value(field, value)?;
    }
    Ok(())
}

pub(crate) fn validate_action_result(
    plugin_id: &PluginId,
    requested_page: &str,
    result: &SchemaActionResult,
) -> TorbenResult<()> {
    if &result.plugin_id != plugin_id || result.page.id != requested_page {
        return Err(TorbenError::new(
            "plugin_response_mismatch",
            "The schema action returned a different plugin or page.",
        ));
    }
    validate_pages(std::slice::from_ref(&result.page))?;
    validate_optional_text(
        result.message.as_deref(),
        MAX_DESCRIPTION_CHARS,
        "action message",
    )
}

fn validate_field(field: &SchemaField) -> TorbenResult<()> {
    validate_id(&field.id, "field")?;
    validate_text(&field.label, MAX_LABEL_CHARS, "field label")?;
    validate_optional_text(
        field.description.as_deref(),
        MAX_DESCRIPTION_CHARS,
        "field description",
    )?;
    validate_optional_text(
        field.placeholder.as_deref(),
        MAX_LABEL_CHARS,
        "field placeholder",
    )?;
    if field.options.len() > MAX_OPTIONS_PER_FIELD {
        return Err(schema_invalid("A schema field contains too many options."));
    }
    let mut option_values = BTreeSet::new();
    for option in &field.options {
        validate_text(&option.value, MAX_LABEL_CHARS, "option value")?;
        validate_text(&option.label, MAX_LABEL_CHARS, "option label")?;
        if !option_values.insert(option.value.as_str()) {
            return Err(schema_invalid("Schema option values must be unique."));
        }
    }
    match field.kind {
        SchemaFieldKind::Select if field.options.is_empty() => {
            return Err(schema_invalid("A select field must declare options."));
        }
        SchemaFieldKind::Text | SchemaFieldKind::Boolean | SchemaFieldKind::Status
            if !field.options.is_empty() =>
        {
            return Err(schema_invalid(
                "Only select fields may declare schema options.",
            ));
        }
        _ => {}
    }
    if field.kind == SchemaFieldKind::Status && !field.read_only {
        return Err(schema_invalid("Status fields must be read-only."));
    }
    if let Some(value) = &field.value {
        validate_field_value(field, value)?;
    }
    Ok(())
}

fn validate_action(action: &SchemaAction) -> TorbenResult<()> {
    validate_id(&action.id, "action")?;
    validate_text(&action.label, MAX_LABEL_CHARS, "action label")?;
    validate_optional_text(
        action.description.as_deref(),
        MAX_DESCRIPTION_CHARS,
        "action description",
    )?;
    validate_optional_text(
        action.confirmation.as_deref(),
        MAX_DESCRIPTION_CHARS,
        "action confirmation",
    )?;
    if action.kind == SchemaActionKind::Destructive
        && action
            .confirmation
            .as_deref()
            .is_none_or(|confirmation| confirmation.trim().is_empty())
    {
        return Err(schema_invalid(
            "A destructive action must provide confirmation text.",
        ));
    }
    Ok(())
}

fn validate_field_value(field: &SchemaField, value: &str) -> TorbenResult<()> {
    if value.chars().count() > MAX_VALUE_CHARS || (field.required && value.trim().is_empty()) {
        return Err(TorbenError::new(
            "plugin_schema_value_invalid",
            "A schema field value is missing or too long.",
        )
        .with_detail("fieldId", &field.id));
    }
    match field.kind {
        SchemaFieldKind::Boolean if !matches!(value, "true" | "false") => Err(TorbenError::new(
            "plugin_schema_value_invalid",
            "A boolean field value is invalid.",
        )
        .with_detail("fieldId", &field.id)),
        SchemaFieldKind::Select if !field.options.iter().any(|option| option.value == value) => {
            Err(TorbenError::new(
                "plugin_schema_value_invalid",
                "A select field value is not one of its declared options.",
            )
            .with_detail("fieldId", &field.id))
        }
        _ => Ok(()),
    }
}

fn validate_id(value: &str, kind: &str) -> TorbenResult<()> {
    if value.is_empty()
        || value.chars().count() > MAX_ID_CHARS
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(schema_invalid(format!(
            "The schema {kind} identifier is invalid."
        )));
    }
    Ok(())
}

fn validate_text(value: &str, maximum: usize, kind: &str) -> TorbenResult<()> {
    if value.trim().is_empty() || value.chars().count() > maximum {
        return Err(schema_invalid(format!("The schema {kind} is invalid.")));
    }
    Ok(())
}

fn validate_optional_text(value: Option<&str>, maximum: usize, kind: &str) -> TorbenResult<()> {
    value.map_or(Ok(()), |value| validate_text(value, maximum, kind))
}

fn ensure_regular_directory(path: &Path) -> TorbenResult<()> {
    let metadata = path.symlink_metadata().map_err(plugin_io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(TorbenError::new(
            "plugin_package_invalid",
            "The installed plugin package is not a regular directory.",
        ));
    }
    Ok(())
}

fn ensure_regular_file(path: &Path) -> TorbenResult<()> {
    let metadata = path.symlink_metadata().map_err(plugin_io_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(TorbenError::new(
            "plugin_package_invalid",
            "An installed plugin package file is not a regular file.",
        ));
    }
    Ok(())
}

fn plugin_io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "plugin_package_io_failed",
        "The installed plugin package could not be read.",
    )
    .with_detail("reason", error.to_string())
}

fn schema_invalid(message: impl Into<String>) -> TorbenError {
    TorbenError::new("plugin_schema_invalid", message)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use torben_contracts::{
        PluginId,
        plugin::{
            SchemaAction, SchemaActionKind, SchemaActionParams, SchemaActionResult, SchemaField,
            SchemaFieldKind, SchemaOption, SchemaPage, SchemaSection,
        },
    };

    use super::{validate_action_request, validate_action_result, validate_pages};

    #[test]
    fn validates_bounded_typed_schema_pages() {
        assert!(validate_pages(&[fixture_page()]).is_ok());

        let mut duplicate = fixture_page();
        let duplicate_field = duplicate.sections[0].fields[0].clone();
        duplicate.sections[0].fields.push(duplicate_field);
        assert_eq!(
            validate_pages(&[duplicate]).unwrap_err().code,
            "plugin_schema_invalid"
        );
    }

    #[test]
    fn destructive_actions_require_confirmation_and_writable_values() {
        let plugin_id = PluginId::new("app.example.schema").unwrap();
        let params = SchemaActionParams {
            plugin_id,
            page_id: "settings".to_owned(),
            section_id: "general".to_owned(),
            action_id: "reset".to_owned(),
            values: BTreeMap::from([("channel".to_owned(), "lts".to_owned())]),
        };

        let error = validate_action_request(&[fixture_page()], &params, false).unwrap_err();
        assert_eq!(error.code, "plugin_schema_confirmation_required");
        assert!(validate_action_request(&[fixture_page()], &params, true).is_ok());

        let mut read_only = params;
        read_only
            .values
            .insert("status".to_owned(), "changed".to_owned());
        let error = validate_action_request(&[fixture_page()], &read_only, true).unwrap_err();
        assert_eq!(error.code, "plugin_schema_value_invalid");
    }

    #[test]
    fn action_result_must_preserve_plugin_and_page_identity() {
        let plugin_id = PluginId::new("app.example.schema").unwrap();
        let mut result = SchemaActionResult {
            plugin_id: plugin_id.clone(),
            page: fixture_page(),
            message: Some("Updated".to_owned()),
        };
        assert!(validate_action_result(&plugin_id, "settings", &result).is_ok());

        result.page.id = "other".to_owned();
        assert_eq!(
            validate_action_result(&plugin_id, "settings", &result)
                .unwrap_err()
                .code,
            "plugin_response_mismatch"
        );
    }

    fn fixture_page() -> SchemaPage {
        SchemaPage {
            id: "settings".to_owned(),
            title: "Settings".to_owned(),
            description: Some("Fixture settings".to_owned()),
            sections: vec![SchemaSection {
                id: "general".to_owned(),
                title: Some("General".to_owned()),
                description: None,
                fields: vec![
                    SchemaField {
                        id: "channel".to_owned(),
                        label: "Channel".to_owned(),
                        description: None,
                        kind: SchemaFieldKind::Select,
                        value: Some("lts".to_owned()),
                        placeholder: None,
                        options: vec![
                            SchemaOption {
                                value: "lts".to_owned(),
                                label: "LTS".to_owned(),
                            },
                            SchemaOption {
                                value: "current".to_owned(),
                                label: "Current".to_owned(),
                            },
                        ],
                        read_only: false,
                        required: true,
                    },
                    SchemaField {
                        id: "status".to_owned(),
                        label: "Status".to_owned(),
                        description: None,
                        kind: SchemaFieldKind::Status,
                        value: Some("Ready".to_owned()),
                        placeholder: None,
                        options: Vec::new(),
                        read_only: true,
                        required: false,
                    },
                ],
                actions: vec![SchemaAction {
                    id: "reset".to_owned(),
                    label: "Reset".to_owned(),
                    description: None,
                    kind: SchemaActionKind::Destructive,
                    enabled: true,
                    confirmation: Some("Reset plugin settings?".to_owned()),
                }],
            }],
        }
    }
}
