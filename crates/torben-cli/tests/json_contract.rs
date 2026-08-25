use std::{
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

use serde_json::Value;

static NEXT_DATA_DIR: AtomicU64 = AtomicU64::new(0);

struct IsolatedDataDir {
    path: PathBuf,
}

impl IsolatedDataDir {
    fn new(label: &str) -> Self {
        let suffix = NEXT_DATA_DIR.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "torben-cli-contract-{}-{suffix}-{label}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create isolated CLI data root");
        Self { path }
    }
}

impl Drop for IsolatedDataDir {
    fn drop(&mut self) {
        let is_owned_fixture = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("torben-cli-contract-"));
        if is_owned_fixture {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn run_json(label: &str, arguments: &[&str]) -> (IsolatedDataDir, Output, Value) {
    let data = IsolatedDataDir::new(label);
    let output = Command::new(env!("CARGO_BIN_EXE_torben"))
        .args(arguments)
        .env("TORBEN_DATA_DIR", &data.path)
        .env("RUST_LOG", "off")
        .output()
        .expect("run the real torben CLI");
    let stdout = std::str::from_utf8(&output.stdout).expect("CLI stdout is UTF-8");
    let envelope = serde_json::from_str(stdout).unwrap_or_else(|error| {
        panic!("CLI stdout is not one JSON envelope: {error}; stdout={stdout:?}")
    });
    (data, output, envelope)
}

fn assert_complete_envelope(envelope: &Value) {
    let object = envelope.as_object().expect("JSON envelope is an object");
    let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, ["data", "error", "ok", "schemaVersion", "warnings"]);
    assert_eq!(envelope["schemaVersion"], 1);
    assert!(envelope["warnings"].is_array());
}

#[test]
fn query_command_emits_one_success_envelope_on_stdout() {
    let (_data, output, envelope) = run_json("query", &["app", "list", "--json"]);

    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert_complete_envelope(&envelope);
    assert_eq!(envelope["ok"], true);
    assert!(envelope["error"].is_null());
    let applications = envelope["data"].as_array().expect("application array");
    let mut ids = applications
        .iter()
        .filter_map(|application| application["id"].as_str())
        .collect::<Vec<_>>();
    ids.sort_unstable();
    assert_eq!(ids, ["codex", "git", "node", "python", "temurin", "vscode"]);
}

#[test]
fn query_error_uses_the_complete_failure_envelope_and_exit_code() {
    let (_data, output, envelope) = run_json("query-error", &["app", "info", "INVALID", "--json"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert_complete_envelope(&envelope);
    assert_eq!(envelope["ok"], false);
    assert!(envelope["data"].is_null());
    assert_eq!(envelope["error"]["code"], "invalid_identifier");
    assert!(envelope["error"]["message"].is_string());
    assert!(envelope["error"]["details"].is_object());
    assert!(envelope["error"].get("remediation").is_some());
}

#[test]
fn mutation_error_keeps_json_stdout_separate_from_diagnostics() {
    let (_data, output, envelope) = run_json("mutation-error", &["use", "node@1.0.0", "--json"]);

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert_complete_envelope(&envelope);
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "version_not_installed");
    assert_eq!(envelope["error"]["details"]["appId"], "node");
    assert_eq!(envelope["error"]["details"]["version"], "1.0.0");
}

#[test]
fn argument_error_uses_the_complete_failure_envelope_and_clap_exit_code() {
    let (_data, output, envelope) = run_json("argument-error", &["app", "info", "--json"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    assert_complete_envelope(&envelope);
    assert_eq!(envelope["ok"], false);
    assert!(envelope["data"].is_null());
    assert_eq!(envelope["error"]["code"], "cli_argument_invalid");
    assert_eq!(
        envelope["error"]["message"],
        "The command-line arguments are invalid."
    );
    assert!(
        envelope["error"]["details"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("required arguments were not provided"))
    );
    assert!(
        envelope["error"]["remediation"]
            .as_str()
            .is_some_and(|remediation| remediation.contains("--help"))
    );
}

#[test]
fn explicit_help_remains_human_readable_when_json_is_also_present() {
    let data = IsolatedDataDir::new("help");
    let output = Command::new(env!("CARGO_BIN_EXE_torben"))
        .args(["app", "info", "--help", "--json"])
        .env("TORBEN_DATA_DIR", &data.path)
        .env("RUST_LOG", "off")
        .output()
        .expect("run real torben CLI help");

    assert!(output.status.success());
    assert!(output.stderr.is_empty(), "stderr={:?}", output.stderr);
    let stdout = std::str::from_utf8(&output.stdout).expect("CLI help is UTF-8");
    assert!(stdout.starts_with("Usage:"), "stdout={stdout:?}");
    assert!(stdout.contains(" app info "), "stdout={stdout:?}");
    assert!(serde_json::from_str::<Value>(stdout).is_err());
}
