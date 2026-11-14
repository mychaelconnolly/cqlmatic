use cqlmatic::{
    contracts::Diagnostic,
    discovery::{self, DiscoverRequest},
    mcp::{self, Transport},
    network::{self, SearchBackend},
    sha256_hex, storage,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

static SIGNAL: Mutex<()> = Mutex::new(());
struct Run {
    directory: PathBuf,
    state: PathBuf,
    pack: PathBuf,
    receipt: PathBuf,
}
impl Run {
    fn new(transport: Transport, max_queries: u32) -> Self {
        let request:DiscoverRequest=serde_json::from_value(json!({"schema_version":1,"environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"synthetic_view"},"start":"2026-01-01T00:00:00Z","end":"2026-01-01T01:00:00Z","sources":[{"repo":"one"},{"type":"two"}],"depth":"sources-only","max_queries":max_queries,"max_seconds":120})).unwrap();
        Self::with_request(transport, request)
    }
    fn with_request(transport: Transport, request: DiscoverRequest) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "cqlmatic-resume-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let run = Self {
            state: directory.join("state.json"),
            pack: directory.join("pack.json"),
            receipt: directory.join("receipt.json"),
            directory,
        };
        mcp::init_resumable(request, &run.state, &run.pack, &run.receipt, transport).unwrap();
        run
    }
    fn saved(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.state).unwrap()).unwrap()
    }
    fn save(&self, value: &Value) {
        storage::Writer::acquire(&self.state)
            .unwrap()
            .commit_bytes(&serde_json::to_vec(value).unwrap(), 64 * 1024 * 1024)
            .unwrap();
    }
    fn probe(&self) -> String {
        mcp::step_resumable(&self.state, &[]).unwrap().probes[0]["probe_id"]
            .as_str()
            .unwrap()
            .into()
    }
    fn answer(&self, id: &str) -> mcp::StepOutput {
        let path = self.directory.join(format!("{id}.json"));
        fs::write(&path, br#"{"result":[{"events":"10"}]}"#).unwrap();
        mcp::step_resumable(&self.state, &[(id.into(), path)]).unwrap()
    }
}
impl Drop for Run {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[derive(Default)]
struct Fake {
    state: PathBuf,
    starts: Vec<String>,
    polls: usize,
    stops: usize,
    cancel_after: Option<usize>,
    cancel_poll: bool,
    fail_cleanup: bool,
    ambiguous: bool,
    start_diagnostic: Option<&'static str>,
    interrupt_poll: bool,
    reuse_job_id: bool,
    answers: std::collections::BTreeMap<String, Vec<Value>>,
}
impl Fake {
    fn for_run(run: &Run) -> Self {
        Self {
            state: run.state.clone(),
            ..Self::default()
        }
    }
}
impl SearchBackend for Fake {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        _: Instant,
    ) -> Result<String, Diagnostic> {
        let saved: Value = serde_json::from_slice(&fs::read(&self.state).unwrap()).unwrap();
        let attempt = saved["session"]["attempts"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(attempt["outcome"], "dispatched");
        assert_eq!(attempt["probe"]["query"], query);
        self.starts
            .push(discovery::probe_id_for(view, query, start, end));
        if let Some(code) = self.start_diagnostic {
            return Err(Diagnostic::error(
                code,
                "Synthetic definite preflight rejection",
            ));
        }
        if self.ambiguous {
            return Err(Diagnostic::error(
                "HTTP_START_AMBIGUOUS",
                "Synthetic ambiguous start",
            ));
        }
        Ok(format!(
            "native-job-{}",
            if self.reuse_job_id {
                1
            } else {
                self.starts.len()
            }
        ))
    }
    fn poll(&mut self, _: &str, job: &str, _: Instant) -> Result<Value, Diagnostic> {
        self.polls += 1;
        let saved: Value = serde_json::from_slice(&fs::read(&self.state).unwrap()).unwrap();
        let attempt = saved["session"]["attempts"]
            .as_array()
            .unwrap()
            .last()
            .unwrap();
        assert_eq!(attempt["job_id"], job);
        assert_eq!(attempt["outcome"], "started");
        if self.interrupt_poll {
            self.interrupt_poll = false;
            panic!("Synthetic interrupted owner after durable job ID");
        }
        if self.cancel_poll {
            self.cancel_poll = false;
            network::CANCELLED.store(true, Ordering::Relaxed);
            return Ok(json!({"done":false,"cancelled":false,"events":[]}));
        }
        let rows = self
            .starts
            .last()
            .and_then(|id| self.answers.get(id))
            .cloned()
            .unwrap_or_else(|| vec![json!({"events":"10"})]);
        Ok(json!({"done":true,"cancelled":false,"metaData":{},"events":rows}))
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        self.stops += 1;
        if self.fail_cleanup {
            return Err(Diagnostic::error(
                "HTTP_TRANSPORT",
                "Synthetic failed cleanup",
            ));
        }
        if self.cancel_after == Some(self.stops) {
            network::CANCELLED.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
    fn observed_at(&self, job: &str) -> Option<String> {
        let offset: i64 = job.strip_prefix("native-job-")?.parse().ok()?;
        let base = time::OffsetDateTime::parse(
            "2026-01-01T01:01:00Z",
            &time::format_description::well_known::Rfc3339,
        )
        .ok()?;
        (base + time::Duration::seconds(offset))
            .format(&time::format_description::well_known::Rfc3339)
            .ok()
    }
}

#[test]
fn native_resume_preserves_origin_and_accepted_captures_without_remote_replay() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let interrupted = Run::new(Transport::Native, 4);
    let mut backend = Fake::for_run(&interrupted);
    backend.cancel_after = Some(1);
    assert_eq!(
        mcp::run_native(&interrupted.state, &mut backend)
            .unwrap()
            .status,
        "paused"
    );
    assert_eq!(
        interrupted.saved()["session"]["captures"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    assert!(!interrupted.pack.exists());
    network::CANCELLED.store(false, Ordering::Relaxed);
    mcp::resume(&interrupted.state).unwrap();
    assert_eq!(
        mcp::run_native(&interrupted.state, &mut backend)
            .unwrap()
            .status,
        "complete"
    );
    assert_eq!(backend.starts.len(), 2);
    assert_eq!(backend.stops, 2);
    assert_ne!(backend.starts[0], backend.starts[1]);
    let uninterrupted = Run::new(Transport::Native, 4);
    let mut other = Fake::for_run(&uninterrupted);
    assert_eq!(
        mcp::run_native(&uninterrupted.state, &mut other)
            .unwrap()
            .status,
        "complete"
    );
    let a: Value = serde_json::from_slice(&fs::read(&interrupted.pack).unwrap()).unwrap();
    let b: Value = serde_json::from_slice(&fs::read(&uninterrupted.pack).unwrap()).unwrap();
    assert_eq!(a["data"], b["data"]);
    let receipt: Value = serde_json::from_slice(&fs::read(&interrupted.receipt).unwrap()).unwrap();
    assert_eq!(receipt["mode"], "native");
    for capture in receipt["captures"].as_array().unwrap() {
        assert_eq!(capture["receipt"]["cleanup"], "confirmed");
        assert!(
            capture["receipt"]["job_id"]
                .as_str()
                .unwrap()
                .starts_with("native-job-")
        );
    }
    assert!(a.to_string().contains("ngsiem-api"));
    assert!(!a.to_string().contains("mcp-observation"));
    assert_eq!(
        mcp::run_native(&interrupted.state, &mut backend)
            .unwrap()
            .status,
        "complete"
    );
    assert_eq!(backend.starts.len(), 2);
}

#[test]
fn full_native_measurement_graph_resumes_without_reissuing_accepted_probes() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let request: DiscoverRequest =
        serde_json::from_str(include_str!("../examples/mcp/discover.request.json")).unwrap();
    let mut answers = std::collections::BTreeMap::new();
    loop {
        let probes = discovery::frontier(&request, &answers).unwrap();
        if probes.is_empty() {
            break;
        }
        for probe in probes {
            let raw = match (probe.kind.as_str(), probe.field.as_deref()) {
                ("volume", _) | ("tag-detect", Some("event_simpleName")) => {
                    include_str!("../examples/mcp/count.json")
                }
                ("tag-detect", _) => r#"{"result":[{"events":"0"}]}"#,
                ("dataset-facet", _) => include_str!("../examples/mcp/dataset.json"),
                ("fieldset", _) => include_str!("../examples/mcp/fieldset.json"),
                ("population", _) => include_str!("../examples/mcp/population.json"),
                ("value-facet", _) => include_str!("../examples/mcp/value.json"),
                ("record-kind-facet", Some("#type")) => include_str!("../examples/mcp/kind.json"),
                ("record-kind-facet", Some("#event_simpleName")) => {
                    include_str!("../examples/mcp/simple-name.json")
                }
                _ => panic!("Unexpected synthetic probe {probe:?}"),
            };
            answers.insert(
                discovery::probe_id(&request.environment.view, &probe),
                mcp::normalize(raw.as_bytes())
                    .unwrap()
                    .rows
                    .into_iter()
                    .map(Value::Object)
                    .collect(),
            );
        }
    }
    let run = Run::with_request(Transport::Native, request);
    let mut backend = Fake::for_run(&run);
    backend.answers = answers;
    backend.cancel_after = Some(4);
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "paused"
    );
    assert_eq!(backend.starts.len(), 4);
    network::CANCELLED.store(false, Ordering::Relaxed);
    mcp::resume(&run.state).unwrap();
    let complete = mcp::run_native(&run.state, &mut backend).unwrap();
    assert_eq!(complete.status, "complete", "{:?}", complete.diagnostics);
    assert_eq!(backend.starts.len(), backend.answers.len());
    assert_eq!(
        backend
            .starts
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        backend.starts.len()
    );
    let pack: Value = serde_json::from_slice(&fs::read(&run.pack).unwrap()).unwrap();
    assert!(!pack["data"]["fields"].as_array().unwrap().is_empty());
    assert!(pack.to_string().contains("ngsiem-api"));
    assert!(!pack.to_string().contains("mcp-observation"));
}

#[test]
fn native_cancel_before_and_during_probe_charges_only_real_attempts() {
    let _lock = SIGNAL.lock().unwrap();
    let run = Run::new(Transport::Native, 3);
    let mut backend = Fake::for_run(&run);
    network::CANCELLED.store(true, Ordering::Relaxed);
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "paused"
    );
    assert!(backend.starts.is_empty());
    network::CANCELLED.store(false, Ordering::Relaxed);
    mcp::resume(&run.state).unwrap();
    backend.cancel_poll = true;
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "paused"
    );
    assert_eq!(backend.stops, 1);
    assert_eq!(
        run.saved()["session"]["attempts"][0]["outcome"],
        "cancelled"
    );
    assert!(
        run.saved()["session"]["captures"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    network::CANCELLED.store(false, Ordering::Relaxed);
    mcp::resume(&run.state).unwrap();
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "complete"
    );
    assert_eq!(backend.starts.len(), 3);
    assert_eq!(backend.starts[0], backend.starts[1]);
}

#[test]
fn uncertain_start_and_unconfirmed_cleanup_block_automatic_retry() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Native, 4);
    let mut backend = Fake::for_run(&run);
    backend.ambiguous = true;
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "cleanup-pending"
    );
    assert!(mcp::resume(&run.state).is_err());
    assert!(mcp::recover_native_cleanup(&run.state, &mut backend).is_err());
    assert_eq!(backend.starts.len(), 1);
    assert_eq!(backend.stops, 0);
    let run = Run::new(Transport::Native, 4);
    let mut backend = Fake::for_run(&run);
    backend.cancel_poll = true;
    backend.fail_cleanup = true;
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "cleanup-pending"
    );
    assert!(mcp::resume(&run.state).is_err());
    network::CANCELLED.store(false, Ordering::Relaxed);
    backend.fail_cleanup = false;
    assert_eq!(
        mcp::recover_native_cleanup(&run.state, &mut backend)
            .unwrap()
            .status,
        "paused"
    );
    assert_eq!(backend.starts.len(), 1);
    assert_eq!(backend.stops, 2);
}

