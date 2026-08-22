//! JSON-RPC 2.0 message handling for MCP stdio (no I/O).

use serde_json::{Value, json};

pub(crate) const PROTOCOL_VERSION: &str = "2024-11-05";
pub(crate) const SERVER_NAME: &str = "cognee-agent";
pub(crate) const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

pub(crate) fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": SERVER_VERSION
        }
    })
}

pub(crate) fn success_response(id: Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

pub(crate) fn error_response(id: Value, code: i64, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {"code": code, "message": message}
    })
    .to_string()
}

/// Handle one newline-stripped JSON-RPC message.
///
/// Returns `None` for notifications (no `id`).
pub fn handle_message(line: &str) -> Option<String> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            return Some(error_response(Value::Null, -32700, "Parse error"));
        }
    };

    let method = msg.get("method").and_then(Value::as_str)?;
    let id = msg.get("id").cloned();

    if method == "initialize" {
        let id = id?;
        return Some(success_response(id, initialize_result()));
    }

    if method == "ping" {
        let id = id?;
        return Some(success_response(id, json!({})));
    }

    let id = id?;
    Some(error_response(id, -32601, "Method not found"))
}
