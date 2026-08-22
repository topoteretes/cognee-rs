#![cfg(feature = "runtime")]

use std::io::{self, BufRead, Cursor, Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;

use async_trait::async_trait;
use cognee_mcp::mcp::{McpServer, ToolRouter};
use cognee_mcp::stdio::run_stdio;
use serde_json::{Value, json};

#[derive(Debug, Default)]
struct FixtureTools;

struct FillBufOnly {
    inner: Cursor<Vec<u8>>,
}

impl Read for FillBufOnly {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buffer)
    }
}

impl BufRead for FillBufOnly {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amount: usize) {
        self.inner.consume(amount);
    }

    fn read_line(&mut self, _buffer: &mut String) -> io::Result<usize> {
        panic!("the stdio server must not use an unbounded read_line")
    }
}

#[async_trait]
impl ToolRouter for FixtureTools {
    fn descriptors(&self) -> Vec<Value> {
        vec![json!({
            "name": "fixture",
            "description": "fixture",
            "inputSchema": {"type": "object"}
        })]
    }

    async fn call(&self, _name: &str, _arguments: Value) -> Value {
        json!({"content": [{"type": "text", "text": "{}"}], "isError": false})
    }
}

#[test]
fn stdio_emits_one_json_line_per_request_and_none_for_notifications() {
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#,
    ]
    .join("\n")
        + "\n";
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_stdio(
        Cursor::new(input),
        &mut stdout,
        &mut stderr,
        McpServer::new(Arc::new(FixtureTools)),
    )
    .expect("stdio loop");

    let lines = String::from_utf8(stdout)
        .expect("UTF-8 stdout")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("protocol JSON"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0]["id"], 1);
    assert_eq!(lines[1]["id"], 2);
    assert_eq!(lines[2]["id"], 3);
    assert!(stderr.is_empty());
}

#[test]
fn oversized_stdio_frames_are_bounded_discarded_and_followed_by_valid_requests() {
    let mut input = vec![b'x'; 1024 * 1024 + 32];
    input.push(b'\n');
    input.extend_from_slice(br#"{"jsonrpc":"2.0","id":7,"method":"initialize","params":{}}"#);
    input.push(b'\n');
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    run_stdio(
        FillBufOnly {
            inner: Cursor::new(input),
        },
        &mut stdout,
        &mut stderr,
        McpServer::new(Arc::new(FixtureTools)),
    )
    .expect("bounded stdio loop");

    let lines = String::from_utf8(stdout)
        .expect("UTF-8 stdout")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("protocol JSON"))
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["error"]["code"], -32600);
    assert_eq!(lines[1]["id"], 7);
    assert!(stderr.is_empty());
}

#[test]
fn cognee_agent_mcp_process_is_protocol_only_and_exits_at_eof() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cognee-agent"))
        .arg("mcp")
        .env_clear()
        .env("APEX_COGNEE_ROOT", temporary.path().join("cognee"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start cognee-agent mcp");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(
            concat!(
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
                "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"ping\"}\n",
            )
            .as_bytes(),
        )
        .expect("write protocol input");

    let output = child.wait_with_output().expect("wait for EOF exit");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).expect("UTF-8 stdout");
    let lines = stdout.lines().collect::<Vec<_>>();
    assert_eq!(lines.len(), 3, "stdout: {stdout}");
    for line in lines {
        serde_json::from_str::<Value>(line).expect("every stdout line is JSON");
    }
    assert!(!temporary.path().join("cognee/locks/engine").exists());
}

#[test]
fn configured_invalid_reference_root_stays_visible_without_disrupting_private_tools() {
    let temporary = tempfile::tempdir().expect("temporary root");
    let mut child = Command::new(env!("CARGO_BIN_EXE_cognee-agent"))
        .arg("mcp")
        .env_clear()
        .env("APEX_COGNEE_ROOT", temporary.path().join("private"))
        .env("APEX_COGNEE_REFERENCE_ROOT", "relative-invalid-root")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start cognee-agent mcp");
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(
            concat!(
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{}}\n",
                "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"tools/call\",\"params\":{\"name\":\"cognee_reference_recall\",\"arguments\":{\"query\":\"fleet standard\"}}}\n",
                "{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"ping\"}\n",
            )
            .as_bytes(),
        )
        .expect("write protocol input");

    let output = child.wait_with_output().expect("wait for EOF exit");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let responses = String::from_utf8(output.stdout)
        .expect("UTF-8 stdout")
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("protocol JSON"))
        .collect::<Vec<_>>();
    assert_eq!(responses.len(), 4);
    assert_eq!(
        responses[1]["result"]["tools"]
            .as_array()
            .expect("tool descriptors")
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect::<Vec<_>>(),
        ["remember", "recall", "forget", "cognee_reference_recall"]
    );
    let error: Value = serde_json::from_str(
        responses[2]["result"]["content"][0]["text"]
            .as_str()
            .expect("reference error text"),
    )
    .expect("reference error JSON");
    assert_eq!(error["code"], "REFERENCE_UNAVAILABLE");
    assert_eq!(error["retryable"], true);
    assert_eq!(responses[3]["result"], json!({}));
}
