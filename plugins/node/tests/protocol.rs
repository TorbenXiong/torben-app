use std::{
    io::{BufRead, BufReader, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    str::FromStr,
};

use serde::{Serialize, de::DeserializeOwned};
use torben_contracts::{
    AppId, ExactVersion, OperationId, PluginId, SourceId,
    plugin::{
        HealthCheckParams, InitializeParams, InitializeResult, InstallPlan, InstallPlanParams,
        JsonRpcRequest, JsonRpcResponse, PLUGIN_PROTOCOL_VERSION, SchemaPageListParams,
        SchemaPageListResult, UninstallPlan, UninstallPlanParams, method,
    },
};

#[test]
fn bundled_node_plugin_serves_handshake_and_install_plan_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_torben-plugin-node"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());

    let initialized: InitializeResult = call(
        &mut input,
        &mut output,
        1,
        method::INITIALIZE,
        &InitializeParams {
            protocol_version: PLUGIN_PROTOCOL_VERSION,
            host_version: ExactVersion::from_str(env!("CARGO_PKG_VERSION")).unwrap(),
            target: current_target(),
            locale: "en-US".to_owned(),
        },
    );
    assert_eq!(initialized.plugin_id.as_str(), "app.torben.plugin.node");
    assert!(
        initialized
            .applications
            .iter()
            .any(|application| application.id.as_str() == "node")
    );

    input.write_all(b"not-json\n").unwrap();
    input.flush().unwrap();
    let mut malformed_line = String::new();
    output.read_line(&mut malformed_line).unwrap();
    let malformed: JsonRpcResponse = serde_json::from_str(&malformed_line).unwrap();
    assert_eq!(malformed.id, 0);
    assert_eq!(malformed.error.unwrap().code, -32700);

    let version = ExactVersion::from_str("24.19.0").unwrap();
    let plan: InstallPlan = call(
        &mut input,
        &mut output,
        2,
        method::INSTALL_PLAN,
        &InstallPlanParams {
            operation_id: OperationId::new(),
            app_id: AppId::new("node").unwrap(),
            version: version.clone(),
            source_id: SourceId::new("node.official").unwrap(),
            target: current_target(),
        },
    );
    assert_eq!(plan.version, version);
    assert_eq!(plan.steps.len(), 5);

    let source_id = SourceId::new("node.official").unwrap();
    let uninstall: UninstallPlan = call(
        &mut input,
        &mut output,
        3,
        method::UNINSTALL_PLAN,
        &UninstallPlanParams {
            operation_id: OperationId::new(),
            app_id: AppId::new("node").unwrap(),
            version: version.clone(),
            source_id: source_id.clone(),
            install_path: "managed/node/24.19.0".to_owned(),
        },
    );
    assert_eq!(uninstall.version, version.clone());
    assert_eq!(uninstall.source_id, source_id);
    assert!(uninstall.preserve_user_data);

    let failed_health = call_response(
        &mut input,
        &mut output,
        4,
        method::HEALTH_CHECK,
        &HealthCheckParams {
            app_id: AppId::new("node").unwrap(),
            version,
            install_path: "definitely-missing-installation".to_owned(),
        },
    );
    let error = failed_health.error.unwrap();
    assert_eq!(error.data.unwrap().code, "managed_command_missing");

    let schema: SchemaPageListResult = call(
        &mut input,
        &mut output,
        5,
        method::SCHEMA_PAGES,
        &SchemaPageListParams {
            plugin_id: PluginId::new("app.torben.plugin.node").unwrap(),
        },
    );
    assert_eq!(schema.pages.len(), 1);
    assert_eq!(schema.pages[0].id, "node");

    drop(input);
    assert!(child.wait().unwrap().success());
}

fn call<P, R>(
    input: &mut ChildStdin,
    output: &mut BufReader<ChildStdout>,
    id: u64,
    method_name: &str,
    params: &P,
) -> R
where
    P: Serialize,
    R: DeserializeOwned,
{
    let response = call_response(input, output, id, method_name, params);
    assert!(response.error.is_none());
    serde_json::from_value(response.result.unwrap()).unwrap()
}

fn call_response<P>(
    input: &mut ChildStdin,
    output: &mut BufReader<ChildStdout>,
    id: u64,
    method_name: &str,
    params: &P,
) -> JsonRpcResponse
where
    P: Serialize,
{
    let params = serde_json::to_value(params).unwrap();
    let request = JsonRpcRequest::new(id, method_name, params);
    serde_json::to_writer(&mut *input, &request).unwrap();
    input.write_all(b"\n").unwrap();
    input.flush().unwrap();

    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&line).unwrap();
    assert_eq!(response.id, id);
    response
}

fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
