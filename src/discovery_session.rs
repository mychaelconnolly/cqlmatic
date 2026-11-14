//! Protected checkpoints for explicitly resumable discovery runs.
use super::*;
use crate::network::{self, JobReceipt};
use std::sync::atomic::Ordering;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transport {
    Mcp,
    Native,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivePeriod {
    start: String,
    end: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Attempt {
    probe: Probe,
    dispatched_at: String,
    job_id: Option<String>,
    outcome: String,
    receipt: Option<JobReceipt>,
    terminal: Option<TerminalEvidence>,
    #[serde(default)]
    no_start_confirmed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rejected_results: Vec<RejectedResult>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RejectedResult {
    evidence: TerminalEvidence,
    diagnostic: Diagnostic,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TerminalEvidence {
    file: PathBuf,
    raw_hex: String,
    raw_sha256: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Session {
    pub(crate) transport: Transport,
    #[serde(default)]
    pub(crate) sparse_fieldset_fallback: bool,
    binding_sha256: String,
    predecessor_sha256: Option<String>,
    periods: Vec<ActivePeriod>,
    attempts: Vec<Attempt>,
    pub(crate) captures: BTreeMap<String, discovery::Capture>,
    capture_sha256: BTreeMap<String, String>,
}

fn error(code: &str, message: &str) -> Diagnostic {
    Diagnostic::error(code, message)
}

fn binding(state: &State, session: &Session) -> String {
    let mut bytes = serde_json::to_vec(&(
        &state.request_sha256,
        &state.created_at,
        &state.pack,
        &state.receipt,
        session.transport,
        &session.predecessor_sha256,
    ))
    .expect("checkpoint identity serializes");
    // Missing/false retains the exact identity and measurement graph of older checkpoints.
    if session.sparse_fieldset_fallback {
        bytes.extend_from_slice(b"\0sparse-fieldset-fallback-v1");
    }
    sha256_hex(&bytes)
}

impl Session {
    pub(crate) fn attempts_len(&self) -> usize {
        self.attempts.len()
    }
    fn active_nanos(&self, now: &str) -> Result<u64, Diagnostic> {
        let now = environment::parse_time(now)?;
        let mut total = 0_u64;
        for period in &self.periods {
            let start = environment::parse_time(&period.start)?;
            if now < start {
                break;
            }
            let end = period
                .end
                .as_deref()
                .map(environment::parse_time)
                .transpose()?
                .unwrap_or(now)
                .min(now);
            let nanos = u64::try_from((end - start).whole_nanoseconds()).map_err(|_| tampered())?;
            total = total.checked_add(nanos).ok_or_else(tampered)?;
        }
        Ok(total)
    }

    fn active_millis(&self, now: &str) -> Result<u64, Diagnostic> {
        Ok(self.active_nanos(now)? / 1_000_000)
    }

    pub(crate) fn remaining(&self, state: &State, now: &str) -> Result<Duration, Diagnostic> {
        if environment::parse_time(now)? < environment::parse_time(&state.created_at)? {
            return Err(tampered());
        }
        let used = self.active_nanos(now)?;
        let budget = state.request.max_seconds * 1_000_000_000;
        if used >= budget {
            return Err(error(
                "MCP_DEADLINE",
                "Discovery active-time budget expired; pause and resume do not renew it.",
            ));
        }
        Ok(Duration::from_nanos(budget - used))
    }

    pub(crate) fn pause_clock(&mut self, now: &str) -> Result<(), Diagnostic> {
        if let Some(period) = self.periods.last_mut()
            && period.end.is_none()
        {
            if environment::parse_time(now)? < environment::parse_time(&period.start)? {
                return Err(tampered());
            }
            period.end = Some(now.into());
        }
        Ok(())
    }
}

pub fn is_resumable(state: &State) -> bool {
    state.session.is_some()
}

pub fn has_in_flight(state: &State) -> bool {
    state.session.as_ref().is_some_and(|session| {
        session.attempts.last().is_some_and(|attempt| {
            let unproven_native_failure = session.transport == Transport::Native
                && attempt.outcome == "failed"
                && attempt.job_id.is_none()
                && attempt.receipt.as_ref().is_none_or(|receipt| {
                    receipt.cleanup == "not-started"
                        && receipt.diagnostic.as_ref().is_some_and(|d| {
                            d.code == "HTTP_STATUS"
                                || (!attempt.no_start_confirmed
                                    && matches!(
                                        d.code.as_str(),
                                        "HTTP_RESPONSE" | "HTTP_RESPONSE_LIMIT"
                                    ))
                        })
                });
            matches!(
                attempt.outcome.as_str(),
                "dispatched" | "started" | "cleanup-pending" | "ambiguous"
            ) || (session.transport == Transport::Mcp
                && attempt.outcome == "failed"
                && attempt.terminal.is_some())
                || unproven_native_failure
        })
    })
}

pub fn resumable_request(path: &Path) -> Result<DiscoverRequest, Diagnostic> {
    let state = read_state(path)?;
    if state.session.is_none() {
        return Err(error(
            "MCP_STATE_RESTART",
            "This run has no resumable checkpoint; keep its original recovery workflow.",
        ));
    }
    Ok(state.request)
}

pub fn resumable_info(path: &Path) -> Result<Value, Diagnostic> {
    let state = read_state(path)?;
    let session = state
        .session
        .as_ref()
        .ok_or_else(|| error("MCP_STATE_RESTART", "This run has no resumable checkpoint."))?;
    let now = discovery::timestamp();
    Ok(
        json!({"transport":session.transport,"status":state.status,"attempts":session.attempts.len(),
        "max_queries":state.request.max_queries,"active_milliseconds":session.active_millis(&now)?,
        "max_seconds":state.request.max_seconds,"in_flight":has_in_flight(&state),
        "request_sha256":state.request_sha256,
        "current_attempt":session.attempts.last().map(|a| json!({"probe_id":discovery::probe_id(&state.request.environment.view,&a.probe),"dispatched_at":a.dispatched_at,"outcome":a.outcome,"repository":state.request.environment.view,"query_sha256":sha256_hex(a.probe.query.as_bytes()),"start":a.probe.start,"end":a.probe.end,"known_job":a.job_id.is_some()}))}),
    )
}

pub(crate) fn validate(state: &State) -> Result<(), Diagnostic> {
    let Some(session) = &state.session else {
        return if matches!(
            state.status.as_str(),
            "paused" | "cancel-requested" | "cleanup-pending"
        ) {
            Err(tampered())
        } else {
            Ok(())
        };
    };
    if state.schema_version != 3
        || session.binding_sha256 != binding(state, session)
        || session.periods.is_empty()
        || session.periods.len() > 4096
        || session.attempts.len() > state.request.max_queries as usize
        || (session.transport == Transport::Mcp && !session.captures.is_empty())
        || (session.transport == Transport::Native && !state.answers.is_empty())
        || session.capture_sha256.len() != session.captures.len()
    {
        return Err(tampered());
    }
    let mut previous = environment::parse_time(&state.created_at)?;
    let now = environment::parse_time(&discovery::timestamp())?;
    for (index, period) in session.periods.iter().enumerate() {
        let start = environment::parse_time(&period.start)?;
        let end = period
            .end
            .as_deref()
            .map(environment::parse_time)
            .transpose()?;
        if start < previous
            || start > now
            || end.is_some_and(|end| end > now)
            || end.is_some_and(|end| end < start)
            || (end.is_none() && index + 1 != session.periods.len())
        {
            return Err(tampered());
        }
        previous = end.unwrap_or(start);
    }
    if state.status == "paused" && session.periods.last().is_some_and(|p| p.end.is_none()) {
        return Err(tampered());
    }
    let mut pending = 0;
    let mut last_dispatch = environment::parse_time(&state.created_at)?;
    for (index, attempt) in session.attempts.iter().enumerate() {
        let id = discovery::probe_id(&state.request.environment.view, &attempt.probe);
        if !matches!(
            attempt.outcome.as_str(),
            "dispatched"
                | "started"
                | "complete"
                | "completed"
                | "cancelled"
                | "failed"
                | "cleanup-pending"
                | "ambiguous"
        ) || environment::parse_time(&attempt.dispatched_at)? < last_dispatch
            || environment::parse_time(&attempt.dispatched_at)? > now
            || (attempt.outcome == "started" && attempt.job_id.is_none())
            || (attempt.no_start_confirmed
                && (attempt.job_id.is_some() || attempt.outcome != "failed"))
            || attempt.job_id.as_ref().is_some_and(|id| {
                id.is_empty() || id.len() > 256 || id.chars().any(char::is_control)
            })
        {
            return Err(tampered());
        }
        last_dispatch = environment::parse_time(&attempt.dispatched_at)?;
        if matches!(
            attempt.outcome.as_str(),
            "dispatched" | "started" | "cleanup-pending" | "ambiguous"
        ) {
            pending += 1;
            if index + 1 != session.attempts.len() {
                return Err(tampered());
            }
        }
        if attempt.outcome == "complete"
            && !state.answers.contains_key(&id)
            && !session.captures.contains_key(&id)
        {
            return Err(tampered());
        }
        if attempt.outcome == "completed"
            && (session.transport != Transport::Mcp
                || attempt.terminal.is_none()
                || index + 1 != session.attempts.len()
                || state.status != "failed"
                || state
                    .failure
                    .as_ref()
                    .is_none_or(|d| d.code != "MCP_DEADLINE")
                || session
                    .periods
                    .last()
                    .and_then(|period| period.end.as_deref())
                    .is_none_or(|end| session.remaining(state, end).is_ok()))
        {
            return Err(tampered());
        }
        if attempt.rejected_results.len() > 16 {
            return Err(tampered());
        }
        for (terminal, evidence) in attempt
            .terminal
            .iter()
            .map(|evidence| (true, evidence))
            .chain(
                attempt
                    .rejected_results
                    .iter()
                    .map(|rejected| (false, &rejected.evidence)),
            )
        {
            if evidence.raw_hex.len() > MAX_CAPTURE_BYTES * 2
                || !evidence.raw_hex.len().is_multiple_of(2)
            {
                return Err(tampered());
            }
            let raw = evidence
                .raw_hex
                .as_bytes()
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| {
                    std::str::from_utf8(pair)
                        .ok()
                        .and_then(|hex| u8::from_str_radix(hex, 16).ok())
                        .ok_or_else(tampered)
                })
                .collect::<Result<Vec<_>, _>>()?;
            if sha256_hex(&raw) != evidence.raw_sha256 {
                return Err(tampered());
            }
            if terminal && attempt.outcome == "completed" {
                complete_result(&raw, &attempt.probe).map_err(|_| tampered())?;
            }
        }
    }
    if pending > 1 || (matches!(state.status.as_str(), "paused" | "activated") && pending != 0) {
        return Err(tampered());
    }
    for (id, answer) in &state.answers {
        let ingested = environment::parse_time(&answer.ingested_at)?;
        if !session.attempts.iter().any(|attempt| {
            attempt.outcome == "complete"
                && discovery::probe_id(&state.request.environment.view, &attempt.probe) == *id
                && environment::parse_time(&attempt.dispatched_at)
                    .is_ok_and(|time| time <= ingested)
        }) || !session.periods.iter().any(|period| {
            environment::parse_time(&period.start).is_ok_and(|start| start <= ingested)
                && period
                    .end
                    .as_deref()
                    .is_none_or(|end| environment::parse_time(end).is_ok_and(|end| ingested <= end))
        }) {
            return Err(tampered());
        }
    }
    for (id, capture) in &session.captures {
        if *id != discovery::probe_id(&state.request.environment.view, &capture.probe)
            || session.capture_sha256.get(id)
                != Some(&sha256_hex(
                    &serde_json::to_vec(capture).map_err(|_| tampered())?,
                ))
            || capture.receipt.completion != "complete"
            || capture.receipt.cleanup != "confirmed"
            || capture.receipt.job_id.is_none()
            || capture.receipt.query != capture.probe.query
            || capture.receipt.start != capture.probe.start
            || capture.receipt.end != capture.probe.end
            || environment::parse_time(&capture.retrieved_at)?
                < environment::parse_time(&capture.probe.end)?
            || !session.attempts.iter().any(|attempt| {
                attempt.outcome == "complete"
                    && discovery::probe_id(&state.request.environment.view, &attempt.probe) == *id
                    && attempt.job_id == capture.receipt.job_id
            })
        {
            return Err(tampered());
        }
        discovery::check_measurement(&capture.probe, &capture.receipt.events)?;
    }
    Ok(())
}

pub(super) fn ensure_predecessor(state: &State) -> Result<(), Diagnostic> {
    if state.activation.is_some() || state.status == "activated" {
        return Ok(());
    }
    let session = state.session.as_ref().ok_or_else(tampered)?;
    if optional_bytes(&state.pack)?.map(|bytes| sha256_hex(&bytes)) != session.predecessor_sha256 {
        return Err(error(
            "MCP_ACTIVATION_CONFLICT",
            "The starting pack changed; no new search or replacement is allowed for this checkpoint.",
        ));
    }
    Ok(())
}

pub fn init_resumable(
    request: DiscoverRequest,
    state_path: &Path,
    pack_path: &Path,
    receipt_path: &Path,
    transport: Transport,
) -> Result<StepOutput, Diagnostic> {
    // Hold the state lock throughout initialization; a partially initialized run is never exposed.
    discovery::validate_request(&request)?;
    distinct_paths(&[state_path, pack_path, receipt_path])?;
    let state_path = path_identity(state_path)?;
    let pack = path_identity(pack_path)?;
    let receipt = path_identity(receipt_path)?;
    let writer = storage::Writer::acquire(&state_path)?;
    let pack_writer = storage::Writer::acquire(&pack)?;
    let _receipt_writer = storage::Writer::acquire(&receipt)?;
    if state_path.exists() || receipt.exists() {
        return Err(error(
            "CLI_INPUT",
            "Resumable discovery requires new state and receipt paths.",
        ));
    }
    if pack_writer
        .existing()?
        .is_some_and(|p| p.environment != request.environment)
    {
        return Err(error(
            "PACK_ENVIRONMENT_MISMATCH",
            "Active pack belongs to another environment.",
        ));
    }
    let created_at = discovery::timestamp();
    let mut state = State {
        format: "cqlmatic-mcp-state".into(),
        schema_version: 3,
        discovery_algorithm: DISCOVERY_ALGORITHM.into(),
        producer_version: env!("CARGO_PKG_VERSION").into(),
        created_at: created_at.clone(),
        request_sha256: digest(&request),
        request,
        pack,
        receipt,
        status: "needs-probes".into(),
        pack_sha256: None,
        failure: None,
        answers: BTreeMap::new(),
        activation: None,
        session: None,
    };
    let mut session = Session {
        transport,
        sparse_fieldset_fallback: true,
        binding_sha256: String::new(),
        predecessor_sha256: optional_bytes(&state.pack)?.map(|bytes| sha256_hex(&bytes)),
        periods: vec![ActivePeriod {
            start: created_at,
            end: None,
        }],
        attempts: vec![],
        captures: BTreeMap::new(),
        capture_sha256: BTreeMap::new(),
    };
    session.binding_sha256 = binding(&state, &session);
    state.session = Some(session);
    let probes = pending(&state)?;
    writer.create_new_bytes(&state_bytes(&state)?, MAX_PACK_BYTES)?;
    Ok(progress(&state, "needs-probes", probes, false, vec![]))
}

pub(super) fn accepted_rows(state: &State) -> Result<BTreeMap<String, Vec<Value>>, Diagnostic> {
    let mut rows = successes(state)?;
    if let Some(session) = &state.session {
        for (id, capture) in &session.captures {
            rows.insert(id.clone(), capture.receipt.events.clone());
        }
    }
    Ok(rows)
}

fn reserve(
    writer: &storage::Writer,
    state: &mut State,
    id: &str,
    transport: Transport,
) -> Result<(), Diagnostic> {
    ensure_predecessor(state)?;
    if state.status != "needs-probes" || has_in_flight(state) {
        return Err(error(
            "DISCOVERY_BUSY",
            "No new probe can start; settle the outstanding attempt or resume the paused run first.",
        ));
    }
    let now = discovery::timestamp();
    elapsed(state, &now)?;
    let session = state.session.as_ref().ok_or_else(tampered)?;
    if session.transport != transport {
        return Err(error(
            "DISCOVERY_TRANSPORT",
            "The checkpoint belongs to a different transport.",
        ));
    }
    if session.attempts.len() >= state.request.max_queries as usize {
        return Err(error(
            "DISCOVERY_EVIDENCE",
            "Discovery attempt budget exhausted; resume does not renew it.",
        ));
    }
    let probe = discovery::frontier_with_policy(
        &state.request,
        &accepted_rows(state)?,
        session.sparse_fieldset_fallback,
    )?
    .into_iter()
    .find(|probe| discovery::probe_id(&state.request.environment.view, probe) == id)
    .ok_or_else(|| {
        error(
            "MCP_UNKNOWN_PROBE",
            "The probe is not outstanding in this checkpoint.",
        )
    })?;
    state
        .session
        .as_mut()
        .expect("session checked")
        .attempts
        .push(Attempt {
            probe,
            dispatched_at: now,
            job_id: None,
            outcome: "dispatched".into(),
            receipt: None,
            terminal: None,
            no_start_confirmed: false,
            rejected_results: vec![],
        });
    store(writer, state)
}

fn current_output(state: &State) -> Result<StepOutput, Diagnostic> {
    let status = match state.status.as_str() {
        "paused" => "paused",
        "cancel-requested" => "cancel-requested",
        "cleanup-pending" => "cleanup-pending",
        "failed" => "failed",
        "activation-blocked" => "activation-blocked",
        _ => "needs-probes",
    };
    let probes = if status == "needs-probes" && !has_in_flight(state) {
        pending(state)?
    } else {
        vec![]
    };
    let diagnostics = match status {
        "cancel-requested" => vec![error(
            "DISCOVERY_CANCEL_REQUESTED",
            "Cancel requested; current search finishing. The host owns remote cancellation and cleanup.",
        )],
        "cleanup-pending" => vec![error(
            "DISCOVERY_CLEANUP_PENDING",
            "The outstanding job must have bound terminal cleanup evidence before another attempt can start.",
        )],
        _ => vec![],
    };
    Ok(progress(state, status, probes, false, diagnostics))
}

pub fn dispatch(path: &Path, probe_id: &str) -> Result<StepOutput, Diagnostic> {
    let writer = storage::Writer::acquire(path)?;
    let mut state = read_state(path)?;
    reserve(&writer, &mut state, probe_id, Transport::Mcp)?;
    current_output(&state)
}

pub fn pause(path: &Path) -> Result<StepOutput, Diagnostic> {
    let writer = storage::Writer::acquire(path).map_err(|d| if d.code == "PACK_BUSY" {
        error("PACK_BUSY", "The executing process owns this discovery checkpoint. Use Ctrl-C in its owning terminal, then inspect the saved cleanup status; this command did not signal another process.")
    } else {d})?;
    let mut state = read_state(path)?;
    if state.session.is_none() {
        return Err(error(
            "MCP_STATE_RESTART",
            "Legacy discovery cannot be paused; preserve its original recovery workflow.",
        ));
    }
    if matches!(
        state.status.as_str(),
        "activated" | "activating" | "activation-blocked" | "failed"
    ) {
        return Err(error(
            "DISCOVERY_PHASE",
            "Activation or terminal failure cannot be converted into a pause.",
        ));
    }
    if has_in_flight(&state) {
        state.status = "cancel-requested".into();
    } else {
        state.status = "paused".into();
        state
            .session
            .as_mut()
            .expect("session checked")
            .pause_clock(&discovery::timestamp())?;
    }
    store(&writer, &state)?;
    current_output(&state)
}

pub fn resume(path: &Path) -> Result<StepOutput, Diagnostic> {
    let writer = storage::Writer::acquire(path)?;
    let mut state = read_state(path)?;
    if state.session.is_none() {
        return Err(error(
            "MCP_STATE_RESTART",
            "Legacy discovery cannot acquire a renewed resume allowance.",
        ));
    }
    ensure_predecessor(&state)?;
    if matches!(state.status.as_str(), "activated" | "activating" | "failed") {
        return advance(&writer, &mut state);
    }
    if has_in_flight(&state) {
        return Err(error(
            "DISCOVERY_CLEANUP_PENDING",
            "Settle the exact outstanding attempt before resuming; no search was started.",
        ));
    }
    if state.status == "paused" {
        if state.session.as_ref().expect("session checked").transport == Transport::Native {
            let (candidate, report) = replay_native(&state);
            if let Some(candidate) = candidate {
                return activate(&writer, &mut state, candidate, report);
            }
        }
        let now = discovery::timestamp();
        elapsed(&state, &now)?;
        let session = state.session.as_mut().expect("session checked");
        if session.periods.len() >= 4096 {
            return Err(error(
                "LOCAL_SIZE",
                "Discovery pause history reached its bounded limit.",
            ));
        }
        session.periods.push(ActivePeriod {
            start: now,
            end: None,
        });
        state.status = "needs-probes".into();
        store(&writer, &state)?;
    }
    if state
        .session
        .as_ref()
        .is_some_and(|s| s.transport == Transport::Mcp)
        && state.status == "needs-probes"
    {
        advance(&writer, &mut state)
    } else {
        current_output(&state)
    }
}

fn complete_result(raw: &[u8], probe: &Probe) -> Result<Normalized, Diagnostic> {
    let normalized = normalize(raw)?;
    if normalized.rows.len() >= 10000 {
        return Err(error(
            "SEARCH_ROW_LIMIT",
            "Result exceeds the evidence row budget.",
        ));
    }
    let rows = normalized
        .rows
        .iter()
        .cloned()
        .map(Value::Object)
        .collect::<Vec<_>>();
    discovery::check_measurement(probe, &rows)?;
    Ok(normalized)
}

pub fn step_resumable(
    path: &Path,
    answers: &[(String, PathBuf)],
) -> Result<StepOutput, Diagnostic> {
    let writer = storage::Writer::acquire(path)?;
    let mut state = read_state(path)?;
    if state.session.is_none() {
        drop(writer);
        return super::step(path, answers);
    }
    if answers.iter().any(|(id, _)| state.answers.contains_key(id)) {
        return Err(error(
            "MCP_ALREADY_ANSWERED",
            "This probe was already answered.",
        ));
    }
    if state.session.as_ref().expect("session checked").transport != Transport::Mcp {
        return Err(error(
            "DISCOVERY_TRANSPORT",
            "Native checkpoints do not accept relayed MCP answers.",
        ));
    }
    if answers.len() > 1 {
        return Err(error(
            "MCP_UNKNOWN_PROBE",
            "A resumable run settles one explicitly dispatched attempt at a time.",
        ));
    }
    if let Some((id, file)) = answers.first() {
        if !matches!(
            state.status.as_str(),
            "needs-probes" | "cancel-requested" | "cleanup-pending"
        ) {
            return Err(error(
                "DISCOVERY_PHASE",
                "A terminal or paused run cannot accept a new host response.",
            ));
        }
        let attempt = state
            .session
            .as_ref()
            .expect("session checked")
            .attempts
            .last()
            .filter(|a| matches!(a.outcome.as_str(), "dispatched" | "cleanup-pending"))
            .ok_or_else(|| {
                error(
                    "MCP_UNKNOWN_PROBE",
                    "Dispatch the exact probe before supplying its original result.",
                )
            })?;
        if discovery::probe_id(&state.request.environment.view, &attempt.probe) != *id {
            return Err(error(
                "MCP_UNKNOWN_PROBE",
                "The result does not match the dispatched probe.",
            ));
        }
        let raw = storage::read_bytes(file, MAX_CAPTURE_BYTES as u64)?;
        let now = discovery::timestamp();
        let normalized = complete_result(&raw, &attempt.probe);
        let result = match normalized {
            Ok(result) => result,
            Err(d) => {
                let a = state
                    .session
                    .as_mut()
                    .expect("session checked")
                    .attempts
                    .last_mut()
                    .expect("attempt checked");
                if a.rejected_results.len() >= 16 {
                    return Err(error(
                        "MCP_REJECTION_LIMIT",
                        "The bounded rejected-result history is full; retain the original file and settle the unresolved host attempt before continuing.",
                    ));
                }
                a.outcome = "cleanup-pending".into();
                a.rejected_results.push(RejectedResult {
                    evidence: TerminalEvidence {
                        file: path_identity(file)?,
                        raw_hex: raw_hex(&raw),
                        raw_sha256: sha256_hex(&raw),
                    },
                    diagnostic: d.clone(),
                });
                state.status = "cleanup-pending".into();
                store(&writer, &state)?;
                let mut output = current_output(&state)?;
                output.diagnostics.insert(0, d);
                return Ok(output);
            }
        };
        if let Err(d) = elapsed(&state, &now) {
            if d.code != "MCP_DEADLINE" {
                return Err(d);
            }
            let session = state.session.as_mut().expect("session checked");
            let attempt = session.attempts.last_mut().expect("attempt checked");
            attempt.outcome = "completed".into();
            attempt.terminal = Some(TerminalEvidence {
                file: path_identity(file)?,
                raw_hex: raw_hex(&raw),
                raw_sha256: sha256_hex(&raw),
            });
            session.pause_clock(&now)?;
            return fail_state(&writer, &mut state, d);
        }
        let probe = state
            .session
            .as_ref()
            .expect("session checked")
            .attempts
            .last()
            .expect("attempt checked")
            .probe
            .clone();
        state.answers.insert(
            id.clone(),
            Answer {
                query: probe.query,
                start: probe.start,
                end: probe.end,
                repository: state.request.environment.view.clone(),
                outcome: "complete".into(),
                trace_id: result.trace_id,
                raw_hex: raw_hex(&raw),
                raw_sha256: sha256_hex(&raw),
                ingested_at: now.clone(),
                diagnostic: None,
            },
        );
        state
            .session
            .as_mut()
            .expect("session checked")
            .attempts
            .last_mut()
            .expect("attempt checked")
            .outcome = "complete".into();
        if matches!(
            state.status.as_str(),
            "cancel-requested" | "cleanup-pending"
        ) {
            state.status = "paused".into();
            state
                .session
                .as_mut()
                .expect("session checked")
                .pause_clock(&now)?;
        }
        store(&writer, &state)?;
    }
    if matches!(
        state.status.as_str(),
        "paused" | "cancel-requested" | "cleanup-pending"
    ) || has_in_flight(&state)
    {
        return current_output(&state);
    }
    ensure_predecessor(&state)?;
    advance(&writer, &mut state)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DelegatedCancellation {
    schema_version: u32,
    request_sha256: String,
    probe_id: String,
    repository: String,
    query_sha256: String,
    start: String,
    end: String,
    dispatched_at: String,
    outcome: String,
    cleanup: String,
    caller_assertion: String,
}

/// Imports protected caller-attested evidence; this does not independently stop a hosted job.
pub fn settle_cancelled(
    path: &Path,
    probe_id: &str,
    evidence_file: &Path,
) -> Result<StepOutput, Diagnostic> {
    let writer = storage::Writer::acquire(path)?;
    let mut state = read_state(path)?;
    let session = state.session.as_ref().ok_or_else(tampered)?;
    if session.transport != Transport::Mcp {
        return Err(error(
            "DISCOVERY_TRANSPORT",
            "Native job cleanup must use its exact saved API job ID.",
        ));
    }
    let attempt = session
        .attempts
        .last()
        .filter(|a| matches!(a.outcome.as_str(), "dispatched" | "cleanup-pending"))
        .ok_or_else(|| {
            error(
                "DISCOVERY_PHASE",
                "No dispatched hosted attempt needs cancellation evidence.",
            )
        })?;
    let raw = storage::read_bytes(evidence_file, MAX_RESULT_BYTES as u64)?;
    let evidence: DelegatedCancellation = serde_json::from_slice(&raw).map_err(|_| {
        error(
            "MCP_CLEANUP_EVIDENCE",
            "Invalid delegated cancellation report.",
        )
    })?;
    if evidence.schema_version != 1
        || evidence.request_sha256 != state.request_sha256
        || evidence.probe_id != probe_id
        || discovery::probe_id(&state.request.environment.view, &attempt.probe) != probe_id
        || evidence.repository != state.request.environment.view
        || evidence.query_sha256 != sha256_hex(attempt.probe.query.as_bytes())
        || evidence.start != attempt.probe.start
        || evidence.end != attempt.probe.end
        || evidence.dispatched_at != attempt.dispatched_at
        || evidence.outcome != "cancelled"
        || evidence.cleanup != "confirmed"
        || evidence.caller_assertion != "exact-host-attempt-terminal-cleanup"
    {
        return Err(error(
            "MCP_CLEANUP_EVIDENCE",
            "Cancellation evidence must attest terminal cleanup of this exact host attempt, query, target and window.",
        ));
    }
    let session = state.session.as_mut().expect("session checked");
    let attempt = session.attempts.last_mut().expect("attempt checked");
    attempt.outcome = "cancelled".into();
    attempt.terminal = Some(TerminalEvidence {
        file: path_identity(evidence_file)?,
        raw_hex: raw_hex(&raw),
        raw_sha256: sha256_hex(&raw),
    });
    session.pause_clock(&discovery::timestamp())?;
    state.status = "paused".into();
    store(&writer, &state)?;
    current_output(&state)
}

struct NativeBackend<'a, B> {
    writer: &'a storage::Writer,
    state: &'a mut State,
    backend: &'a mut B,
    replay: Option<String>,
}

struct CapturedBackend<'a>(&'a BTreeMap<String, discovery::Capture>);

impl SearchBackend for CapturedBackend<'_> {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        _: Instant,
    ) -> Result<String, Diagnostic> {
        let id = discovery::probe_id_for(view, query, start, end);
        if !self.0.contains_key(&id) {
            return Err(error(
                "DISCOVERY_CAPTURE_REQUIRED",
                "The next native measurement has no accepted capture.",
            ));
        }
        Ok(id)
    }
    fn poll(&mut self, _: &str, id: &str, _: Instant) -> Result<Value, Diagnostic> {
        let capture = self.0.get(id).ok_or_else(tampered)?;
        Ok(
            json!({"done":true,"cancelled":false,"events":capture.receipt.events,
            "metaData":capture.receipt.metadata.clone().unwrap_or_else(||json!({}))}),
        )
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        Ok(())
    }
    fn observed_at(&self, id: &str) -> Option<String> {
        self.0.get(id).map(|capture| capture.retrieved_at.clone())
    }
}

fn replay_native(state: &State) -> (Option<Pack>, DiscoverReport) {
    let session = state.session.as_ref().expect("native session checked");
    let (candidate, mut report) = discovery::execute_with_policy(
        &state.request,
        &mut CapturedBackend(&session.captures),
        Origin::Api,
        Duration::from_secs(state.request.max_seconds),
        session.sparse_fieldset_fallback,
    );
    if candidate.is_some() {
        for capture in &mut report.captures {
            let id = discovery::probe_id(&state.request.environment.view, &capture.probe);
            *capture = session
                .captures
                .get(&id)
                .expect("capture-only replay uses accepted probes")
                .clone();
        }
    }
    (candidate, report)
}

impl<B: SearchBackend> SearchBackend for NativeBackend<'_, B> {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        deadline: std::time::Instant,
    ) -> Result<String, Diagnostic> {
        let id = discovery::probe_id_for(view, query, start, end);
        if let Some(capture) = self
            .state
            .session
            .as_ref()
            .expect("session checked")
            .captures
            .get(&id)
        {
            self.replay = Some(id);
            return capture.receipt.job_id.clone().ok_or_else(tampered);
        }
        self.replay = None;
        reserve(self.writer, self.state, &id, Transport::Native)?;
        match self.backend.start(view, query, start, end, deadline) {
            Ok(job) => {
                let attempt = self
                    .state
                    .session
                    .as_mut()
                    .expect("session checked")
                    .attempts
                    .last_mut()
                    .expect("reserved");
                attempt.job_id = Some(job.clone());
                attempt.outcome = "started".into();
                if let Err(d) = store(self.writer, self.state) {
                    let cleanup = self.backend.stop(
                        view,
                        &job,
                        std::time::Instant::now() + Duration::from_secs(5),
                    );
                    self.state
                        .session
                        .as_mut()
                        .expect("session checked")
                        .attempts
                        .last_mut()
                        .expect("reserved")
                        .outcome = if cleanup.is_ok() {
                        "cancelled"
                    } else {
                        "cleanup-pending"
                    }
                    .into();
                    let _ = store(self.writer, self.state);
                    return Err(d);
                }
                Ok(job)
            }
            Err(d) => {
                let attempt = self
                    .state
                    .session
                    .as_mut()
                    .expect("session checked")
                    .attempts
                    .last_mut()
                    .expect("reserved");
                attempt.outcome = if matches!(
                    d.code.as_str(),
                    "HTTP_START_AMBIGUOUS" | "HTTP_TRANSPORT" | "HTTP_STATUS"
                ) {
                    "ambiguous"
                } else {
                    attempt.no_start_confirmed = true;
                    "failed"
                }
                .into();
                store(self.writer, self.state)?;
                Err(d)
            }
        }
    }
    fn poll(
        &mut self,
        view: &str,
        job: &str,
        deadline: std::time::Instant,
    ) -> Result<Value, Diagnostic> {
        if let Some(id) = &self.replay {
            let capture = self
                .state
                .session
                .as_ref()
                .expect("session checked")
                .captures
                .get(id)
                .ok_or_else(tampered)?;
            return Ok(
                json!({"done":true,"cancelled":false,"events":capture.receipt.events,"metaData":capture.receipt.metadata.clone().unwrap_or_else(||json!({}))}),
            );
        }
        self.backend.poll(view, job, deadline)
    }
    fn stop(
        &mut self,
        view: &str,
        job: &str,
        deadline: std::time::Instant,
    ) -> Result<(), Diagnostic> {
        if self.replay.is_some() {
            return Ok(());
        }
        let result = self.backend.stop(view, job, deadline);
        let attempt = self
            .state
            .session
            .as_mut()
            .expect("session checked")
            .attempts
            .last_mut()
            .ok_or_else(tampered)?;
        if attempt.job_id.as_deref() != Some(job) {
            return Err(tampered());
        }
        attempt.outcome = if result.is_ok() {
            "cancelled"
        } else {
            "cleanup-pending"
        }
        .into();
        store(self.writer, self.state)?;
        result
    }
    fn observed_at(&self, job: &str) -> Option<String> {
        self.replay
            .as_ref()
            .and_then(|id| self.state.session.as_ref()?.captures.get(id))
            .map(|c| c.retrieved_at.clone())
            .or_else(|| self.backend.observed_at(job))
    }
    fn checkpoint(
        &mut self,
        view: &str,
        receipt: &JobReceipt,
        retrieved_at: &str,
    ) -> Result<(), Diagnostic> {
        let id = discovery::probe_id_for(view, &receipt.query, &receipt.start, &receipt.end);
        if self
            .state
            .session
            .as_ref()
            .expect("session checked")
            .captures
            .contains_key(&id)
        {
            return Ok(());
        }
        if receipt.job_id.is_none()
            && self
                .state
                .session
                .as_ref()
                .expect("session checked")
                .attempts
                .last()
                .is_none_or(|attempt| discovery::probe_id(view, &attempt.probe) != id)
        {
            return Ok(());
        }
        let budget = elapsed(self.state, &discovery::timestamp());
        let session = self.state.session.as_mut().expect("session checked");
        let attempt = session.attempts.last_mut().ok_or_else(tampered)?;
        if discovery::probe_id(view, &attempt.probe) != id {
            return Err(tampered());
        }
        attempt.receipt = Some(receipt.clone());
        if receipt.job_id.is_none() && attempt.job_id.is_some() {
            return store(self.writer, self.state);
        }
        if receipt.completion == "complete"
            && receipt.cleanup == "confirmed"
            && let Err(d) = budget
        {
            attempt.outcome = "failed".into();
            store(self.writer, self.state)?;
            return Err(d);
        }
        if receipt.completion == "complete" && receipt.cleanup == "confirmed" {
            discovery::check_measurement(&attempt.probe, &receipt.events)?;
            if environment::parse_time(retrieved_at)? < environment::parse_time(&attempt.probe.end)?
            {
                return Err(error(
                    "DISCOVERY_EVIDENCE",
                    "Capture predates its measurement window.",
                ));
            }
            attempt.outcome = "complete".into();
            let capture = discovery::Capture {
                probe: attempt.probe.clone(),
                retrieved_at: retrieved_at.into(),
                receipt: receipt.clone(),
            };
            session.capture_sha256.insert(
                id.clone(),
                sha256_hex(&serde_json::to_vec(&capture).map_err(|_| tampered())?),
            );
            session.captures.insert(id, capture);
        } else if receipt.cleanup == "unconfirmed" || receipt.cleanup == "unknown-job" {
            attempt.outcome = "cleanup-pending".into();
        } else if receipt.completion == "cancelled" {
            attempt.outcome = "cancelled".into();
        } else {
            attempt.outcome = "failed".into();
        }
        store(self.writer, self.state)
    }
}

