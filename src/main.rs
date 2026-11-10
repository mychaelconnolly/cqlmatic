#![deny(unsafe_code)]
mod inventory_cli;
mod setup_cli;
use clap::{Args, Parser, Subcommand, ValueEnum};
use cqlmatic::{
    app,
    assess::{self, AssessOutcome, AssessRequest, AssessResult},
    catalog,
    contracts::*,
    discovery,
    environment::{self, Pack},
    inventory::{self, DiscoveryMode, InventoryState, UnverifiedFieldLead},
    mcp, network, sha256_hex, storage,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, BufRead, Read, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    name = "cqlmatic",
    version,
    about = "Generate, validate, and lint CrowdStrike CQL locally.",
    long_about = "A native, deterministic CQL toolkit. Generation, checking, catalog lookup and pack inspection are offline. Discovery executes bounded authenticated searches only with --execute. No internal model calls."
)]
struct Cli {
    #[arg(
        long,
        value_enum,
        help = "Print a bundled machine-readable contract and exit"
    )]
    schema: Option<Schema>,
    #[command(subcommand)]
    command: Option<Command>,
}
#[derive(Clone, Copy, ValueEnum)]
enum Schema {
    Generate,
    Check,
    Pack,
    Discovery,
    Result,
    Assess,
    Setup,
    Inventory,
    Status,
}
#[derive(Clone, Copy, ValueEnum)]
enum Standard {
    Cps,
    Ecs,
}
#[derive(Subcommand)]
enum Command {
    /// Configure a supported MCP client through a resumable local setup flow.
    Setup(setup_cli::SetupArgs),
    /// Show the offline environment discovery checklist and next actions.
    Status(inventory_cli::StatusArgs),
    /// Generate through a recipe, typed plan, or bounded English request.
    Generate(GenerateArgs),
    /// Validate and lint existing CQL within the bundled coverage profile.
    Check(CheckArgs),
    /// Assess a saved MCP search result offline (exit 0 no findings, 1 advisory/inconclusive, 2 error).
    Assess(AssessArgs),
    /// Retrieve focused knowledge, function contracts, recipes, and coverage.
    Catalog(CatalogArgs),
    /// Import, inspect, or explicitly discover an environment pack.
    Env {
        #[command(subcommand)]
        command: EnvCommand,
    },
}
#[derive(Args)]
struct Common {
    #[arg(long, help = "Emit compact structured results")]
    json: bool,
    #[arg(
        long,
        help = "Read one versioned request per stdin line; continue on record errors"
    )]
    jsonl: bool,
    #[arg(
        long,
        help = "Optional local environment snapshot; never triggers discovery"
    )]
    pack: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Saved inventory state; defaults to the local inventory state"
    )]
    inventory_state: Option<PathBuf>,
    #[arg(
        long,
        help = "Require current observed pack evidence for source fields"
    )]
    require_observed: bool,
    #[arg(
        long,
        help = "Accept positive current partial source/field observations with a coverage warning"
    )]
    accept_partial_coverage: bool,
    #[arg(
        long,
        help = "Return exit 1 for advisory warnings as well as unsuccessful checks"
    )]
    fail_on_warning: bool,
    #[arg(
        long,
        help = "Accept mixed record kinds with an ambiguity warning instead of requiring context"
    )]
    accept_mixed_record_kinds: bool,
}
#[derive(Args)]
struct GenerateArgs {
    #[command(flatten)]
    common: Common,
    #[arg(
        long,
        value_name = "FILE",
        help = "Full v1 GenerateRequest JSON; '-' reads stdin"
    )]
    request: Option<PathBuf>,
    #[arg(long, value_name = "FILE", help = "Typed Plan JSON; '-' reads stdin")]
    plan: Option<PathBuf>,
    #[arg(long, help = "Recipe ID; use catalog --recipes for choices")]
    recipe: Option<String>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Recipe parameter JSON, used with --recipe"
    )]
    params: Option<PathBuf>,
    #[arg(
        long,
        help = "Bounded English; quote values and consume the full request"
    )]
    intent: Option<String>,
    #[arg(long, help = "Literal repository selector for --intent")]
    repo: Option<String>,
    #[arg(long = "type", help = "Literal #type selector for --intent")]
    type_: Option<String>,
    #[arg(long, help = "Literal #event_simpleName selector for --intent")]
    event: Option<String>,
}
#[derive(Args)]
struct CheckArgs {
    #[command(flatten)]
    common: Common,
    #[arg(value_name = "QUERY", help = "CQL text, or '-' to read stdin")]
    query: Option<String>,
    #[arg(long, value_name = "FILE", help = "Read CQL from a file")]
    file: Option<PathBuf>,
    #[arg(long, value_name = "FILE", help = "Read a full v1 CheckRequest JSON")]
    request: Option<PathBuf>,
}
#[derive(Args)]
struct AssessArgs {
    #[arg(long, help = "Emit compact structured results")]
    json: bool,
    #[arg(
        long,
        help = "Read one v1 AssessRequest per stdin line; continue on record errors"
    )]
    jsonl: bool,
    #[arg(
        long,
        help = "Optional local environment snapshot; never triggers discovery"
    )]
    pack: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Saved inventory state; defaults to the local inventory state"
    )]
    inventory_state: Option<PathBuf>,
    #[arg(long, value_name = "FILE", help = "Full v1 AssessRequest JSON")]
    request: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Read the assessed CQL query from a file"
    )]
    query_file: Option<PathBuf>,
    #[arg(
        long,
        value_name = "FILE",
        help = "Read the saved MCP search result JSON"
    )]
    result: Option<PathBuf>,
}
#[derive(Args)]
struct CatalogArgs {
    #[arg(default_value = "", help = "Record ID or search text")]
    term: String,
    #[arg(long)]
    functions: bool,
    #[arg(long)]
    recipes: bool,
    #[arg(long)]
    profile: bool,
    #[arg(
        long,
        value_enum,
        help = "Find standard field names; these are predictions, not tenant observations"
    )]
    standard: Option<Standard>,
    #[arg(
        long,
        help = "Include full knowledge records instead of the compact index"
    )]
    full: bool,
    #[arg(long,default_value_t=10,value_parser=clap::value_parser!(u32).range(1..=100))]
    limit: u32,
}
#[derive(Subcommand)]
enum EnvCommand {
    /// Build and reconcile the environment reference map with explicit evidence.
    Inventory(inventory_cli::InventoryArgs),
    /// Derive a local API-client account binding without authenticating or exposing keys.
    Identity {
        #[arg(long, value_parser = parse_cloud)]
        cloud: Cloud,
        #[arg(long)]
        view: String,
        #[arg(long)]
        credentials_file: Option<PathBuf>,
    },
    /// Read a snapshot, never credentials or the network.
    Inspect {
        file: PathBuf,
        #[arg(
            long,
            help = "Emit compact source and count overview; use detail flags to narrow"
        )]
        summary: bool,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        field: Option<String>,
        #[arg(long)]
        source: Option<String>,
        #[arg(long,default_value_t=20,value_parser=clap::value_parser!(u32).range(1..=100))]
        limit: u32,
    },
    /// Validate and import native JSON or pinned legacy JSONL; source files are untouched.
    Import {
        #[arg(long)]
        from: PathBuf,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, help = "EnvironmentId JSON; required for legacy input")]
        environment: Option<PathBuf>,
        #[arg(long, help = "Explicitly replace an existing output snapshot")]
        replace: bool,
    },
    /// Print a plan, run API searches with --execute, or relay MCP probes with --mcp (exit 3 while pending).
    Discover {
        #[arg(long)]
        request: Option<PathBuf>,
        #[arg(long)]
        execute: bool,
        #[arg(
            long,
            help = "Credential-free agent-relayed discovery; exit 3 needs probes or activation retry"
        )]
        mcp: bool,
        #[arg(
            long,
            help = "New state path at init, existing state path at subsequent steps"
        )]
        state: Option<PathBuf>,
        #[arg(
            long = "answer",
            value_name = "PROBE_ID=FILE",
            help = "Saved MCP tool result for an emitted probe; repeatable"
        )]
        answers: Vec<String>,
        #[arg(long)]
        pack: Option<PathBuf>,
        #[arg(long)]
        credentials_file: Option<PathBuf>,
        #[arg(
            long,
            help = "New protected local receipt path, required with --execute"
        )]
        receipt: Option<PathBuf>,
    },
    /// Read parser metadata using the explicit environment; additional parser-read scope may be needed.
    Parsers {
        #[arg(long)]
        environment: PathBuf,
        #[arg(long)]
        credentials_file: Option<PathBuf>,
        #[arg(long)]
        id: Option<String>,
        #[arg(long)]
        template: bool,
        #[arg(long, default_value_t = 0)]
        offset: u32,
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
}