#[test]
fn interrupted_native_owner_requires_exact_known_job_cleanup_before_retry() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Native, 3);
    let mut backend = Fake::for_run(&run);
    backend.interrupt_poll = true;
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mcp::run_native(
            &run.state,
            &mut backend
        )))
        .is_err()
    );
    assert_eq!(
        run.saved()["session"]["attempts"][0]["job_id"],
        "native-job-1"
    );
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "cleanup-pending"
    );
    assert_eq!(backend.starts.len(), 1);
    assert_eq!(
        mcp::recover_native_cleanup(&run.state, &mut backend)
            .unwrap()
            .status,
        "paused"
    );
    assert_eq!(backend.stops, 1);
    mcp::resume(&run.state).unwrap();
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "complete"
    );
    assert_eq!(backend.starts.len(), 3);
}

#[test]
fn mcp_pause_waits_for_exact_dispatched_result_and_never_activates_partial_pack() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Mcp, 2);
    let id = run.probe();
    assert!(
        mcp::step_resumable(&run.state, &[(id.clone(), run.directory.join("absent"))]).is_err()
    );
    mcp::dispatch(&run.state, &id).unwrap();
    assert!(mcp::dispatch(&run.state, &id).is_err());
    assert_eq!(mcp::pause(&run.state).unwrap().status, "cancel-requested");
    assert!(mcp::resume(&run.state).is_err());
    assert_eq!(run.answer(&id).status, "paused");
    assert!(!run.pack.exists());
    let saved = run.saved()["answers"][&id].clone();
    let next = mcp::resume(&run.state).unwrap();
    let other = next.probes[0]["probe_id"].as_str().unwrap();
    assert_ne!(other, id);
    mcp::dispatch(&run.state, other).unwrap();
    assert_eq!(run.answer(other).status, "complete");
    assert_eq!(run.saved()["answers"][&id], saved);
}

