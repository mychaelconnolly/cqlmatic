use cqlmatic::{
    contracts::Diagnostic,
    discovery::{self, DiscoverRequest, Probe},
    environment::Pack,
    mcp::{self, Transport},
    network::{self, SearchBackend},
    sha256_hex, storage,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

static SIGNAL: Mutex<()> = Mutex::new(());

fn request(cap: Option<usize>, ordinary_source: bool) -> DiscoverRequest {
    let mut r: DiscoverRequest = serde_json::from_value(json!({
        "schema_version":1,
        "environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"synthetic_view"},
        "start":"2026-01-01T00:00:00Z","end":"2026-01-01T01:00:00Z",
        "sources":[{"repo":"sparse","type":"literal,type"}],
        "population_fields":[{"source":{"repo":"sparse","type":"literal,type"},"fields":["field.real","field.zero"]}],
        "value_fields":["field.real"],"max_queries":120,"max_seconds":3600
    })).unwrap();
    r.max_population_fields_per_source = cap;
    if ordinary_source {
        let source = serde_json::from_value(json!({"repo":"recent"})).unwrap();
        r.sources.push(source);
        r.population_fields.push(
            serde_json::from_value(
                json!({"source":{"repo":"recent"},"fields":["field.real","field.zero"]}),
            )
            .unwrap(),
        );
    }
    r
}

#[derive(Default)]
struct Scenario {
    empty_full: bool,
    zero_volume: bool,
}
impl Scenario {
    fn rows(&self, r: &DiscoverRequest, p: &Probe) -> Vec<Value> {
        let total = if self.zero_volume { 0 } else { 3 };
        match p.kind.as_str() {
            "volume" => vec![json!({"events":total})],
            "dataset-facet" => vec![json!({"#event.dataset":"synthetic","events":total})],
            "fieldset"
                if (p.source.repo.as_deref() == Some("sparse") && p.start != r.start)
                    || self.empty_full =>
            {
                vec![]
            }
            "fieldset" => ["field.real", "hint.only", "paired", "#paired", "event.kind"]
                .map(|f| json!({"field":f}))
                .to_vec(),
            "tag-detect" => vec![
                json!({"events":if matches!(p.field.as_deref(),Some("type"|"event.dataset")){total}else{0}}),
            ],
            "record-kind-facet" if p.field.as_deref() == Some("#type") => {
                vec![json!({"#type":"literal,type","events":total})]
            }
            "record-kind-facet" => vec![
                json!({p.field.as_ref().unwrap():"alpha","events":1}),
                json!({p.field.as_ref().unwrap():"beta","events":2}),
            ],
            "kind-fieldset" => vec![json!({"field":"rare.extra"})],
            "population" => {
                let full = p.start == r.start;
                let mut row = json!({"total":if full {total}else{20}});
                for (i, f) in p.fields.iter().enumerate() {
                    row[format!("c{i}")] = json!(if f == "field.real" {
                        if full { 1 } else { 10 }
                    } else {
                        0
                    });
                }
                vec![row]
            }
            "value-facet" => vec![json!({p.field.as_ref().unwrap():"sample","events":1})],
            _ => panic!("Unexpected probe {p:?}"),
        }
    }
    fn graph(&self, r: &DiscoverRequest) -> (Vec<Probe>, BTreeMap<String, Vec<Value>>) {
        let mut probes = vec![];
        let mut answers = BTreeMap::new();
        loop {
            let pending = discovery::frontier(r, &answers).unwrap();
            if pending.is_empty() {
                return (probes, answers);
            }
            assert!(probes.len() < 200);
            for p in pending {
                assert!(
                    answers
                        .insert(
                            discovery::probe_id(&r.environment.view, &p),
                            self.rows(r, &p)
                        )
                        .is_none()
                );
                probes.push(p);
            }
        }
    }
}

struct Backend {
    answers: BTreeMap<String, Vec<Value>>,
    calls: Vec<String>,
    stops: usize,
    cancel_after: Option<usize>,
}
impl Backend {
    fn new(answers: BTreeMap<String, Vec<Value>>) -> Self {
        Self {
            answers,
            calls: vec![],
            stops: 0,
            cancel_after: None,
        }
    }
}
impl SearchBackend for Backend {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        _: Instant,
    ) -> Result<String, Diagnostic> {
        let id = discovery::probe_id_for(view, query, start, end);
        assert!(
            self.answers.contains_key(&id),
            "Unexpected probe {query} {start}/{end}"
        );
        assert!(!self.calls.contains(&id), "Accepted probe repeated: {id}");
        self.calls.push(id);
        Ok(format!("native-job-{}", self.calls.len()))
    }
    fn poll(&mut self, _: &str, job: &str, _: Instant) -> Result<Value, Diagnostic> {
        let index = job
            .strip_prefix("native-job-")
            .unwrap()
            .parse::<usize>()
            .unwrap()
            - 1;
        Ok(
            json!({"done":true,"cancelled":false,"metaData":{},"events":self.answers[&self.calls[index]]}),
        )
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        self.stops += 1;
        if self.cancel_after == Some(self.stops) {
            network::CANCELLED.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
    fn observed_at(&self, _: &str) -> Option<String> {
        Some("2026-02-01T00:00:00Z".into())
    }
}

struct Saved {
    directory: PathBuf,
    state: PathBuf,
    pack: PathBuf,
    receipt: PathBuf,
}
impl Saved {
    fn new(r: DiscoverRequest, transport: Transport) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "cqlmatic-sparse-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let run = Self {
            state: directory.join("state.json"),
            pack: directory.join("pack.json"),
            receipt: directory.join("receipt.json"),
            directory,
        };
        mcp::init_resumable(r, &run.state, &run.pack, &run.receipt, transport).unwrap();
        run
    }
    fn read(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.state).unwrap()).unwrap()
    }
    fn save(&self, v: &Value) {
        storage::Writer::acquire(&self.state)
            .unwrap()
            .commit_bytes(&serde_json::to_vec(v).unwrap(), 64 * 1024 * 1024)
            .unwrap();
    }
    fn legacy(&self, missing: bool) {
        let mut v = self.read();
        let binding = sha256_hex(
            &serde_json::to_vec(&json!([
                v["request_sha256"],
                v["created_at"],
                v["pack"],
                v["receipt"],
                v["session"]["transport"],
                v["session"]["predecessor_sha256"]
            ]))
            .unwrap(),
        );
        v["session"]["binding_sha256"] = json!(binding);
        if missing {
            v["session"]
                .as_object_mut()
                .unwrap()
                .remove("sparse_fieldset_fallback");
        } else {
            v["session"]["sparse_fieldset_fallback"] = json!(false);
        }
        self.save(&v);
        mcp::inspect_state(&self.state).unwrap();
    }
    fn answer(&self, p: &Probe, rows: Vec<Value>) -> mcp::StepOutput {
        let id = discovery::probe_id("synthetic_view", p);
        mcp::dispatch(&self.state, &id).unwrap();
        self.settle(p, rows)
    }
    fn settle(&self, p: &Probe, rows: Vec<Value>) -> mcp::StepOutput {
        let id = discovery::probe_id("synthetic_view", p);
        let file = self.directory.join(format!("{id}.json"));
        fs::write(&file, json!({"result":rows}).to_string()).unwrap();
        mcp::step_resumable(&self.state, &[(id, file)]).unwrap()
    }
}
impl Drop for Saved {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}
fn wire(p: &Value) -> Probe {
    let a = &p["mcp"]["arguments"];
    Probe {
        kind: p["kind"].as_str().unwrap().into(),
        source: serde_json::from_value(p["source"].clone()).unwrap(),
        query: a["query_string"].as_str().unwrap().into(),
        start: a["start"].as_str().unwrap().into(),
        end: a["end"].as_str().unwrap().into(),
        fields: serde_json::from_value(p["fields"].clone()).unwrap(),
        field: serde_json::from_value(p["field"].clone()).unwrap(),
    }
}
fn field<'a>(
    pack: &'a Pack,
    r: &DiscoverRequest,
    index: usize,
    path: &str,
    tag: bool,
) -> &'a Value {
    let id = discovery::field_id(&discovery::source_id(&r.sources[index]), path, tag);
    pack.data.fields.iter().find(|f| f["id"] == id).unwrap()
}

