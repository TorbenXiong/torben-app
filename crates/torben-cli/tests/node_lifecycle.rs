#![cfg(feature = "test-fixtures")]

use std::{
    collections::BTreeMap,
    io::{Read as _, Write as _},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    str::FromStr,
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use torben_contracts::{
    AppId, ExactVersion, OperationId, OperationState, plugin::PLUGIN_PROTOCOL_VERSION,
};
use torben_core::{
    NodeFixtureConfiguration, NodeProvider, StateStore, TorbenCore, TorbenPaths, TorbenTaskClient,
    test_fixtures,
};

const VERSION: &str = "24.19.0";
const SIGNATURE: &[u8] = b"torben-real-cli-node-fixture-signature-v1";
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);

struct IsolatedRoot {
    path: PathBuf,
}

impl IsolatedRoot {
    fn new() -> Self {
        let suffix = NEXT_ROOT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "torben-cli-node-lifecycle-{}-{suffix}",
            std::process::id()
        ));
        std::fs::create_dir_all(&path).expect("create isolated CLI lifecycle root");
        Self { path }
    }
}

impl Drop for IsolatedRoot {
    fn drop(&mut self) {
        let owned = self
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("torben-cli-node-lifecycle-"));
        if owned {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[test]
#[allow(clippy::too_many_lines)]
fn real_cli_completes_node_install_select_shim_and_uninstall_lifecycle() {
    let root = IsolatedRoot::new();
    let version = ExactVersion::from_str(VERSION).expect("valid fixture version");
    let official_distribution = NodeProvider::official()
        .expect("create official provider without network access")
        .distribution(&version)
        .expect("resolve the current target distribution");
    let fixture_node = compile_fixture_executable(
        &root.path,
        "fixture-node",
        &format!("fn main() {{ println!(\"v{VERSION}\"); }}\n"),
    );
    let archive = test_fixtures::build_node_archive(&official_distribution, &fixture_node)
        .expect("build fixture Node.js archive");
    let manifest = format!(
        "{}  {}\n",
        test_fixtures::sha256_hex(&archive),
        official_distribution.archive_name
    );
    let version_prefix = format!("/dist/v{VERSION}");
    let routes = BTreeMap::from([
        (
            format!("{version_prefix}/SHASUMS256.txt"),
            manifest.into_bytes(),
        ),
        (
            format!("{version_prefix}/SHASUMS256.txt.sig"),
            SIGNATURE.to_vec(),
        ),
        (
            format!("{version_prefix}/{}", official_distribution.archive_name),
            archive,
        ),
    ]);
    let (base_url, server) = fixture_server(routes, 3);

    let paths = TorbenPaths::for_test(root.path.clone());
    let install_path = paths.app_version_dir("node", VERSION);
    let plugin = compile_fixture_plugin(
        &root.path,
        &base_url,
        &official_distribution.archive_name,
        &install_path,
    );
    let shim = real_shim_next_to_cli();
    let config = root.path.join("node-fixture.json");
    std::fs::write(
        &config,
        serde_json::to_vec_pretty(&json!({
            "baseUrl": base_url,
            "checksumSignatureHex": encode_hex(SIGNATURE),
            "pluginExecutable": plugin,
            "shimExecutable": shim
        }))
        .expect("serialize CLI fixture configuration"),
    )
    .expect("write CLI fixture configuration");

    let versions = run_json(&root.path, &config, &["version", "list", "node", "--json"]);
    assert_success(&versions);
    assert_eq!(versions.envelope["data"][0]["version"], VERSION);

    let installed = run_json(&root.path, &config, &["install", "node@lts", "--json"]);
    assert_success(&installed);
    assert_eq!(installed.envelope["data"]["version"], VERSION);
    assert_eq!(
        installed.envelope["data"]["installPath"],
        install_path.display().to_string()
    );
    assert!(install_path.is_dir());

    let selected = run_json(&root.path, &config, &["use", "node@24.19.0", "--json"]);
    assert_success(&selected);
    assert_eq!(selected.envelope["data"]["version"], VERSION);

    let shim_path = run_json(&root.path, &config, &["shim", "path", "--json"]);
    assert_success(&shim_path);
    let shim_directory = PathBuf::from(
        shim_path.envelope["data"]["path"]
            .as_str()
            .expect("shim path is a string"),
    );
    assert_new_terminal_command(&root.path, &shim_directory, "node --version", "v24.19.0");
    assert_new_terminal_command(&root.path, &shim_directory, "npm --version", "11.0.0");
    assert_new_terminal_command(&root.path, &shim_directory, "npx --version", "11.0.0");

    let cleared = run_json(&root.path, &config, &["use", "node@none", "--json"]);
    assert_success(&cleared);
    assert!(cleared.envelope["data"]["version"].is_null());

    let uninstalled = run_json(
        &root.path,
        &config,
        &["uninstall", "node@24.19.0", "--json"],
    );
    assert_success(&uninstalled);
    assert!(!install_path.exists());

    let missing = run_json(&root.path, &config, &["use", "node@24.19.0", "--json"]);
    assert_eq!(missing.output.status.code(), Some(1));
    assert_complete_envelope(&missing.envelope);
    assert_eq!(missing.envelope["ok"], false);
    assert_eq!(missing.envelope["error"]["code"], "version_not_installed");
    assert!(missing.output.stderr.is_empty());

    server.join().expect("fixture HTTP server completes");
}

#[test]
fn fixture_feature_rejects_non_loopback_http_sources() {
    let root = IsolatedRoot::new();
    let cli = Path::new(env!("CARGO_BIN_EXE_torben"));
    let config = root.path.join("invalid-node-fixture.json");
    std::fs::write(
        &config,
        serde_json::to_vec(&json!({
            "baseUrl": "http://example.com/dist/",
            "checksumSignatureHex": "01",
            "pluginExecutable": cli,
            "shimExecutable": cli
        }))
        .expect("serialize invalid fixture configuration"),
    )
    .expect("write invalid fixture configuration");

    let result = run_json(&root.path, &config, &["app", "list", "--json"]);
    assert_eq!(result.output.status.code(), Some(1));
    assert!(result.output.stderr.is_empty());
    assert_complete_envelope(&result.envelope);
    assert_eq!(result.envelope["ok"], false);
    assert_eq!(
        result.envelope["error"]["code"],
        "test_fixture_configuration_invalid"
    );
    assert_eq!(result.envelope["error"]["details"]["field"], "baseUrl");
}

#[test]
#[allow(clippy::too_many_lines)]
fn real_cli_cancels_an_in_flight_node_download_and_rolls_back() {
    let root = IsolatedRoot::new();
    let version = ExactVersion::from_str(VERSION).expect("valid fixture version");
    let distribution = NodeProvider::official()
        .expect("create official provider without network access")
        .distribution(&version)
        .expect("resolve the current target distribution");
    let fixture_node = compile_fixture_executable(
        &root.path,
        "fixture-node-cancel",
        &format!("fn main() {{ println!(\"v{VERSION}\"); }}\n"),
    );
    let archive = test_fixtures::build_node_archive(&distribution, &fixture_node)
        .expect("build cancellable fixture Node.js archive");
    let manifest = format!(
        "{}  {}\n",
        test_fixtures::sha256_hex(&archive),
        distribution.archive_name
    );
    let version_prefix = format!("/dist/v{VERSION}");
    let archive_path = format!("{version_prefix}/{}", distribution.archive_name);
    let routes = BTreeMap::from([
        (
            format!("{version_prefix}/SHASUMS256.txt"),
            manifest.into_bytes(),
        ),
        (
            format!("{version_prefix}/SHASUMS256.txt.sig"),
            SIGNATURE.to_vec(),
        ),
        (archive_path.clone(), archive),
    ]);
    let (base_url, download_started, release_download, server) =
        pausing_fixture_server(routes, archive_path);

    let paths = TorbenPaths::for_test(root.path.clone());
    let install_path = paths.app_version_dir("node", VERSION);
    let plugin = compile_fixture_plugin(
        &root.path,
        &base_url,
        &distribution.archive_name,
        &install_path,
    );
    let config = root.path.join("node-cancel-fixture.json");
    std::fs::write(
        &config,
        serde_json::to_vec_pretty(&json!({
            "baseUrl": base_url,
            "checksumSignatureHex": encode_hex(SIGNATURE),
            "pluginExecutable": plugin,
            "shimExecutable": real_shim_next_to_cli()
        }))
        .expect("serialize cancellable CLI fixture configuration"),
    )
    .expect("write cancellable CLI fixture configuration");

    let install = cli_command(&root.path, &config)
        .args(["install", "node@lts", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the real torben install process");
    download_started
        .recv_timeout(Duration::from_secs(10))
        .expect("Node.js archive download reaches the paused fixture response");

    let tasks = run_json(&root.path, &config, &["task", "list", "--json"]);
    assert_success(&tasks);
    let operation_id = tasks.envelope["data"]
        .as_array()
        .and_then(|operations| operations.first())
        .and_then(|operation| operation["operationId"].as_str())
        .expect("active install operation is visible from another CLI process");
    assert_eq!(tasks.envelope["data"][0]["state"], "running");

    let cancelled = run_json(
        &root.path,
        &config,
        &["task", "cancel", operation_id, "--json"],
    );
    assert_success(&cancelled);
    assert_eq!(cancelled.envelope["data"]["requested"], true);
    assert_eq!(cancelled.envelope["data"]["operationId"], operation_id);

    let install_output = wait_for_child_output(install, Duration::from_secs(10));
    let _ = release_download.send(());
    let install_result = parse_json_output(install_output.expect("install observes cancellation"));
    server.join().expect("paused fixture HTTP server completes");

    assert_eq!(install_result.output.status.code(), Some(1));
    assert!(install_result.output.stderr.is_empty());
    assert_complete_envelope(&install_result.envelope);
    assert_eq!(install_result.envelope["ok"], false);
    assert_eq!(
        install_result.envelope["error"]["code"],
        "operation_cancelled"
    );
    assert!(install_result.envelope["data"].is_null());

    let operation_id = OperationId::from_str(operation_id).expect("valid persisted operation ID");
    let events = TorbenTaskClient::open(paths.clone())
        .expect("open task-only Core client after cancellation")
        .operation_events()
        .expect("read persisted cancellation events");
    let mut events = events
        .into_iter()
        .filter(|event| event.operation_id == operation_id)
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.sequence);
    let states = events
        .into_iter()
        .map(|event| event.state)
        .collect::<Vec<_>>();
    assert!(
        states.ends_with(&[
            OperationState::Cancelling,
            OperationState::Failed,
            OperationState::RolledBack
        ]),
        "unexpected cancellation state sequence: {states:?}"
    );

    let download = paths
        .download_dir("node", VERSION)
        .join(&distribution.archive_name);
    assert!(!install_path.exists());
    assert!(!download.exists());
    assert!(!download.with_extension("partial").exists());
    assert!(
        StateStore::open(paths.state_database())
            .expect("open state after cancellation")
            .list_installations()
            .expect("list installations after cancellation")
            .is_empty()
    );
    let marker = paths
        .operation_dir()
        .join(format!("{operation_id}.json.cancel"));
    assert!(!marker.exists());
    assert!(!marker.with_extension("cancel.next").exists());
}

#[test]
#[allow(clippy::too_many_lines)]
fn desktop_core_and_real_cli_serialize_mutations_while_tasks_remain_cancellable() {
    let root = IsolatedRoot::new();
    let version = ExactVersion::from_str(VERSION).expect("valid fixture version");
    let distribution = NodeProvider::official()
        .expect("create official provider without network access")
        .distribution(&version)
        .expect("resolve the current target distribution");
    let fixture_node = compile_fixture_executable(
        &root.path,
        "fixture-node-desktop-cli",
        &format!("fn main() {{ println!(\"v{VERSION}\"); }}\n"),
    );
    let archive = test_fixtures::build_node_archive(&distribution, &fixture_node)
        .expect("build concurrent desktop and CLI fixture archive");
    let manifest = format!(
        "{}  {}\n",
        test_fixtures::sha256_hex(&archive),
        distribution.archive_name
    );
    let version_prefix = format!("/dist/v{VERSION}");
    let archive_path = format!("{version_prefix}/{}", distribution.archive_name);
    let routes = BTreeMap::from([
        (
            format!("{version_prefix}/SHASUMS256.txt"),
            manifest.into_bytes(),
        ),
        (
            format!("{version_prefix}/SHASUMS256.txt.sig"),
            SIGNATURE.to_vec(),
        ),
        (archive_path.clone(), archive),
    ]);
    let (base_url, download_started, release_download, server) =
        pausing_fixture_server(routes, archive_path);
    let paths = TorbenPaths::for_test(root.path.clone());
    let install_path = paths.app_version_dir("node", VERSION);
    let plugin = compile_fixture_plugin(
        &root.path,
        &base_url,
        &distribution.archive_name,
        &install_path,
    );
    let shim = real_shim_next_to_cli();
    let config = root.path.join("desktop-cli-concurrency-fixture.json");
    std::fs::write(
        &config,
        serde_json::to_vec_pretty(&json!({
            "baseUrl": base_url,
            "checksumSignatureHex": encode_hex(SIGNATURE),
            "pluginExecutable": plugin,
            "shimExecutable": shim
        }))
        .expect("serialize desktop and CLI fixture configuration"),
    )
    .expect("write desktop and CLI fixture configuration");

    let desktop_core = TorbenCore::open_node_fixture(
        paths.clone(),
        NodeFixtureConfiguration {
            base_url,
            checksum_signature: SIGNATURE.to_vec(),
            plugin_executable: plugin,
            shim_executable: shim,
        },
    )
    .expect("open the fixture Core used by desktop commands");
    let (desktop_result_tx, desktop_result_rx) = mpsc::channel();
    let desktop_worker = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build the desktop command fixture runtime");
        let app_id = AppId::new("node").expect("valid Node.js application ID");
        desktop_result_tx
            .send(runtime.block_on(desktop_core.install(&app_id, "lts")))
            .expect("report the desktop command result");
    });
    download_started
        .recv_timeout(Duration::from_secs(10))
        .expect("desktop Node.js download reaches the paused fixture response");

    let mut cli_mutation = cli_command(&root.path, &config)
        .arg("use")
        .arg(format!("node@{VERSION}"))
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start a real CLI mutation while the desktop Core holds the lock");
    thread::sleep(Duration::from_millis(250));
    assert!(
        cli_mutation
            .try_wait()
            .expect("poll the waiting CLI mutation")
            .is_none(),
        "the CLI mutation bypassed the desktop workspace lock"
    );

    let tasks = run_json(&root.path, &config, &["task", "list", "--json"]);
    assert_success(&tasks);
    let operation_id = tasks.envelope["data"]
        .as_array()
        .and_then(|operations| operations.first())
        .and_then(|operation| operation["operationId"].as_str())
        .expect("desktop operation is visible to the real CLI task client")
        .to_owned();
    assert_eq!(tasks.envelope["data"][0]["state"], "running");

    let cancelled = run_json(
        &root.path,
        &config,
        &["task", "cancel", &operation_id, "--json"],
    );
    assert_success(&cancelled);
    assert_eq!(cancelled.envelope["data"]["requested"], true);
    assert_eq!(cancelled.envelope["data"]["operationId"], operation_id);

    let desktop_result = desktop_result_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("desktop command observes the CLI cancellation")
        .expect_err("cancelled desktop installation fails");
    assert_eq!(desktop_result.code, "operation_cancelled");
    desktop_worker.join().expect("join the desktop Core worker");
    let _ = release_download.send(());
    server.join().expect("paused fixture HTTP server completes");

    let cli_result = parse_json_output(
        wait_for_child_output(cli_mutation, Duration::from_secs(10))
            .expect("CLI mutation resumes after the desktop lock is released"),
    );
    assert_eq!(cli_result.output.status.code(), Some(1));
    assert!(cli_result.output.stderr.is_empty());
    assert_complete_envelope(&cli_result.envelope);
    assert_eq!(cli_result.envelope["ok"], false);
    assert_eq!(
        cli_result.envelope["error"]["code"],
        "version_not_installed"
    );

    let operation_id = OperationId::from_str(&operation_id).expect("valid operation ID");
    let events = TorbenTaskClient::open(paths.clone())
        .expect("open task client after cross-entry cancellation")
        .operation_events()
        .expect("read cross-entry operation events");
    let mut events = events
        .into_iter()
        .filter(|event| event.operation_id == operation_id)
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.sequence);
    let states = events
        .into_iter()
        .map(|event| event.state)
        .collect::<Vec<_>>();
    assert!(
        states.ends_with(&[
            OperationState::Cancelling,
            OperationState::Failed,
            OperationState::RolledBack
        ]),
        "unexpected cross-entry cancellation state sequence: {states:?}"
    );
    assert!(!install_path.exists());
    assert!(
        StateStore::open(paths.state_database())
            .expect("open state after cross-entry cancellation")
            .list_installations()
            .expect("list installations after cross-entry cancellation")
            .is_empty()
    );
}