fn input_error(message: &str) -> Diagnostic {
    Diagnostic::error("CLI_INPUT", message)
}
fn parse_cloud(s: &str) -> Result<Cloud, String> {
    match s {
        "us1" => Ok(Cloud::Us1),
        "us2" => Ok(Cloud::Us2),
        "eu1" => Ok(Cloud::Eu1),
        _ => Err("Choose us1, us2 or eu1.".into()),
    }
}
fn bytes(path: &Path, limit: usize) -> Result<Vec<u8>, Diagnostic> {
    if path == Path::new("-") {
        let mut bytes = Vec::new();
        io::stdin()
            .lock()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| input_error("Cannot read stdin."))?;
        if bytes.len() > limit {
            return Err(input_error("Input byte budget exceeded."));
        }
        Ok(bytes)
    } else {
        storage::read_bytes(path, limit as u64)
    }
}
fn json_file<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, Diagnostic> {
    serde_json::from_slice(&bytes(path, MAX_REQUEST_BYTES)?)
        .map_err(|e| Diagnostic::error("REQUEST_JSON", format!("Invalid request: {e}")))
}
fn output(value: &impl serde::Serialize, pretty: bool) -> Result<(), Diagnostic> {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    if pretty {
        serde_json::to_writer_pretty(&mut out, value)
    } else {
        serde_json::to_writer(&mut out, value)
    }
    .map_err(|_| Diagnostic::error("OUTPUT_IO", "Cannot write structured output."))?;
    out.write_all(b"\n")
        .and_then(|_| out.flush())
        .map_err(|_| Diagnostic::error("OUTPUT_IO", "Cannot finish structured output."))
}
fn human(text: &str) -> String {
    let mut safe = String::new();
    for c in text.chars() {
        if c.is_control() {
            safe.extend(c.escape_default());
        } else {
            safe.push(c);
        }
    }
    safe
}
fn line(text: &str) -> Result<(), Diagnostic> {
    let mut out = io::stdout().lock();
    writeln!(out, "{text}")
        .and_then(|_| out.flush())
        .map_err(|_| Diagnostic::error("OUTPUT_IO", "Cannot write output."))
}
fn persist_discovery(file: std::fs::File, report: &mut discovery::DiscoverReport) {
    if let Err(d) = storage::finish_receipt(file, report) {
        report.status = "error".into();
        report.diagnostics.push(d);
    }
}
fn import_report(path: &Path, pack_id: &str, error: Option<&Diagnostic>) -> Value {
    match error {
        None => {
            json!({"status":"imported","path":path,"pack_id":pack_id,"source_files_modified":false})
        }
        Some(d) => {
            json!({"status":"error","path":path,"pack_id":pack_id,"source_files_modified":false,"active_pack_updated":true,"durability_confirmed":false,"diagnostics":[d]})
        }
    }
}
fn import_commit_result(
    path: &Path,
    pack_id: &str,
    committed: Result<(), Diagnostic>,
) -> Result<(Value, i32), Diagnostic> {
    match committed {
        Ok(()) => Ok((import_report(path, pack_id, None), 0)),
        Err(d) if d.code == "PACK_DURABILITY" => Ok((import_report(path, pack_id, Some(&d)), 2)),
        Err(d) => Err(d),
    }
}
fn reserved_preflight<T>(
    file: std::fs::File,
    path: &Path,
    preflight: impl FnOnce() -> Result<T, Diagnostic>,
) -> Result<(std::fs::File, T), Diagnostic> {
    match preflight() {
        Ok(value) => Ok((file, value)),
        Err(d) => {
            drop(file);
            std::fs::remove_file(path).map_err(|_| {
                Diagnostic::error(
                    "LOCAL_IO",
                    "Cannot remove empty receipt after discovery preflight failure.",
                )
            })?;
            Err(d)
        }
    }
}
fn emit_analysis(a: &Analysis, common: &Common, generated: bool) -> Result<i32, Diagnostic> {
    if common.json || common.jsonl {
        output(a, false)?;
    } else {
        if let Some(query) = &a.query {
            line(query)?;
        } else if !generated {
            line(&format!(
                "{} ({})",
                match a.outcome {
                    Outcome::Ready => "accepted",
                    Outcome::NeedsContext => "needs context",
                    Outcome::Unsupported => "unsupported",
                    Outcome::Invalid => "invalid",
                    Outcome::Error => "error",
                },
                a.validation.profile_id
            ))?;
        }
        for d in &a.diagnostics {
            writeln!(
                io::stderr().lock(),
                "{}: {}",
                human(&d.code),
                human(&d.message)
            )
            .map_err(|_| Diagnostic::error("OUTPUT_IO", "Cannot write diagnostics."))?;
        }
        for (label, fields) in [
            ("missing context", &a.grounding.missing),
            ("historical evidence", &a.grounding.stale),
        ] {
            for f in fields {
                writeln!(
                    io::stderr().lock(),
                    "{label}: {}{}",
                    human(f),
                    a.grounding
                        .basis
                        .get(f)
                        .map(|basis| format!(": {}", human(basis)))
                        .unwrap_or_default()
                )
                .map_err(|_| Diagnostic::error("OUTPUT_IO", "Cannot write diagnostics."))?;
            }
        }
        for f in &a.grounding.conflicting {
            writeln!(io::stderr().lock(), "conflicting context: {}", human(f))
                .map_err(|_| Diagnostic::error("OUTPUT_IO", "Cannot write diagnostics."))?;
        }
    }
    Ok(a.exit_code(common.fail_on_warning))
}
fn context(mut c: Context, common: &Common) -> Context {
    c.require_observed |= common.require_observed;
    c.accept_mixed_record_kinds |= common.accept_mixed_record_kinds;
    c.accept_partial_coverage |= common.accept_partial_coverage;
    c
}
fn pack(common: &Common) -> Result<Option<Pack>, Diagnostic> {
    common.pack.as_deref().map(storage::read_pack).transpose()
}

struct SavedInventory {
    path: PathBuf,
    state: InventoryState,
    active_pack: Option<Pack>,
}