#[test]
fn sparse_fallback_matches_api_mcp_and_preserves_actual_windows_and_schema_hints() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for cap in [Some(2), None] {
        let r = request(cap, true);
        let scenario = Scenario::default();
        let (probes, answers) = scenario.graph(&r);
        let mut backend = Backend::new(answers);
        let (pack, report) = discovery::execute(&r, &mut backend);
        assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
        let pack = pack.unwrap();
        let saved = Saved::new(r.clone(), Transport::Mcp);
        let mut output = mcp::step(&saved.state, &[]).unwrap();
        let mut relayed = BTreeSet::new();
        while output.status == "needs-probes" {
            let p = wire(&output.probes[0]);
            assert!(relayed.insert(discovery::probe_id(&r.environment.view, &p)));
            output = saved.answer(&p, scenario.rows(&r, &p));
        }
        assert_eq!(output.status, "complete", "{:?}", output.diagnostics);
        assert_eq!(relayed, backend.calls.iter().cloned().collect());
        let relayed_pack = storage::read_pack(&saved.pack).unwrap();
        let full = format!("{}/{}", r.start, r.end);
        let recent = format!("2026-01-01T00:45:00Z/{}", r.end);
        for (index, window) in [(0, &full), (1, &recent)] {
            for (path, tag) in [
                ("field.real", false),
                ("field.zero", false),
                ("hint.only", false),
                ("paired", false),
                ("paired", true),
            ] {
                let f = field(&pack, &r, index, path, tag);
                let other = field(&relayed_pack, &r, index, path, tag);
                assert_eq!(f["coverage"], other["coverage"]);
                assert_eq!(f["observation_window"], other["observation_window"]);
                assert_eq!(
                    f["observation_window"],
                    if path == "field.zero" { &full } else { window }.as_str()
                );
                let proof = pack
                    .data
                    .provenance
                    .iter()
                    .find(|p| p["id"] == f["provenance_ids"][0])
                    .unwrap();
                assert_eq!(proof["observation_window"], f["observation_window"]);
            }
            let hint = field(&pack, &r, index, "hint.only", false);
            assert_eq!(hint["evidence_kind"], "documented");
            assert!(hint["coverage"]["observed"].is_null());
            assert!(hint["coverage"]["rate"].is_null());
            assert_eq!(
                field(&pack, &r, index, "field.zero", false)["coverage"]["observed"],
                false
            );
            let value = &pack.data.values[index];
            let proof = pack
                .data
                .provenance
                .iter()
                .find(|p| p["id"] == value["provenance_ids"][0])
                .unwrap();
            assert_eq!(proof["observation_window"], window.as_str());
            assert_eq!(
                field(&pack, &r, index, "Vendor", true)["observation_window"],
                window.as_str()
            );
        }
        assert_eq!(
            field(&pack, &r, 0, "field.real", false)["coverage"]["rate"],
            1.0 / 3.0
        );
        assert_eq!(
            field(&pack, &r, 1, "field.real", false)["coverage"]["rate"],
            0.5
        );
        assert_eq!(
            probes
                .iter()
                .filter(|p| p.source == r.sources[0] && p.kind == "fieldset")
                .count(),
            2
        );
        assert_eq!(
            probes
                .iter()
                .filter(|p| p.source == r.sources[1] && p.kind == "fieldset")
                .count(),
            1
        );
        for p in probes.iter().filter(|p| p.source == r.sources[0]) {
            assert!(
                p.query
                    .starts_with("#repo = \"sparse\" | #type = \"literal,type\" |")
            );
            if p.kind != "fieldset" {
                assert_eq!(p.start, r.start);
            }
        }
    }
}

