//! Records inventory evidence from a client's post-tool hook payload.
//!
//! A hook's output and exit status can reach the model, so this command prints nothing and
//! always exits 0. Each search it sees is counted by outcome in `captures/tally.json`.
use crate::{inventory_cli, setup_cli};
use clap::{Args, ValueEnum};
use cqlmatic::{
    app,
    contracts::{Diagnostic, EnvironmentId},
    discovery,
    inventory::{InventoryAction, InventoryState, QueryResultAssertion},
    mcp,
    setup::SEARCH_TOOL,
    storage,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// The search tool's documented repository when a call names none.
const DEFAULT_REPOSITORY: &str = "search-all";
const MAX_PAYLOAD_BYTES: u64 = 2 * mcp::MAX_RESULT_BYTES as u64;
const MAX_TALLY_BYTES: u64 = 65_536;

#[derive(Debug, Args)]
pub struct CaptureArgs {
    #[arg(
        long,
        value_enum,
        help = "Client whose post-tool hook payload arrives on stdin"
    )]
    client: Client,
    #[arg(
        long,
        value_name = "NAME",
        help = "MCP server of the confirmed inventory target, as the client's tool names spell it"
    )]
    server: String,
    #[arg(
        long,
        value_name = "FILE",
        help = "Workflow state; defaults to local cqlmatic/inventory/state.json"
    )]
    state: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
enum Client {
    ClaudeCode,
    Cline,
    Codex,
    CopilotApp,
    CopilotCli,
    CopilotVscode,
    Cursor,
    GeminiCli,
    /// Any client or wrapper that supplies `tool_name`, `tool_input` and `tool_response`.
    Generic,
    Kiro,
    QwenCode,
}

/// One search call as a client hook reported it. `body` is absent when the call failed.
struct Search {
    repository: String,
    query: String,
    start: String,
    end: Option<String>,
    body: Option<Vec<u8>>,
}

pub fn run(args: CaptureArgs) -> Result<i32, Diagnostic> {
    let mut payload = Vec::new();
    if std::io::stdin()
        .take(MAX_PAYLOAD_BYTES + 1)
        .read_to_end(&mut payload)
        .is_ok()
        && payload.len() as u64 <= MAX_PAYLOAD_BYTES
    {
        let _ = capture(&args, &payload);
    }
    Ok(0)
}

fn capture(args: &CaptureArgs, payload: &[u8]) -> Result<(), Diagnostic> {
    let Some(search) = search(payload, &args.server, args.client) else {
        return Ok(());
    };
    let path = inventory_cli::state_path(args.state.clone())?;
    let Some(state) = inventory_cli::load_existing(&path)? else {
        return Ok(());
    };
    let Some(environment) = state.target.as_ref().map(|target| &target.environment) else {
        return Ok(());
    };
    let directory = path
        .parent()
        .expect("An absolute state path has a parent")
        .join("captures");
    setup_cli::protect_directory(&directory)?;
    let outcome = observe(&path, &directory, &state, environment, search);
    tally(&directory, outcome)
}

fn observe(
    path: &Path,
    directory: &Path,
    state: &InventoryState,
    environment: &EnvironmentId,
    search: Search,
) -> &'static str {
    if search.repository != environment.view {
        return "other-view";
    }
    let Some(body) = search.body else {
        return "search-failed";
    };
    match mcp::normalize(&body) {
        Ok(result) if result.rows.is_empty() => return "no-rows",
        Ok(_) => {}
        Err(_) => return "search-failed",
    }
    let Ok(hints) = app::inventory_hints(&search.query) else {
        return "query-not-read";
    };
    let [hint] = hints.as_slice() else {
        return "no-single-source";
    };
    let source = hint.source.clone();
    if !state
        .sources
        .iter()
        .any(|known| known.expected && known.selectors == source)
    {
        let action = InventoryAction::RecordInventoryHints {
            environment: environment.clone(),
            hints,
        };
        return match apply(path, action) {
            Ok(extra) if extra["new_work_revisions"] == 0 => "nothing-new",
            Ok(_) => "new-source-lead",
            Err(_) => "not-recorded",
        };
    }
    let retrieved_at = discovery::timestamp();
    let digest = cqlmatic::sha256_hex(&body);
    let name = format!("{}-{}", retrieved_at.replace([':', '-'], ""), &digest[..12]);
    let query_file = directory.join(format!("{name}.cql"));
    let result_file = directory.join(format!("{name}.json"));
    let query_created = private_file(&query_file, search.query.as_bytes()).is_ok();
    let result_created = query_created && private_file(&result_file, &body).is_ok();
    let failure = if result_created {
        apply(
            path,
            InventoryAction::ImportQueryObservations {
                query_file: query_file.to_string_lossy().into_owned(),
                result_file: result_file.to_string_lossy().into_owned(),
                environment: environment.clone(),
                source,
                start: search.start,
                // A call without `end` searched up to the moment it ran.
                end: search.end.unwrap_or_else(|| retrieved_at.clone()),
                retrieved_at: retrieved_at.clone(),
                caller_assertion: QueryResultAssertion::ClientHookCapture,
            },
        )
        .err()
        .map(|failure| failure.code)
    } else {
        Some("CAPTURE_WRITE".into())
    };
    let Some(failure) = failure else {
        return "recorded";
    };
    // An error can follow a replaced state, so ask the state whether the observation was saved.
    let saved = inventory_cli::load_existing(path).map(|state| {
        state.is_some_and(|state| {
            state.query_observations.iter().any(|observation| {
                observation.retrieved_at == retrieved_at && observation.result_sha256 == digest
            })
        })
    });
    match saved {
        Ok(true) => "recorded",
        // Unknown: keep the copies an observation may refer to.
        Err(_) => "not-recorded",
        Ok(false) => {
            // This call created these files, so no saved observation refers to them.
            if query_created {
                let _ = fs::remove_file(&query_file);
            }
            if result_created {
                let _ = fs::remove_file(&result_file);
            }
            if matches!(failure.as_str(), "PACK_BUSY" | "CAPTURE_WRITE") {
                "not-recorded"
            } else {
                "not-creditable"
            }
        }
    }
}