#[test]
#[allow(clippy::too_many_lines)]
fn real_cli_restart_recovers_an_interrupted_node_install() {
    let root = IsolatedRoot::new();
    let version = ExactVersion::from_str(VERSION).expect("valid fixture version");
    let distribution = NodeProvider::official()
        .expect("create official provider without network access")
        .distribution(&version)
        .expect("resolve the current target distribution");
    let fixture_node = compile_fixture_executable(
        &root.path,
        "fixture-node-recovery",
        &format!("fn main() {{ println!(\"v{VERSION}\"); }}\n"),
    );
    let archive = test_fixtures::build_node_archive(&distribution, &fixture_node)
        .expect("build recoverable fixture Node.js archive");
    let manifest = format!(
        "{}  {}\n",
        test_fixtures::sha256_hex(&archive),
        distribution.archive_name
    );
    let version_prefix = format!("/dist/v{VERSION}");
    let archive_route = format!("{version_prefix}/{}", distribution.archive_name);
    let routes = BTreeMap::from([
        (
            format!("{version_prefix}/SHASUMS256.txt"),
            manifest.into_bytes(),
        ),
        (
            format!("{version_prefix}/SHASUMS256.txt.sig"),
            SIGNATURE.to_vec(),
        ),
        (archive_route.clone(), archive),
    ]);
    let (base_url, download_started, release_download, server) =
        pausing_fixture_server(routes, archive_route);

    let paths = TorbenPaths::for_test(root.path.clone());
    let install_path = paths.app_version_dir("node", VERSION);
    let download = paths
        .download_dir("node", VERSION)
        .join(&distribution.archive_name);
    let partial = download.with_extension("partial");
    let plugin = compile_fixture_plugin(
        &root.path,
        &base_url,
        &distribution.archive_name,
        &install_path,
    );
    let config = root.path.join("node-recovery-fixture.json");
    std::fs::write(
        &config,
        serde_json::to_vec_pretty(&json!({
            "baseUrl": base_url,
            "checksumSignatureHex": encode_hex(SIGNATURE),
            "pluginExecutable": plugin,
            "shimExecutable": real_shim_next_to_cli()
        }))
        .expect("serialize restart-recovery CLI fixture configuration"),
    )
    .expect("write restart-recovery CLI fixture configuration");

    let mut install = cli_command(&root.path, &config)
        .args(["install", "node@lts", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the interruptible torben install process");
    download_started
        .recv_timeout(Duration::from_secs(10))
        .expect("Node.js archive download reaches the paused fixture response");
    wait_for_path(&partial, Duration::from_secs(5));

    let tasks = run_json(&root.path, &config, &["task", "list", "--json"]);
    assert_success(&tasks);
    let operation_id = tasks.envelope["data"]
        .as_array()
        .and_then(|operations| operations.first())
        .and_then(|operation| operation["operationId"].as_str())
        .expect("interrupted install operation is visible before termination")
        .to_owned();
    assert_eq!(tasks.envelope["data"][0]["state"], "running");

    install
        .kill()
        .expect("terminate the install process without graceful cleanup");
    let killed = install
        .wait_with_output()
        .expect("collect the terminated install process");
    assert!(!killed.status.success());
    let _ = release_download.send(());
    server
        .join()
        .expect("interrupted fixture HTTP server completes");

    assert!(partial.is_file());
    assert!(!download.exists());
    assert!(!install_path.exists());
    assert!(
        StateStore::open(paths.state_database())
            .expect("open state before startup recovery")
            .list_installations()
            .expect("list installations before startup recovery")
            .is_empty()
    );

    let restarted = run_json(&root.path, &config, &["app", "list", "--json"]);
    assert_success(&restarted);
    assert!(
        restarted.envelope["data"]
            .as_array()
            .is_some_and(|applications| !applications.is_empty())
    );

    let recovered_tasks = run_json(&root.path, &config, &["task", "list", "--json"]);
    assert_success(&recovered_tasks);
    let recovered = recovered_tasks.envelope["data"]
        .as_array()
        .and_then(|operations| {
            operations
                .iter()
                .find(|operation| operation["operationId"].as_str() == Some(operation_id.as_str()))
        })
        .expect("recovered operation remains in durable task history");
    assert_eq!(recovered["state"], "rolled_back");
    assert_eq!(recovered["phase"], "rollback");

    let operation_id = OperationId::from_str(&operation_id).expect("valid recovered operation ID");
    let mut events = TorbenTaskClient::open(paths.clone())
        .expect("open task-only client after restart recovery")
        .operation_events()
        .expect("read restart recovery events")
        .into_iter()
        .filter(|event| event.operation_id == operation_id)
        .collect::<Vec<_>>();
    events.sort_by_key(|event| event.sequence);
    let states = events
        .into_iter()
        .map(|event| event.state)
        .collect::<Vec<_>>();
    assert!(
        states.ends_with(&[OperationState::Failed, OperationState::RolledBack]),
        "unexpected restart recovery state sequence: {states:?}"
    );
    assert!(!partial.exists());
    assert!(!download.exists());
    assert!(!install_path.exists());
    assert!(
        std::fs::read_dir(paths.staging_dir())
            .expect("read staging after recovery")
            .next()
            .is_none()
    );
    assert!(
        StateStore::open(paths.state_database())
            .expect("open state after startup recovery")
            .list_installations()
            .expect("list installations after startup recovery")
            .is_empty()
    );
}

struct JsonCommandOutput {
    output: Output,
    envelope: Value,
}

fn run_json(root: &Path, config: &Path, arguments: &[&str]) -> JsonCommandOutput {
    let output = cli_command(root, config)
        .args(arguments)
        .output()
        .expect("run the real torben CLI process");
    parse_json_output(output)
}

fn cli_command(root: &Path, config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_torben"));
    command
        .env("TORBEN_DATA_DIR", root)
        .env("TORBEN_TEST_FIXTURE_CONFIG", config)
        .env("RUST_LOG", "off");
    command
}

fn parse_json_output(output: Output) -> JsonCommandOutput {
    let stdout = std::str::from_utf8(&output.stdout).expect("CLI stdout is UTF-8");
    let envelope = serde_json::from_str(stdout).unwrap_or_else(|error| {
        panic!("CLI stdout is not one JSON envelope: {error}; stdout={stdout:?}")
    });
    JsonCommandOutput { output, envelope }
}

fn wait_for_child_output(mut child: Child, timeout: Duration) -> Result<Output, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                return child
                    .wait_with_output()
                    .map_err(|error| format!("read completed child output: {error}"));
            }
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let output = child.wait_with_output().map_err(|error| {
                    format!("terminate timed-out install process and read output: {error}")
                })?;
                return Err(format!(
                    "child process did not finish before timeout; stdout={} stderr={}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            Err(error) => return Err(format!("poll install process: {error}")),
        }
    }
}