#[test]
fn delegated_cancel_proof_is_attempt_bound_and_retries_consume_budget() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Mcp, 2);
    let id = run.probe();
    mcp::dispatch(&run.state, &id).unwrap();
    mcp::pause(&run.state).unwrap();
    let state = run.saved();
    let a = &state["session"]["attempts"][0];
    let proof = run.directory.join("cancel.json");
    let mut evidence = json!({"schema_version":1,"request_sha256":state["request_sha256"],"probe_id":id,"repository":"synthetic_view","query_sha256":sha256_hex(a["probe"]["query"].as_str().unwrap().as_bytes()),"start":a["probe"]["start"],"end":a["probe"]["end"],"dispatched_at":a["dispatched_at"],"outcome":"cancelled","cleanup":"confirmed","caller_assertion":"exact-host-attempt-terminal-cleanup"});
    evidence["repository"] = json!("foreign");
    fs::write(&proof, serde_json::to_vec(&evidence).unwrap()).unwrap();
    assert!(mcp::settle_cancelled(&run.state, &id, &proof).is_err());
    evidence["repository"] = json!("synthetic_view");
    fs::write(&proof, serde_json::to_vec(&evidence).unwrap()).unwrap();
    assert_eq!(
        mcp::settle_cancelled(&run.state, &id, &proof)
            .unwrap()
            .status,
        "paused"
    );
    mcp::resume(&run.state).unwrap();
    mcp::dispatch(&run.state, &id).unwrap();
    let output = run.answer(&id);
    assert_eq!(output.status, "failed");
    assert_eq!(mcp::resumable_info(&run.state).unwrap()["attempts"], 2);
    assert!(!run.pack.exists());
}

