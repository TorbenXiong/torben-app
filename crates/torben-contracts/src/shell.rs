use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShellIntegrationState {
    Disabled,
    Managed,
    External,
    Outdated,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellIntegrationStatus {
    pub state: ShellIntegrationState,
    pub shim_path: String,
    pub targets: Vec<String>,
    pub new_terminal_required: bool,
}

#[cfg(test)]
mod tests {
    use super::{ShellIntegrationState, ShellIntegrationStatus};

    #[test]
    fn shell_status_uses_stable_wire_values() {
        let status = ShellIntegrationStatus {
            state: ShellIntegrationState::Managed,
            shim_path: "C:\\Torben\\shims".to_owned(),
            targets: vec!["HKCU\\Environment\\Path".to_owned()],
            new_terminal_required: true,
        };

        assert_eq!(
            serde_json::to_string(&status).unwrap(),
            r#"{"state":"managed","shimPath":"C:\\Torben\\shims","targets":["HKCU\\Environment\\Path"],"newTerminalRequired":true}"#
        );
    }
}