#[test]
fn empty_recent_fieldset_before_volume_defers_auxiliaries_and_reaches_one_fallback() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for cap in [None, Some(2)] {
        let r = request(cap, false);
        let scenario = Scenario::default();
        let saved = Saved::new(r.clone(), Transport::Mcp);
        let initial = mcp::step(&saved.state, &[]).unwrap();
        let p = wire(
            initial
                .probes
                .iter()
                .find(|p| p["kind"] == "fieldset")
                .unwrap(),
        );
        let mut output = saved.answer(&p, vec![]);
        assert!(
            output
                .probes
                .iter()
                .all(|p| matches!(p["kind"].as_str(), Some("volume" | "dataset-facet")))
        );
        let dataset = wire(
            output
                .probes
                .iter()
                .find(|p| p["kind"] == "dataset-facet")
                .unwrap(),
        );
        output = saved.answer(&dataset, scenario.rows(&r, &dataset));
        assert_eq!(output.probes.len(), 1);
        assert_eq!(output.probes[0]["kind"], "volume");
        let volume = wire(&output.probes[0]);
        output = saved.answer(&volume, scenario.rows(&r, &volume));
        assert_eq!(output.probes.len(), 1);
        let fallback = wire(&output.probes[0]);
        assert_eq!(fallback.kind, "fieldset");
        assert_eq!(fallback.start, r.start);
        assert_ne!(
            discovery::probe_id(&r.environment.view, &fallback),
            discovery::probe_id(&r.environment.view, &p)
        );
        assert_eq!(mcp::pause(&saved.state).unwrap().status, "paused");
        output = mcp::resume(&saved.state).unwrap();
        assert_eq!(wire(&output.probes[0]), fallback);
        let mut seen = BTreeSet::from([
            discovery::probe_id(&r.environment.view, &p),
            discovery::probe_id(&r.environment.view, &dataset),
            discovery::probe_id(&r.environment.view, &volume),
        ]);
        while output.status == "needs-probes" {
            let p = wire(&output.probes[0]);
            assert!(seen.insert(discovery::probe_id(&r.environment.view, &p)));
            output = saved.answer(&p, scenario.rows(&r, &p));
        }
        assert_eq!(output.status, "complete", "{:?}", output.diagnostics);
    }
}