#[test]
fn request_mutations_fail_closed() {
    let run = Run::new(Transport::Mcp, 3);
    mcp::pause(&run.state).unwrap();
    let mut foreign = run.saved();
    foreign["request"]["environment"]["view"] = json!("foreign");
    run.save(&foreign);
    assert!(mcp::resume(&run.state).is_err());
}

#[test]
fn independent_cancel_does_not_signal_or_modify_an_owned_checkpoint() {
    let run = Run::new(Transport::Native, 3);
    let before = fs::read(&run.state).unwrap();
    let _owner = storage::Writer::acquire(&run.state).unwrap();
    let diagnostic = mcp::pause(&run.state).unwrap_err();
    assert_eq!(diagnostic.code, "PACK_BUSY");
    assert!(diagnostic.message.contains("Ctrl-C"));
    assert_eq!(fs::read(&run.state).unwrap(), before);
}

#[test]
fn native_job_ids_are_not_used_as_probe_cache_keys() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Native, 2);
    let mut backend = Fake::for_run(&run);
    backend.reuse_job_id = true;
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "complete"
    );
    assert_eq!(backend.starts.len(), 2);
    assert_eq!(backend.stops, 2);
}

fn replace_starting_pack(run: &Run) {
    let request = mcp::resumable_request(&run.state).unwrap();
    let pack = cqlmatic::environment::Pack::new(
        request.environment,
        discovery::timestamp(),
        Default::default(),
    )
    .unwrap();
    storage::Writer::acquire(&run.pack)
        .unwrap()
        .commit(&pack)
        .unwrap();
}

#[test]
fn changed_pack_does_not_prevent_known_job_cleanup_or_allow_resume() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Native, 3);
    let mut backend = Fake::for_run(&run);
    backend.interrupt_poll = true;
    assert!(
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mcp::run_native(
            &run.state,
            &mut backend
        )))
        .is_err()
    );
    replace_starting_pack(&run);
    assert_eq!(
        mcp::recover_native_cleanup(&run.state, &mut backend)
            .unwrap()
            .status,
        "paused"
    );
    assert_eq!(backend.stops, 1);
    assert!(mcp::resume(&run.state).is_err());
    assert_eq!(backend.starts.len(), 1);
}

#[test]
fn changed_pack_keeps_the_host_capture_but_blocks_further_searches() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Mcp, 3);
    let id = run.probe();
    mcp::dispatch(&run.state, &id).unwrap();
    replace_starting_pack(&run);
    let file = run.directory.join("result.json");
    fs::write(&file, br#"{"result":[{"events":"10"}]}"#).unwrap();
    assert_eq!(
        mcp::step_resumable(&run.state, &[(id.clone(), file)])
            .unwrap_err()
            .code,
        "MCP_ACTIVATION_CONFLICT"
    );
    assert_eq!(run.saved()["answers"][&id]["outcome"], "complete");
    assert_eq!(mcp::resumable_info(&run.state).unwrap()["in_flight"], false);
    assert!(mcp::resume(&run.state).is_err());
}

fn cancellation_proof(run: &Run, id: &str) -> PathBuf {
    let saved = run.saved();
    let attempt = saved["session"]["attempts"]
        .as_array()
        .unwrap()
        .last()
        .unwrap();
    let proof = run.directory.join("cleanup.json");
    fs::write(&proof,serde_json::to_vec(&json!({"schema_version":1,"request_sha256":saved["request_sha256"],"probe_id":id,"repository":"synthetic_view","query_sha256":sha256_hex(attempt["probe"]["query"].as_str().unwrap().as_bytes()),"start":attempt["probe"]["start"],"end":attempt["probe"]["end"],"dispatched_at":attempt["dispatched_at"],"outcome":"cancelled","cleanup":"confirmed","caller_assertion":"exact-host-attempt-terminal-cleanup"})).unwrap()).unwrap();
    proof
}