pub fn recover_native_cleanup(
    path: &Path,
    backend: &mut impl SearchBackend,
) -> Result<StepOutput, Diagnostic> {
    if backend.delegated_lifecycle() {
        return Err(error(
            "DISCOVERY_TRANSPORT",
            "Native cleanup requires an API-owned job lifecycle.",
        ));
    }
    let writer = storage::Writer::acquire(path)?;
    let mut state = read_state(path)?;
    if state
        .session
        .as_ref()
        .is_none_or(|s| s.transport != Transport::Native)
        || !has_in_flight(&state)
    {
        return Err(error(
            "DISCOVERY_PHASE",
            "No native job needs cleanup recovery.",
        ));
    }
    let job = state
        .session
        .as_ref()
        .expect("session checked")
        .attempts
        .last()
        .expect("in flight")
        .job_id
        .clone()
        .ok_or_else(|| {
            error(
                "DISCOVERY_CLEANUP_PENDING",
                "The start outcome has no known job ID; automatic cleanup or retry is unsafe.",
            )
        })?;
    let result = backend.stop(
        &state.request.environment.view,
        &job,
        std::time::Instant::now() + Duration::from_secs(5),
    );
    let session = state.session.as_mut().expect("session checked");
    let attempt = session.attempts.last_mut().expect("in flight");
    attempt.outcome = if result.is_ok() {
        "cancelled"
    } else {
        "cleanup-pending"
    }
    .into();
    if result.is_ok() {
        session.pause_clock(&discovery::timestamp())?;
        state.status = "paused".into();
    } else {
        state.status = "cleanup-pending".into();
    }
    store(&writer, &state)?;
    current_output(&state)
}