/// Parallel tool calls run their hooks at once, so wait briefly for another writer to finish.
fn patiently<T>(mut attempt: impl FnMut() -> Result<T, Diagnostic>) -> Result<T, Diagnostic> {
    let mut waits = 0;
    loop {
        match attempt() {
            Err(busy) if busy.code == "PACK_BUSY" && waits < 50 => {
                waits += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            result => return result,
        }
    }
}

fn apply(path: &Path, action: InventoryAction) -> Result<Value, Diagnostic> {
    patiently(|| inventory_cli::apply_and_store(path, action.clone())).map(|(_, extra)| extra)
}

/// Creates a new owner-only file, leaving nothing behind when it cannot be written in full.
fn private_file(path: &Path, bytes: &[u8]) -> Result<(), Diagnostic> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let failed = || Diagnostic::error("CAPTURE_WRITE", "Cannot save the captured search.");
    let mut file = options.open(path).map_err(|_| failed())?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| {
            let _ = fs::remove_file(path);
            failed()
        })
}

fn tally(directory: &Path, outcome: &str) -> Result<(), Diagnostic> {
    let path = directory.join("tally.json");
    let writer = patiently(|| storage::Writer::acquire(&path))?;
    let mut counts: BTreeMap<String, u64> = fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    *counts.entry(outcome.into()).or_default() += 1;
    let bytes = serde_json::to_vec_pretty(&counts).expect("A string-keyed count map serializes");
    writer.commit_bytes(&bytes, MAX_TALLY_BYTES)
}

/// One MCP tool call in the form every client's payload is reduced to.
struct Call {
    /// The tool's name within its server.
    tool: String,
    arguments: Value,
    /// The result as the client handed it over; absent when the call failed.
    response: Option<Value>,
    saved: Option<Saved>,
}

/// How a client reports a result it moved into a file instead of handing it over.
struct Saved {
    /// How the notice that replaces the result begins.
    notice: &'static str,
    /// The text that precedes the file's path.
    before_path: &'static str,
    /// The only directory such a file is read from.
    directory: PathBuf,
    /// Text the file's name must contain.
    name: &'static str,
}