#[test]
fn rejected_mcp_results_remain_in_flight_until_exact_cleanup_and_charged_retry() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for (raw, code) in [
        (
            br#"{"isError":true,"content":[{"type":"text","text":"request timed out"}]}"#
                .as_slice(),
            "MCP_DEADLINE",
        ),
        (br#"{"unexpected":"synthetic"}"#.as_slice(), "MCP_SHAPE"),
        (
            br#"{"result":[],"error":"synthetic tool error"}"#.as_slice(),
            "MCP_TOOL_ERROR",
        ),
    ] {
        let run = Run::new(Transport::Mcp, 4);
        let id = run.probe();
        mcp::dispatch(&run.state, &id).unwrap();
        let result = run.directory.join("rejected.json");
        fs::write(&result, raw).unwrap();
        let output = mcp::step_resumable(&run.state, &[(id.clone(), result)]).unwrap();
        assert_eq!(output.status, "cleanup-pending");
        assert_eq!(output.exit_code(), 3);
        assert_eq!(output.answered, 0);
        assert!(output.diagnostics.iter().any(|d| d.code == code));
        assert!(mcp::has_in_flight(&mcp::inspect_state(&run.state).unwrap()));
        assert!(mcp::dispatch(&run.state, &id).is_err());
        assert!(mcp::resume(&run.state).is_err());
        assert_eq!(
            run.saved()["session"]["attempts"][0]["rejected_results"][0]["evidence"]["raw_sha256"],
            sha256_hex(raw)
        );
        let proof = cancellation_proof(&run, &id);
        assert_eq!(
            mcp::settle_cancelled(&run.state, &id, &proof)
                .unwrap()
                .status,
            "paused"
        );
        assert_eq!(mcp::resumable_info(&run.state).unwrap()["attempts"], 1);
        mcp::resume(&run.state).unwrap();
        mcp::dispatch(&run.state, &id).unwrap();
        let next = run.answer(&id);
        assert_eq!(next.status, "needs-probes");
        assert_eq!(mcp::resumable_info(&run.state).unwrap()["attempts"], 2);
        let other = next.probes[0]["probe_id"].as_str().unwrap();
        mcp::dispatch(&run.state, other).unwrap();
        assert_eq!(run.answer(other).status, "complete");
        assert_eq!(mcp::resumable_info(&run.state).unwrap()["attempts"], 3);
        assert_eq!(
            run.saved()["session"]["attempts"][0]["rejected_results"][0]["evidence"]["raw_sha256"],
            sha256_hex(raw)
        );
    }
}

#[test]
fn acceptable_original_result_can_settle_a_previously_uncertain_host_call() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Mcp, 3);
    let id = run.probe();
    mcp::dispatch(&run.state, &id).unwrap();
    let file = run.directory.join("timeout.json");
    let raw = br#"{"result":[],"error":"client timeout"}"#;
    fs::write(&file, raw).unwrap();
    assert_eq!(
        mcp::step_resumable(&run.state, &[(id.clone(), file)])
            .unwrap()
            .status,
        "cleanup-pending"
    );
    assert_eq!(run.answer(&id).status, "paused");
    assert!(!mcp::has_in_flight(
        &mcp::inspect_state(&run.state).unwrap()
    ));
    assert_eq!(mcp::resumable_info(&run.state).unwrap()["attempts"], 1);
    assert_eq!(
        run.saved()["session"]["attempts"][0]["rejected_results"][0]["evidence"]["raw_sha256"],
        sha256_hex(raw)
    );
    assert!(
        !mcp::resume(&run.state)
            .unwrap()
            .probes
            .iter()
            .any(|probe| probe["probe_id"] == id)
    );
}

#[test]
fn older_format_three_failures_are_not_revived_or_assumed_clean() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Mcp, 3);
    let id = run.probe();
    mcp::dispatch(&run.state, &id).unwrap();
    let file = run.directory.join("timeout.json");
    fs::write(&file, br#"{"result":[],"error":"client timeout"}"#).unwrap();
    let output = mcp::step_resumable(&run.state, &[(id.clone(), file)]).unwrap();
    let mut old = run.saved();
    old["status"] = json!("failed");
    old["failure"] = serde_json::to_value(&output.diagnostics[0]).unwrap();
    old["session"]["attempts"][0]["outcome"] = json!("failed");
    old["session"]["attempts"][0]["terminal"] =
        old["session"]["attempts"][0]["rejected_results"][0]["evidence"].clone();
    old["session"]["attempts"][0]
        .as_object_mut()
        .unwrap()
        .remove("rejected_results");
    run.save(&old);
    assert!(mcp::has_in_flight(&mcp::inspect_state(&run.state).unwrap()));
    assert_eq!(mcp::resume(&run.state).unwrap().status, "failed");
    assert!(mcp::dispatch(&run.state, &id).is_err());
}

#[test]
fn definite_preflight_rejection_and_older_unclassified_native_failure_stay_distinct() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Native, 3);
    let mut backend = Fake::for_run(&run);
    backend.start_diagnostic = Some("HTTP_RESPONSE");
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "failed"
    );
    assert!(!mcp::has_in_flight(
        &mcp::inspect_state(&run.state).unwrap()
    ));
    let mut old = run.saved();
    old["session"]["attempts"][0]
        .as_object_mut()
        .unwrap()
        .remove("no_start_confirmed");
    run.save(&old);
    assert!(mcp::has_in_flight(&mcp::inspect_state(&run.state).unwrap()));
    assert_eq!(mcp::resume(&run.state).unwrap().status, "failed");
    assert_eq!(backend.starts.len(), 1);
}

