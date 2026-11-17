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
    io::{self, BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
};

/// Longer messages are passed through without being kept or read.
const MAX_MESSAGE_BYTES: usize = 2 * mcp::MAX_RESULT_BYTES;
const MAX_PENDING: usize = 256;
/// Finished searches waiting to be recorded; further ones are passed through unrecorded.
const MAX_QUEUED: usize = 64;

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
        if !line.windows(10).any(|bytes| bytes == b"tools/call") {
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
        if self.0.is_empty() {
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

/// Forwards `from` to `to` as its bytes arrive. Each complete line of at most
/// `MAX_MESSAGE_BYTES` is handed to `before` ahead of its last bytes and to `after` once they
/// are forwarded. A longer line is forwarded without being kept.
fn relay(
    mut from: impl BufRead,
    mut to: impl Write,
    mut before: impl FnMut(&[u8]),
    mut after: impl FnMut(&[u8]),
) -> io::Result<()> {
    let mut line = Vec::new();
    let mut kept = true;
    loop {
        let chunk = from.fill_buf()?;
        if chunk.is_empty() {
            return to.flush();
        }
        let (part, ends) = match chunk.iter().position(|byte| *byte == b'\n') {
            Some(end) => (&chunk[..=end], true),
            None => (chunk, false),
        };
        kept = kept && line.len() + part.len() <= MAX_MESSAGE_BYTES;
        if kept {
            line.extend_from_slice(part);
        } else {
            line = Vec::new();
        }
        if ends && kept {
            before(&line);
        }
        to.write_all(part)?;
        let read = part.len();
        if ends {
            to.flush()?;
            if kept {
                after(&line);
            }
            line.clear();
            kept = true;
        }
        from.consume(read);
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
    let to_server = server.stdin.take().expect("The server's input is piped");
    let from_server = server.stdout.take().expect("The server's output is piped");
    let pending = Arc::new(Mutex::new(Pending::default()));

    let noted = Arc::clone(&pending);
    // Never joined: it may still be waiting on the client when the server exits. Its end
    // closes the server's input.
    thread::spawn(move || {
        let note = |line: &[u8]| {
            if let Ok(mut pending) = noted.lock() {
                pending.sent(line);
            }
        };
        let _ = relay(io::stdin().lock(), to_server, note, |_| {});
    });

    // One recorder takes finished searches in turn, so they never wait on each other.
    let (queue, searches) = mpsc::sync_channel::<Call>(MAX_QUEUED);
    let state = args.state;
    let recorder = thread::spawn(move || {
        for call in searches {
            let _ = capture_cli::record(state.clone(), call);
        }
    });
    // The client has its answer before the search is queued.
    let finished = move |line: &[u8]| {
        let call = pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.answered(line));
        if let Some(call) = call.filter(Call::may_be_search) {
            let _ = queue.try_send(call);
        }
    };
    let forwarded = relay(
        BufReader::new(from_server),
        io::stdout().lock(),
        |_| {},
        finished,
    );
    if forwarded.is_err() {
        // The client is gone but may still hold the server's input open, so the server would
        // never see its input end.
        let _ = server.kill();
    }
    let status = server
        .wait()
        .map_err(|_| failed("Cannot wait for the MCP server command."))?;
    let _ = recorder.join();
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
    fn relay_forwards_everything_and_keeps_only_lines_within_the_limit() {
        let long = vec![b'x'; MAX_MESSAGE_BYTES + 1];
        let mut input = b"first\n".to_vec();
        input.extend_from_slice(&long);
        input.extend_from_slice(b"\nlast\nunfinished");
        let (mut output, mut before, mut after) = (vec![], vec![], vec![]);
        // A small buffer splits every line across many reads.
        relay(
            BufReader::with_capacity(7, input.as_slice()),
            &mut output,
            |line| before.push(line.to_vec()),
            |line| after.push(line.to_vec()),
        )
        .unwrap();
        assert_eq!(output, input);
        assert_eq!(before, [b"first\n".to_vec(), b"last\n".to_vec()]);
        assert_eq!(after, before);

        struct Closed;
        impl Write for Closed {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert!(relay(input.as_slice(), Closed, |_| {}, |_| {}).is_err());
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