fn select_inventory(
    selected_path: Option<&Path>,
    context: &mut Context,
    pack: Option<&Pack>,
) -> Result<(Option<SavedInventory>, bool), Diagnostic> {
    let path = if let Some(path) = selected_path {
        std::path::absolute(path)
            .map_err(|_| input_error("Cannot resolve inventory state path."))?
    } else {
        inventory_cli::default_state_path()?
    };
    if !setup_cli::private_input_exists(&path)? {
        return if selected_path.is_some() {
            Err(input_error("Selected inventory state does not exist."))
        } else {
            Ok((None, false))
        };
    }
    let state = match read_inventory_state(&path) {
        Ok(state) => state,
        Err(d) if selected_path.is_none() && pack.is_some() => {
            return Err(Diagnostic::warning(
                "CQL_QUICK_INVENTORY_IGNORED",
                format!(
                    "Default inventory is invalid and was ignored; the explicit pack was used: {}",
                    d.message
                ),
            ));
        }
        Err(d) => return Err(d),
    };
    if state.active_pack.is_none() && !state.packs.is_empty() {
        return Err(input_error(
            "Inventory has retained packs but no active pack for evidence checks.",
        ));
    }
    if state.target.is_none() {
        if !state.unverified_leads.is_empty()
            || !state.query_observations.is_empty()
            || state.scout_plan.is_some()
            || !state.scouting.is_empty()
            || !state.sources.is_empty()
            || !state.parser_pages.is_empty()
            || !state.parsers.is_empty()
            || !state.parser_exclusions.is_empty()
            || !state.packs.is_empty()
            || !state.discovery_runs.is_empty()
            || !state.parser_runs.is_empty()
            || !state.failures.is_empty()
            || state.reconciled_evidence_sha256.is_some()
            || state.reconciled_at.is_some()
        {
            return Err(input_error(
                "Inventory evidence requires a confirmed target.",
            ));
        }
        return Ok((None, false));
    }
    let mut quick = SavedInventory {
        path,
        state,
        active_pack: None,
    };
    let target_applied = apply_inventory_target(context, pack, &quick)?;
    quick.active_pack = load_active_pack(&quick.state)?;
    Ok((Some(quick), target_applied))
}

fn read_inventory_state(path: &Path) -> Result<InventoryState, Diagnostic> {
    let data = storage::read_bytes(path, inventory::MAX_STATE_BYTES)?;
    let state: InventoryState = serde_json::from_slice(&data)
        .map_err(|_| input_error("Inventory state is invalid JSON."))?;
    state.validate()?;
    Ok(state)
}

fn load_active_pack(state: &InventoryState) -> Result<Option<Pack>, Diagnostic> {
    if state.active_pack.is_none() && !state.packs.is_empty() {
        return Err(input_error(
            "Inventory has retained packs but no active pack for evidence checks.",
        ));
    }
    state
        .active_pack
        .as_ref()
        .map(|reference| {
            let file = Path::new(&reference.path);
            if !setup_cli::private_input_exists(file)? {
                return Err(input_error("Active inventory pack does not exist."));
            }
            let pack = storage::read_pack(file)?;
            if pack.content_digest != reference.content_digest
                || state
                    .target
                    .as_ref()
                    .is_none_or(|target| pack.environment != target.environment)
            {
                return Err(input_error(
                    "Active pack differs from its protected inventory reference.",
                ));
            }
            Ok(pack)
        })
        .transpose()
}

fn save_quick_leads(
    quick: &SavedInventory,
    leads: Vec<UnverifiedFieldLead>,
) -> Result<usize, Diagnostic> {
    if leads.is_empty() {
        return Ok(0);
    }
    let parent = quick
        .path
        .parent()
        .ok_or_else(|| input_error("Quick inventory state needs a parent directory."))?;
    setup_cli::protect_directory(parent)?;
    let writer = storage::Writer::acquire(&quick.path)?;
    if !setup_cli::private_input_exists(&quick.path)? {
        return Err(input_error(
            "Quick inventory disappeared before leads could be saved.",
        ));
    }
    let mut current = read_inventory_state(&quick.path)?;
    if current.discovery_mode != DiscoveryMode::Quick {
        return Err(input_error(
            "Quick inventory mode changed before leads could be saved.",
        ));
    }
    if current.target.as_ref().map(|target| &target.environment)
        != quick
            .state
            .target
            .as_ref()
            .map(|target| &target.environment)
    {
        return Err(input_error(
            "Quick inventory target changed before leads could be saved.",
        ));
    }
    load_active_pack(&current)?;
    let before = current.unverified_leads.len();
    current.add_unverified_leads(leads)?;
    let added = current.unverified_leads.len() - before;
    if added == 0 {
        return Ok(0);
    }
    current.schema_version = 2;
    current.updated_at = discovery::timestamp();
    current.validate()?;
    setup_cli::protect_directory(parent)?;
    writer.commit_bytes(
        &serde_json::to_vec_pretty(&current)
            .map_err(|_| input_error("Cannot serialize Quick inventory state."))?,
        inventory::MAX_STATE_BYTES,
    )?;
    Ok(added)
}

fn inventory_info(code: &str, message: impl Into<String>) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(code, message);
    diagnostic.severity = Severity::Info;
    diagnostic
}

fn apply_inventory_target(
    context: &mut Context,
    pack: Option<&Pack>,
    quick: &SavedInventory,
) -> Result<bool, Diagnostic> {
    let target = &quick
        .state
        .target
        .as_ref()
        .expect("Quick inventory has a confirmed target")
        .environment;
    if context.environment.as_ref().is_some_and(|e| e != target)
        || pack.is_some_and(|pack| &pack.environment != target)
    {
        return Err(Diagnostic::warning(
            "CQL_QUICK_TARGET_MISMATCH",
            "Inventory target differs from the explicit request or pack; no inventory evidence or query leads were applied.",
        ));
    }
    if context.environment.is_none() {
        context.environment = Some(target.clone());
        Ok(true)
    } else {
        Ok(false)
    }
}

fn inventory_result(
    mut result: Analysis,
    quick: Option<&SavedInventory>,
    target_applied: bool,
    capture_leads: bool,
    notice: Option<Diagnostic>,
    query: Option<String>,
) -> Analysis {
    if target_applied && let Some(quick) = quick {
        let target = &quick
            .state
            .target
            .as_ref()
            .expect("Quick inventory has a confirmed target")
            .environment;
        result.diagnostics.push(inventory_info(
            "CQL_QUICK_TARGET",
            format!(
                "Confirmed inventory target supplied: {}.",
                serde_json::to_string(target).expect("environment identity serializes")
            ),
        ));
    }
    if let Some(notice) = notice {
        if notice.severity == Severity::Error && result.validation.status == Acceptance::Accepted {
            result.outcome = Outcome::NeedsContext;
            result.query = None;
            result
                .grounding
                .conflicting
                .push("Selected inventory state or active evidence is unavailable.".into());
        }
        result.diagnostics.push(notice);
        return result;
    }
    let Some(quick) = quick.filter(|_| capture_leads) else {
        return result;
    };
    if result.validation.status != Acceptance::Accepted {
        return result;
    }
    let Some(query) = query else {
        return result;
    };
    match app::unverified_leads(&query).and_then(|leads| save_quick_leads(quick, leads)) {
        Ok(0) => {}
        Ok(added) => result.diagnostics.push(inventory_info(
            "CQL_QUICK_LEADS",
            format!("Saved {added} unverified source/field lead(s) in Quick inventory."),
        )),
        Err(d) => result.diagnostics.push(Diagnostic::warning(
            "CQL_QUICK_LEAD_SAVE",
            format!("Quick inventory could not save query leads: {}", d.message),
        )),
    }
    result
}

fn inventory_notice(diagnostic: Diagnostic) -> Diagnostic {
    if diagnostic.severity == Severity::Warning {
        diagnostic
    } else {
        Diagnostic::error(
            "CQL_QUICK_INVENTORY",
            format!("Inventory unavailable: {}", diagnostic.message),
        )
    }
}