#[test]
fn full_empty_short_zero_and_insufficient_budget_never_activate() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for case in ["full-empty", "short", "zero", "budget"] {
        let mut r = request(Some(2), false);
        if case == "short" {
            r.start = "2026-01-01T00:45:00Z".into();
        }
        if case == "budget" {
            r.max_queries = 3;
        }
        let scenario = Scenario {
            empty_full: case == "full-empty" || case == "short",
            zero_volume: case == "zero",
        };
        let mut answers = BTreeMap::new();
        for p in discovery::frontier(&r, &answers).unwrap() {
            answers.insert(
                discovery::probe_id(&r.environment.view, &p),
                scenario.rows(&r, &p),
            );
        }
        if case == "full-empty" {
            let fallback = discovery::frontier(&r, &answers).unwrap().remove(0);
            answers.insert(discovery::probe_id(&r.environment.view, &fallback), vec![]);
        }
        let mut backend = Backend::new(answers);
        let (pack, report) = discovery::execute(&r, &mut backend);
        assert!(pack.is_none());
        assert_ne!(report.status, "complete");
        assert_eq!(
            backend.calls.len(),
            if case == "full-empty" { 4 } else { 3 }
        );
        let saved = Saved::new(r.clone(), Transport::Mcp);
        let mut output = mcp::step(&saved.state, &[]).unwrap();
        while output.status == "needs-probes" {
            let p = wire(&output.probes[0]);
            output = saved.answer(&p, scenario.rows(&r, &p));
        }
        assert_eq!(output.status, "failed", "{case}: {:?}", output.diagnostics);
        assert!(!saved.pack.exists());
        assert!(!saved.receipt.exists());
        assert_eq!(mcp::resume(&saved.state).unwrap().status, "failed");
    }
}

#[test]
fn sparse_native_resume_matches_uninterrupted_without_accepted_remote_replay() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let r = request(Some(2), false);
    let (_, answers) = Scenario::default().graph(&r);
    let interrupted = Saved::new(r.clone(), Transport::Native);
    let mut backend = Backend::new(answers.clone());
    backend.cancel_after = Some(4);
    assert_eq!(
        mcp::run_native(&interrupted.state, &mut backend)
            .unwrap()
            .status,
        "paused"
    );
    assert_eq!(backend.calls.len(), 4);
    assert!(!interrupted.pack.exists());
    let before = interrupted.read()["session"]["captures"].clone();
    assert_eq!(before.as_object().unwrap().len(), 4);
    network::CANCELLED.store(false, Ordering::Relaxed);
    backend.cancel_after = None;
    mcp::resume(&interrupted.state).unwrap();
    assert_eq!(
        mcp::run_native(&interrupted.state, &mut backend)
            .unwrap()
            .status,
        "complete"
    );
    let uninterrupted = Saved::new(r, Transport::Native);
    let mut other = Backend::new(answers);
    assert_eq!(
        mcp::run_native(&uninterrupted.state, &mut other)
            .unwrap()
            .status,
        "complete"
    );
    assert_eq!(backend.calls, other.calls);
    let a = storage::read_pack(&interrupted.pack).unwrap();
    let b = storage::read_pack(&uninterrupted.pack).unwrap();
    assert_eq!(
        serde_json::to_value(a.data).unwrap(),
        serde_json::to_value(b.data).unwrap()
    );
    for (id, capture) in before.as_object().unwrap() {
        assert_eq!(&interrupted.read()["session"]["captures"][id], capture);
    }
}

