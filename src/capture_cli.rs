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
use serde::Deserialize;
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
/// Claude Code replaces an oversized tool result with a notice naming the file that holds it.
const SAVED_NOTICE: &str = "Output has been saved to ";

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
    Codex,
    /// Any client or wrapper that supplies `tool_name`, `tool_input` and `tool_response`.
    Generic,
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
    let name = format!(
        "{}-{}",
        retrieved_at.replace([':', '-'], ""),
        &cqlmatic::sha256_hex(&body)[..12]
    );
    let query_file = directory.join(format!("{name}.cql"));
    let result_file = directory.join(format!("{name}.json"));
    let recorded = private_file(&query_file, search.query.as_bytes())
        .and_then(|()| private_file(&result_file, &body))
        .and_then(|()| {
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
                    retrieved_at,
                    caller_assertion: QueryResultAssertion::ClientHookCapture,
                },
            )
        });
    if recorded.is_ok() {
        return "recorded";
    }
    // Copies are kept only for results the inventory holds an observation of.
    let _ = fs::remove_file(&query_file);
    let _ = fs::remove_file(&result_file);
    "not-creditable"
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

fn private_file(path: &Path, bytes: &[u8]) -> Result<(), Diagnostic> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .map_err(|_| Diagnostic::error("CAPTURE_WRITE", "Cannot save the captured search."))
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

/// The post-tool hook fields Claude Code and Codex share.
#[derive(Deserialize)]
struct Hook {
    hook_event_name: Option<String>,
    tool_name: String,
    tool_input: Value,
    #[serde(default)]
    tool_response: Value,
    transcript_path: Option<String>,
}

fn search(payload: &[u8], server: &str, client: Client) -> Option<Search> {
    let hook: Hook = serde_json::from_slice(payload).ok()?;
    let tool = hook
        .tool_name
        .strip_prefix("mcp__")?
        .strip_prefix(server)?
        .strip_prefix("__")?;
    let arguments = match tool {
        SEARCH_TOOL => &hook.tool_input,
        // The hosted server runs its tools through one gateway tool.
        "execute_tool" if hook.tool_input["name"] == SEARCH_TOOL => &hook.tool_input["arguments"],
        _ => return None,
    };
    let text = |name: &str| arguments[name].as_str().map(str::to_owned);
    Some(Search {
        repository: text("repository").unwrap_or_else(|| DEFAULT_REPOSITORY.into()),
        query: text("query_string")?,
        start: text("start")?,
        end: text("end"),
        // Claude Code reports a failed call as a different event without a result.
        body: (hook.hook_event_name.as_deref() != Some("PostToolUseFailure"))
            .then(|| body(&hook, client))
            .flatten(),
    })
}

fn body(hook: &Hook, client: Client) -> Option<Vec<u8>> {
    let text = |blocks: &Value| match blocks.as_array()?.as_slice() {
        [block] if block["type"] == "text" => {
            block["text"].as_str().map(|text| text.as_bytes().to_vec())
        }
        _ => None,
    };
    match &hook.tool_response {
        Value::String(notice)
            if client == Client::ClaudeCode && notice.starts_with("Error: result") =>
        {
            let (_, rest) = notice.split_once(SAVED_NOTICE)?;
            let saved = Path::new(rest.lines().next()?.trim_end_matches('.'));
            // Only this session's own saved results are read.
            let results = Path::new(hook.transcript_path.as_deref()?)
                .with_extension("")
                .join("tool-results");
            (saved.parent() == Some(&results))
                .then(|| saved_result(saved))
                .flatten()
        }
        Value::String(body) => Some(body.clone().into_bytes()),
        blocks @ Value::Array(_) => text(blocks),
        // The MCP result object. A tool-level error carries no search result.
        result @ Value::Object(_) if result["isError"] != true => text(&result["content"]),
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