fn analyze_generate(
    mut request: GenerateRequest,
    common: &Common,
    pack: Option<&Pack>,
) -> Analysis {
    request.context = context(request.context, common);
    let (quick, target_applied, notice) = match select_inventory(
        common.inventory_state.as_deref(),
        &mut request.context,
        pack,
    ) {
        Ok((quick, applied)) => (quick, applied, None),
        Err(d) => (None, false, Some(inventory_notice(d))),
    };
    let evidence = quick.as_ref().map(|quick| app::GroundingEvidence {
        environment: &quick
            .state
            .target
            .as_ref()
            .expect("confirmed target")
            .environment,
        active_pack: quick.active_pack.as_ref(),
        query_observations: &quick.state.query_observations,
    });
    let primary = pack.or_else(|| quick.as_ref().and_then(|quick| quick.active_pack.as_ref()));
    let result = app::generate_with_evidence(request, primary, evidence);
    let query = result.query.clone().or_else(|| {
        result.interpretation.plan.as_ref().and_then(|plan| {
            cqlmatic::language::plan::lower(plan)
                .ok()
                .map(|pipeline| pipeline.render())
        })
    });
    inventory_result(
        result,
        quick.as_ref(),
        target_applied,
        quick
            .as_ref()
            .is_some_and(|quick| quick.state.discovery_mode == DiscoveryMode::Quick),
        notice,
        query,
    )
}

fn analyze_check(mut request: CheckRequest, common: &Common, pack: Option<&Pack>) -> Analysis {
    request.context = context(request.context, common);
    let (quick, target_applied, notice) = match select_inventory(
        common.inventory_state.as_deref(),
        &mut request.context,
        pack,
    ) {
        Ok((quick, applied)) => (quick, applied, None),
        Err(d) => (None, false, Some(inventory_notice(d))),
    };
    let evidence = quick.as_ref().map(|quick| app::GroundingEvidence {
        environment: &quick
            .state
            .target
            .as_ref()
            .expect("confirmed target")
            .environment,
        active_pack: quick.active_pack.as_ref(),
        query_observations: &quick.state.query_observations,
    });
    let query = request.query.clone();
    let primary = pack.or_else(|| quick.as_ref().and_then(|quick| quick.active_pack.as_ref()));
    let result = app::check_with_evidence(request, primary, evidence);
    inventory_result(
        result,
        quick.as_ref(),
        target_applied,
        quick
            .as_ref()
            .is_some_and(|quick| quick.state.discovery_mode == DiscoveryMode::Quick),
        notice,
        Some(query),
    )
}

/// Bounded line reader drains oversized records before continuing to the next request.
fn next_line(reader: &mut impl BufRead) -> io::Result<Option<(Vec<u8>, bool)>> {
    let mut line = Vec::new();
    let mut oversized = false;
    let mut any = false;
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(if any { Some((line, oversized)) } else { None });
        }
        any = true;
        let n = chunk
            .iter()
            .position(|c| *c == b'\n')
            .map_or(chunk.len(), |n| n + 1);
        let ended = chunk[n - 1] == b'\n';
        if line.len() + n <= MAX_REQUEST_BYTES {
            line.extend_from_slice(&chunk[..n]);
        } else {
            oversized = true;
        }
        reader.consume(n);
        if ended {
            return Ok(Some((line, oversized)));
        }
    }
}
fn jsonl<T: serde::de::DeserializeOwned>(
    common: &Common,
    mut analyze: impl FnMut(T) -> Analysis,
    generated: bool,
) -> Result<i32, Diagnostic> {
    let mut reader = io::stdin().lock();
    let mut exit = 0;
    let mut row = 0;
    let failed = |diagnostic| {
        let mut result = Analysis::failed(None, diagnostic);
        result.grounding.policy = (&context(Context::default(), common)).into();
        result
    };
    while let Some((line, oversized)) =
        next_line(&mut reader).map_err(|_| input_error("Cannot read request stream."))?
    {
        row += 1;
        if !oversized && line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let result = if oversized {
            failed(input_error("Request line exceeds 1 MiB.").at(&format!("/line/{row}")))
        } else {
            match serde_json::from_slice::<T>(&line) {
                Ok(r) => analyze(r),
                Err(e) => failed(
                    Diagnostic::error("REQUEST_JSON", format!("Line {row}: {e}"))
                        .at(&format!("/line/{row}")),
                ),
            }
        };
        exit = exit.max(emit_analysis(&result, common, generated)?);
    }
    Ok(exit)
}
fn generate(args: GenerateArgs) -> Result<i32, Diagnostic> {
    let modes = usize::from(args.request.is_some())
        + usize::from(args.plan.is_some())
        + usize::from(args.recipe.is_some())
        + usize::from(args.intent.is_some())
        + usize::from(args.common.jsonl);
    if modes != 1 {
        return Err(input_error(
            "Choose exactly one of --request, --plan, --recipe, --intent, or --jsonl.",
        ));
    }
    if args.params.is_some() != args.recipe.is_some() {
        return Err(input_error("--recipe and --params must be used together."));
    }
    if args.intent.is_none()
        && (args.repo.is_some() || args.type_.is_some() || args.event.is_some())
    {
        return Err(input_error(
            "Source flags belong to --intent; other modes supply their own source.",
        ));
    }
    let p = pack(&args.common)?;
    if args.common.jsonl {
        return jsonl(
            &args.common,
            |r: GenerateRequest| analyze_generate(r, &args.common, p.as_ref()),
            true,
        );
    }
    let request = if let Some(path) = &args.request {
        json_file(path)?
    } else {
        let input = if let Some(path) = &args.plan {
            GenerateInput::Plan {
                plan: json_file(path)?,
            }
        } else if let Some(recipe) = &args.recipe {
            let params: Value = json_file(args.params.as_ref().unwrap())?;
            let request = serde_json::from_value(json!({"recipe":recipe,"params":params}))
                .map_err(|e| {
                    Diagnostic::error("RECIPE_JSON", format!("Invalid recipe parameters: {e}"))
                })?;
            GenerateInput::Recipe { request }
        } else {
            GenerateInput::English {
                text: args.intent.unwrap(),
                source: Scope {
                    repo: args.repo,
                    type_: args.type_,
                    event: args.event,
                },
            }
        };
        GenerateRequest {
            schema_version: 1,
            id: None,
            input,
            context: Context::default(),
        }
    };
    emit_analysis(
        &analyze_generate(request, &args.common, p.as_ref()),
        &args.common,
        true,
    )
}
fn check(args: CheckArgs) -> Result<i32, Diagnostic> {
    let modes = usize::from(args.request.is_some())
        + usize::from(args.file.is_some())
        + usize::from(args.query.is_some())
        + usize::from(args.common.jsonl);
    if modes != 1 {
        return Err(input_error(
            "Choose one inline query, --file, --request, or --jsonl.",
        ));
    }
    let p = pack(&args.common)?;
    if args.common.jsonl {
        return jsonl(
            &args.common,
            |r: CheckRequest| analyze_check(r, &args.common, p.as_ref()),
            false,
        );
    }
    let request = if let Some(path) = args.request {
        json_file(&path)?
    } else {
        let query = if let Some(path) = args.file {
            String::from_utf8(bytes(&path, MAX_REQUEST_BYTES)?)
                .map_err(|_| input_error("CQL must be UTF-8."))?
        } else {
            let s = args.query.unwrap();
            if s == "-" {
                String::from_utf8(bytes(Path::new("-"), MAX_REQUEST_BYTES)?)
                    .map_err(|_| input_error("CQL must be UTF-8."))?
            } else {
                s
            }
        };
        CheckRequest {
            schema_version: 1,
            id: None,
            query,
            context: Context::default(),
        }
    };
    emit_analysis(
        &analyze_check(request, &args.common, p.as_ref()),
        &args.common,
        false,
    )
}
fn emit_assessment(result: &AssessResult, json: bool) -> Result<i32, Diagnostic> {
    if json {
        output(result, false)?;
    } else {
        for finding in &result.findings {
            line(&format!(
                "{} ({}): {}",
                human(&finding.code),
                match finding.severity {
                    Severity::Error => "error",
                    Severity::Warning => "warning",
                    Severity::Info => "info",
                },
                human(&finding.message)
            ))?;
        }
        line(&format!(
            "{}: {} row(s), {} finding(s)",
            match result.outcome {
                AssessOutcome::NoFindings => "no-findings",
                AssessOutcome::Advisory => "advisory",
                AssessOutcome::Inconclusive => "inconclusive",
                AssessOutcome::Error => "error",
            },
            result.rows,
            result.findings.len()
        ))?;
    }
    Ok(match result.outcome {
        AssessOutcome::NoFindings => 0,
        AssessOutcome::Advisory | AssessOutcome::Inconclusive => 1,
        AssessOutcome::Error => 2,
    })
}
fn failed_assessment(id: Option<String>, query: &str, diagnostic: Diagnostic) -> AssessResult {
    AssessResult {
        schema_version: 1,
        id,
        outcome: AssessOutcome::Error,
        query_sha256: sha256_hex(query.as_bytes()),
        rows: 0,
        caps: vec![],
        columns: Default::default(),
        findings: vec![diagnostic],
    }
}
fn parse_assess_request(raw: &[u8]) -> Result<AssessRequest, Diagnostic> {
    // Reject duplicate keys before result becomes a Value and loses them.
    mcp::parse_saved_result(raw).map_err(|_| {
        Diagnostic::error(
            "REQUEST_JSON",
            "Invalid request: expected valid JSON with unique object keys.",
        )
    })?;
    // Saved results accept identical host repeats; requests still require one document.
    serde_json::from_slice(raw)
        .map_err(|e| Diagnostic::error("REQUEST_JSON", format!("Invalid request: {e}")))
}
fn analyze_assess(
    mut request: AssessRequest,
    args: &AssessArgs,
    pack: Option<&Pack>,
) -> AssessResult {
    let (quick, target_applied, notice) =
        match select_inventory(args.inventory_state.as_deref(), &mut request.context, pack) {
            Ok((quick, applied)) => (quick, applied, None),
            Err(d) => {
                let notice = inventory_notice(d);
                if notice.severity == Severity::Error {
                    return failed_assessment(request.id, &request.query, notice);
                }
                (None, false, Some(notice))
            }
        };
    let primary = pack.or_else(|| quick.as_ref().and_then(|quick| quick.active_pack.as_ref()));
    let mut result = assess::assess(request, primary);
    if target_applied {
        result.findings.push(inventory_info(
            "CQL_QUICK_TARGET",
            "Confirmed inventory supplied the otherwise unspecified assessment target.",
        ));
    }
    if let Some(notice) = notice {
        if result.outcome == AssessOutcome::NoFindings {
            result.outcome = AssessOutcome::Advisory;
        }
        result.findings.push(notice);
    }
    result
}

