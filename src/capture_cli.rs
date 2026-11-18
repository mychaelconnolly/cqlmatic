//! Records inventory evidence from a client's post-tool hook payload.
//!
//! A hook's output and exit status can reach the model, so the recorder prints nothing and
//! always exits 0. Each search it sees is counted by outcome in `tally.json`, in the
//! `<state file>.captures` folder beside the state.
mod install;

use crate::{inventory_cli, setup_cli};
use clap::{Args, ValueEnum};
use cqlmatic::{
    app,
    contracts::{Diagnostic, EnvironmentId},
    discovery,
    inventory::{InventoryAction, InventoryState, QueryObservation, QueryResultAssertion},
    mcp,
    setup::SEARCH_TOOL,
    storage,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

/// The search tool's documented repository when a call names none.
const DEFAULT_REPOSITORY: &str = "search-all";
const MAX_PAYLOAD_BYTES: u64 = 2 * mcp::MAX_RESULT_BYTES as u64;
const MAX_TALLY_BYTES: u64 = 65_536;
const MAX_AUDIT_BYTES: u64 = 1_048_576;
/// A file no saved observation refers to may be removed once it is this old. A running capture
/// finishes well within it.
const UNREFERENCED_AFTER: Duration = Duration::from_secs(600);

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
    #[arg(long, help = "Install or print the hook for the selected client")]
    install: bool,
    #[arg(
        long,
        requires = "install",
        help = "Report the hook without writing files"
    )]
    dry_run: bool,
    #[arg(
        long,
        requires = "install",
        value_name = "TEXT",
        help = "Use this command prefix verbatim instead of the running executable"
    )]
    program: Option<String>,
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
    if args.install {
        return install::run(&args);
    }
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
    match call(payload, &args.server, args.client) {
        Some(call) => record(args.state.clone(), call),
        None => Ok(()),
    }
}

/// Records a finished tool call when it is a search of the confirmed inventory's target.
pub(crate) fn record(state: Option<PathBuf>, call: Call) -> Result<(), Diagnostic> {
    let Some(search) = search(call) else {
        return Ok(());
    };
    let path = inventory_cli::state_path(state)?;
    let Some(state) = inventory_cli::load_existing(&path)? else {
        return Ok(());
    };
    let Some(environment) = state.target.as_ref().map(|target| &target.environment) else {
        return Ok(());
    };
    // Each state file has its own folder, so states sharing a directory keep separate copies.
    let mut name = path
        .file_name()
        .expect("A state path names a file")
        .to_os_string();
    name.push(".captures");
    let directory = path.with_file_name(name);
    setup_cli::protect_directory(&directory)?;
    let outcome = observe(&path, &directory, &state, environment, search);
    let counted = tally(&directory, outcome);
    if matches!(outcome, "recorded" | "not-recorded") {
        tidy(&path, &directory);
    }
    counted
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
    // A call without `end` searched up to the moment it ran.
    let end = search.end.unwrap_or_else(|| retrieved_at.clone());
    // Judge the search before anything is written; most that fail here are aggregates.
    let Ok(observation) = QueryObservation::from_result(
        search.query.as_bytes(),
        &body,
        environment.clone(),
        source.clone(),
        search.start.clone(),
        end.clone(),
        retrieved_at.clone(),
        QueryResultAssertion::ClientHookCapture,
        path.to_string_lossy().into_owned(),
    ) else {
        return "not-creditable";
    };
    if state.capture_is_covered(&observation).unwrap_or(false) {
        return "already-covered";
    }
    let digest = observation.result_sha256;
    let name = copy_name(&retrieved_at, &digest);
    let query_file = directory.join(format!("{name}.cql"));
    let result_file = directory.join(format!("{name}.json"));
    let mut created = Vec::new();
    if private_file(&query_file, search.query.as_bytes()).is_ok() {
        created.push(query_file.as_path());
        if private_file(&result_file, &body).is_ok() {
            created.push(result_file.as_path());
        }
    }
    let imported = created.len() == 2
        && apply(
            path,
            InventoryAction::ImportQueryObservations {
                query_file: query_file.to_string_lossy().into_owned(),
                result_file: result_file.to_string_lossy().into_owned(),
                environment: environment.clone(),
                source,
                start: search.start,
                end,
                retrieved_at: retrieved_at.clone(),
                caller_assertion: QueryResultAssertion::ClientHookCapture,
            },
        )
        .is_ok();
    if imported {
        return "recorded";
    }
    after_failed_import(path, &retrieved_at, &digest, &created)
}