fn call(payload: &[u8], server: &str, client: Client) -> Option<Call> {
    let hook: Value = serde_json::from_slice(payload).ok()?;
    let text = |name: &str| hook[name].as_str();
    // Some clients hand over arguments or results as JSON text.
    let parsed = |value: &Value| match value {
        Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| value.clone()),
        value => value.clone(),
    };
    // The tool's own name, when `name` places it on this server.
    let on_server = |name: &str, before: &str, after: &str| {
        name.strip_prefix(before)?
            .strip_prefix(server)?
            .strip_prefix(after)
            .map(str::to_owned)
    };
    Some(match client {
        Client::ClaudeCode | Client::Codex | Client::Generic => Call {
            tool: on_server(text("tool_name")?, "mcp__", "__")?,
            arguments: hook["tool_input"].clone(),
            // Claude Code reports a failed call as a different event without a result.
            response: (text("hook_event_name") != Some("PostToolUseFailure"))
                .then(|| hook["tool_response"].clone()),
            saved: text("transcript_path")
                .filter(|_| client == Client::ClaudeCode)
                .map(|transcript| Saved {
                    notice: "Error: result",
                    before_path: "Output has been saved to ",
                    directory: Path::new(transcript)
                        .with_extension("")
                        .join("tool-results"),
                    name: "",
                }),
        },
        Client::Cline => {
            // The command-line client sends `tool_result`; the editor extension `postToolUse`.
            let (name, arguments, failed, output) = match &hook["tool_result"] {
                Value::Null => {
                    let legacy = &hook["postToolUse"];
                    (
                        &legacy["toolName"],
                        &legacy["parameters"],
                        legacy["success"] == false,
                        &legacy["result"],
                    )
                }
                record => (
                    &record["name"],
                    &record["input"],
                    !record["error"].is_null(),
                    &record["output"],
                ),
            };
            Call {
                tool: on_server(name.as_str()?, "", "__")?,
                arguments: arguments.clone(),
                response: (!failed).then(|| parsed(output)),
                saved: None,
            }
        }
        Client::CopilotApp | Client::CopilotCli | Client::CopilotVscode => Call {
            tool: on_server(text("toolName")?, "", "-")?,
            arguments: parsed(&hook["toolArgs"]),
            // A failed call carries `error` in place of a result.
            response: hook["toolResult"]["textResultForLlm"]
                .as_str()
                .map(|text| text.into()),
            saved: Some(Saved {
                notice: "Output too large to read at once",
                before_path: "Saved to: ",
                directory: std::env::temp_dir(),
                name: "-copilot-tool-output-",
            }),
        },
        Client::Cursor => Call {
            tool: text("tool_name")
                .filter(|_| text("mcp_server_name") == Some(server))?
                .into(),
            arguments: parsed(&hook["tool_input"]),
            response: Some(parsed(&hook["result_json"])),
            saved: None,
        },
        Client::GeminiCli => {
            let context = &hook["mcp_context"];
            Call {
                tool: context["tool_name"]
                    .as_str()
                    .filter(|_| context["server_name"] == server)?
                    .into(),
                arguments: hook["tool_input"].clone(),
                // `returnDisplay` holds the result text without the wrapper added for the model.
                response: hook["tool_response"]["error"]
                    .is_null()
                    .then(|| hook["tool_response"]["returnDisplay"].clone()),
                saved: None,
            }
        }
        Client::Kiro => {
            let name = text("tool_name")?;
            Call {
                tool: on_server(name, "mcp_", "_").or_else(|| on_server(name, "@", "/"))?,
                arguments: hook["tool_input"].clone(),
                response: (hook["tool_response"]["success"] != false)
                    .then(|| hook["tool_response"]["result"][0].clone()),
                saved: None,
            }
        }
        Client::QwenCode => Call {
            tool: on_server(text("tool_name")?, "mcp__", "__")?,
            arguments: hook["tool_input"].clone(),
            response: (text("hook_event_name") != Some("PostToolUseFailure"))
                .then(|| hook["tool_response"]["returnDisplay"].clone()),
            saved: None,
        },
    })
}

fn search(payload: &[u8], server: &str, client: Client) -> Option<Search> {
    let call = call(payload, server, client)?;
    let arguments = match call.tool.as_str() {
        SEARCH_TOOL => &call.arguments,
        // The hosted server runs its tools through one gateway tool.
        "execute_tool" if call.arguments["name"] == SEARCH_TOOL => &call.arguments["arguments"],
        _ => return None,
    };
    let text = |name: &str| arguments[name].as_str().map(str::to_owned);
    Some(Search {
        repository: text("repository").unwrap_or_else(|| DEFAULT_REPOSITORY.into()),
        query: text("query_string")?,
        start: text("start")?,
        end: text("end"),
        body: body(&call),
    })
}

/// The bytes `mcp::normalize` judges: the result text, or the whole MCP result object so that
/// its error flag and structured copy are checked with it.
fn body(call: &Call) -> Option<Vec<u8>> {
    match call.response.as_ref()? {
        Value::String(text) => match &call.saved {
            Some(saved) if text.starts_with(saved.notice) => {
                let (_, rest) = text.split_once(saved.before_path)?;
                let path = Path::new(rest.lines().next()?.trim_end_matches('.'));
                (path.parent() == Some(&saved.directory)
                    && path.file_name()?.to_string_lossy().contains(saved.name))
                .then(|| saved_result(path))
                .flatten()
            }
            _ => Some(text.clone().into_bytes()),
        },
        // A bare content list is the MCP result object without its wrapper.
        blocks @ Value::Array(_) => {
            serde_json::to_vec(&serde_json::json!({ "content": blocks })).ok()
        }
        result @ Value::Object(_) => serde_json::to_vec(result).ok(),
        _ => None,
    }
}

fn saved_result(path: &Path) -> Option<Vec<u8>> {
    let limit = mcp::MAX_RESULT_BYTES as u64;
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > limit {
        return None;
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .ok()?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}
