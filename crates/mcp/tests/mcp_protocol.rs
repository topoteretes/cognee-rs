#![cfg(feature = "runtime")]

use std::sync::Arc;

use async_trait::async_trait;
use cognee_mcp::mcp::{McpServer, ToolRouter};
use serde_json::{Value, json};

#[derive(Debug, Default)]
struct RecordingTools;

#[async_trait]
impl ToolRouter for RecordingTools {
    fn descriptors(&self) -> Vec<Value> {
        vec![json!({
            "name": "fixture",
            "description": "fixture tool",
            "inputSchema": {"type": "object"}
        })]
    }

    async fn call(&self, name: &str, arguments: Value) -> Value {
        json!({
            "content": [{
                "type": "text",
                "text": json!({"name": name, "arguments": arguments}).to_string()
            }],
            "isError": false
        })
    }
}

fn parse(response: Option<String>) -> Value {
    serde_json::from_str(&response.expect("JSON-RPC response")).expect("valid JSON")
}

#[tokio::test]
async fn tools_require_a_successful_initialize_request() {
    let mut server = McpServer::new(Arc::new(RecordingTools));

    for request in [
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "fixture", "arguments": {}}
        }),
    ] {
        let response = parse(server.handle_line(&request.to_string()).await);
        assert_eq!(response["error"]["code"], -32002);
        assert_eq!(response["error"]["message"], "Server not initialized");
    }
}

#[tokio::test]
async fn initialized_session_lists_and_calls_tools() {
    let mut server = McpServer::new(Arc::new(RecordingTools));

    let initialized = parse(
        server
            .handle_line(
                &json!({
                    "jsonrpc": "2.0",
                    "id": "init",
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": {"name": "test", "version": "0"}
                    }
                })
                .to_string(),
            )
            .await,
    );
    assert_eq!(initialized["result"]["serverInfo"]["name"], "cognee-agent");

    assert!(
        server
            .handle_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .await
            .is_none()
    );

    let listed = parse(
        server
            .handle_line(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#)
            .await,
    );
    assert_eq!(listed["result"]["tools"][0]["name"], "fixture");

    let called = parse(
        server
            .handle_line(
                r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"fixture","arguments":{"query":"earlier"}}}"#,
            )
            .await,
    );
    assert_eq!(called["result"]["isError"], false);
    let text = called["result"]["content"][0]["text"]
        .as_str()
        .expect("text result");
    assert_eq!(
        serde_json::from_str::<Value>(text).expect("tool JSON"),
        json!({"name": "fixture", "arguments": {"query": "earlier"}})
    );
}

#[tokio::test]
async fn notifications_stay_silent_and_parse_and_method_errors_are_preserved() {
    let mut server = McpServer::new(Arc::new(RecordingTools));

    assert!(
        server
            .handle_line(r#"{"jsonrpc":"2.0","method":"unknown/notification"}"#)
            .await
            .is_none()
    );

    let parse_error = parse(server.handle_line("{").await);
    assert_eq!(parse_error["error"]["code"], -32700);
    assert!(parse_error["id"].is_null());

    let unknown = parse(
        server
            .handle_line(r#"{"jsonrpc":"2.0","id":7,"method":"no/such"}"#)
            .await,
    );
    assert_eq!(unknown["error"]["code"], -32601);
    assert_eq!(unknown["id"], 7);
}
