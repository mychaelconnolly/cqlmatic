//! Inventory CLI I/O. The pure inventory module never reads files or credentials.
use clap::Args;
use cqlmatic::{
    contracts::Diagnostic,
    discovery,
    environment::Pack,
    inventory::{self, *},
    mcp, network, storage,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Debug, Args)]
pub struct InventoryArgs {
    #[arg(
        long,
        value_name = "FILE",
        help = "Versioned inventory action; '-' reads stdin"
    )]
    pub request: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Workflow state; defaults to local cqlmatic/inventory/state.json"
    )]
    pub state: Option<PathBuf>,
    #[arg(long, help = "Emit structured inventory status and next actions")]
    pub json: bool,
    #[arg(
        long,
        requires = "json",
        help = "Emit compact counts, progress and next actions"
    )]
    pub summary: bool,
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    #[arg(long, value_name = "FILE", help = "Optional versioned status request")]
    pub request: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Workflow state; defaults to local cqlmatic/inventory/state.json"
    )]
    pub state: Option<PathBuf>,
    #[arg(long, help = "Emit structured inventory status")]
    pub json: bool,
    #[arg(
        long,
        requires = "json",
        help = "Emit compact counts, progress and next actions"
    )]
    pub summary: bool,
}

fn fail(message: &str) -> Diagnostic {
    Diagnostic::error("INVENTORY_INPUT", message)
}
fn output(value: &impl Serialize) -> Result<(), Diagnostic> {
    let mut out = io::stdout().lock();
    serde_json::to_writer(&mut out, value).map_err(|_| fail("Cannot emit inventory result."))?;
    writeln!(out).map_err(|_| fail("Cannot emit inventory result."))
}
fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, Diagnostic> {
    let raw = if path == Path::new("-") {
        let mut bytes = Vec::new();
        io::stdin()
            .take(MAX_STATE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| fail("Cannot read inventory request from stdin."))?;
        if bytes.len() as u64 > MAX_STATE_BYTES {
            return Err(fail("Inventory input exceeds 64 MiB."));
        }
        bytes
    } else {
        storage::read_bytes(path, MAX_STATE_BYTES)?
    };
    serde_json::from_slice(&raw).map_err(|_| {
        fail("Invalid inventory JSON or unknown fields; inspect the inventory schema.")
    })
}

pub fn default_state_path() -> Result<PathBuf, Diagnostic> {
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local).join("cqlmatic/inventory/state.json"));
    }
    if let Some(local) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(local).join("cqlmatic/inventory/state.json"));
    }
    let home = std::env::var_os("HOME")
        .ok_or_else(|| fail("Cannot locate local data directory; supply --state FILE."))?;
    #[cfg(target_os = "macos")]
    {
        Ok(PathBuf::from(home).join("Library/Application Support/cqlmatic/inventory/state.json"))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(PathBuf::from(home).join(".local/share/cqlmatic/inventory/state.json"))
    }
}

