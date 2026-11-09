use std::io::{self, BufRead, BufReader, BufWriter, Write};

use serde_json::{json, Map, Value};

use crate::core::{Error, Result};
use crate::retrieval::semantic::SemanticCache;
use crate::storage::Store;

use super::repo_tools::McpConfig;
use super::tools;

pub const MCP_PROTOCOL_VERSION: &str = "2025-06-18";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[MCP_PROTOCOL_VERSION];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    AwaitInitialize,
    AwaitInitialized,
    Ready,
}

/// Serves with the repository tools disabled (no allowed roots).
pub fn serve(store: &dyn Store) -> Result<()> {
    serve_with_config(store, &McpConfig::none())
}

pub fn serve_with_config(store: &dyn Store, config: &McpConfig) -> Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());
    serve_with_io(store, config, &mut reader, &mut writer)
}

fn serve_with_io<R: BufRead, W: Write>(
    store: &dyn Store,
    config: &McpConfig,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    let mut phase = Phase::AwaitInitialize;
    let mut semantic = SemanticCache::default();
    let mut line = Vec::new();
    loop {
        line.clear();
        let bytes_read = reader
            .read_until(b'\n', &mut line)
            .map_err(|e| Error::McpTransport {
                detail: format!("cannot read stdin: {e}"),
            })?;
        if bytes_read == 0 {
            writer.flush().map_err(|e| Error::McpTransport {
                detail: format!("cannot flush stdout: {e}"),
            })?;
            return Ok(());
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        if line.last() == Some(&b'\r') {
            line.pop();
        }

        let response = match serde_json::from_slice::<Value>(&line) {
            Ok(message) => handle_message(store, config, &mut semantic, &mut phase, message),
            Err(error) => Some(json_rpc_error(
                Value::Null,
                -32700,
                format!("parse error: {error}"),
            )),
        };
        if let Some(response) = response {
            serde_json::to_writer(&mut *writer, &response).map_err(|e| Error::McpTransport {
                detail: format!("cannot encode JSON-RPC response: {e}"),
            })?;
            writer.write_all(b"\n").map_err(|e| Error::McpTransport {
                detail: format!("cannot write stdout: {e}"),
            })?;
            writer.flush().map_err(|e| Error::McpTransport {
                detail: format!("cannot flush stdout: {e}"),
            })?;
        }
    }
}

fn handle_message(
    store: &dyn Store,
    config: &McpConfig,
    semantic: &mut SemanticCache,
    phase: &mut Phase,
    message: Value,
) -> Option<Value> {
    let Some(object) = message.as_object() else {
        return Some(json_rpc_error(
            Value::Null,
            -32600,
            "invalid request: expected a JSON-RPC object",
        ));
    };
    if object.get("jsonrpc") != Some(&Value::String("2.0".to_string())) {
        return Some(json_rpc_error(
            object.get("id").cloned().unwrap_or(Value::Null),
            -32600,
            "invalid request: jsonrpc must be '2.0'",
        ));
    }
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        return Some(json_rpc_error(
            object.get("id").cloned().unwrap_or(Value::Null),
            -32600,
            "invalid request: method must be a string",
        ));
    };
    let id = match object.get("id") {
        Some(id) if valid_id(id) => Some(id.clone()),
        Some(_) => {
            return Some(json_rpc_error(
                Value::Null,
                -32600,
                "invalid request: id must be a string or number, not null",
            ));
        }
        None => None,
    };
    if object
        .get("params")
        .is_some_and(|params| !params.is_object() && !params.is_array())
    {
        return id
            .map(|id| json_rpc_error(id, -32600, "invalid request: params must be an object"));
    }
    let params = object.get("params").and_then(Value::as_object);

    if method.starts_with("notifications/") {
        if id.is_some() {
            return Some(json_rpc_error(
                id.unwrap_or(Value::Null),
                -32600,
                "invalid request: notifications must not have an id",
            ));
        }
        if method == "notifications/initialized" && *phase == Phase::AwaitInitialized {
            *phase = Phase::Ready;
        }
        return None;
    }

    let Some(id) = id else {
        // Valid JSON-RPC notifications have no response. MCP client
        // notifications are handled above; request methods sent as
        // notifications are ignored as required by JSON-RPC.
        return None;
    };

    if method == "initialize" {
        if *phase != Phase::AwaitInitialize {
            return Some(json_rpc_error(
                id,
                -32600,
                "initialize may only be sent once",
            ));
        }
        let Some(params) = params else {
            return Some(json_rpc_error(
                id,
                -32602,
                "initialize params must be an object",
            ));
        };
        let protocol_version = match params.get("protocolVersion").and_then(Value::as_str) {
            Some(version) => version,
            None => {
                return Some(json_rpc_error(
                    id,
                    -32602,
                    "initialize requires a string protocolVersion",
                ));
            }
        };
        let client_info_valid = params
            .get("clientInfo")
            .and_then(Value::as_object)
            .is_some_and(|client_info| {
                client_info.get("name").and_then(Value::as_str).is_some()
                    && client_info.get("version").and_then(Value::as_str).is_some()
            });
        if !params.get("capabilities").is_some_and(Value::is_object) || !client_info_valid {
            return Some(json_rpc_error(
                id,
                -32602,
                "initialize requires object capabilities and clientInfo with string name and version",
            ));
        }
        let negotiated_version = SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .copied()
            .find(|supported| *supported == protocol_version)
            .unwrap_or(SUPPORTED_PROTOCOL_VERSIONS[0]);
        let result = json!({
            "protocolVersion": negotiated_version,
            "capabilities": { "tools": {} },
            "serverInfo": {
                "name": "mindspace",
                "version": env!("CARGO_PKG_VERSION")
            },
            "instructions": "Memory writes are local and durable. Manual updates supersede only manually created memories; document-derived memories must be updated at their source."
        });
        *phase = Phase::AwaitInitialized;
        return Some(json_rpc_result(id, result));
    }

    if method == "ping" {
        return Some(json_rpc_result(id, json!({})));
    }

    if *phase != Phase::Ready {
        return Some(json_rpc_error(
            id,
            -32002,
            "server is not initialized; complete initialize and notifications/initialized first",
        ));
    }

    match method {
        "tools/list" => {
            if object.get("params").is_some() && params.is_none() {
                return Some(json_rpc_error(
                    id,
                    -32602,
                    "tools/list params must be an object",
                ));
            }
            if params.is_some_and(|p| {
                p.keys().any(|key| key != "cursor" && key != "_meta")
                    || p.get("_meta").is_some_and(|meta| !meta.is_object())
                    || p.get("cursor").is_some_and(|cursor| {
                        cursor.as_str().is_none_or(|cursor| !cursor.is_empty())
                    })
            }) {
                return Some(json_rpc_error(
                    id,
                    -32602,
                    "tools/list accepts only an empty cursor",
                ));
            }
            Some(json_rpc_result(
                id,
                json!({ "tools": tools::definitions_for(config) }),
            ))
        }
        "tools/call" => {
            let Some(params) = params else {
                return Some(json_rpc_error(
                    id,
                    -32602,
                    "tools/call params must be an object",
                ));
            };
            if params
                .keys()
                .any(|key| key != "name" && key != "arguments" && key != "_meta")
                || params.get("_meta").is_some_and(|meta| !meta.is_object())
            {
                return Some(json_rpc_error(
                    id,
                    -32602,
                    "tools/call accepts only name and arguments",
                ));
            }
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(json_rpc_error(
                    id,
                    -32602,
                    "tools/call requires a string name",
                ));
            };
            if !tools::contains_for(config, name) {
                return Some(json_rpc_error(id, -32602, format!("unknown tool: {name}")));
            }
            let arguments = match params.get("arguments") {
                None => Value::Object(Map::new()),
                Some(Value::Object(arguments)) => Value::Object(arguments.clone()),
                Some(_) => {
                    return Some(json_rpc_error(
                        id,
                        -32602,
                        "tools/call arguments must be an object",
                    ));
                }
            };
            Some(json_rpc_result(
                id,
                tools::call_result(store, config, semantic, name, &arguments),
            ))
        }
        _ => Some(json_rpc_error(
            id,
            -32601,
            format!("method not found: {method}"),
        )),
    }
}

