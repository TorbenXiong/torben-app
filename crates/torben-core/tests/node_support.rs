use std::str::FromStr;

use torben_contracts::{AppId, ExactVersion, InstallRecord, InstallScope, PluginId, SourceId};
use torben_core::{StateStore, TorbenCore, TorbenPaths};

#[test]
fn node_plugin_is_opt_in_and_preserves_runtime_data_on_removal() {
    let root = tempfile::tempdir().unwrap();
    let paths = TorbenPaths::for_test(root.path().to_path_buf());
    let core = TorbenCore::open(paths.clone()).unwrap();
    let app_id = AppId::new("node").unwrap();
    let plugin_id = PluginId::new("app.torben.plugin.node").unwrap();
    #[cfg(not(feature = "test-fixtures"))]
    {
        assert!(core.application(&app_id).unwrap().capabilities.is_empty());
        assert!(
            !core
                .plugins()
                .unwrap()
                .iter()
                .find(|plugin| plugin.id == plugin_id)
                .unwrap()
                .enabled
        );
    }
    core.install_bundled_node(b"fixture plugin", b"fixture shim")
        .unwrap();
    assert!(
        core.application(&app_id)
            .unwrap()
            .capabilities
            .contains(&"install".to_owned())
    );
    assert!(
        paths
            .plugin_dir()
            .join(plugin_id.as_str())
            .join(env!("CARGO_PKG_VERSION"))
            .join(format!(
                "torben-plugin-node{}",
                std::env::consts::EXE_SUFFIX
            ))
            .is_file()
    );
    let version = ExactVersion::from_str("24.19.0").unwrap();
    let installation = paths.app_version_dir("node", &version.to_string());
    std::fs::create_dir_all(&installation).unwrap();
    let store = StateStore::open(paths.state_database()).unwrap();
    store
        .add_installation(&InstallRecord {
            app_id: app_id.clone(),
            version: version.clone(),
            source_id: SourceId::new("node.official").unwrap(),
            scope: InstallScope::Managed,
            install_path: installation.display().to_string(),
            installed_at: "fixture".to_owned(),
            health: "healthy".to_owned(),
        })
        .unwrap();
    assert_eq!(
        core.uninstall_bundled_node().unwrap_err().code,
        "plugin_in_use"
    );
    let data = paths.data_dir().join("node/config");
    std::fs::create_dir_all(&data).unwrap();
    std::fs::write(data.join("npmrc"), "fixture-setting=true").unwrap();
    store.remove_installation(&app_id, &version).unwrap();
    core.uninstall_bundled_node().unwrap();
    assert_eq!(
        std::fs::read_to_string(data.join("npmrc")).unwrap(),
        "fixture-setting=true"
    );
    #[cfg(not(feature = "test-fixtures"))]
    assert!(core.application(&app_id).unwrap().capabilities.is_empty());
}