fn wait_for_path(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for fixture path {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(20));
    }
}

fn assert_success(result: &JsonCommandOutput) {
    assert!(
        result.output.status.success(),
        "CLI failed: stdout={} stderr={}",
        String::from_utf8_lossy(&result.output.stdout),
        String::from_utf8_lossy(&result.output.stderr)
    );
    assert!(result.output.stderr.is_empty());
    assert_complete_envelope(&result.envelope);
    assert_eq!(result.envelope["ok"], true);
    assert!(result.envelope["error"].is_null());
}

fn assert_complete_envelope(envelope: &Value) {
    let object = envelope.as_object().expect("JSON envelope is an object");
    let mut keys = object.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();
    assert_eq!(keys, ["data", "error", "ok", "schemaVersion", "warnings"]);
    assert_eq!(envelope["schemaVersion"], 1);
    assert!(envelope["warnings"].is_array());
}

fn compile_fixture_executable(directory: &Path, name: &str, source: &str) -> Vec<u8> {
    let source_path = directory.join(format!("{name}.rs"));
    let executable = directory.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&source_path, source).expect("write Rust fixture source");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .arg(&source_path)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("run rustc for fixture executable");
    assert!(
        output.status.success(),
        "fixture rustc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read(executable).expect("read compiled fixture executable")
}