fn valid_id(id: &Value) -> bool {
    id.as_str().is_some() || id.is_number()
}

fn json_rpc_result(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn json_rpc_error(id: Value, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message.into() }
    })
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;
    use crate::storage::sqlite::SqliteStore;

    fn run(input: &str) -> String {
        let store = SqliteStore::open_in_memory().unwrap();
        let mut reader = Cursor::new(input.as_bytes());
        let mut output = Vec::new();
        serve_with_io(&store, &McpConfig::none(), &mut reader, &mut output).unwrap();
        String::from_utf8(output).unwrap()
    }

    fn initialize_message(id: Value, version: &str) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "initialize",
            "params": {
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "1" }
            }
        })
        .to_string()
    }

    fn input_lines(lines: Vec<String>) -> String {
        format!("{}\n", lines.join("\n"))
    }

    #[test]
    fn initialization_negotiates_supported_version_and_tool_capability() {
        let output = run(&input_lines(vec![
            initialize_message(json!("init"), "unknown"),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
            json!({ "jsonrpc": "2.0", "id": 4, "method": "ping" }).to_string(),
        ]));
        let messages: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages[0]["id"], "init");
        assert_eq!(
            messages[0]["result"]["protocolVersion"],
            MCP_PROTOCOL_VERSION
        );
        assert!(messages[0]["result"]["capabilities"]["tools"].is_object());
        assert_eq!(messages[1]["id"], 4);
        assert_eq!(messages[1]["result"], json!({}));
    }

    #[test]
    fn malformed_json_and_invalid_requests_get_protocol_errors() {
        let output = run(
            "not-json\n[]\n{\"jsonrpc\":\"2.0\",\"id\":null,\"method\":\"ping\"}\n{\"jsonrpc\":\"2.0\",\"id\":2.5,\"method\":\"ping\"}\n",
        );
        let messages: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages[0]["error"]["code"], -32700);
        assert_eq!(messages[1]["error"]["code"], -32600);
        assert_eq!(messages[2]["error"]["code"], -32600);
        assert_eq!(messages[3]["id"], 2.5);
        assert_eq!(messages[3]["result"], json!({}));
    }

    #[test]
    fn tool_methods_require_initialized_notification() {
        let output = run(&input_lines(vec![
            initialize_message(json!(1), MCP_PROTOCOL_VERSION),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }).to_string(),
        ]));
        let messages: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages[1]["error"]["code"], -32002);
    }

    #[test]
    fn unknown_method_and_unknown_tool_are_distinct() {
        let output = run(&input_lines(vec![
            initialize_message(json!(1), MCP_PROTOCOL_VERSION),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
            json!({ "jsonrpc": "2.0", "id": 2, "method": "missing/method" }).to_string(),
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "missing_tool", "arguments": {} }
            })
            .to_string(),
        ]));
        let messages: Vec<Value> = output
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(messages[1]["error"]["code"], -32601);
        assert_eq!(messages[2]["error"]["code"], -32602);
        assert_eq!(messages[2]["id"], 3);
    }

    #[test]
    fn notifications_and_eof_do_not_emit_extra_output() {
        let output = run(&input_lines(vec![
            json!({ "jsonrpc": "2.0", "method": "notifications/cancelled" }).to_string(),
            json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }).to_string(),
        ]));
        assert!(output.is_empty());
    }
}