/// The name shared by a captured search's query and result copies.
fn copy_name(retrieved_at: &str, digest: &str) -> String {
    format!("{}-{}", retrieved_at.replace([':', '-'], ""), &digest[..12])
}

/// Removes what a stopped capture, a failed save or a dropped capture left behind in this
/// state's own folder: aged copies, and aged audit records of its captures, that no saved
/// observation refers to. Failures are ignored; the next capture tries again.
fn tidy(path: &Path, directory: &Path) {
    let (Ok(Some(state)), Ok(directory)) = (
        inventory_cli::load_existing(path),
        fs::canonicalize(directory),
    ) else {
        return;
    };
    let aged = |file: &Path| {
        fs::symlink_metadata(file)
            .ok()
            .filter(fs::Metadata::is_file)
            .and_then(|meta| meta.modified().ok())
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= UNREFERENCED_AFTER)
    };
    let files = |directory: &Path, extensions: &[&str]| {
        fs::read_dir(directory)
            .into_iter()
            .flatten()
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|file| {
                file.extension()
                    .is_some_and(|found| extensions.iter().any(|extension| found == *extension))
                    && aged(file)
            })
            .collect::<Vec<_>>()
    };
    let record = |audit: &Path| {
        fs::File::open(audit)
            .ok()
            .and_then(|file| serde_json::from_reader::<_, Value>(file.take(MAX_AUDIT_BYTES)).ok())
    };
    let observations = || {
        state
            .captured_observations
            .iter()
            .chain(&state.query_observations)
    };

    // Copies are named `<time>-<twelve digits of the result's hash>`; a current capture's are kept
    // by name alone.
    let copies = files(&directory, &["json", "cql"])
        .into_iter()
        .filter(|file| {
            let stem = file.file_stem().unwrap_or_default().to_string_lossy();
            stem.rsplit_once('-').is_some_and(|(_, digits)| {
                digits.len() == 12 && digits.bytes().all(|digit| digit.is_ascii_hexdigit())
            }) && !state
                .captured_observations
                .iter()
                .any(|capture| copy_name(&capture.retrieved_at, &capture.result_sha256) == stem)
        })
        .collect::<Vec<_>>();
    // Audit records of this state's captures that no saved observation refers to. Records of
    // imports and of other states' captures are left alone.
    let audits = path
        .parent()
        .map(|parent| files(&parent.join("query-observations"), &["json"]))
        .unwrap_or_default()
        .into_iter()
        .filter(|audit| {
            !observations().any(|observation| Path::new(&observation.audit_path) == audit)
                && record(audit).is_some_and(|record| {
                    record["observation"]["caller_assertion"] == "client-hook-capture"
                        && record["query_file"]
                            .as_str()
                            .is_some_and(|query| Path::new(query).parent() == Some(&directory))
                })
        })
        .collect::<Vec<_>>();
    if copies.is_empty() && audits.is_empty() {
        return;
    }

    // An audit record names the files its observation refers to. Those stay, whichever
    // observation it is; when one record cannot be read, nothing is removed.
    let mut named = BTreeSet::new();
    for observation in observations() {
        let Some(record) = record(Path::new(&observation.audit_path)) else {
            return;
        };
        for file in ["query_file", "result_file"] {
            named.insert(record[file].as_str().map(PathBuf::from));
        }
    }
    for copy in copies {
        if !named.contains(&Some(copy.clone())) {
            let _ = fs::remove_file(copy);
        }
    }
    for audit in audits {
        let _ = fs::remove_file(&audit);
        // The writer that created the record left its lock file beside it.
        if let Some(name) = audit.file_name() {
            let lock = format!(".{}.lock", name.to_string_lossy());
            let _ = fs::remove_file(audit.with_file_name(lock));
        }
    }
}