#[test]
fn old_session_policy_preserves_historical_uncapped_prefix_and_pending_attempt() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for missing in [false, true] {
        let r = request(None, false);
        let saved = Saved::new(r.clone(), Transport::Mcp);
        saved.legacy(missing);
        let initial = mcp::step(&saved.state, &[]).unwrap();
        let recent = wire(
            initial
                .probes
                .iter()
                .find(|p| p["kind"] == "fieldset")
                .unwrap(),
        );
        let mut output = saved.answer(&recent, vec![]);
        let tag = wire(
            output
                .probes
                .iter()
                .find(|p| p["kind"] == "tag-detect")
                .unwrap(),
        );
        assert_ne!(tag.start, r.start);
        output = saved.answer(&tag, vec![json!({"events":0})]);
        let outstanding = wire(
            output
                .probes
                .iter()
                .find(|p| p["kind"] == "tag-detect")
                .unwrap(),
        );
        mcp::dispatch(
            &saved.state,
            &discovery::probe_id(&r.environment.view, &outstanding),
        )
        .unwrap();
        let before = saved.read()["answers"].clone();
        assert_eq!(before.as_object().unwrap().len(), 2);
        mcp::inspect_state(&saved.state).unwrap();
        output = saved.settle(&outstanding, vec![json!({"events":0})]);
        assert_eq!(output.status, "needs-probes");
        for (id, answer) in before.as_object().unwrap() {
            assert_eq!(&saved.read()["answers"][id], answer);
        }
        assert!(!output.probes.iter().any(|p| p["kind"] == "fieldset"));
        assert_eq!(mcp::resumable_info(&saved.state).unwrap()["attempts"], 3);
    }
    let saved = Saved::new(request(Some(2), false), Transport::Mcp);
    let mut changed = saved.read();
    changed["session"]["sparse_fieldset_fallback"] = json!(false);
    saved.save(&changed);
    assert!(mcp::inspect_state(&saved.state).is_err());
}

#[test]
fn legacy_native_and_format_two_replay_keep_their_original_empty_sample_failure() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    for missing in [false, true] {
        let r = request(Some(2), false);
        let scenario = Scenario::default();
        let saved = Saved::new(r.clone(), Transport::Native);
        saved.legacy(missing);
        let answers = discovery::frontier(&r, &BTreeMap::new())
            .unwrap()
            .into_iter()
            .map(|p| {
                (
                    discovery::probe_id(&r.environment.view, &p),
                    scenario.rows(&r, &p),
                )
            })
            .collect();
        let mut backend = Backend::new(answers);
        backend.cancel_after = Some(2);
        assert_eq!(
            mcp::run_native(&saved.state, &mut backend).unwrap().status,
            "paused"
        );
        network::CANCELLED.store(false, Ordering::Relaxed);
        backend.cancel_after = None;
        mcp::resume(&saved.state).unwrap();
        assert_eq!(
            mcp::run_native(&saved.state, &mut backend).unwrap().status,
            "failed"
        );
        assert_eq!(backend.calls.len(), 3);
        assert!(!saved.pack.exists());
    }
    let r = request(Some(2), false);
    let scenario = Scenario::default();
    let saved = Saved::new(r.clone(), Transport::Mcp);
    let legacy = saved.directory.join("format-two.json");
    let pack = saved.directory.join("format-two-pack.json");
    let receipt = saved.directory.join("format-two-receipt.json");
    let initial = mcp::init(r.clone(), &legacy, &pack, &receipt).unwrap();
    let mut answers = vec![];
    for p in initial.probes.iter().map(wire) {
        let id = discovery::probe_id(&r.environment.view, &p);
        let file = saved.directory.join(format!("legacy-{id}.json"));
        fs::write(&file, json!({"result":scenario.rows(&r,&p)}).to_string()).unwrap();
        answers.push((id, file));
    }
    let output = mcp::step(&legacy, &answers).unwrap();
    assert_eq!(output.status, "failed");
    assert!(output.probes.is_empty());
    assert!(!pack.exists());
    assert!(!receipt.exists());
}

#[test]
fn automatic_sparse_population_keeps_the_cap_and_full_window_rare_schema_hints() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let mut r = request(Some(1), false);
    r.population_fields.clear();
    let (probes, answers) = Scenario::default().graph(&r);
    let populations = probes
        .iter()
        .filter(|p| p.kind == "population")
        .collect::<Vec<_>>();
    assert_eq!(populations.len(), 1);
    assert_eq!(populations[0].fields, vec!["field.real"]);
    assert_eq!(populations[0].start, r.start);
    assert_eq!(
        probes.iter().filter(|p| p.kind == "kind-fieldset").count(),
        2
    );
    let mut backend = Backend::new(answers);
    let (pack, report) = discovery::execute(&r, &mut backend);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    let pack = pack.unwrap();
    for path in ["hint.only", "rare.extra"] {
        let f = field(&pack, &r, 0, path, false);
        assert!(f["coverage"]["observed"].is_null());
        assert!(f["coverage"]["rate"].is_null());
        assert_eq!(f["observation_window"], format!("{}/{}", r.start, r.end));
    }
}