fn state_path(explicit: Option<PathBuf>) -> Result<PathBuf, Diagnostic> {
    let path = explicit.map_or_else(default_state_path, Ok)?;
    std::path::absolute(path).map_err(|_| fail("Cannot resolve inventory state path."))
}
fn load(path: &Path) -> Result<InventoryState, Diagnostic> {
    if !path
        .try_exists()
        .map_err(|_| fail("Cannot inspect inventory state."))?
    {
        return InventoryState::new(&discovery::timestamp());
    }
    let state: InventoryState = read_json(path)?;
    state.validate()?;
    Ok(state)
}
fn packs(state: &InventoryState) -> Result<Vec<Pack>, Diagnostic> {
    state
        .packs
        .iter()
        .map(|p| storage::read_pack(Path::new(&p.path)))
        .collect()
}
fn protect_state(path: &Path) -> Result<(), Diagnostic> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    crate::setup_cli::protect_directory(parent)
}
fn snapshot_directory(path: &Path, label: &str, start: usize) -> Result<PathBuf, Diagnostic> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let key = cqlmatic::sha256_hex(absolute_path(path)?.as_bytes());
    for index in start..start.saturating_add(10_000) {
        let directory = parent.join(format!("inventory-{}-{label}-{index}", &key[..12]));
        if directory
            .try_exists()
            .map_err(|_| fail("Cannot inspect inventory snapshot directory."))?
        {
            continue;
        }
        crate::setup_cli::protect_directory(&directory)?;
        return Ok(directory);
    }
    Err(fail("Cannot allocate a new inventory snapshot directory."))
}
fn cumulative_pack(state: &InventoryState) -> Result<Option<Pack>, Diagnostic> {
    let loaded = packs(state)?;
    inventory::assess(state, &loaded)?;
    if let Some(active) = &state.active_pack {
        return Ok(loaded
            .into_iter()
            .find(|p| p.content_digest == active.content_digest));
    }
    // Older workflow states may contain separate source snapshots. Merge them once
    // before the next run so its activated pack remains directly usable by writing.
    let mut loaded = loaded;
    loaded.sort_by_key(|p| cqlmatic::environment::parse_time(&p.created_at).ok());
    let mut cumulative = None;
    for pack in loaded {
        cumulative = Some(match cumulative {
            Some(previous) => cqlmatic::environment::merge(&previous, &pack)?,
            None => pack,
        });
    }
    Ok(cumulative)
}
fn render(status: &InventoryStatus) -> Result<(), Diagnostic> {
    let mut out = io::stdout().lock();
    writeln!(
        out,
        "{}",
        if status.blank_canvas {
            "Environment inventory: blank canvas"
        } else if status.full_map_complete {
            "Environment inventory: full scoped map complete"
        } else {
            "Environment inventory: work remains"
        }
    )
    .map_err(|_| fail("Cannot emit inventory status."))?;
    writeln!(
        out,
        "Discovery mode: {:?}; Quick depth: {:?}",
        status.discovery_mode, status.quick_depth
    )
    .map_err(|_| fail("Cannot emit inventory status."))?;
    writeln!(out, "{}", status.message).map_err(|_| fail("Cannot emit inventory status."))?;
    if let Some(pack) = &status.active_pack {
        writeln!(
            out,
            "Active pack: {:?} (SHA-256 {})",
            pack.path, pack.content_digest
        )
        .map_err(|_| fail("Cannot emit inventory status."))?;
    }
    writeln!(
        out,
        "Target: {}; scouting: {}; parser catalog: {} ({}); reconciliation: {}",
        status.target,
        status.scouting,
        status.parser_catalog,
        status.parser_count,
        status.reconciliation
    )
    .map_err(|_| fail("Cannot emit inventory status."))?;
    for source in &status.sources {
        let selectors = [
            source
                .selectors
                .repo
                .as_ref()
                .map(|repo| format!("#repo={repo:?}")),
            source
                .selectors
                .type_
                .as_ref()
                .map(|type_| format!("#type={type_:?}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        writeln!(
            out,
            "{} [{selectors}]: {}; parser {}; measurements {}; observed {} / {} fields",
            source.source_id,
            source.status,
            source.parser,
            source.measurements,
            source.fields.iter().filter(|f| f.observed).count(),
            source.fields.len()
        )
        .map_err(|_| fail("Cannot emit inventory status."))?;
        for action in &source.next_actions {
            writeln!(out, "  - {action}").map_err(|_| fail("Cannot emit inventory status."))?;
        }
    }
    for run in &status.discovery_runs {
        writeln!(
            out,
            "Discovery {:?}: {}; queries {}/{}; elapsed {:.1}/{} seconds; sources {}",
            run.state_path,
            run.status,
            run.answered,
            run.max_queries,
            run.elapsed_seconds,
            run.max_seconds,
            run.source_ids.len()
        )
        .map_err(|_| fail("Cannot emit inventory status."))?;
    }
    for failure in &status.failures {
        writeln!(out, "Failure [{:?}]: {:?}", failure.stage, failure.message)
            .map_err(|_| fail("Cannot emit inventory status."))?;
    }
    for action in &status.next_actions {
        writeln!(out, "- {action}").map_err(|_| fail("Cannot emit inventory status."))?;
    }
    Ok(())
}

pub fn status(args: StatusArgs) -> Result<i32, Diagnostic> {
    let request: Option<StatusRequest> = args.request.as_ref().map(|p| read_json(p)).transpose()?;
    if request.as_ref().is_some_and(|r| r.schema_version != 1) {
        return Err(fail("Unsupported status request version."));
    }
    if args.state.is_some() && request.as_ref().is_some_and(|r| r.state.is_some()) {
        return Err(fail(
            "Select state in either --state or the request, not both.",
        ));
    }
    let path = state_path(
        args.state
            .or_else(|| request.and_then(|r| r.state.map(PathBuf::from))),
    )?;
    let state = load(&path)?;
    let report = inventory::assess(&state, &packs(&state)?)?;
    if args.json {
        if args.summary {
            output(&inventory::summary(&report))?;
        } else {
            output(&report)?;
        }
    } else {
        render(&report)?;
    }
    Ok(0)
}

pub fn run(args: InventoryArgs) -> Result<i32, Diagnostic> {
    let path = state_path(args.state)?;
    if args.request.is_none() {
        let state = load(&path)?;
        let report = inventory::assess(&state, &packs(&state)?)?;
        if args.json || !io::stdin().is_terminal() {
            if args.json {
                if args.summary {
                    output(&inventory::summary(&report))?;
                } else {
                    output(&report)?;
                }
            } else {
                render(&report)?;
            }
            return Ok(0);
        }
        return interactive(&path, state, report);
    }
    let request: InventoryRequest = read_json(args.request.as_deref().expect("request checked"))?;
    if request.schema_version != 1 {
        return Err(fail("Unsupported inventory request version."));
    }
    protect_state(&path)?;
    let writer = storage::Writer::acquire(&path)?;
    protect_state(&path)?;
    let mut state = load(&path)?;
    let now = discovery::timestamp();
    state.schema_version = 2;
    let extra = apply(&path, &mut state, request.action, &now)?;
    state.updated_at = now;
    state.validate()?;
    let report = inventory::assess(&state, &packs(&state)?)?;
    let bytes =
        serde_json::to_vec_pretty(&state).map_err(|_| fail("Cannot serialize inventory state."))?;
    writer.commit_bytes(&bytes, MAX_STATE_BYTES)?;
    if args.json {
        if args.summary {
            output(
                &json!({"schema_version":1,"summary":true,"state":path,"status":inventory::summary(&report),"action":action_summary(&extra)}),
            )?;
        } else {
            output(&json!({"schema_version":1,"state":path,"status":report,"action":extra}))?;
        }
    } else {
        render(&report)?;
        if !extra.is_null() {
            output(&extra)?;
        }
    }
    Ok(0)
}

fn action_summary(action: &Value) -> Value {
    if action.is_null() {
        return Value::Null;
    }
    let mut compact = action.clone();
    if let Some(relay) = compact.get_mut("relay").and_then(Value::as_object_mut) {
        if let Some(probes) = relay.remove("probes") {
            relay.insert(
                "pending_probe_count".into(),
                json!(probes.as_array().map_or(0, Vec::len)),
            );
        }
        if relay.get("status").and_then(Value::as_str) == Some("needs-probes") {
            compact["next_action"] = json!(
                "Submit discovery-step with answers=[] and --json without --summary to obtain exact pending probe arguments."
            );
        } else if let Some(compact) = compact.as_object_mut() {
            compact.remove("next_action");
        }
    }
    compact
}

fn observation_input(path: &Path) -> Result<Vec<u8>, Diagnostic> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    crate::setup_cli::protect_directory(parent)?;
    crate::setup_cli::verify_private_file(path)?;
    fn same_file(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if left.dev() != right.dev()
                || left.ino() != right.ino()
                || left.nlink() != right.nlink()
            {
                return false;
            }
        }
        left.len() == right.len()
            && left.modified().ok() == right.modified().ok()
            && left.created().ok() == right.created().ok()
            && left.permissions() == right.permissions()
    }
    let before =
        std::fs::symlink_metadata(path).map_err(|_| fail("Cannot inspect observation input."))?;
    let mut file = std::fs::File::open(path).map_err(|_| fail("Cannot open observation input."))?;
    let opened = file
        .metadata()
        .map_err(|_| fail("Cannot inspect opened observation input."))?;
    if !same_file(&before, &opened) || opened.len() > mcp::MAX_RESULT_BYTES as u64 {
        return Err(fail("Observation input changed or exceeds 8 MiB."));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(mcp::MAX_RESULT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| fail("Cannot read observation input."))?;
    let after = file
        .metadata()
        .map_err(|_| fail("Cannot reinspect observation input."))?;
    let current = std::fs::symlink_metadata(path)
        .map_err(|_| fail("Cannot reinspect observation input path."))?;
    crate::setup_cli::protect_directory(parent)?;
    crate::setup_cli::verify_private_file(path)?;
    if !same_file(&opened, &after)
        || !same_file(&after, &current)
        || bytes.len() as u64 != opened.len()
    {
        return Err(fail("Observation input changed while it was being read."));
    }
    Ok(bytes)
}

#[allow(clippy::too_many_arguments)]
fn import_query_observations(
    path: &Path,
    state: &mut InventoryState,
    query_file: &str,
    result_file: &str,
    environment: cqlmatic::contracts::EnvironmentId,
    source: cqlmatic::contracts::Scope,
    start: String,
    end: String,
    retrieved_at: String,
    caller_assertion: QueryResultAssertion,
) -> Result<Value, Diagnostic> {
    let query_file = Path::new(query_file);
    let result_file = Path::new(result_file);
    distinct_from_state(path, query_file)?;
    distinct_from_state(path, result_file)?;
    let query = observation_input(query_file)?;
    let result = observation_input(result_file)?;
    if state
        .target
        .as_ref()
        .is_none_or(|t| t.environment != environment)
    {
        return Err(fail(
            "Confirm the exact query target environment before importing observations.",
        ));
    }
    let source_id = discovery::source_id(&source);
    if !state
        .sources
        .iter()
        .any(|s| s.id == source_id && s.expected && s.selectors == source)
    {
        return Err(fail(
            "Query observations require an exact confirmed expected source.",
        ));
    }
    let key = cqlmatic::sha256_hex(&serde_json::to_vec(&json!({
        "query_sha256":cqlmatic::sha256_hex(&query),"result_sha256":cqlmatic::sha256_hex(&result),
        "environment":environment,"source":source,"start":start,"end":end,"retrieved_at":retrieved_at,
        "caller_assertion":caller_assertion
    })).map_err(|_| fail("Cannot identify query observation audit."))?);
    let parent = path
        .parent()
        .ok_or_else(|| fail("Inventory state requires a parent directory."))?;
    let directory = parent.join("query-observations");
    let audit_path = directory.join(format!("{key}.json"));
    let observation = QueryObservation::from_result(
        &query,
        &result,
        environment,
        source,
        start,
        end,
        retrieved_at,
        caller_assertion,
        absolute_path(&audit_path)?,
    )?;
    let audit = json!({"schema_version":1,"query_file":canonical(query_file)?,"result_file":canonical(result_file)?,"observation":observation});
    state.record_query_observation(observation.clone())?;
    crate::setup_cli::protect_directory(&directory)?;
    if audit_path.exists() {
        let existing: Value = serde_json::from_slice(&observation_input(&audit_path)?)
            .map_err(|_| fail("Saved query observation audit is invalid."))?;
        if existing != audit {
            return Err(fail(
                "Saved query observation audit differs from this exact import.",
            ));
        }
    } else {
        let bytes = serde_json::to_vec_pretty(&audit)
            .map_err(|_| fail("Cannot serialize query observation audit."))?;
        storage::Writer::acquire(&audit_path)?.create_new_bytes(&bytes, MAX_STATE_BYTES)?;
    }
    Ok(
        json!({"query_observation_id":observation.id,"audit_path":observation.audit_path,
        "query_sha256":observation.query_sha256,"result_sha256":observation.result_sha256,
        "field_count":observation.fields.len(),"pack_updated":false,"executes_searches":false}),
    )
}

fn canonical(path: &Path) -> Result<String, Diagnostic> {
    std::fs::canonicalize(path)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| fail("Cannot resolve evidence path."))
}
fn distinct_from_state(state: &Path, input: &Path) -> Result<(), Diagnostic> {
    if state.exists() && canonical(state)? == canonical(input)? {
        return Err(fail("Evidence input must differ from inventory state."));
    }
    Ok(())
}
fn apply(
    path: &Path,
    state: &mut InventoryState,
    action: InventoryAction,
    now: &str,
) -> Result<Value, Diagnostic> {
    match action {
        InventoryAction::Init {} => {
            if path.exists() {
                return Err(fail(
                    "Inventory already exists; resume it or select a new --state path.",
                ));
            }
            Ok(json!({"initialized":true,"executes_searches":false}))
        }
        InventoryAction::SetDiscoveryMode { mode, quick_depth } => {
            state.discovery_mode = mode;
            state.quick_depth = quick_depth;
            Ok(json!({"discovery_mode":mode,"quick_depth":quick_depth,"executes_searches":false}))
        }
        InventoryAction::ImportQueryObservations {
            query_file,
            result_file,
            environment,
            source,
            start,
            end,
            retrieved_at,
            caller_assertion,
        } => import_query_observations(
            path,
            state,
            &query_file,
            &result_file,
            environment,
            source,
            start,
            end,
            retrieved_at,
            caller_assertion,
        ),
        InventoryAction::PrepareScout {
            environment,
            start,
            end,
            max_seconds,
        } => {
            let plan = state.prepare_scout_with_budget(environment, start, end, max_seconds)?;
            Ok(
                json!({"mode":"agent-relayed-mcp","tool":"falcon_search_ngsiem","arguments":plan.arguments,"query_sha256":plan.query_sha256,"search_budget":{"max_queries":1,"max_seconds":plan.max_seconds,"enforced_by":"mcp-host"},"authorization_required":true,"executes_searches":false,"next_action":"Obtain caller authorization for this exact target and UTC window. Configure the MCP host to enforce one query and an authorized finite time budget, using the displayed max_seconds when present, before running the exact arguments; CQLmatic cannot enforce host elapsed time. Save the entire unedited result and submit import-scout with its path and exact arguments."}),
            )
        }
        InventoryAction::ImportScout {
            result_file,
            arguments,
            retrieved_at,
        } => {
            distinct_from_state(path, Path::new(&result_file))?;
            let raw = storage::read_bytes(Path::new(&result_file), mcp::MAX_RESULT_BYTES as u64)?;
            state.import_scout(arguments, &raw, &retrieved_at)?;
            Ok(Value::Null)
        }
        InventoryAction::ConfirmTarget {
            environment,
            scope,
            context,
            expected_sources,
        } => {
            state.confirm(environment, scope, context, expected_sources, now)?;
            Ok(Value::Null)
        }
        InventoryAction::ReviewSource {
            source_id,
            purpose,
            data_model,
            reviewed_at,
            evidence,
        } => {
            state.source_mut(&source_id)?.review = Some(SourceReview {
                purpose,
                data_model,
                reviewed_at,
                evidence,
            });
            Ok(Value::Null)
        }
        InventoryAction::ImportParsers {
            pages,
            details,
            retrieved_at,
        } => {
            if pages.len() > 100 || details.len() > 10_000 {
                return Err(fail(
                    "Saved parser import exceeds 100 pages / 10000 parser details.",
                ));
            }
            let mut captured_pages = Vec::new();
            let mut captured_details = Vec::new();
            for page in pages {
                distinct_from_state(path, Path::new(&page.result_file))?;
                let raw = storage::read_bytes(
                    Path::new(&page.result_file),
                    mcp::MAX_RESULT_BYTES as u64,
                )?;
                captured_pages.push(ParserPage {
                    offset: page.offset,
                    limit: page.limit,
                    snapshot: RawSnapshot::new(&raw, &retrieved_at)?,
                });
            }
            for detail in details {
                distinct_from_state(path, Path::new(&detail.result_file))?;
                let raw = storage::read_bytes(
                    Path::new(&detail.result_file),
                    mcp::MAX_RESULT_BYTES as u64,
                )?;
                captured_details.push(parse_parser(
                    &detail.parser_id,
                    RawSnapshot::new(&raw, &retrieved_at)?,
                )?);
            }
            state.import_parser_inventory(captured_pages, captured_details)?;
            Ok(Value::Null)
        }
        InventoryAction::FetchParsers {
            credentials_file,
            max_pages,
            max_parsers,
            max_seconds,
        } => {
            if credentials_file.trim().is_empty() {
                return Err(fail(
                    "Supply a protected credential file; credentials are never saved in inventory state.",
                ));
            }
            distinct_from_state(path, Path::new(&credentials_file))?;
            let target = state.target.as_ref().ok_or_else(|| {
                fail("Confirm CID, cloud and view before requesting parser inventory.")
            })?;
            let credentials = network::Credentials::load(
                Some(Path::new(&credentials_file)),
                &target.environment,
            )?;
            let mut client = network::FalconClient::new(&target.environment, credentials)?;
            let outcome = fetch_parsers(&mut client, max_pages, max_parsers, max_seconds)?;
            state.import_parser_inventory(outcome.pages, outcome.details)?;
            state.parser_runs.push(outcome.run.clone());
            if let Some(diagnostic) = &outcome.run.failure {
                state.failures.push(InventoryFailure {
                    stage: "parser-inventory".into(),
                    source_id: None,
                    message: diagnostic.message.clone(),
                    recorded_at: now.into(),
                });
            }
            Ok(
                json!({"parser_count":state.parsers.len(),"run":outcome.run,"credentials_persisted":false}),
            )
        }
        InventoryAction::BindParser {
            source_id,
            parser_id,
            parser_sha256,
            reviewed_at,
            evidence,
            semantics,
        } => {
            let binding = ParserBinding::Parser {
                parser_id: parser_id.clone(),
                parser_sha256,
                reviewed_at,
                evidence,
                semantics,
            };
            state.validate_binding(&binding)?;
            state.parser_exclusions.retain(|e| e.parser_id != parser_id);
            state.source_mut(&source_id)?.binding = Some(binding);
            Ok(Value::Null)
        }
        InventoryAction::ParserNotApplicable {
            source_id,
            reason,
            reviewed_at,
            evidence,
        } => {
            let binding = ParserBinding::NotApplicable {
                reason,
                reviewed_at,
                evidence,
            };
            state.validate_binding(&binding)?;
            state.source_mut(&source_id)?.binding = Some(binding);
            Ok(Value::Null)
        }
        InventoryAction::ExcludeParser {
            parser_id,
            parser_sha256,
            reviewed_at,
            reason,
        } => {
            state.parser_version(&parser_id, &parser_sha256)?;
            if state.sources.iter().any(|s| matches!(&s.binding, Some(ParserBinding::Parser { parser_id: id, .. }) if id == &parser_id)) { return Err(fail("A bound parser cannot also be excluded.")); }
            state.parser_exclusions.retain(|e| e.parser_id != parser_id);
            state.parser_exclusions.push(ParserExclusion {
                parser_id,
                parser_sha256,
                reviewed_at,
                reason,
            });
            Ok(Value::Null)
        }
        InventoryAction::PrepareDiscovery {
            start,
            end,
            source_ids,
            value_fields,
            kind_fields,
            population_fields,
            max_queries,
            max_seconds,
        } => {
            let request = state.discovery_request(
                start,
                end,
                &source_ids,
                value_fields,
                kind_fields,
                population_fields,
                max_queries,
                max_seconds,
            )?;
            let prior = cumulative_pack(state)?;
            let run_dir = snapshot_directory(path, "discovery", state.discovery_runs.len() + 1)?;
            let relay = run_dir.join("relay-state.json");
            let pack = run_dir.join("pack.json");
            let receipt = run_dir.join("receipt.json");
            if let Some(prior) = prior {
                let bytes = serde_json::to_vec_pretty(&prior)
                    .map_err(|_| fail("Cannot serialize the cumulative environment pack."))?;
                storage::Writer::acquire(&pack)?.create_new_bytes(&bytes, MAX_STATE_BYTES)?;
            }
            let selected: Vec<_> = state
                .sources
                .iter()
                .filter(|s| request.sources.contains(&s.selectors))
                .map(|s| s.id.clone())
                .collect();
            let output = mcp::init(request.clone(), &relay, &pack, &receipt)?;
            state.discovery_runs.push(DiscoveryRun {
                state_path: absolute_path(&relay)?,
                pack_path: absolute_path(&pack)?,
                receipt_path: absolute_path(&receipt)?,
                start: request.start,
                end: request.end,
                source_ids: selected,
                value_fields: request.value_fields,
                max_queries,
                max_seconds,
                answered: output.answered,
                elapsed_seconds: 0.0,
                status: output.status.into(),
                mode: state.discovery_mode,
                quick_depth: state.quick_depth,
            });
            Ok(
                json!({"warning":DISCOVERY_WARNING,"relay":output,"next_action":"Relay each emitted probe through the existing CrowdStrike MCP, save each entire result, then submit discovery-step with probe_id and result_file. The CLI does not implement MCP OAuth or internal model calls."}),
            )
        }
        InventoryAction::DiscoveryStep { answers } => {
            let run = state
                .discovery_runs
                .last_mut()
                .ok_or_else(|| fail("Prepare discovery before relaying saved answers."))?;
            let already_complete = run.status == "complete";
            let answers: Vec<_> = answers
                .into_iter()
                .map(|a| (a.probe_id, PathBuf::from(a.result_file)))
                .collect();
            for (_, input) in &answers {
                distinct_from_state(path, input)?;
            }
            let result = mcp::step(Path::new(&run.state_path), &answers);
            if let Ok(relay) = read_json::<mcp::State>(Path::new(&run.state_path)) {
                run.answered = relay.answers.len();
                run.status = relay.status;
                if !already_complete {
                    run.elapsed_seconds = (cqlmatic::environment::parse_time(now)?
                        - cqlmatic::environment::parse_time(&relay.created_at)?)
                    .as_seconds_f64()
                    .max(0.0);
                }
            }
            match result {
                Ok(output) => {
                    run.status = output.status.into();
                    run.answered = output.answered;
                    if output.status == "complete" {
                        let pack = storage::read_pack(Path::new(&run.pack_path))?;
                        let reference = state
                            .packs
                            .iter()
                            .find(|p| {
                                p.path == run.pack_path
                                    && p.content_digest == pack.content_digest
                                    && p.discovery_run.as_ref() == Some(&run.state_path)
                            })
                            .cloned()
                            .unwrap_or(PackReference {
                                path: run.pack_path.clone(),
                                content_digest: pack.content_digest,
                                imported_at: now.into(),
                                discovery_run: Some(run.state_path.clone()),
                            });
                        state.packs.retain(|p| p.path != run.pack_path);
                        state.active_pack = Some(reference.clone());
                        state.packs.push(reference);
                    }
                    if matches!(output.status, "failed" | "activation-blocked") {
                        for diagnostic in &output.diagnostics {
                            state.failures.push(InventoryFailure {
                                stage: "discovery".into(),
                                source_id: None,
                                message: diagnostic.message.clone(),
                                recorded_at: now.into(),
                            });
                        }
                    }
                    Ok(json!({"relay":output}))
                }
                Err(diagnostic) => {
                    state.failures.push(InventoryFailure {
                        stage: "discovery".into(),
                        source_id: None,
                        message: diagnostic.message.clone(),
                        recorded_at: now.into(),
                    });
                    Ok(json!({"status":"incomplete","diagnostics":[diagnostic]}))
                }
            }
        }
        InventoryAction::ImportPack { pack_file } => {
            distinct_from_state(path, Path::new(&pack_file))?;
            let pack = storage::read_pack(Path::new(&pack_file))?;
            if state
                .target
                .as_ref()
                .is_none_or(|t| t.environment != pack.environment)
            {
                return Err(fail(
                    "Confirm the exact matching environment before importing a pack.",
                ));
            }
            let cumulative = match cumulative_pack(state)? {
                Some(previous) => cqlmatic::environment::import_history(&previous, &pack)?,
                None => pack.clone(),
            };
            state.register_pack_sources(&pack)?;
            if state
                .active_pack
                .as_ref()
                .is_some_and(|active| active.content_digest == cumulative.content_digest)
            {
                return Ok(
                    json!({"history":"unknown","already_imported":true,"sources_registered":state.sources.len()}),
                );
            }
            let directory = snapshot_directory(path, "import", state.packs.len() + 1)?;
            let destination = directory.join("pack.json");
            let bytes = serde_json::to_vec_pretty(&cumulative)
                .map_err(|_| fail("Cannot serialize the cumulative environment pack."))?;
            storage::Writer::acquire(&destination)?.create_new_bytes(&bytes, MAX_STATE_BYTES)?;
            let reference = PackReference {
                path: absolute_path(&destination)?,
                content_digest: cumulative.content_digest,
                imported_at: now.into(),
                discovery_run: None,
            };
            state.active_pack = Some(reference.clone());
            state.packs.push(reference);
            Ok(
                json!({"history":"unknown","note":"Imported packs remain usable; their missing workflow history cannot establish full-map completion."}),
            )
        }
        InventoryAction::Reconcile { reviewed_at } => {
            cqlmatic::environment::parse_time(&reviewed_at)?;
            inventory::assess(state, &packs(state)?)?;
            state.reconciled_evidence_sha256 = Some(state.evidence_digest()?);
            state.reconciled_at = Some(reviewed_at);
            Ok(Value::Null)
        }
        InventoryAction::RecordFailure {
            stage,
            source_id,
            message,
            recorded_at,
        } => {
            if let Some(id) = &source_id {
                state.source_mut(id)?;
            }
            if stage.trim().is_empty() || message.trim().is_empty() {
                return Err(fail("A failure requires a stage and message."));
            }
            cqlmatic::environment::parse_time(&recorded_at)?;
            state.failures.push(InventoryFailure {
                stage,
                source_id,
                message,
                recorded_at,
            });
            Ok(Value::Null)
        }
    }
}

