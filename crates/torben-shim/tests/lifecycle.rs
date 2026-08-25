use std::{
    iter,
    path::{Path, PathBuf},
    process::{Command, Output},
    str::FromStr,
};

use torben_contracts::{AppId, ExactVersion, InstallRecord, InstallScope, OperationId, SourceId};
use torben_core::{StateStore, TorbenCore, TorbenPaths};

struct IsolatedRoot {
    path: PathBuf,
}

impl IsolatedRoot {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("torben-shim-e2e-{}", OperationId::new()));
        std::fs::create_dir_all(&path).expect("create isolated shim test root");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for IsolatedRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[test]
fn fresh_shim_processes_follow_persisted_node_selection() {
    let root = IsolatedRoot::new();
    let paths = TorbenPaths::for_test(root.path().to_path_buf());
    let core = TorbenCore::open(paths.clone()).expect("open isolated core");
    let store = StateStore::open(paths.state_database()).expect("open isolated state store");
    let app_id = AppId::new("node").expect("valid Node.js app id");
    let version_a = ExactVersion::from_str("22.17.0").expect("valid first fixture version");
    let version_b = ExactVersion::from_str("24.4.0").expect("valid second fixture version");
    let install_a = paths.app_version_dir(app_id.as_str(), &version_a.to_string());
    let install_b = paths.app_version_dir(app_id.as_str(), &version_b.to_string());

    compile_fixture_node_tools(&install_a, &version_a);
    compile_fixture_node_tools(&install_b, &version_b);
    store
        .add_installation(&install_record(&app_id, &version_a, &install_a))
        .expect("record first fixture installation");
    store
        .add_installation(&install_record(&app_id, &version_b, &install_b))
        .expect("record second fixture installation");
    store
        .set_selection(&app_id, &version_a)
        .expect("select first fixture version");

    let real_shim = Path::new(env!("CARGO_BIN_EXE_torben-shim"));
    core.install_shims(real_shim)
        .expect("install real command shim aliases");
    let shim_directory = paths.shim_dir();

    assert_new_terminal_commands(&shim_directory, root.path(), &version_a);

    store
        .set_selection(&app_id, &version_b)
        .expect("switch persisted selection");
    assert_new_terminal_commands(&shim_directory, root.path(), &version_b);

    drop(core);
    drop(store);

    let reopened_core = TorbenCore::open(paths.clone()).expect("reopen isolated core");
    let reopened_store = StateStore::open(paths.state_database()).expect("reopen state store");
    assert_eq!(
        reopened_core
            .selected_version(&app_id)
            .expect("read reopened selection"),
        Some(version_b.clone())
    );
    assert_new_terminal_commands(&shim_directory, root.path(), &version_b);

    let installations = reopened_store
        .list_installations()
        .expect("list persisted installations");
    assert_eq!(installations.len(), 2);
    assert!(
        installations
            .iter()
            .any(|record| record.version == version_a)
    );
    assert!(
        installations
            .iter()
            .any(|record| record.version == version_b)
    );

    reopened_core
        .clear_selection(&app_id)
        .expect("clear persisted selection through Core");
    let output = run_from_new_terminal("node", &shim_directory, root.path());
    assert_eq!(output.status.code(), Some(127));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no_selected_version"),
        "unexpected shim stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn install_record(app_id: &AppId, version: &ExactVersion, install_path: &Path) -> InstallRecord {
    InstallRecord {
        app_id: app_id.clone(),
        version: version.clone(),
        source_id: SourceId::new("node.official").expect("valid Node.js source id"),
        scope: InstallScope::Managed,
        install_path: install_path.display().to_string(),
        installed_at: "2026-08-24T00:00:00Z".to_owned(),
        health: "healthy".to_owned(),
    }
}

fn compile_fixture_node_tools(install_path: &Path, version: &ExactVersion) {
    let node = if cfg!(windows) {
        install_path.join("node.exe")
    } else {
        install_path.join("bin").join("node")
    };
    std::fs::create_dir_all(node.parent().expect("fixture executable parent"))
        .expect("create fixture installation");
    let source = install_path.join("fixture-node.rs");
    std::fs::write(
        &source,
        format!("fn main() {{ println!(\"v{version}\"); }}\n"),
    )
    .expect("write fixture Node.js source");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .arg(&source)
        .arg("-o")
        .arg(&node)
        .output()
        .expect("run rustc for fixture Node.js executable");
    assert!(
        output.status.success(),
        "fixture rustc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = std::fs::metadata(&node)
            .expect("read fixture executable permissions")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&node, permissions).expect("make fixture Node.js executable");
    }

    if cfg!(windows) {
        for command in ["npm", "npx"] {
            std::fs::write(
                install_path.join(format!("{command}.cmd")),
                format!("@echo off\r\necho v{version}\r\n"),
            )
            .expect("write fixture Node.js companion command");
        }
    } else {
        for command in ["npm", "npx"] {
            std::fs::copy(&node, install_path.join("bin").join(command))
                .expect("copy fixture Node.js companion command");
        }
    }
}

fn run_from_new_terminal(command: &str, shim_directory: &Path, root: &Path) -> Output {
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        iter::once(shim_directory.to_path_buf()).chain(std::env::split_paths(&inherited_path)),
    )
    .expect("compose isolated terminal PATH");
    let mut process = if cfg!(windows) {
        let mut process = Command::new("cmd.exe");
        process.args(["/d", "/s", "/c", command]);
        process
    } else {
        let mut process = Command::new("sh");
        process.args(["-c", command]);
        process
    };
    process
        .env("TORBEN_DATA_DIR", root)
        .env("PATH", path)
        .output()
        .expect("run installed Node.js shim")
}

fn assert_new_terminal_commands(shim_directory: &Path, root: &Path, expected: &ExactVersion) {
    for command in ["node", "npm", "npx"] {
        assert_successful_version(
            &run_from_new_terminal(command, shim_directory, root),
            expected,
        );
    }
}

fn assert_successful_version(output: &Output, expected: &ExactVersion) {
    assert!(
        output.status.success(),
        "shim failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("v{expected}")
    );
}