fn assess_jsonl(args: &AssessArgs, pack: Option<&Pack>) -> Result<i32, Diagnostic> {
    let mut reader = io::stdin().lock();
    let mut exit = 0;
    let mut row = 0;
    while let Some((line, oversized)) =
        next_line(&mut reader).map_err(|_| input_error("Cannot read request stream."))?
    {
        row += 1;
        if !oversized && line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let result = if oversized {
            failed_assessment(
                None,
                "",
                input_error("Request line exceeds 1 MiB.").at(&format!("/line/{row}")),
            )
        } else {
            match parse_assess_request(&line) {
                Ok(request) => analyze_assess(request, args, pack),
                Err(mut diagnostic) => {
                    diagnostic.message = format!("Line {row}: {}", diagnostic.message);
                    failed_assessment(None, "", diagnostic.at(&format!("/line/{row}")))
                }
            }
        };
        exit = exit.max(emit_assessment(&result, true)?);
    }
    Ok(exit)
}
fn assess(args: AssessArgs) -> Result<i32, Diagnostic> {
    if args.query_file.is_some() != args.result.is_some() {
        return Err(input_error(
            "--query-file and --result must be used together.",
        ));
    }
    let modes = usize::from(args.request.is_some())
        + usize::from(args.jsonl)
        + usize::from(args.query_file.is_some());
    if modes != 1 {
        return Err(input_error(
            "Choose exactly one of --request, --jsonl, or --query-file with --result.",
        ));
    }
    let pack = args.pack.as_deref().map(storage::read_pack).transpose()?;
    if args.jsonl {
        return assess_jsonl(&args, pack.as_ref());
    }
    let result = if let Some(path) = &args.request {
        let raw = bytes(path, MAX_REQUEST_BYTES)?;
        analyze_assess(parse_assess_request(&raw)?, &args, pack.as_ref())
    } else {
        let query = String::from_utf8(bytes(args.query_file.as_ref().unwrap(), MAX_REQUEST_BYTES)?)
            .map_err(|_| input_error("CQL must be UTF-8."))?;
        let raw = bytes(args.result.as_ref().unwrap(), mcp::MAX_RESULT_BYTES)?;
        match mcp::parse_saved_result(&raw) {
            Ok(result) => analyze_assess(
                AssessRequest {
                    schema_version: 1,
                    id: None,
                    query,
                    result,
                    context: Context::default(),
                },
                &args,
                pack.as_ref(),
            ),
            Err(diagnostic) if diagnostic.code == "MCP_TOO_LARGE" => {
                failed_assessment(None, &query, diagnostic)
            }
            Err(_) => failed_assessment(
                None,
                &query,
                Diagnostic::error("RESULT_JSON", "Saved MCP result must be valid JSON."),
            ),
        }
    };
    emit_assessment(&result, args.json)
}
fn catalog(args: CatalogArgs) -> Result<i32, Diagnostic> {
    let flags = u8::from(args.functions)
        + u8::from(args.recipes)
        + u8::from(args.profile)
        + u8::from(args.standard.is_some());
    if flags > 1 {
        return Err(input_error("Choose one catalog category."));
    }
    if let Some(standard) = args.standard {
        output(
            &catalog::standard(
                match standard {
                    Standard::Cps => "cps",
                    Standard::Ecs => "ecs",
                },
                &args.term,
                args.limit as usize,
            ),
            true,
        )?;
    } else if args.profile {
        output(
            &json!({"profile_id":PROFILE,"assurance":"bounded offline syntax, signature and placement checks; no execution proof","max_query_utf16":MAX_QUERY_UTF16,"max_request_bytes":MAX_REQUEST_BYTES,"max_depth":MAX_DEPTH,"max_nodes":MAX_NODES,"regex":"conservative syntax subset; engine-specific escapes and Unicode case folding are unsupported","offsets":"zero-based UTF-8 bytes; end exclusive; one-based Unicode line/column","knowledge_revision":environment::KNOWLEDGE_REVISION,"functions":catalog::FUNCTIONS.len(),"analyst_workflow":"cql:workflow:analyst","knowledge_records":catalog::records().len()}),
            true,
        )?;
    } else if args.recipes {
        output(&app::recipe_catalog(), true)?;
    } else if args.functions {
        let matches = catalog::FUNCTIONS
            .iter()
            .filter(|f| {
                args.term.is_empty() || f.name.to_lowercase().contains(&args.term.to_lowercase())
            })
            .take(args.limit as usize)
            .map(|f| {
                let mut value = serde_json::to_value(f).expect("function serializes");
                value["scalar_computation"] = json!(f.scalar_computation());
                value
            })
            .collect::<Vec<_>>();
        output(&matches, true)?;
    } else {
        output(
            &catalog::search(&args.term, args.limit as usize, args.full),
            args.full,
        )?;
    }
    Ok(0)
}
fn env(command: EnvCommand) -> Result<i32, Diagnostic> {
    match command {
        EnvCommand::Inventory(args) => inventory_cli::run(args),
        EnvCommand::Identity {
            cloud,
            view,
            credentials_file,
        } => {
            let identity =
                network::Credentials::identity(credentials_file.as_deref(), cloud, view)?;
            output(&identity, true)?;
            Ok(0)
        }
        EnvCommand::Inspect {
            file,
            summary,
            full,
            field,
            source,
            limit,
        } => {
            let mut p = storage::read_pack(&file)?;
            if summary {
                if full || field.is_some() || source.is_some() {
                    return Err(input_error(
                        "--summary is an overview; use --full, --field, or --source for details.",
                    ));
                }
                let sources = p
                    .data
                    .sources
                    .iter()
                    .map(|source| {
                        let id = source["id"].as_str().unwrap_or("");
                        json!({"id":id,"name":source["name"],"selectors":source["selectors"],
                            "inventory_status":source["inventory_status"],
                            "field_count":p.data.fields.iter().filter(|field| field["source_id"] == id).count()})
                    })
                    .collect::<Vec<_>>();
                output(
                    &json!({"summary":true,"format_version":p.format_version,
                    "environment":p.environment,"pack_id":p.content_digest,"created_at":p.created_at,
                    "counts":{"sources":sources.len(),"fields":p.data.fields.len(),
                        "values":p.data.values.len(),"provenance":p.data.provenance.len(),
                        "annotations":p.data.annotations.len()},"sources":sources}),
                    true,
                )?;
            } else if full {
                if source.is_some() || field.is_some() {
                    p.data.sources.retain(|s| {
                        source.as_ref().is_none_or(|q| {
                            s["id"].as_str() == Some(q)
                                || s["name"].as_str() == Some(q)
                                || s.pointer("/selectors/repo").and_then(Value::as_str) == Some(q)
                        })
                    });
                    let ids = p
                        .data
                        .sources
                        .iter()
                        .filter_map(|s| s["id"].as_str().map(str::to_owned))
                        .collect::<BTreeSet<_>>();
                    p.data.fields.retain(|f| {
                        f["source_id"].as_str().is_some_and(|id| ids.contains(id))
                            && field.as_ref().is_none_or(|q| {
                                f["path"].as_str().is_some_and(|path| path.contains(q))
                            })
                    });
                    if field.is_some() {
                        let with_fields = p
                            .data
                            .fields
                            .iter()
                            .filter_map(|f| f["source_id"].as_str())
                            .collect::<BTreeSet<_>>();
                        p.data.sources.retain(|s| {
                            s["id"].as_str().is_some_and(|id| with_fields.contains(id))
                        });
                    }
                    let fids = p
                        .data
                        .fields
                        .iter()
                        .filter_map(|f| f["id"].as_str())
                        .collect::<BTreeSet<_>>();
                    p.data
                        .values
                        .retain(|v| v["field_id"].as_str().is_some_and(|id| fids.contains(id)));
                    let ids = p
                        .data
                        .sources
                        .iter()
                        .filter_map(|s| s["id"].as_str())
                        .collect::<BTreeSet<_>>();
                    p.data
                        .annotations
                        .retain(|a| a["source_id"].as_str().is_some_and(|id| ids.contains(id)));
                    let provenance_ids = p
                        .data
                        .sources
                        .iter()
                        .chain(&p.data.fields)
                        .chain(&p.data.values)
                        .chain(&p.data.annotations)
                        .filter_map(|item| item["provenance_ids"].as_array())
                        .flat_map(|ids| ids.iter().filter_map(Value::as_str).map(str::to_owned))
                        .collect::<BTreeSet<_>>();
                    p.data.provenance.retain(|prov| {
                        prov["id"]
                            .as_str()
                            .is_some_and(|id| provenance_ids.contains(id))
                    });
                }
                output(&p, true)?;
            } else if field.is_some() || source.is_some() {
                let ids = p
                    .data
                    .sources
                    .iter()
                    .filter(|s| {
                        source.as_ref().is_none_or(|q| {
                            s["id"].as_str() == Some(q)
                                || s["name"].as_str() == Some(q)
                                || s.pointer("/selectors/repo").and_then(Value::as_str) == Some(q)
                        })
                    })
                    .filter_map(|s| s["id"].as_str())
                    .collect::<Vec<_>>();
                let fields = p
                    .data
                    .fields
                    .iter()
                    .filter(|f| {
                        ids.contains(&f["source_id"].as_str().unwrap_or(""))
                            && field
                                .as_ref()
                                .is_none_or(|q| f["path"].as_str().is_some_and(|p| p.contains(q)))
                    })
                    .take(limit as usize)
                    .collect::<Vec<_>>();
                let fids = fields
                    .iter()
                    .filter_map(|f| f["id"].as_str())
                    .collect::<Vec<_>>();
                let values = p
                    .data
                    .values
                    .iter()
                    .filter(|v| fids.contains(&v["field_id"].as_str().unwrap_or("")))
                    .take(limit as usize)
                    .collect::<Vec<_>>();
                output(
                    &json!({"environment":p.environment,"pack_id":p.content_digest,"fields":fields,"values":values}),
                    true,
                )?;
            } else {
                output(
                    &json!({"format_version":p.format_version,"environment":p.environment,"pack_id":p.content_digest,"created_at":p.created_at,"sources":p.data.sources,"field_count":p.data.fields.len(),"value_count":p.data.values.len(),"annotations":p.data.annotations}),
                    true,
                )?;
            }
            Ok(0)
        }
        EnvCommand::Import {
            from,
            out,
            environment,
            replace,
        } => {
            let explicit: Option<EnvironmentId> =
                environment.as_deref().map(json_file).transpose()?;
            let p = if from.is_dir() {
                let e=explicit.clone().ok_or_else(||input_error("Legacy import requires --environment FILE with explicit cloud/account/view."))?;
                Pack::new(e, discovery::timestamp(), storage::legacy_data(&from)?)?
            } else {
                let p = storage::read_pack(&from)?;
                if explicit.as_ref().is_some_and(|e| e != &p.environment) {
                    return Err(input_error(
                        "Imported snapshot conflicts with the explicit environment.",
                    ));
                }
                p
            };
            let writer = storage::Writer::acquire(&out)?;
            if out.exists() && !replace {
                return Err(input_error(
                    "Output exists. Use a new path or explicitly pass --replace.",
                ));
            }
            let (report, code) = import_commit_result(&out, &p.content_digest, writer.commit(&p))?;
            output(&report, true)?;
            Ok(code)
        }
        EnvCommand::Discover {
            request,
            execute,
            mcp: mcp_mode,
            state,
            answers,
            pack,
            credentials_file,
            receipt,
        } => {
            if mcp_mode {
                if execute || credentials_file.is_some() {
                    return Err(input_error(
                        "--mcp cannot use --execute or --credentials-file.",
                    ));
                }
                let state = state.ok_or_else(|| input_error("--mcp requires --state PATH."))?;
                let result = if let Some(request) = request {
                    if !answers.is_empty() {
                        return Err(input_error(
                            "--answer is only accepted after initialization.",
                        ));
                    }
                    let pack =
                        pack.ok_or_else(|| input_error("--mcp init requires --pack PATH."))?;
                    let receipt = receipt
                        .ok_or_else(|| input_error("--mcp init requires --receipt PATH."))?;
                    let request: discovery::DiscoverRequest = json_file(&request)?;
                    mcp::init(request, &state, &pack, &receipt)?
                } else {
                    if pack.is_some() || receipt.is_some() {
                        return Err(input_error(
                            "--pack and --receipt are only accepted at --mcp init.",
                        ));
                    }
                    let answers = answers
                        .iter()
                        .map(|pair| {
                            let (id, path) = pair
                                .split_once('=')
                                .ok_or_else(|| input_error("Use --answer PROBE_ID=FILE."))?;
                            if !id.starts_with("p-") || path.is_empty() {
                                return Err(input_error("Use --answer PROBE_ID=FILE."));
                            }
                            Ok((id.into(), PathBuf::from(path)))
                        })
                        .collect::<Result<Vec<_>, Diagnostic>>()?;
                    mcp::step(&state, &answers)?
                };
                let code = result.exit_code();
                output(&result, false)?;
                return Ok(code);
            }
            if state.is_some() || !answers.is_empty() {
                return Err(input_error("--state and --answer require --mcp."));
            }
            let request =
                request.ok_or_else(|| input_error("Discovery requires --request FILE."))?;
            let r: discovery::DiscoverRequest = json_file(&request)?;
            discovery::validate_request(&r)?;
            if !execute {
                output(&discovery::initial_plan(&r)?, true)?;
                return Ok(0);
            }
            let path = pack.ok_or_else(|| input_error("--execute requires --pack PATH."))?;
            let receipt =
                receipt.ok_or_else(|| input_error("--execute requires a new --receipt PATH."))?;
            if receipt.exists() {
                return Err(input_error(
                    "Receipt already exists; choose a new run path.",
                ));
            }
            if receipt == path {
                return Err(input_error("Pack and receipt paths must be different."));
            }
            let writer = storage::Writer::acquire(&path)?;
            let old = writer.existing()?;
            if old.as_ref().is_some_and(|p| p.environment != r.environment) {
                return Err(input_error(
                    "Active pack belongs to a different cloud/account/view.",
                ));
            }
            let receipt_file = storage::reserve_receipt(&receipt)?;
            if path.exists()
                && std::fs::canonicalize(&path).ok() == std::fs::canonicalize(&receipt).ok()
            {
                drop(receipt_file);
                let _ = std::fs::remove_file(&receipt);
                return Err(input_error("Pack and receipt resolve to the same file."));
            }
            let (receipt_file, mut client) = reserved_preflight(receipt_file, &receipt, || {
                install_cancel()?;
                let credentials =
                    network::Credentials::load(credentials_file.as_deref(), &r.environment)?;
                network::FalconClient::new(&r.environment, credentials)
            })?;
            let (candidate, mut report) = discovery::execute(&r, &mut client);
            if let Some(candidate) = candidate {
                let merged = if let Some(old) = old {
                    environment::merge(&old, &candidate)
                } else {
                    Ok(candidate)
                };
                match merged.and_then(|p| writer.commit(&p)) {
                    Ok(()) => report.active_pack_updated = true,
                    Err(d) => {
                        report.active_pack_updated = d.code == "PACK_DURABILITY";
                        report.status = "error".into();
                        report.diagnostics.push(d);
                    }
                }
            }
            persist_discovery(receipt_file, &mut report);
            output(&report, false).map_err(|_|Diagnostic::error("OUTPUT_IO",format!("Cannot emit discovery report; active_pack_updated={}. Inspect the reserved receipt path for persisted evidence.",report.active_pack_updated)))?;
            Ok(
                if report.active_pack_updated && report.status == "complete" {
                    0
                } else {
                    2
                },
            )
        }
        EnvCommand::Parsers {
            environment,
            credentials_file,
            id,
            template,
            offset,
            limit,
        } => {
            let e: EnvironmentId = json_file(&environment)?;
            let credentials = network::Credentials::load(credentials_file.as_deref(), &e)?;
            let mut client = network::FalconClient::new(&e, credentials)?;
            let deadline = Instant::now() + Duration::from_secs(30);
            let result = if let Some(id) = id {
                client.parser(&id, template, deadline)?
            } else {
                if template {
                    return Err(input_error("--template requires --id."));
                }
                client.list_parsers(offset, limit, deadline)?
            };
            output(&result, true)?;
            Ok(0)
        }
    }
}