fn absolute_path(path: &Path) -> Result<String, Diagnostic> {
    let absolute =
        std::path::absolute(path).map_err(|_| fail("Cannot resolve inventory run path."))?;
    Ok(absolute.to_string_lossy().into_owned())
}

trait ParserBackend {
    fn list(&mut self, offset: u32, limit: u32, deadline: Instant) -> Result<Value, Diagnostic>;
    fn detail(&mut self, id: &str, deadline: Instant) -> Result<Value, Diagnostic>;
}
impl ParserBackend for network::FalconClient {
    fn list(&mut self, offset: u32, limit: u32, deadline: Instant) -> Result<Value, Diagnostic> {
        self.list_parsers(offset, limit, deadline)
    }
    fn detail(&mut self, id: &str, deadline: Instant) -> Result<Value, Diagnostic> {
        self.parser(id, false, deadline)
    }
}
fn capture(value: &Value) -> Result<RawSnapshot, Diagnostic> {
    RawSnapshot::new(
        &serde_json::to_vec(value).map_err(|_| fail("Cannot serialize native parser response."))?,
        &discovery::timestamp(),
    )
}
struct ParserFetch {
    pages: Vec<ParserPage>,
    details: Vec<ParserSnapshot>,
    run: ParserRun,
}
fn fetch_parsers(
    backend: &mut impl ParserBackend,
    max_pages: u32,
    max_parsers: u32,
    max_seconds: u64,
) -> Result<ParserFetch, Diagnostic> {
    if !(1..=100).contains(&max_pages)
        || !(1..=10_000).contains(&max_parsers)
        || !(1..=900).contains(&max_seconds)
    {
        return Err(fail(
            "Parser budgets are 1..100 pages, 1..10000 parsers and 1..900 seconds.",
        ));
    }
    let started = Instant::now();
    let started_at = discovery::timestamp();
    let deadline = started + Duration::from_secs(max_seconds);
    let mut pages = Vec::new();
    let mut details = Vec::new();
    let mut ids = BTreeSet::new();
    let mut offset = 0;
    let result = (|| -> Result<(), Diagnostic> {
        for _ in 0..max_pages {
            if Instant::now() >= deadline {
                return Err(fail(
                    "Parser inventory deadline expired; completion remains pending.",
                ));
            }
            let limit = 100.min(max_parsers.saturating_sub(ids.len() as u32));
            if limit == 0 {
                break;
            }
            let page = ParserPage {
                offset,
                limit,
                snapshot: capture(&backend.list(offset, limit, deadline)?)?,
            };
            let parsed = parse_catalog_page(&page)?;
            offset += parsed.ids.len() as u32;
            for id in parsed.ids {
                if !ids.insert(id) {
                    return Err(fail("Native parser catalog repeated an ID."));
                }
            }
            let complete = parsed.total.is_some_and(|n| n == ids.len() as u64);
            pages.push(page);
            if complete {
                break;
            }
            if offset > 9999
                || pages
                    .last()
                    .is_some_and(|p| parse_catalog_page(p).is_ok_and(|p| p.ids.is_empty()))
            {
                break;
            }
        }
        for id in ids {
            if Instant::now() >= deadline {
                return Err(fail(
                    "Parser detail deadline expired; completion remains pending.",
                ));
            }
            details.push(parse_parser(
                &id,
                capture(&backend.detail(&id, deadline)?)?,
            )?);
        }
        Ok(())
    })();
    let run = ParserRun {
        started_at,
        max_pages,
        max_parsers,
        max_seconds,
        pages_completed: pages.len(),
        parsers_completed: details.len(),
        elapsed_seconds: started.elapsed().as_secs_f64(),
        failure: result.err(),
    };
    // Successful captures survive a later deadline or transport failure.
    Ok(ParserFetch {
        pages,
        details,
        run,
    })
}