pub fn run_native(path: &Path, backend: &mut impl SearchBackend) -> Result<StepOutput, Diagnostic> {
    if backend.delegated_lifecycle() {
        return Err(error(
            "DISCOVERY_TRANSPORT",
            "A native run requires API-owned job cleanup, not a delegated MCP lifecycle.",
        ));
    }
    let writer = storage::Writer::acquire(path)?;
    let mut state = read_state(path)?;
    ensure_predecessor(&state)?;
    if state
        .session
        .as_ref()
        .is_none_or(|s| s.transport != Transport::Native)
    {
        return Err(error(
            "DISCOVERY_TRANSPORT",
            "This is not a resumable native checkpoint.",
        ));
    }
    if matches!(state.status.as_str(), "activated" | "activating" | "failed") {
        return advance(&writer, &mut state);
    }
    if has_in_flight(&state) {
        state.status = "cleanup-pending".into();
        store(&writer, &state)?;
        return current_output(&state);
    }
    if state.status == "paused" {
        return current_output(&state);
    }
    let (candidate, report) = replay_native(&state);
    if let Some(candidate) = candidate {
        return activate(&writer, &mut state, candidate, report);
    }
    if state.status == "activation-blocked" {
        return Ok(progress(
            &state,
            "activation-blocked",
            vec![],
            false,
            report.diagnostics,
        ));
    }
    let remaining = elapsed(&state, &discovery::timestamp())?;
    let request = state.request.clone();
    let (candidate, mut report) = {
        let mut checkpoint = NativeBackend {
            writer: &writer,
            state: &mut state,
            backend,
            replay: None,
        };
        let sparse_fieldset_fallback = checkpoint
            .state
            .session
            .as_ref()
            .expect("session checked")
            .sparse_fieldset_fallback;
        discovery::execute_with_policy(
            &request,
            &mut checkpoint,
            Origin::Api,
            remaining,
            sparse_fieldset_fallback,
        )
    };
    if has_in_flight(&state) {
        state.status = "cleanup-pending".into();
        store(&writer, &state)?;
        return current_output(&state);
    }
    if network::CANCELLED.load(Ordering::Relaxed) {
        state.status = "paused".into();
        state
            .session
            .as_mut()
            .expect("session checked")
            .pause_clock(&discovery::timestamp())?;
        store(&writer, &state)?;
        return current_output(&state);
    }
    if let Some(candidate) = candidate {
        for capture in &mut report.captures {
            let id = discovery::probe_id(&request.environment.view, &capture.probe);
            *capture = state
                .session
                .as_ref()
                .expect("session checked")
                .captures
                .get(&id)
                .ok_or_else(tampered)?
                .clone();
        }
        ensure_predecessor(&state)?;
        return activate(&writer, &mut state, candidate, report);
    }
    let diagnostic = report
        .diagnostics
        .into_iter()
        .next()
        .unwrap_or_else(|| error("DISCOVERY_EVIDENCE", "Native discovery did not complete."));
    fail_state(&writer, &mut state, diagnostic)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paused_intervals_preserve_the_exact_remaining_allowance() {
        let mut request: DiscoverRequest =
            serde_json::from_str(include_str!("../examples/mcp/discover.request.json")).unwrap();
        request.max_seconds = 120;
        let state = State {
            format: "cqlmatic-mcp-state".into(),
            schema_version: 3,
            discovery_algorithm: DISCOVERY_ALGORITHM.into(),
            producer_version: env!("CARGO_PKG_VERSION").into(),
            created_at: "2026-01-01T00:00:00Z".into(),
            request_sha256: digest(&request),
            request,
            pack: PathBuf::new(),
            receipt: PathBuf::new(),
            status: "paused".into(),
            pack_sha256: None,
            failure: None,
            answers: BTreeMap::new(),
            activation: None,
            session: None,
        };
        let mut session = Session {
            transport: Transport::Mcp,
            sparse_fieldset_fallback: false,
            binding_sha256: String::new(),
            predecessor_sha256: None,
            periods: vec![ActivePeriod {
                start: state.created_at.clone(),
                end: Some("2026-01-01T00:01:00.000000001Z".into()),
            }],
            attempts: vec![],
            captures: BTreeMap::new(),
            capture_sha256: BTreeMap::new(),
        };
        assert_eq!(
            session.remaining(&state, "2026-02-01T00:00:00Z").unwrap(),
            Duration::from_nanos(59_999_999_999)
        );
        session.periods.push(ActivePeriod {
            start: "2026-02-01T00:00:00Z".into(),
            end: None,
        });
        assert_eq!(
            session.remaining(&state, "2026-02-01T00:00:59Z").unwrap(),
            Duration::from_nanos(999_999_999)
        );
        assert_eq!(
            session
                .remaining(&state, "2026-02-01T00:01:00Z")
                .unwrap_err()
                .code,
            "MCP_DEADLINE"
        );
        assert!(session.pause_clock("2026-01-31T23:59:59Z").is_err());
    }
}