#[test]
fn old_generic_start_status_is_uncertain_even_with_previous_nonstart_boolean() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let run = Run::new(Transport::Native, 3);
    let mut backend = Fake::for_run(&run);
    backend.start_diagnostic = Some("HTTP_START_REJECTED");
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "failed"
    );
    assert!(!mcp::has_in_flight(
        &mcp::inspect_state(&run.state).unwrap()
    ));
    let mut old = run.saved();
    assert_eq!(old["session"]["attempts"][0]["no_start_confirmed"], true);
    let diagnostic = &mut old["session"]["attempts"][0]["receipt"]["diagnostic"];
    diagnostic["code"] = json!("HTTP_STATUS");
    diagnostic["message"] = json!("API returned HTTP 504. Gateway timeout");
    old["failure"] = diagnostic.clone();
    run.save(&old);
    assert!(mcp::has_in_flight(&mcp::inspect_state(&run.state).unwrap()));
    assert_eq!(mcp::resume(&run.state).unwrap().status, "failed");
    assert!(mcp::recover_native_cleanup(&run.state, &mut backend).is_err());
    assert_eq!(
        mcp::run_native(&run.state, &mut backend).unwrap().status,
        "failed"
    );
    assert_eq!(backend.starts.len(), 1);
    assert!(!run.pack.exists());
}

#[test]
fn interrupted_legacy_failed_start_without_receipt_cannot_issue_another_start() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for legacy_boolean in [None, Some(true)] {
        let run = Run::new(Transport::Native, 3);
        let mut backend = Fake::for_run(&run);
        backend.start_diagnostic = Some("HTTP_START_REJECTED");
        assert_eq!(
            mcp::run_native(&run.state, &mut backend).unwrap().status,
            "failed"
        );
        assert!(!mcp::has_in_flight(
            &mcp::inspect_state(&run.state).unwrap()
        ));
        let mut interrupted = run.saved();
        interrupted["status"] = json!("needs-probes");
        interrupted["failure"] = Value::Null;
        let attempt = interrupted["session"]["attempts"][0]
            .as_object_mut()
            .unwrap();
        attempt.insert("receipt".into(), Value::Null);
        if let Some(value) = legacy_boolean {
            attempt.insert("no_start_confirmed".into(), json!(value));
        } else {
            attempt.remove("no_start_confirmed");
        }
        run.save(&interrupted);
        let saved = mcp::inspect_state(&run.state).unwrap();
        assert_eq!(saved.status, "needs-probes");
        assert!(mcp::has_in_flight(&saved));
        assert_eq!(mcp::resumable_info(&run.state).unwrap()["in_flight"], true);
        assert!(mcp::resume(&run.state).is_err());
        assert_eq!(
            mcp::run_native(&run.state, &mut backend).unwrap().status,
            "cleanup-pending"
        );
        assert!(mcp::has_in_flight(&mcp::inspect_state(&run.state).unwrap()));
        assert!(mcp::recover_native_cleanup(&run.state, &mut backend).is_err());
        assert_eq!(backend.starts.len(), 1);
        assert_eq!(backend.stops, 0);
        assert_eq!(mcp::resumable_info(&run.state).unwrap()["attempts"], 1);
        assert!(!run.pack.exists());
        assert!(!run.receipt.exists());
    }
}

fn age_checkpoint(run: &Run, closed_seconds: Option<i64>) {
    let mut saved = run.saved();
    let base = cqlmatic::environment::parse_time("2026-01-02T00:00:00Z").unwrap();
    let at = |seconds| {
        (base + time::Duration::seconds(seconds))
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap()
    };
    saved["created_at"] = json!(at(0));
    saved["session"]["periods"] = json!([{"start":at(0),"end":closed_seconds.map(at)}]);
    for (index, attempt) in saved["session"]["attempts"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        attempt["dispatched_at"] = json!(at(index as i64 + 1));
    }
    let mut binding = serde_json::to_vec(&(
        &saved["request_sha256"],
        &saved["created_at"],
        &saved["pack"],
        &saved["receipt"],
        &saved["session"]["transport"],
        &saved["session"]["predecessor_sha256"],
    ))
    .unwrap();
    if saved["session"]["sparse_fieldset_fallback"] == true {
        binding.extend_from_slice(b"\0sparse-fieldset-fallback-v1");
    }
    saved["session"]["binding_sha256"] = json!(sha256_hex(&binding));
    run.save(&saved);
    mcp::inspect_state(&run.state).unwrap();
}