#[allow(clippy::too_many_lines)]
fn compile_fixture_plugin(
    directory: &Path,
    base_url: &str,
    archive_name: &str,
    install_path: &Path,
) -> PathBuf {
    let initialize = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "protocolVersion": PLUGIN_PROTOCOL_VERSION,
            "pluginId": "app.torben.plugin.node",
            "pluginVersion": env!("CARGO_PKG_VERSION"),
            "applications": [{
                "id": "node",
                "displayName": "Node.js",
                "summary": "real CLI lifecycle fixture",
                "categories": ["runtime"],
                "capabilities": ["versions", "install", "select", "uninstall"],
                "sources": [{
                    "id": "node.official",
                    "displayName": "Official Node.js archive",
                    "managed": true
                }]
            }]
        }
    })
    .to_string();
    let versions = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": { "versions": [{
            "version": VERSION,
            "ltsName": "Krypton",
            "releasedAt": "2026-08-03",
            "recommended": true
        }] }
    })
    .to_string();
    let resolved = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": { "requested": "lts", "resolved": VERSION }
    })
    .to_string();
    let version_url = format!("{base_url}v{VERSION}/");
    let plan = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "result": {
            "appId": "node",
            "version": VERSION,
            "sourceId": "node.official",
            "steps": [
                {
                    "type": "download",
                    "url": format!("{version_url}{archive_name}"),
                    "destination_name": archive_name
                },
                {
                    "type": "verify_sha256_manifest",
                    "manifest_url": format!("{version_url}SHASUMS256.txt"),
                    "signature_url": format!("{version_url}SHASUMS256.txt.sig"),
                    "archive_name": archive_name
                },
                {
                    "type": "extract_archive",
                    "archive_name": archive_name,
                    "strip_components": 0
                },
                {
                    "type": "health_check",
                    "executable": "node",
                    "arguments": ["--version"],
                    "expected_output": format!("v{VERSION}")
                },
                { "type": "create_shims", "commands": ["node", "npm", "npx"] }
            ],
            "metadata": { "target": test_fixtures::node_plugin_target() }
        }
    })
    .to_string();
    let health = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {
            "healthy": true,
            "actualVersion": VERSION,
            "message": "healthy"
        }
    })
    .to_string();
    let uninstall = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "result": {
            "appId": "node",
            "version": VERSION,
            "sourceId": "node.official",
            "installPath": install_path.display().to_string(),
            "preserveUserData": true
        }
    })
    .to_string();
    let source = format!(
        r#"use std::io::{{BufRead, Write}};
fn main() {{
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {{
        let line = line.expect("read request");
        let response = if line.contains("initialize") {{ Some({initialize:?}) }}
            else if line.contains("versions.list") {{ Some({versions:?}) }}
            else if line.contains("version.resolve") {{ Some({resolved:?}) }}
            else if line.contains("uninstall.plan") {{ Some({uninstall:?}) }}
            else if line.contains("install.plan") {{ Some({plan:?}) }}
            else if line.contains("health.check") {{ Some({health:?}) }}
            else if line.contains("shutdown") {{ None }}
            else {{ std::process::exit(2) }};
        if let Some(response) = response {{
            writeln!(stdout, "{{}}", response).expect("write response");
            stdout.flush().expect("flush response");
        }} else {{ break; }}
    }}
}}
"#
    );
    let source_path = directory.join("fixture-plugin.rs");
    let executable = directory.join(format!("fixture-plugin{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&source_path, source).expect("write fixture plugin source");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(rustc)
        .arg(&source_path)
        .arg("-o")
        .arg(&executable)
        .output()
        .expect("run rustc for fixture plugin");
    assert!(
        output.status.success(),
        "fixture plugin rustc failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    executable
}

fn real_shim_next_to_cli() -> PathBuf {
    let cli = Path::new(env!("CARGO_BIN_EXE_torben"));
    let shim = cli
        .parent()
        .expect("torben CLI has a parent directory")
        .join(format!("torben-shim{}", std::env::consts::EXE_SUFFIX));
    assert!(
        shim.is_file(),
        "real torben-shim sidecar is missing at {}; build torben-shim before this test",
        shim.display()
    );
    shim
}

fn fixture_server(
    routes: BTreeMap<String, Vec<u8>>,
    expected_requests: usize,
) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture HTTP server");
    let address = listener.local_addr().expect("read fixture HTTP address");
    let server = thread::spawn(move || {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener.accept().expect("accept fixture HTTP request");
            let mut request = [0_u8; 4096];
            let read = stream
                .read(&mut request)
                .expect("read fixture HTTP request");
            let request = String::from_utf8_lossy(&request[..read]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("fixture HTTP request path");
            let (status, body) = routes.get(path).map_or_else(
                || ("404 Not Found", b"not found".as_slice()),
                |body| ("200 OK", body.as_slice()),
            );
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .expect("write fixture HTTP headers");
            stream.write_all(body).expect("write fixture HTTP body");
            stream.flush().expect("flush fixture HTTP response");
        }
    });
    (format!("http://{address}/dist/"), server)
}

fn pausing_fixture_server(
    routes: BTreeMap<String, Vec<u8>>,
    paused_path: String,
) -> (
    String,
    mpsc::Receiver<()>,
    mpsc::Sender<()>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind pausing fixture HTTP server");
    let address = listener
        .local_addr()
        .expect("read pausing fixture HTTP address");
    let expected_requests = routes.len();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let server = thread::spawn(move || {
        for _ in 0..expected_requests {
            let (mut stream, _) = listener
                .accept()
                .expect("accept pausing fixture HTTP request");
            let mut request = [0_u8; 4096];
            let read = stream
                .read(&mut request)
                .expect("read pausing fixture HTTP request");
            let request = String::from_utf8_lossy(&request[..read]);
            let path = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .expect("pausing fixture HTTP request path");
            let Some(body) = routes.get(path) else {
                write_fixture_response(&mut stream, "404 Not Found", b"not found");
                continue;
            };
            if path == paused_path {
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                stream
                    .write_all(headers.as_bytes())
                    .expect("write paused fixture HTTP headers");
                stream.flush().expect("flush paused fixture HTTP headers");
                started_tx.send(()).expect("report paused archive download");
                let _ = release_rx.recv_timeout(Duration::from_secs(15));
                let _ = stream.write_all(body);
                let _ = stream.flush();
            } else {
                write_fixture_response(&mut stream, "200 OK", body);
            }
        }
    });
    (
        format!("http://{address}/dist/"),
        started_rx,
        release_tx,
        server,
    )
}

fn write_fixture_response(stream: &mut std::net::TcpStream, status: &str, body: &[u8]) {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .expect("write fixture HTTP response headers");
    stream
        .write_all(body)
        .expect("write fixture HTTP response body");
    stream.flush().expect("flush fixture HTTP response");
}

fn assert_new_terminal_command(root: &Path, shim_directory: &Path, command: &str, expected: &str) {
    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(shim_directory.to_path_buf()).chain(std::env::split_paths(&inherited_path)),
    )
    .expect("compose fresh terminal PATH");
    let mut process = if cfg!(windows) {
        let mut process = Command::new("cmd.exe");
        process.args(["/d", "/s", "/c", command]);
        process
    } else {
        let mut process = Command::new("sh");
        process.args(["-c", command]);
        process
    };
    let output = process
        .env("TORBEN_DATA_DIR", root)
        .env("PATH", path)
        .output()
        .expect("run command through a fresh terminal process");
    assert!(
        output.status.success(),
        "fresh terminal command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), expected);
}

fn encode_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|byte| {
            let byte = *byte;
            [
                char::from(DIGITS[usize::from(byte >> 4)]),
                char::from(DIGITS[usize::from(byte & 0x0f)]),
            ]
        })
        .collect()
}
