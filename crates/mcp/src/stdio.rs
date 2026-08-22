//! Newline-delimited MCP stdio transport.

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::config::{AgentConfig, EnvSource};
use crate::error::AgentError;
use crate::mcp::McpServer;
use crate::reference::ReferenceConfig;
use crate::tools::McpTools;

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrameRead {
    Eof,
    Complete,
    TooLarge,
}

pub fn run_mcp_from_env(env: &impl EnvSource) -> Result<(), AgentError> {
    let config =
        AgentConfig::from_env(env).map_err(|_| AgentError::Blocked("configuration_drift"))?;
    let tools = match ReferenceConfig::from_env(env) {
        Ok(Some(reference)) => McpTools::production(config).with_production_reference(reference),
        Ok(None) => McpTools::production(config),
        Err(_) => McpTools::production(config).with_reference_unavailable(),
    };
    let server = McpServer::new(Arc::new(tools));
    run_stdio(
        io::stdin().lock(),
        io::stdout().lock(),
        io::stderr().lock(),
        server,
    )
    .map_err(|_| AgentError::Engine("mcp_stdio"))
}

pub fn run_stdio<R: BufRead, W: Write, E: Write>(
    mut input: R,
    mut output: W,
    mut diagnostics: E,
    mut server: McpServer,
) -> io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(io::Error::other)?;
    let mut line = Vec::with_capacity(8 * 1024);
    loop {
        let frame = match read_bounded_line(&mut input, &mut line) {
            Ok(frame) => frame,
            Err(error) => {
                let _ = writeln!(diagnostics, "cognee mcp: input_io");
                return Err(error);
            }
        };
        match frame {
            FrameRead::Eof => return Ok(()),
            FrameRead::TooLarge => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32600, "message": "Request too large"}
                });
                writeln!(output, "{response}")?;
                output.flush()?;
                continue;
            }
            FrameRead::Complete => {}
        }
        let request = match std::str::from_utf8(&line) {
            Ok(request) => request.trim_end_matches(['\r', '\n']),
            Err(_) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": Value::Null,
                    "error": {"code": -32700, "message": "Parse error"}
                });
                writeln!(output, "{response}")?;
                output.flush()?;
                continue;
            }
        };
        if request.trim().is_empty() {
            continue;
        }
        if let Some(response) = runtime.block_on(server.handle_line(request)) {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
}

fn read_bounded_line<R: BufRead>(input: &mut R, line: &mut Vec<u8>) -> io::Result<FrameRead> {
    line.clear();
    let mut read_any = false;
    let mut too_large = false;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            return Ok(if read_any {
                if too_large {
                    FrameRead::TooLarge
                } else {
                    FrameRead::Complete
                }
            } else {
                FrameRead::Eof
            });
        }
        read_any = true;
        let newline = available.iter().position(|byte| *byte == b'\n');
        let consumed = newline.map_or(available.len(), |index| index + 1);
        if !too_large {
            let retained = consumed.min((MAX_MESSAGE_BYTES + 1).saturating_sub(line.len()));
            line.extend_from_slice(&available[..retained]);
            too_large = line.len() > MAX_MESSAGE_BYTES;
        }
        input.consume(consumed);
        if newline.is_some() {
            return Ok(if too_large {
                FrameRead::TooLarge
            } else {
                FrameRead::Complete
            });
        }
    }
}
