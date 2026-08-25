#![allow(clippy::needless_pass_by_value)]

use std::{ffi::OsString, path::Path, process::Command};

use torben_contracts::{AppId, TorbenError, TorbenResult};
use torben_core::TorbenCore;

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("torben shim error[{}]: {}", error.code, error.message);
            std::process::exit(127);
        }
    }
}

fn run() -> TorbenResult<i32> {
    let executable = std::env::current_exe().map_err(io_error)?;
    let stem = executable
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("torben-shim")
        .to_ascii_lowercase();
    let mut arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    let command = if stem == "torben-shim" {
        if arguments.is_empty() {
            return Err(TorbenError::new(
                "shim_command_missing",
                "Pass a command when invoking torben-shim directly.",
            ));
        }
        arguments.remove(0).to_string_lossy().to_string()
    } else {
        stem
    };
    let core = TorbenCore::open_default()?;
    let app_id = app_for_command(&command)?;
    apply_managed_arguments(&app_id, &mut arguments);
    let target = core.executable_for(&app_id, &command)?;
    let status = Command::new(&target)
        .args(arguments)
        .status()
        .map_err(|error| {
            TorbenError::new("shim_start_failed", "Could not start the selected command.")
                .with_detail("path", target.display().to_string())
                .with_detail("reason", error.to_string())
        })?;
    Ok(status.code().unwrap_or(1))
}

fn apply_managed_arguments(app_id: &AppId, arguments: &mut Vec<OsString>) {
    if app_id.as_str() == "vscode"
        && !arguments
            .iter()
            .any(|argument| argument == "--disable-updates")
    {
        arguments.insert(0, OsString::from("--disable-updates"));
    }
}

fn app_for_command(command: &str) -> TorbenResult<AppId> {
    match command {
        "node" | "npm" | "npx" => AppId::new("node"),
        "java" | "javac" => AppId::new("temurin"),
        "python" | "python3" | "pip" | "pip3" => AppId::new("python"),
        "git" => AppId::new("git"),
        "code" => AppId::new("vscode"),
        "codex" => AppId::new("codex"),
        _ => Err(
            TorbenError::new("unsupported_command", "This shim alias is not supported.")
                .with_detail("command", command),
        ),
    }
}

fn io_error(error: std::io::Error) -> TorbenError {
    TorbenError::new(
        "shim_io_failed",
        "The command shim could not access its environment.",
    )
    .with_detail("reason", error.to_string())
}

#[allow(dead_code)]
fn _is_safe_alias(path: &Path) -> bool {
    matches!(
        path.file_stem().and_then(|value| value.to_str()),
        Some(
            "node"
                | "npm"
                | "npx"
                | "java"
                | "javac"
                | "python"
                | "python3"
                | "pip"
                | "pip3"
                | "git"
                | "code"
                | "codex"
                | "torben-shim"
        )
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{app_for_command, apply_managed_arguments};

    #[test]
    fn maps_runtime_commands_to_their_selected_application() {
        assert_eq!(app_for_command("node").unwrap().as_str(), "node");
        assert_eq!(app_for_command("npm").unwrap().as_str(), "node");
        assert_eq!(app_for_command("java").unwrap().as_str(), "temurin");
        assert_eq!(app_for_command("javac").unwrap().as_str(), "temurin");
        assert_eq!(app_for_command("python").unwrap().as_str(), "python");
        assert_eq!(app_for_command("pip3").unwrap().as_str(), "python");
        assert_eq!(app_for_command("git").unwrap().as_str(), "git");
        assert_eq!(app_for_command("code").unwrap().as_str(), "vscode");
        assert_eq!(app_for_command("codex").unwrap().as_str(), "codex");
        assert_eq!(
            app_for_command("unknown").unwrap_err().code,
            "unsupported_command"
        );
    }

    #[test]
    fn managed_vscode_launch_disables_updates_once() {
        let app_id = app_for_command("code").unwrap();
        let mut arguments = vec![OsString::from(".")];

        apply_managed_arguments(&app_id, &mut arguments);
        apply_managed_arguments(&app_id, &mut arguments);

        assert_eq!(
            arguments,
            [OsString::from("--disable-updates"), OsString::from(".")]
        );
    }
}
