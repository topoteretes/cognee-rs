use serde_json::json;

use cognee_mcp::protocol::handle_message;

#[test]
fn initialize_returns_mcp_2024_server_info() {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "test", "version": "0"}
        }
    });
    let raw = serde_json::to_string(&req).unwrap();
    let out = handle_message(&raw).expect("initialize is a request");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["jsonrpc"], "2.0");
    assert_eq!(v["id"], 1);
    assert_eq!(v["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(v["result"]["serverInfo"]["name"], "cognee-agent");
    assert_eq!(
        v["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
    assert!(v["result"]["capabilities"]["tools"].is_object());
}

#[test]
fn initialized_notification_has_no_response() {
    let raw = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
    assert_eq!(handle_message(raw), None);
}

#[test]
fn unknown_method_is_minus_32601() {
    let raw = r#"{"jsonrpc":"2.0","id":7,"method":"no/such"}"#;
    let v: serde_json::Value =
        serde_json::from_str(&handle_message(raw).expect("error response")).unwrap();
    assert_eq!(v["error"]["code"], -32601);
    assert_eq!(v["id"], 7);
}

#[test]
fn ping_returns_empty_object() {
    let raw = r#"{"jsonrpc":"2.0","id":"p","method":"ping"}"#;
    let v: serde_json::Value =
        serde_json::from_str(&handle_message(raw).expect("ping response")).unwrap();
    assert_eq!(v["result"], json!({}));
}
