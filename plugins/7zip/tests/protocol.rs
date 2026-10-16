use std::{
    io::{BufRead, BufReader, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    str::FromStr,
};

use serde::{Serialize, de::DeserializeOwned};
use torben_contracts::{
    AppId, ExactVersion, OperationId, PluginId, SourceId,
    plugin::{
        InitializeParams, InitializeResult, InstallPlan, InstallPlanParams, InstallStep,
        JsonRpcRequest, JsonRpcResponse, PLUGIN_PROTOCOL_VERSION, SchemaPageListParams,
        SchemaPageListResult, method,
    },
};

#[test]
fn bundled_7zip_plugin_serves_handshake_and_schema_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_torben-plugin-7zip"))
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
    assert_eq!(initialized.plugin_id.as_str(), "app.torben.plugin.7zip");
    let schema: SchemaPageListResult = call(
        &mut input,
        &mut output,
        2,
        method::SCHEMA_PAGES,
        &SchemaPageListParams {
            plugin_id: PluginId::new("app.torben.plugin.7zip").unwrap(),
        },
    );
    assert_eq!(schema.pages[0].id, "7zip");
    let plan: InstallPlan = call(
        &mut input,
        &mut output,
        3,
        method::INSTALL_PLAN,
        &InstallPlanParams {
            operation_id: OperationId::new(),
            app_id: AppId::new("7zip").unwrap(),
            version: ExactVersion::from_str("26.4.0").unwrap(),
            source_id: SourceId::new("7zip.official").unwrap(),
            target: current_target(),
        },
    );
    drop(input);
    assert!(child.wait().unwrap().success());
    let InstallStep::VerifySha256 { expected, .. } = &plan.steps[1] else {
        panic!("The plan must verify the MSI before extracting it");
    };
    assert_eq!(expected.len(), 64);
    assert_eq!(
        expected,
        "0b01334a418654293513449f61d6bfc99e5196ca371e3c3dc961eda57cd535c6"
    );
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
    let request = JsonRpcRequest::new(id, method_name, serde_json::to_value(params).unwrap());
    serde_json::to_writer(&mut *input, &request).unwrap();
    input.write_all(b"\n").unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&line).unwrap();
    assert!(response.error.is_none());
    serde_json::from_value(response.result.unwrap()).unwrap()
}

fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
