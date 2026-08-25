use serde::{Deserialize, Serialize};

use crate::TorbenError;

pub const API_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiEnvelope<T> {
    pub schema_version: u32,
    pub ok: bool,
    pub data: Option<T>,
    pub error: Option<TorbenError>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl<T> ApiEnvelope<T> {
    pub fn success(data: T) -> Self {
        Self {
            schema_version: API_SCHEMA_VERSION,
            ok: true,
            data: Some(data),
            error: None,
            warnings: Vec::new(),
        }
    }

    pub fn failure(error: TorbenError) -> Self {
        Self {
            schema_version: API_SCHEMA_VERSION,
            ok: false,
            data: None,
            error: Some(error),
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{API_SCHEMA_VERSION, ApiEnvelope};
    use crate::TorbenError;

    #[test]
    fn success_and_failure_serialize_the_complete_stable_envelope() {
        assert_eq!(
            serde_json::to_value(ApiEnvelope::success(json!({ "version": "24.19.0" }))).unwrap(),
            json!({
                "schemaVersion": API_SCHEMA_VERSION,
                "ok": true,
                "data": { "version": "24.19.0" },
                "error": null,
                "warnings": []
            })
        );

        assert_eq!(
            serde_json::to_value(ApiEnvelope::<serde_json::Value>::failure(TorbenError::new(
                "version_not_installed",
                "Install the version before selecting it."
            )))
            .unwrap(),
            json!({
                "schemaVersion": API_SCHEMA_VERSION,
                "ok": false,
                "data": null,
                "error": {
                    "code": "version_not_installed",
                    "message": "Install the version before selecting it.",
                    "details": {},
                    "remediation": null
                },
                "warnings": []
            })
        );
    }
}