#[test]
fn late_complete_original_closes_lifecycle_without_accepting_evidence() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for phase in ["dispatched", "cancel-requested", "cleanup-pending"] {
        let run = Run::new(Transport::Mcp, 3);
        let id = run.probe();
        mcp::dispatch(&run.state, &id).unwrap();
        if phase == "cancel-requested" {
            mcp::pause(&run.state).unwrap();
        } else if phase == "cleanup-pending" {
            let timeout = run.directory.join("timeout.json");
            fs::write(&timeout, br#"{"result":[],"error":"client timeout"}"#).unwrap();
            assert_eq!(
                mcp::step_resumable(&run.state, &[(id.clone(), timeout)])
                    .unwrap()
                    .status,
                "cleanup-pending"
            );
        }
        age_checkpoint(&run, None);
        let original = run.directory.join("original.json");
        let raw = br#"{"result":[{"events":"10"}]}"#;
        fs::write(&original, raw).unwrap();
        let before = fs::read(&run.state).unwrap();
        assert_eq!(
            mcp::step_resumable(&run.state, &[("p-foreign".into(), original.clone())])
                .unwrap_err()
                .code,
            "MCP_UNKNOWN_PROBE"
        );
        assert_eq!(fs::read(&run.state).unwrap(), before);
        let output = mcp::step_resumable(&run.state, &[(id.clone(), original.clone())]).unwrap();
        assert_eq!(output.status, "failed", "{phase}");
        assert!(output.terminal_failure);
        assert_eq!(output.diagnostics[0].code, "MCP_DEADLINE");
        assert_eq!(output.answered, 0);
        let saved = run.saved();
        let attempt = &saved["session"]["attempts"][0];
        assert_eq!(attempt["outcome"], "completed");
        assert_eq!(attempt["terminal"]["raw_sha256"], sha256_hex(raw));
        assert!(!attempt["terminal"]["raw_hex"].as_str().unwrap().is_empty());
        assert!(saved["session"]["periods"][0]["end"].is_string());
        assert_eq!(saved["answers"], json!({}));
        assert_eq!(saved["session"]["captures"], json!({}));
        assert_eq!(saved["session"]["attempts"].as_array().unwrap().len(), 1);
        fs::remove_file(&original).unwrap();
        assert!(!mcp::has_in_flight(
            &mcp::inspect_state(&run.state).unwrap()
        ));
        assert_eq!(mcp::resume(&run.state).unwrap().status, "failed");
        assert!(mcp::dispatch(&run.state, &id).is_err());
        assert!(!run.pack.exists() && !run.receipt.exists());
        assert_eq!(run.saved(), saved);
        let mut altered = saved.clone();
        altered["session"]["attempts"][0]["terminal"]["raw_sha256"] = json!("0".repeat(64));
        run.save(&altered);
        assert_eq!(
            mcp::inspect_state(&run.state).unwrap_err().code,
            "MCP_STATE_TAMPERED"
        );
        let mut altered = saved.clone();
        altered["status"] = json!("needs-probes");
        altered.as_object_mut().unwrap().remove("failure");
        run.save(&altered);
        assert_eq!(
            mcp::inspect_state(&run.state).unwrap_err().code,
            "MCP_STATE_TAMPERED"
        );
    }
}

#[test]
fn late_rejected_originals_still_need_exact_cleanup() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let capped = serde_json::to_vec(&json!({"result":vec![json!({"events":"10"});10000]})).unwrap();
    for raw in [
        br#"{"result":[],"error":"client timeout"}"#.as_slice(),
        br#"{"result":[],"error":"synthetic tool error"}"#.as_slice(),
        br#"{"unexpected":"synthetic"}"#.as_slice(),
        br#"{"result":[{"events":"10"}],"partial":true}"#.as_slice(),
        br#"{"result":[]}"#.as_slice(),
        capped.as_slice(),
    ] {
        let run = Run::new(Transport::Mcp, 3);
        let id = run.probe();
        mcp::dispatch(&run.state, &id).unwrap();
        age_checkpoint(&run, None);
        let file = run.directory.join("rejected.json");
        fs::write(&file, raw).unwrap();
        let output = mcp::step_resumable(&run.state, &[(id.clone(), file)]).unwrap();
        assert_eq!(output.status, "cleanup-pending");
        assert!(!output.terminal_failure);
        assert_eq!(output.answered, 0);
        assert!(mcp::has_in_flight(&mcp::inspect_state(&run.state).unwrap()));
        assert!(mcp::resume(&run.state).is_err());
        assert!(mcp::dispatch(&run.state, &id).is_err());
        assert_eq!(
            run.saved()["session"]["attempts"][0]["outcome"],
            "cleanup-pending"
        );
        assert!(!run.pack.exists() && !run.receipt.exists());
    }
}

#[test]
fn complete_native_capture_graph_can_activate_after_budget_without_remote_calls() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for status in ["activation-blocked", "needs-probes"] {
        for seconds in [119, 120, 121] {
            let run = Run::new(Transport::Native, 2);
            let mut backend = Fake::for_run(&run);
            let held = storage::Writer::acquire(&run.pack).unwrap();
            assert_eq!(
                mcp::run_native(&run.state, &mut backend).unwrap().status,
                "activation-blocked"
            );
            drop(held);
            let mut saved = run.saved();
            saved["status"] = json!(status);
            run.save(&saved);
            age_checkpoint(&run, Some(seconds));
            let captures = run.saved()["session"]["captures"].clone();
            let attempts = run.saved()["session"]["attempts"].clone();
            let calls = (backend.starts.len(), backend.polls, backend.stops);
            let output = mcp::run_native(&run.state, &mut backend).unwrap();
            assert_eq!(output.status, "complete", "{status} {seconds}");
            assert_eq!(output.answered, 2);
            assert_eq!((backend.starts.len(), backend.polls, backend.stops), calls);
            assert_eq!(run.saved()["session"]["captures"], captures);
            assert_eq!(run.saved()["session"]["attempts"], attempts);
            let receipt: Value = serde_json::from_slice(&fs::read(&run.receipt).unwrap()).unwrap();
            for capture in receipt["captures"].as_array().unwrap() {
                let id = discovery::probe_id_for(
                    "synthetic_view",
                    capture["probe"]["query"].as_str().unwrap(),
                    capture["probe"]["start"].as_str().unwrap(),
                    capture["probe"]["end"].as_str().unwrap(),
                );
                assert_eq!(*capture, captures[&id]);
            }
            let pack: Value = serde_json::from_slice(&fs::read(&run.pack).unwrap()).unwrap();
            assert!(pack.to_string().contains("ngsiem-api"));
            assert!(!pack.to_string().contains("mcp-observation"));
        }
    }
}