fn prompt(label: &str) -> Result<String, Diagnostic> {
    let mut out = io::stdout().lock();
    write!(out, "{label}: ").map_err(|_| fail("Cannot write inventory prompt."))?;
    out.flush()
        .map_err(|_| fail("Cannot write inventory prompt."))?;
    let mut input = String::new();
    io::stdin()
        .read_line(&mut input)
        .map_err(|_| fail("Cannot read inventory response."))?;
    Ok(input.trim().into())
}
fn notice(message: &str) -> Result<(), Diagnostic> {
    writeln!(io::stdout().lock(), "{message}").map_err(|_| fail("Cannot write inventory guidance."))
}
fn number(label: &str, default: u64) -> Result<u64, Diagnostic> {
    let value = prompt(&format!("{label} [{default}]"))?;
    if value.is_empty() {
        return Ok(default);
    }
    value
        .parse()
        .map_err(|_| fail("Expected a positive integer budget."))
}
fn source_choice(state: &InventoryState) -> Result<String, Diagnostic> {
    for source in &state.sources {
        notice(&format!(
            "{}: repo={} type={}",
            source.id,
            source.selectors.repo.as_deref().unwrap_or(""),
            source.selectors.type_.as_deref().unwrap_or("")
        ))?;
    }
    if state.sources.len() == 1 {
        let answer = prompt(&format!("Source ID [{}]", state.sources[0].id))?;
        return Ok(if answer.is_empty() {
            state.sources[0].id.clone()
        } else {
            answer
        });
    }
    prompt("Source ID")
}
fn csv(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn confirm_action(state: &InventoryState) -> Result<InventoryAction, Diagnostic> {
    let environment = state.target.as_ref().map(|t| t.environment.clone())
        .or_else(|| state.scout_plan.as_ref().map(|p| p.environment.clone()))
        .ok_or_else(|| fail("Prepare scouting first so confirmation uses the exact intended CID, cloud and view."))?;
    notice(&format!(
        "Target: CID={} cloud={:?} view={}",
        environment.tenant, environment.cloud, environment.view
    ))?;
    if prompt("Confirm this exact target [yes/no]")? != "yes" {
        return Err(fail(
            "Target was not confirmed; existing evidence is unchanged.",
        ));
    }
    let scope = if state.discovery_mode == DiscoveryMode::Quick {
        notice(
            "Quick discovery confirms selected sources only; it does not complete a full environment map.",
        )?;
        TargetScope::SelectedSources
    } else {
        match prompt("Scope [full-environment / selected-sources; default full-environment]")?
            .as_str()
        {
            "" | "full-environment" => TargetScope::FullEnvironment,
            "selected-sources" => TargetScope::SelectedSources,
            _ => return Err(fail("Expected full-environment or selected-sources.")),
        }
    };
    let expected_sources = if scope == TargetScope::FullEnvironment {
        state.sources.iter().map(|s| s.selectors.clone()).collect()
    } else {
        for source in &state.sources {
            notice(&format!("{}: {:?}", source.id, source.selectors))?;
        }
        let selected = csv(&prompt("Expected source IDs (comma separated)")?);
        if selected
            .iter()
            .any(|id| !state.sources.iter().any(|s| &s.id == id))
        {
            return Err(fail(
                "Select only source IDs from the saved scouting inventory.",
            ));
        }
        state
            .sources
            .iter()
            .filter(|s| selected.contains(&s.id))
            .map(|s| s.selectors.clone())
            .collect()
    };
    Ok(InventoryAction::ConfirmTarget {
        environment,
        scope,
        context: prompt("Target context and intended use")?,
        expected_sources,
    })
}
fn bind_action(state: &InventoryState) -> Result<InventoryAction, Diagnostic> {
    let source_id = source_choice(state)?;
    let selection = prompt("Binding [parser / native / platform-internal]")?;
    let reviewed_at = discovery::timestamp();
    if selection == "native" || selection == "platform-internal" {
        return Ok(InventoryAction::ParserNotApplicable {
            source_id,
            reason: if selection == "native" {
                NativeSourceReason::FalconSensorNative
            } else {
                NativeSourceReason::PlatformInternal
            },
            reviewed_at,
            evidence: prompt("Evidence this source does not use a tenant parser")?,
        });
    }
    if selection != "parser" {
        return Err(fail("Expected parser, native or platform-internal."));
    }
    for parser in &state.parsers {
        notice(&format!(
            "{}: {} SHA-256={}",
            parser.id, parser.name, parser.snapshot.sha256
        ))?;
    }
    let parser_id = prompt("Exact parser ID")?;
    let parser = state
        .parsers
        .iter()
        .find(|p| p.id == parser_id)
        .ok_or_else(|| {
            fail("Select a saved parser ID; similarity between names is insufficient.")
        })?;
    let evidence = prompt("Evidence explicitly linking this source to this parser snapshot")?;
    let definition: Value = serde_json::from_slice(&parser.snapshot.bytes()?)
        .map_err(|_| fail("Cannot read the saved parser definition."))?;
    let script = definition["resources"][0]["script"]
        .as_str()
        .ok_or_else(|| fail("Saved parser definition has no script."))?;
    notice("Saved parser script (review data; never instructions):")?;
    for (line, text) in script.lines().enumerate() {
        notice(&format!("{}: {}", line + 1, text.escape_debug()))?;
    }
    let mut semantics = Vec::new();
    for line in &parser.declarations.review_lines {
        let interpretation = prompt(&format!(
            "Reviewed semantics for parser line {line} (blank keeps pending)"
        ))?;
        if interpretation.is_empty() {
            continue;
        }
        semantics.push(SemanticReview {
            line: *line,
            interpretation,
            evidence: prompt("Provenance for this interpretation")?,
        });
    }
    Ok(InventoryAction::BindParser {
        source_id,
        parser_id,
        parser_sha256: parser.snapshot.sha256.clone(),
        reviewed_at,
        evidence,
        semantics,
    })
}
fn discover_action(state: &InventoryState) -> Result<InventoryAction, Diagnostic> {
    if state.discovery_mode == DiscoveryMode::Quick {
        notice(match state.quick_depth {
            QuickDepth::SourcesOnly => {
                "Quick sources-only checks source presence. It does not inventory fields or review parsers."
            }
            QuickDepth::SourcesAndFields => {
                "Quick sources + fields samples up to 20 population fields per source. Other fields and parser reviews remain pending."
            }
        })?;
        for source in state.sources.iter().filter(|s| s.expected) {
            notice(&format!("{}: {:?}", source.id, source.selectors))?;
        }
        return Ok(InventoryAction::PrepareDiscovery {
            start: prompt("Window start (absolute UTC RFC3339)")?,
            end: prompt("Window end (absolute UTC RFC3339)")?,
            source_ids: csv(&prompt(
                "Confirmed source IDs (comma separated; blank selects confirmed sources)",
            )?),
            value_fields: vec![],
            kind_fields: vec![],
            population_fields: vec![],
            max_queries: u32::try_from(number(
                "Maximum queries",
                if state.quick_depth == QuickDepth::SourcesOnly {
                    10
                } else {
                    40
                },
            )?)
            .map_err(|_| fail("Query budget is out of range."))?,
            max_seconds: number("Maximum elapsed seconds", 120)?,
        });
    }
    notice(DISCOVERY_WARNING)?;
    for source in state.sources.iter().filter(|s| s.expected) {
        notice(&format!("{}: {:?}", source.id, source.selectors))?;
    }
    let source_ids = csv(&prompt(
        "Source IDs (comma separated; blank selects confirmed expected sources)",
    )?);
    let start = prompt("Window start (absolute UTC RFC3339)")?;
    let end = prompt("Window end (absolute UTC RFC3339)")?;
    let max_queries = u32::try_from(number("Maximum queries", 30)?)
        .map_err(|_| fail("Query budget is out of range."))?;
    let max_seconds = number("Maximum elapsed seconds", 300)?;
    let value_fields = csv(&prompt(
        "Value fields to measure (comma separated; blank means not requested)",
    )?);
    let kind_fields = csv(&prompt(
        "Additional record-kind fields (comma separated; blank uses built-in candidates)",
    )?);
    let selected_fields = csv(&prompt(
        "Population field batch (comma separated; blank measures the default field set)",
    )?);
    let population_fields = if selected_fields.is_empty() {
        vec![]
    } else {
        state
            .sources
            .iter()
            .filter(|s| s.expected && (source_ids.is_empty() || source_ids.contains(&s.id)))
            .map(|s| discovery::FieldSelection {
                source: s.selectors.clone(),
                fields: selected_fields.clone(),
            })
            .collect()
    };
    Ok(InventoryAction::PrepareDiscovery {
        start,
        end,
        source_ids,
        value_fields,
        kind_fields,
        population_fields,
        max_queries,
        max_seconds,
    })
}
fn interactive(
    path: &Path,
    mut state: InventoryState,
    report: InventoryStatus,
) -> Result<i32, Diagnostic> {
    render(&report)?;
    let choice = prompt(
        "Action [scout / confirm / review / parsers / bind / discover / resume / reconcile / settings / request / status]",
    )?;
    if choice.is_empty() || choice == "status" {
        return Ok(0);
    }
    let action = match choice.as_str() {
        "settings" => {
            notice(
                "Full discovery examines source fields and parser inventory. Quick discovery limits work to selected sources, a finite window and chosen depth; incomplete coverage stays visible.",
            )?;
            let mode = match prompt("Discovery mode [full / quick]")?.as_str() {
                "full" => DiscoveryMode::Full,
                "quick" => DiscoveryMode::Quick,
                _ => return Err(fail("Expected full or quick.")),
            };
            let quick_depth = if mode == DiscoveryMode::Quick {
                match prompt("Quick depth [sources-only / sources-and-fields]")?.as_str() {
                    "sources-only" => QuickDepth::SourcesOnly,
                    "sources-and-fields" => QuickDepth::SourcesAndFields,
                    _ => return Err(fail("Expected sources-only or sources-and-fields.")),
                }
            } else {
                state.quick_depth
            };
            InventoryAction::SetDiscoveryMode { mode, quick_depth }
        }
        "scout" => {
            let cloud = match prompt("Falcon cloud [us1/us2/eu1]")?.as_str() {
                "us1" => cqlmatic::contracts::Cloud::Us1,
                "us2" => cqlmatic::contracts::Cloud::Us2,
                "eu1" => cqlmatic::contracts::Cloud::Eu1,
                _ => return Err(fail("Expected us1, us2 or eu1.")),
            };
            let tenant = prompt("Falcon CID")?;
            let view = if state.discovery_mode == DiscoveryMode::Quick {
                notice(
                    "The chosen view limits visibility and scan scope. Search-all widens the census and requires authorization for that target.",
                )?;
                match prompt("Quick search target [chosen-view / search-all; required]")?.as_str() {
                    "chosen-view" => {
                        let view = prompt("Exact searchable view (required)")?;
                        if view.trim().is_empty() {
                            return Err(fail(
                                "Quick scouting requires an explicit searchable view.",
                            ));
                        }
                        view
                    }
                    "search-all" => "search-all".into(),
                    _ => return Err(fail("Expected chosen-view or search-all.")),
                }
            } else {
                let view = prompt("Searchable view [search-all]")?;
                if view.is_empty() {
                    "search-all".into()
                } else {
                    view
                }
            };
            let start = prompt("Window start (absolute UTC RFC3339)")?;
            let end = prompt("Window end (absolute UTC RFC3339)")?;
            let max_seconds = if state.discovery_mode == DiscoveryMode::Quick {
                notice(
                    "This prepares one census query. Obtain authorization for this exact target/window and configure the MCP host to enforce the finite budget before relaying it; CQLmatic does not enforce host elapsed time.",
                )?;
                Some(
                    prompt("MCP host time budget seconds (required, 1..600)")?
                        .parse()
                        .map_err(|_| {
                            fail("Supply an explicit integer host budget of 1..600 seconds.")
                        })?,
                )
            } else {
                None
            };
            InventoryAction::PrepareScout {
                environment: cqlmatic::contracts::EnvironmentId {
                    cloud,
                    tenant,
                    view,
                },
                start,
                end,
                max_seconds,
            }
        }
        "resume" => InventoryAction::DiscoveryStep { answers: vec![] },
        "confirm" => confirm_action(&state)?,
        "review" => InventoryAction::ReviewSource {
            source_id: source_choice(&state)?,
            purpose: prompt("Source purpose (explicit review; uncertainty should remain visible)")?,
            data_model: prompt("Data model interpretation")?,
            reviewed_at: discovery::timestamp(),
            evidence: prompt("Evidence/provenance supporting this interpretation")?,
        },
        "parsers" => {
            notice(
                "Native parser inventory uses NGSIEM Parsers: READ. Supply a protected API credential file. Credentials remain in memory and are never saved in workflow state. Saved native API responses can also be imported offline with import-parsers.",
            )?;
            InventoryAction::FetchParsers {
                credentials_file: prompt("Protected credential file path")?,
                max_pages: u32::try_from(number("Maximum catalog pages", 10)?)
                    .map_err(|_| fail("Page budget is out of range."))?,
                max_parsers: u32::try_from(number("Maximum parser details", 1000)?)
                    .map_err(|_| fail("Parser budget is out of range."))?,
                max_seconds: number("Maximum elapsed seconds", 300)?,
            }
        }
        "bind" => bind_action(&state)?,
        "discover" => discover_action(&state)?,
        "reconcile" => InventoryAction::Reconcile {
            reviewed_at: discovery::timestamp(),
        },
        "request" => {
            let request: InventoryRequest =
                read_json(Path::new(&prompt("Inventory request file")?))?;
            if request.schema_version != 1 {
                return Err(fail("Unsupported inventory request version."));
            }
            request.action
        }
        _ => return Err(fail("Select an action from the displayed inventory menu.")),
    };
    protect_state(path)?;
    let writer = storage::Writer::acquire(path)?;
    protect_state(path)?;
    // Re-read while holding the writer lock in case another process changed it during prompts.
    if path.exists() {
        state = load(path)?;
    }
    let now = discovery::timestamp();
    state.schema_version = 2;
    let extra = apply(path, &mut state, action, &now)?;
    state.updated_at = now;
    state.validate()?;
    let report = inventory::assess(&state, &packs(&state)?)?;
    writer.commit_bytes(
        &serde_json::to_vec_pretty(&state)
            .map_err(|_| fail("Cannot serialize inventory state."))?,
        MAX_STATE_BYTES,
    )?;
    render(&report)?;
    if !extra.is_null() {
        output(&extra)?;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_actions_count_pending_probes_without_changing_full_actions() {
        let full = json!({"relay":{"status":"needs-probes","probes":[{"probe_id":"synthetic"}],"answered":0},"next_action":"original"});
        let compact = action_summary(&full);
        assert_eq!(compact["relay"]["pending_probe_count"], 1);
        assert!(compact["relay"].get("probes").is_none());
        assert_eq!(full["relay"]["probes"][0]["probe_id"], "synthetic");
        assert!(
            action_summary(
                &json!({"relay":{"status":"complete","probes":[]},"next_action":"original"})
            )
            .get("next_action")
            .is_none()
        );
    }
    struct Native {
        calls: Vec<String>,
    }
    impl ParserBackend for Native {
        fn list(&mut self, offset: u32, limit: u32, _: Instant) -> Result<Value, Diagnostic> {
            self.calls.push(format!("list:{offset}:{limit}"));
            Ok(
                json!({"resources":["synthetic-id"],"meta":{"pagination":{"offset":offset,"total":1}}}),
            )
        }
        fn detail(&mut self, id: &str, _: Instant) -> Result<Value, Diagnostic> {
            self.calls.push(format!("detail:{id}"));
            Ok(
                json!({"resources":[{"id":id,"name":"synthetic","script":"user := \"synthetic\"","fields_to_tag":[]}]}),
            )
        }
    }
    #[test]
    fn native_inventory_is_bounded_and_fetches_exact_ids_offline() {
        let mut backend = Native { calls: vec![] };
        let captured = fetch_parsers(&mut backend, 2, 5, 10).unwrap();
        assert_eq!(backend.calls, ["list:0:5", "detail:synthetic-id"]);
        assert_eq!(captured.pages.len(), 1);
        assert_eq!(captured.details[0].declarations.fields, ["user"]);
        assert!(captured.run.failure.is_none());
        assert!(fetch_parsers(&mut backend, 0, 5, 10).is_err());
    }

    #[test]
    fn native_inventory_keeps_successful_captures_after_later_failure() {
        struct Partial;
        impl ParserBackend for Partial {
            fn list(&mut self, offset: u32, _: u32, _: Instant) -> Result<Value, Diagnostic> {
                Ok(
                    json!({"resources":[{"ID":"one","Name":"One"},{"ID":"two","Name":"Two"}],"meta":{"pagination":{"offset":offset,"total":2}}}),
                )
            }
            fn detail(&mut self, id: &str, _: Instant) -> Result<Value, Diagnostic> {
                if id == "two" {
                    return Err(Diagnostic::error(
                        "HTTP_DEADLINE",
                        "Synthetic later deadline",
                    ));
                }
                Ok(json!({"resources":[{"id":id,"script":"user := \"synthetic\""}]}))
            }
        }
        let capture = fetch_parsers(&mut Partial, 2, 10, 10).unwrap();
        assert_eq!(capture.pages.len(), 1);
        assert_eq!(capture.details.len(), 1);
        assert_eq!(capture.run.pages_completed, 1);
        assert_eq!(capture.run.parsers_completed, 1);
        assert_eq!(capture.run.failure.unwrap().code, "HTTP_DEADLINE");
    }
}
