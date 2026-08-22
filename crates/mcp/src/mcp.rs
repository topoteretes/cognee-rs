//! Stateful MCP request routing over JSON-RPC 2.0.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::protocol::{error_response, initialize_result, success_response};

const SERVER_NOT_INITIALIZED: i64 = -32002;

#[async_trait]
pub trait ToolRouter: Send + Sync {
    fn descriptors(&self) -> Vec<Value>;
    async fn call(&self, name: &str, arguments: Value) -> Value;
}

pub struct McpServer {
    initialized: bool,
    tools: Arc<dyn ToolRouter>,
}

impl McpServer {
    pub fn new(tools: Arc<dyn ToolRouter>) -> Self {
        Self {
            initialized: false,
            tools,
        }
    }

    pub async fn handle_line(&mut self, line: &str) -> Option<String> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(_) => return Some(error_response(Value::Null, -32700, "Parse error")),
        };
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            return Some(error_response(Value::Null, -32600, "Invalid Request"));
        };
        let id = message.get("id").cloned()?;

        match method {
            "initialize" => {
                self.initialized = true;
                Some(success_response(id, initialize_result()))
            }
            "ping" => Some(success_response(id, json!({}))),
            "tools/list" if !self.initialized => Some(error_response(
                id,
                SERVER_NOT_INITIALIZED,
                "Server not initialized",
            )),
            "tools/call" if !self.initialized => Some(error_response(
                id,
                SERVER_NOT_INITIALIZED,
                "Server not initialized",
            )),
            "tools/list" => Some(success_response(
                id,
                json!({"tools": self.tools.descriptors()}),
            )),
            "tools/call" => {
                let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return Some(error_response(id, -32602, "Invalid params"));
                };
                let arguments = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                if !arguments.is_object() {
                    return Some(error_response(id, -32602, "Invalid params"));
                }
                let result = self.tools.call(name, arguments).await;
                Some(success_response(id, result))
            }
            _ => Some(error_response(id, -32601, "Method not found")),
        }
    }
}
