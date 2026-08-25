use std::{
    io::{BufRead, BufReader, Write},
    process::{ChildStdin, ChildStdout, Command, Stdio},
    str::FromStr,
};

use serde::{Serialize, de::DeserializeOwned};
use torben_contracts::{
    ExactVersion, PluginId,
    plugin::{
        InitializeParams, InitializeResult, JsonRpcRequest, JsonRpcResponse,
        PLUGIN_PROTOCOL_VERSION, SchemaPageListParams, SchemaPageListResult, method,
    },
};

#[test]
fn bundled_python_plugin_serves_handshake_and_schema_over_stdio() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_torben-plugin-python"))
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
    assert_eq!(initialized.plugin_id.as_str(), "app.torben.plugin.python");
    assert!(
        initialized
            .applications
            .iter()
            .any(|application| application.id.as_str() == "python")
    );

    let schema: SchemaPageListResult = call(
        &mut input,
        &mut output,
        2,
        method::SCHEMA_PAGES,
        &SchemaPageListParams {
            plugin_id: PluginId::new("app.torben.plugin.python").unwrap(),
        },
    );
    assert_eq!(schema.pages[0].id, "python");

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
    let request = JsonRpcRequest::new(id, method_name, serde_json::to_value(params).unwrap());
    serde_json::to_writer(&mut *input, &request).unwrap();
    input.write_all(b"\n").unwrap();
    input.flush().unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    let response: JsonRpcResponse = serde_json::from_str(&line).unwrap();
    assert_eq!(response.id, id);
    assert!(response.error.is_none());
    serde_json::from_value(response.result.unwrap()).unwrap()
}

fn current_target() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}
