//! Runs an MCP server command and passes its messages through unchanged, recording each
//! finished search on the way. For clients whose hooks cannot hand over a tool's result.
//!
//! Standard output is the client's MCP connection, so nothing else is ever written to it.
use crate::capture_cli::{self, Call};
use clap::Args;
use cqlmatic::{contracts::Diagnostic, mcp};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    thread,
};

/// Longer messages are passed through without being read.
const MAX_MESSAGE_BYTES: usize = 2 * mcp::MAX_RESULT_BYTES;
const MAX_PENDING: usize = 256;

#[derive(Debug, Args)]
pub struct PassThroughArgs {
    #[arg(
        long,
        value_name = "FILE",
        help = "Workflow state; defaults to local cqlmatic/inventory/state.json"
    )]
    state: Option<PathBuf>,
    #[arg(
        last = true,
        required = true,
        value_name = "COMMAND",
        help = "The MCP server command and its arguments"
    )]
    command: Vec<OsString>,
}

/// Tool calls the client has sent and the server has not answered, by request id.
#[derive(Default)]
struct Pending(BTreeMap<String, (String, Value)>);

impl Pending {
    /// Notes a `tools/call` request on its way to the server.
    fn sent(&mut self, line: &[u8]) {
        if line.len() > MAX_MESSAGE_BYTES || !line.windows(10).any(|bytes| bytes == b"tools/call") {
            return;
        }
        let Ok(message) = serde_json::from_slice::<Value>(line) else {
            return;
        };
        let (Some("tools/call"), false, Some(tool)) = (
            message["method"].as_str(),
            message["id"].is_null(),
            message["params"]["name"].as_str(),
        ) else {
            return;
        };
        // A client that never reads its answers must not grow this without limit.
        if self.0.len() >= MAX_PENDING {
            self.0.clear();
        }
        self.0.insert(
            message["id"].to_string(),
            (tool.into(), message["params"]["arguments"].clone()),
        );
    }

    /// The finished call, when `line` is the server's answer to a noted request.
    fn answered(&mut self, line: &[u8]) -> Option<Call> {
        if self.0.is_empty() || line.len() > MAX_MESSAGE_BYTES {
            return None;
        }
        let mut message: Value = serde_json::from_slice(line).ok()?;
        if !message["method"].is_null() {
            return None;
        }
        let (tool, arguments) = self.0.remove(&message["id"].to_string())?;
        let result = match message["result"].take() {
            Value::Object(mut result) => {
                // Protocol metadata is not part of the tool's result.
                result.remove("_meta");
                Some(Value::Object(result))
            }
            _ => None,
        };
        Some(Call::on_connection(tool, arguments, result))
    }
}

pub fn run(args: PassThroughArgs) -> Result<i32, Diagnostic> {
    let failed = |message| Diagnostic::error("PASS_THROUGH", message);
    let mut server = Command::new(&args.command[0])
        .args(&args.command[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|_| failed("Cannot start the MCP server command."))?;
    let mut to_server = server.stdin.take().expect("The server's input is piped");
    let from_server = server.stdout.take().expect("The server's output is piped");
    let pending = Arc::new(Mutex::new(Pending::default()));

    let noted = Arc::clone(&pending);
    // Never joined: it may still be waiting on the client when the server exits. Its end
    // closes the server's input.
    thread::spawn(move || {
        let mut client = std::io::stdin().lock();
        let mut line = Vec::new();
        while matches!(client.read_until(b'\n', &mut line), Ok(read) if read > 0) {
            if let Ok(mut pending) = noted.lock() {
                pending.sent(&line);
            }
            if to_server
                .write_all(&line)
                .and_then(|()| to_server.flush())
                .is_err()
            {
                break;
            }
            line.clear();
        }
    });

    let mut from_server = BufReader::new(from_server);
    let mut client = std::io::stdout().lock();
    let mut captures = vec![];
    let mut line = Vec::new();
    while matches!(from_server.read_until(b'\n', &mut line), Ok(read) if read > 0) {
        let call = pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.answered(&line));
        // The client gets its answer before anything is recorded.
        if client
            .write_all(&line)
            .and_then(|()| client.flush())
            .is_err()
        {
            break;
        }
        if let Some(call) = call {
            let state = args.state.clone();
            captures.push(thread::spawn(move || {
                let _ = capture_cli::record(state, call);
            }));
        }
        line.clear();
    }
    drop(client);
    let status = server
        .wait()
        .map_err(|_| failed("Cannot wait for the MCP server command."))?;
    for capture in captures {
        let _ = capture.join();
    }
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn line(message: Value) -> Vec<u8> {
        format!("{message}\n").into_bytes()
    }

    #[test]
    fn answers_are_paired_with_the_tool_calls_they_finish() {
        let mut pending = Pending::default();
        let call = |id: Value| {
            line(
                json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {
                    "name": "falcon_search_ngsiem", "arguments": {"query_string": "x"}
                }}),
            )
        };
        pending.sent(&call(json!(1)));
        pending.sent(&call(json!("1")));
        pending.sent(&call(json!(2)));
        // Other requests, notifications and unreadable lines are not tool calls.
        pending.sent(&line(
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
        ));
        pending.sent(&line(
            json!({"jsonrpc": "2.0", "method": "tools/call", "params": {"name": "x"}}),
        ));
        pending.sent(b"tools/call but not JSON\n");
        assert_eq!(pending.0.len(), 3);

        // A request from the server may reuse an id; it answers nothing.
        assert!(
            pending
                .answered(&line(json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})))
                .is_none()
        );
        assert!(
            pending
                .answered(&line(json!({"jsonrpc": "2.0", "id": 3, "result": {}})))
                .is_none()
        );

        let finished = pending
            .answered(&line(json!({"jsonrpc": "2.0", "id": 1, "result": {
                "content": [{"type": "text", "text": "{}"}], "_meta": {"progress": 1}
            }})))
            .unwrap();
        assert_eq!(finished.tool, "falcon_search_ngsiem");
        assert_eq!(finished.arguments, json!({"query_string": "x"}));
        assert_eq!(
            finished.response,
            Some(json!({"content": [{"type": "text", "text": "{}"}]}))
        );
        // The string id "1" is a different request from the number 1.
        let failed = pending
            .answered(&line(
                json!({"jsonrpc": "2.0", "id": "1", "error": {"code": -32000, "message": "x"}}),
            ))
            .unwrap();
        assert_eq!(failed.response, None);
        assert_eq!(pending.0.len(), 1);
        assert!(
            pending
                .answered(&line(json!({"jsonrpc": "2.0", "id": 1, "result": {}})))
                .is_none()
        );
    }
}