#[test]
fn incomplete_native_graph_never_starts_or_reserves_on_activation_retry() {
    let _lock = SIGNAL.lock().unwrap();
    for (status, seconds) in [
        ("needs-probes", 120),
        ("activation-blocked", 119),
        ("activation-blocked", 120),
    ] {
        network::CANCELLED.store(false, Ordering::Relaxed);
        let run = Run::new(Transport::Native, 3);
        let mut backend = Fake::for_run(&run);
        backend.cancel_after = Some(1);
        assert_eq!(
            mcp::run_native(&run.state, &mut backend).unwrap().status,
            "paused"
        );
        network::CANCELLED.store(false, Ordering::Relaxed);
        let mut saved = run.saved();
        saved["status"] = json!(status);
        run.save(&saved);
        age_checkpoint(&run, Some(seconds));
        let before = fs::read(&run.state).unwrap();
        let calls = (backend.starts.len(), backend.polls, backend.stops);
        let output = mcp::run_native(&run.state, &mut backend);
        if status == "needs-probes" {
            assert_eq!(output.unwrap_err().code, "MCP_DEADLINE");
        } else {
            let output = output.unwrap();
            assert_eq!(output.status, "activation-blocked");
            assert_eq!(output.diagnostics[0].code, "DISCOVERY_CAPTURE_REQUIRED");
        }
        assert_eq!(fs::read(&run.state).unwrap(), before);
        assert_eq!((backend.starts.len(), backend.polls, backend.stops), calls);
        assert!(!run.pack.exists() && !run.receipt.exists());
    }
}

#[test]
fn explicit_native_resume_can_activate_complete_paused_graph_after_budget() {
    let _lock = SIGNAL.lock().unwrap();
    for completed in [1, 2] {
        network::CANCELLED.store(false, Ordering::Relaxed);
        let run = Run::new(Transport::Native, 3);
        let mut backend = Fake::for_run(&run);
        backend.cancel_after = Some(completed);
        assert_eq!(
            mcp::run_native(&run.state, &mut backend).unwrap().status,
            "paused"
        );
        network::CANCELLED.store(false, Ordering::Relaxed);
        age_checkpoint(&run, Some(120));
        let saved = run.saved();
        let calls = (backend.starts.len(), backend.polls, backend.stops);
        assert_eq!(
            mcp::run_native(&run.state, &mut backend).unwrap().status,
            "paused"
        );
        assert_eq!(run.saved(), saved);
        let output = mcp::resume(&run.state);
        if completed == 2 {
            assert_eq!(output.unwrap().status, "complete");
            assert_eq!(
                run.saved()["session"]["periods"],
                saved["session"]["periods"]
            );
            assert_eq!(
                run.saved()["session"]["captures"],
                saved["session"]["captures"]
            );
            assert_eq!(
                run.saved()["session"]["attempts"],
                saved["session"]["attempts"]
            );
            assert!(run.pack.exists() && run.receipt.exists());
        } else {
            assert_eq!(output.unwrap_err().code, "MCP_DEADLINE");
            assert_eq!(run.saved(), saved);
            assert!(!run.pack.exists() && !run.receipt.exists());
        }
        assert_eq!((backend.starts.len(), backend.polls, backend.stops), calls);
    }
}

#[test]
fn cached_native_activation_keeps_predecessor_binding() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for status in ["activation-blocked", "needs-probes", "paused"] {
        let run = Run::new(Transport::Native, 2);
        let mut backend = Fake::for_run(&run);
        let held = storage::Writer::acquire(&run.pack).unwrap();
        assert_eq!(
            mcp::run_native(&run.state, &mut backend).unwrap().status,
            "activation-blocked"
        );
        drop(held);
        let mut saved = run.saved();
        saved["status"] = json!(status);
        run.save(&saved);
        age_checkpoint(&run, Some(120));
        replace_starting_pack(&run);
        let pack = fs::read(&run.pack).unwrap();
        let state = fs::read(&run.state).unwrap();
        let calls = (backend.starts.len(), backend.polls, backend.stops);
        assert_eq!(
            mcp::run_native(&run.state, &mut backend).unwrap_err().code,
            "MCP_ACTIVATION_CONFLICT"
        );
        assert_eq!(
            mcp::resume(&run.state).unwrap_err().code,
            "MCP_ACTIVATION_CONFLICT"
        );
        assert_eq!(fs::read(&run.pack).unwrap(), pack);
        assert_eq!(fs::read(&run.state).unwrap(), state);
        assert_eq!((backend.starts.len(), backend.polls, backend.stops), calls);
        assert!(!run.receipt.exists());
    }
}