/// Settles a failed import. The error can follow a replaced state, so the copies this call
/// created are removed only when the saved state holds no observation of them.
fn after_failed_import(
    path: &Path,
    retrieved_at: &str,
    digest: &str,
    created: &[&Path],
) -> &'static str {
    let saved = inventory_cli::load_existing(path).map(|state| {
        state.is_some_and(|state| {
            state.captured_observations.iter().any(|observation| {
                observation.retrieved_at == retrieved_at && observation.result_sha256 == digest
            })
        })
    });
    match saved {
        Ok(true) => "recorded",
        Ok(false) => {
            for file in created {
                let _ = fs::remove_file(file);
            }
            "not-recorded"
        }
        // Unknown: keep the copies an observation may refer to.
        Err(_) => "not-recorded",
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
pub(crate) struct Call {
    /// The tool's name within its server.
    pub(crate) tool: String,
    pub(crate) arguments: Value,
    /// The result as the client handed it over; absent when the call failed.
    pub(crate) response: Option<Value>,
    saved: Option<Saved>,
}

impl Call {
    /// Whether the call names the search tool or the hosted server's gateway to it.
    pub(crate) fn may_be_search(&self) -> bool {
        self.tool == SEARCH_TOOL
            || (self.tool == "execute_tool" && self.arguments["name"] == SEARCH_TOOL)
    }
    /// A call seen on the server's own connection: `result` is the MCP result object, absent
    /// when the server answered with an error.
    pub(crate) fn on_connection(tool: String, arguments: Value, result: Option<Value>) -> Self {
        Self {
            tool,
            arguments,
            response: result,
            saved: None,
        }
    }
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

fn search(call: Call) -> Option<Search> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_failed_import_removes_copies_only_when_the_state_holds_no_observation_of_them() {
        let root =
            std::env::temp_dir().join(format!("cqlmatic-capture-unit-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let path = root.join("workflow/state.json");
        let environment: EnvironmentId = serde_json::from_value(json!({
            "cloud": "us1", "tenant": "00000000000000000000000000000000", "view": "third-party"
        }))
        .unwrap();
        let confirm = serde_json::from_value(json!({
            "kind": "confirm-target", "environment": environment, "scope": "selected-sources",
            "context": "Synthetic capture target", "expected_sources": [{"type": "synthetic"}]
        }))
        .unwrap();
        apply(&path, confirm).unwrap();
        let state = inventory_cli::load_existing(&path).unwrap().unwrap();
        let directory = root.join("workflow/state.json.captures");
        setup_cli::protect_directory(&directory).unwrap();
        let search = Search {
            repository: "third-party".into(),
            query: "#type=synthetic | head(5)".into(),
            start: "2026-01-01T00:00:00Z".into(),
            end: Some("2026-01-01T01:00:00Z".into()),
            body: Some(br#"{"result":[{"user":"ann"}]}"#.to_vec()),
        };
        assert_eq!(
            observe(&path, &directory, &state, &environment, search),
            "recorded"
        );
        // A capture is its own category: no imported observation, no change to the evidence
        // that reconciliation commits to.
        let mut after = inventory_cli::load_existing(&path).unwrap().unwrap();
        assert!(after.query_observations.is_empty());
        assert_eq!(
            after.evidence_digest().unwrap(),
            state.evidence_digest().unwrap()
        );
        let saved = after.captured_observations.remove(0);
        let copies = fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        let copies = copies.iter().map(PathBuf::as_path).collect::<Vec<_>>();
        assert_eq!(copies.len(), 2);
        let settle = |retrieved_at: &str| {
            after_failed_import(&path, retrieved_at, &saved.result_sha256, &copies)
        };

        // The import reported an error after the state had been replaced.
        assert_eq!(settle(&saved.retrieved_at), "recorded");
        assert!(copies.iter().all(|copy| copy.exists()));

        // A state that cannot be read leaves the answer unknown.
        let bytes = fs::read(&path).unwrap();
        fs::write(&path, b"{").unwrap();
        assert_eq!(settle("2026-01-01T02:00:00Z"), "not-recorded");
        assert!(copies.iter().all(|copy| copy.exists()));

        // The state holds no observation of these copies.
        fs::write(&path, bytes).unwrap();
        assert_eq!(settle("2026-01-01T02:00:00Z"), "not-recorded");
        assert!(copies.iter().all(|copy| !copy.exists()));
        fs::remove_dir_all(root).unwrap();
    }
}