fn install_cancel() -> Result<(), Diagnostic> {
    // ctrlc accepts one handler per process; installing once and reusing it keeps repeated
    // discovery calls in one process (for example in-process retries) from failing spuriously.
    static INSTALLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let installed = *INSTALLED.get_or_init(|| {
        ctrlc::set_handler(|| {
            network::CANCELLED.store(true, Ordering::Relaxed);
        })
        .is_ok()
    });
    if installed {
        Ok(())
    } else {
        Err(Diagnostic::error(
            "SIGNAL_HANDLER",
            "Cannot install cancellation handler.",
        ))
    }
}
fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = if cli.schema.is_some() && cli.command.is_some() {
        Err(input_error("--schema cannot be combined with a command."))
    } else if let Some(schema) = cli.schema {
        let text = match schema {
            Schema::Generate => include_str!("../schemas/generate.json"),
            Schema::Check => include_str!("../schemas/check.json"),
            Schema::Pack => include_str!("../schemas/pack.json"),
            Schema::Discovery => include_str!("../schemas/discovery.json"),
            Schema::Result => include_str!("../schemas/result.json"),
            Schema::Assess => include_str!("../schemas/assess.json"),
            Schema::Setup => include_str!("../schemas/setup.json"),
            Schema::Inventory => include_str!("../schemas/inventory.json"),
            Schema::Status => include_str!("../schemas/status.json"),
        };
        line(text).map(|_| 0)
    } else {
        match cli.command {
            Some(Command::Setup(args)) => setup_cli::run(args),
            Some(Command::Status(args)) => inventory_cli::status(args),
            Some(Command::Generate(args)) => generate(args),
            Some(Command::Check(args)) => check(args),
            Some(Command::Assess(args)) => assess(args),
            Some(Command::Catalog(args)) => catalog(args),
            Some(Command::Env { command }) => env(command),
            None => Err(input_error(
                "Choose setup, status, generate, check, assess, catalog, env, or --schema. Use --help for details.",
            )),
        }
    };
    match result {
        Ok(code) => ExitCode::from(code as u8),
        Err(d) => {
            let _ = writeln!(
                io::stderr().lock(),
                "{}",
                serde_json::to_string(&d).unwrap_or_else(|_| "{\"code\":\"ERROR\"}".into())
            );
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    fn parsed_discovery(args: Vec<OsString>) -> Result<i32, Diagnostic> {
        let cli = Cli::try_parse_from(args).map_err(|e| input_error(&e.to_string()))?;
        match cli.command {
            Some(Command::Env {
                command: command @ EnvCommand::Discover { .. },
            }) => env(command),
            _ => panic!("expected parsed env discover command"),
        }
    }

    fn exit_code(result: &Result<i32, Diagnostic>) -> i32 {
        result.as_ref().copied().unwrap_or(2)
    }

    fn test_directory() -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-cli-boundary-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }

    #[test]
    fn receipt_failure_keeps_activation_state_and_marks_operation_failed() {
        let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
        let mut report = discovery::DiscoverReport {
            schema_version: 1,
            environment: EnvironmentId {
                cloud: Cloud::Us1,
                tenant: "0".repeat(32),
                view: "search-all".into(),
            },
            status: "complete".into(),
            active_pack_updated: true,
            captures: vec![],
            diagnostics: vec![],
        };
        persist_discovery(file, &mut report);
        assert!(report.active_pack_updated);
        assert_eq!(report.status, "error");
        assert_eq!(report.diagnostics.len(), 1);
    }

    #[test]
    fn missing_credentials_release_preflight_receipt_for_retry() {
        let dir = test_directory();
        let request = dir.join("request.json");
        std::fs::write(
            &request,
            include_str!("../examples/mcp/discover.request.json"),
        )
        .unwrap();
        let pack = dir.join("pack.json");
        let receipt = dir.join("receipt.json");
        for attempt in 0..2 {
            let result = parsed_discovery(vec![
                "cqlmatic".into(),
                "env".into(),
                "discover".into(),
                "--execute".into(),
                "--request".into(),
                request.clone().into_os_string(),
                "--pack".into(),
                pack.clone().into_os_string(),
                "--receipt".into(),
                receipt.clone().into_os_string(),
                "--credentials-file".into(),
                dir.join("missing-credentials").into_os_string(),
            ]);
            assert_eq!(exit_code(&result), 2, "attempt {attempt}");
            assert_eq!(result.unwrap_err().code, "AUTH_FILE", "attempt {attempt}");
            assert!(!receipt.exists());
            assert!(!pack.exists());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn mcp_cli_dispatch_initializes_and_repeats_a_pending_step_offline() {
        let dir = test_directory();
        let request = dir.join("request.json");
        std::fs::write(
            &request,
            include_str!("../examples/mcp/discover.request.json"),
        )
        .unwrap();
        let state = dir.join("state.json");
        let pack = dir.join("pack.json");
        let receipt = dir.join("receipt.json");
        let first = env(EnvCommand::Discover {
            request: Some(request),
            execute: false,
            mcp: true,
            state: Some(state.clone()),
            answers: vec![],
            pack: Some(pack.clone()),
            credentials_file: None,
            receipt: Some(receipt.clone()),
        })
        .unwrap();
        assert_eq!(first, 3);
        assert!(state.exists());
        assert!(!pack.exists());
        assert!(!receipt.exists());
        let pending = env(EnvCommand::Discover {
            request: None,
            execute: false,
            mcp: true,
            state: Some(state),
            answers: vec![],
            pack: None,
            credentials_file: None,
            receipt: None,
        })
        .unwrap();
        assert_eq!(pending, 3);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parsed_mcp_cli_steps_accept_relative_and_absolute_answers_and_reject_bad_flags() {
        let dir = test_directory();
        let request = dir.join("request.json");
        std::fs::write(
            &request,
            include_str!("../examples/mcp/discover.request.json"),
        )
        .unwrap();
        let state = dir.join("state.json");
        let pack = dir.join("pack.json");
        let receipt = dir.join("receipt.json");
        let init = || {
            vec![
                OsString::from("cqlmatic"),
                "env".into(),
                "discover".into(),
                "--mcp".into(),
                "--request".into(),
                request.clone().into_os_string(),
                "--state".into(),
                state.clone().into_os_string(),
                "--pack".into(),
                pack.clone().into_os_string(),
                "--receipt".into(),
                receipt.clone().into_os_string(),
            ]
        };
        for flag in ["--execute", "--credentials-file"] {
            let mut args = init();
            args.push(flag.into());
            if flag == "--credentials-file" {
                args.push(dir.join("missing-credentials").into_os_string());
            }
            let result = parsed_discovery(args);
            assert_eq!(exit_code(&result), 2);
            assert_eq!(result.unwrap_err().code, "CLI_INPUT");
            assert!(!state.exists());
        }
        assert_eq!(parsed_discovery(init()).unwrap(), 3);
        let first = mcp::step(&state, &[]).unwrap();
        assert_eq!(first.status, "needs-probes");
        let run_step = |pair: &str| {
            parsed_discovery(vec![
                "cqlmatic".into(),
                "env".into(),
                "discover".into(),
                "--mcp".into(),
                "--state".into(),
                state.clone().into_os_string(),
                "--answer".into(),
                pair.into(),
            ])
        };
        let malformed = run_step("p-bad");
        assert_eq!(exit_code(&malformed), 2);
        assert_eq!(malformed.unwrap_err().code, "CLI_INPUT");
        for (index, relative) in [(0, true), (1, false)] {
            let p = &first.probes[index];
            let answer = dir.join(format!("answer-{index}.json"));
            let (fixture, body) = if p["kind"] == "volume" {
                (
                    "examples/mcp/count.json",
                    include_str!("../examples/mcp/count.json"),
                )
            } else {
                (
                    "examples/mcp/dataset.json",
                    include_str!("../examples/mcp/dataset.json"),
                )
            };
            std::fs::write(&answer, body).unwrap();
            let path = if relative {
                PathBuf::from(fixture)
            } else {
                answer
            };
            let pair = format!("{}={}", p["probe_id"].as_str().unwrap(), path.display());
            assert_eq!(run_step(&pair).unwrap(), 3);
        }
        assert_eq!(mcp::step(&state, &[]).unwrap().answered, 2);
        assert!(!receipt.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn parsed_mcp_cli_later_failure_exits_two_and_preserves_existing_pack() {
        let dir = test_directory();
        let request = dir.join("request.json");
        std::fs::write(
            &request,
            include_str!("../examples/mcp/discover.request.json"),
        )
        .unwrap();
        let state = dir.join("state.json");
        let pack = dir.join("pack.json");
        let receipt = dir.join("receipt.json");
        let environment =
            serde_json::from_slice::<discovery::DiscoverRequest>(&std::fs::read(&request).unwrap())
                .unwrap()
                .environment;
        let old = Pack::new(
            environment,
            discovery::timestamp(),
            environment::PackData::default(),
        )
        .unwrap();
        storage::Writer::acquire(&pack)
            .unwrap()
            .commit(&old)
            .unwrap();
        let before = std::fs::read(&pack).unwrap();
        assert_eq!(
            parsed_discovery(vec![
                "cqlmatic".into(),
                "env".into(),
                "discover".into(),
                "--mcp".into(),
                "--request".into(),
                request.into_os_string(),
                "--state".into(),
                state.clone().into_os_string(),
                "--pack".into(),
                pack.clone().into_os_string(),
                "--receipt".into(),
                receipt.clone().into_os_string(),
            ])
            .unwrap(),
            3
        );
        let id = mcp::step(&state, &[]).unwrap().probes[1]["probe_id"]
            .as_str()
            .unwrap()
            .to_string();
        let answer = dir.join("error.json");
        std::fs::write(&answer, r#"{"result":[],"error":"synthetic tool error"}"#).unwrap();
        let result = parsed_discovery(vec![
            "cqlmatic".into(),
            "env".into(),
            "discover".into(),
            "--mcp".into(),
            "--state".into(),
            state.clone().into_os_string(),
            "--answer".into(),
            format!("{}={}", id, answer.display()).into(),
        ]);
        assert_eq!(exit_code(&result), 2);
        assert_eq!(mcp::step(&state, &[]).unwrap().status, "failed");
        assert_eq!(std::fs::read(&pack).unwrap(), before);
        assert!(!receipt.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn import_durability_report_preserves_updated_state_and_exit_two() {
        let d = Diagnostic::error("PACK_DURABILITY", "injected directory sync failure");
        let (report, code) =
            import_commit_result(Path::new("active.json"), "fixture-digest", Err(d)).unwrap();
        assert_eq!(code, 2);
        assert_eq!(report["status"], "error");
        assert_eq!(report["active_pack_updated"], true);
        assert_eq!(report["durability_confirmed"], false);
        assert_eq!(report["diagnostics"][0]["code"], "PACK_DURABILITY");
    }
}
