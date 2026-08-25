use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedLibraryStatus {
    pub path: String,
    pub default_path: String,
    pub custom: bool,
    pub bytes_used: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedLibraryMigrationResult {
    pub previous_path: String,
    pub current_path: String,
    pub bytes_copied: u64,
    pub source_cleanup_pending: bool,
}

#[cfg(test)]
mod tests {
    use super::ManagedLibraryStatus;

    #[test]
    fn managed_library_status_uses_stable_wire_names() {
        let status = ManagedLibraryStatus {
            path: "/data/apps".to_owned(),
            default_path: "/default/apps".to_owned(),
            custom: true,
            bytes_used: 42,
        };
        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"path":"/data/apps","defaultPath":"/default/apps","custom":true,"bytesUsed":42}"#
        );
    }
}
