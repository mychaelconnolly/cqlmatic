use cqlmatic::{
    app,
    contracts::{Diagnostic, GenerateRequest, Outcome, Severity},
    discovery::{self, DiscoverRequest, DiscoveryDepth, Probe},
    environment::{Pack, PackData},
    mcp::{self, State, StepOutput},
    network::SearchBackend,
    sha256_hex, storage,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

const REQUEST: &str = include_str!("../examples/mcp/discover.request.json");
const COUNT: &str = include_str!("../examples/mcp/count.json");
const CENSUS: &str = include_str!("../examples/mcp/census.json");
const DATASET: &str = include_str!("../examples/mcp/dataset.json");
const FIELDSET: &str = include_str!("../examples/mcp/fieldset.json");
const POPULATION: &str = include_str!("../examples/mcp/population.json");
const VALUE: &str = include_str!("../examples/mcp/value.json");
const KIND: &str = include_str!("../examples/mcp/kind.json");
const SIMPLE_NAME: &str = include_str!("../examples/mcp/simple-name.json");
const ZERO: &str = r#"{"result":[{"events":"0"}]}"#;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-mcp-discovery-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn request() -> DiscoverRequest {
    serde_json::from_str(REQUEST).unwrap()
}
fn fixture(kind: &str, field: Option<&str>) -> &'static str {
    match kind {
        "census" => CENSUS,
        "volume" => COUNT,
        "tag-detect" if field == Some("event_simpleName") => COUNT,
        "tag-detect" => ZERO,
        "dataset-facet" => DATASET,
        "fieldset" => FIELDSET,
        "population" => POPULATION,
        "value-facet" => VALUE,
        "record-kind-facet" if field == Some("#type") => KIND,
        "record-kind-facet" if field == Some("#event_simpleName") => SIMPLE_NAME,
        _ => panic!("unexpected probe kind: {kind}"),
    }
}
fn code(output: &StepOutput) -> &str {
    &output.diagnostics[0].code
}
fn paths(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        dir.join("state.json"),
        dir.join("pack.json"),
        dir.join("receipt.json"),
    )
}
fn existing_pack(path: &Path, r: &DiscoverRequest, different_view: bool) -> Vec<u8> {
    let mut environment = r.environment.clone();
    if different_view {
        environment.view = "another-view".into();
    }
    let pack = Pack::new(environment, discovery::timestamp(), PackData::default()).unwrap();
    storage::Writer::acquire(path)
        .unwrap()
        .commit(&pack)
        .unwrap();
    fs::read(path).unwrap()
}
fn answer_batch(dir: &Path, state: &Path, progress: &StepOutput) -> StepOutput {
    let answers = progress
        .probes
        .iter()
        .enumerate()
        .map(|(i, probe)| {
            assert_eq!(probe["mcp"]["tool"], "falcon_search_ngsiem");
            let id = probe["probe_id"].as_str().unwrap();
            let kind = probe["kind"].as_str().unwrap();
            let path = dir.join(format!("answer-{i}.json"));
            fs::write(&path, fixture(kind, probe["field"].as_str())).unwrap();
            (id.to_string(), path)
        })
        .collect::<Vec<_>>();
    mcp::step(state, &answers).unwrap()
}
struct FakeApi {
    answers: BTreeMap<String, mcp::Answer>,
    starts: Vec<(String, String, String, String)>,
    active: Option<String>,
    stops: usize,
}
impl SearchBackend for FakeApi {
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
            "API probe missing in MCP run"
        );
        self.starts
            .push((view.into(), query.into(), start.into(), end.into()));
        self.active = Some(id.clone());
        Ok(id)
    }
    fn poll(&mut self, _: &str, job: &str, _: Instant) -> Result<Value, Diagnostic> {
        assert_eq!(self.active.as_deref(), Some(job));
        let bytes = self.answers[job]
            .raw_hex
            .as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect::<Vec<_>>();
        let rows = mcp::normalize(&bytes)
            .unwrap()
            .rows
            .into_iter()
            .map(Value::Object)
            .collect::<Vec<_>>();
        Ok(json!({"done":true,"cancelled":false,"metaData":{},"events":rows}))
    }
    fn stop(&mut self, _: &str, job: &str, _: Instant) -> Result<(), Diagnostic> {
        assert_eq!(self.active.as_deref(), Some(job));
        self.active = None;
        self.stops += 1;
        Ok(())
    }
}
fn recorded(state: &Path) -> State {
    serde_json::from_slice(&fs::read(state).unwrap()).unwrap()
}
fn emitted(probe: &Probe, view: &str) -> (String, String, String, String) {
    (
        view.into(),
        probe.query.clone(),
        probe.start.clone(),
        probe.end.clone(),
    )
}

#[test]
fn census_discovery_finishes_in_four_batches() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let mut r = request();
    r.census = true;
    r.sources.clear();
    let mut output = mcp::init(r, &state, &pack, &receipt).unwrap();
    let mut rounds = 0;
    while output.status == "needs-probes" {
        rounds += 1;
        assert!(rounds <= 4);
        output = answer_batch(dir.path(), &state, &output);
    }
    assert_eq!(rounds, 4);
    assert_eq!(output.status, "complete", "{:?}", output.diagnostics);
    assert_eq!(storage::read_pack(&pack).unwrap().data.sources.len(), 1);
}

#[test]
fn source_only_activation_recovers_an_undated_negative_only_after_its_snapshot() {
    for (start, end, recover) in [
        ("2026-03-01T00:00:00Z", "2026-03-02T00:00:00Z", false),
        ("2026-07-01T00:00:00Z", "2026-07-02T00:00:00Z", true),
    ] {
        let dir = Temp::new();
        let (state, pack, receipt) = paths(dir.path());
        let mut r = request();
        r.depth = DiscoveryDepth::SourcesOnly;
        r.value_fields.clear();
        r.start = start.into();
        r.end = end.into();
        let old = Pack::new(
            r.environment.clone(),
            "2026-06-01T00:00:00Z".into(),
            PackData {
                sources: vec![json!({
                    "id":discovery::source_id(&r.sources[0]),"kind":"repo","name":"synthetic_repo",
                    "inventory_status":"observed-only","selectors":{"repo":"synthetic_repo"},
                    "evidence":{"observed":false},"provenance_ids":["prov:old-volume"]
                })],
                provenance: vec![json!({
                    "id":"prov:old-volume","source_type":"mcp-observation",
                    "canonical_source":"Synthetic measurement","retrieved_at":"2026-01-03T00:00:00Z",
                    "observation_window":"2026-01-01T00:00:00Z/2026-01-02T00:00:00Z",
                    "redaction_status":"none","coverage_status":"observed-only"
                })],
                ..PackData::default()
            },
        )
        .unwrap();
        old.validate().unwrap();
        storage::Writer::acquire(&pack)
            .unwrap()
            .commit(&old)
            .unwrap();
        let before = fs::read(&pack).unwrap();
        let initial = mcp::init(r, &state, &pack, &receipt).unwrap();
        assert_eq!(initial.probes.len(), 1);
        assert_eq!(initial.probes[0]["kind"], "volume");
        let output = answer_batch(dir.path(), &state, &initial);
        if recover {
            assert_eq!(output.status, "complete", "{:?}", output.diagnostics);
            assert!(output.active_pack_updated);
            let activated = storage::read_pack(&pack).unwrap();
            assert_eq!(activated.data.sources[0]["evidence"]["observed"], true);
            assert_eq!(
                activated.data.sources[0]["evidence"]["observation_window"],
                format!("{start}/{end}")
            );
            assert!(receipt.exists());
        } else {
            assert_eq!(output.status, "failed");
            assert!(
                output.diagnostics[0]
                    .message
                    .starts_with("Source measurement ordering conflict")
            );
            assert!(!output.active_pack_updated);
            assert_eq!(fs::read(&pack).unwrap(), before);
            assert!(!receipt.exists());
        }
    }
}

#[test]
fn synthetic_frontier_matches_api_probes_and_observed_pack() {
    let dir = Temp::new();
    let (state, pack_path, receipt_path) = paths(dir.path());
    let r = request();
    let mut initial = mcp::init(r, &state, &pack_path, &receipt_path).unwrap();
    assert_eq!(initial.exit_code(), 3);
    assert_eq!(initial.status, "needs-probes");
    assert_eq!(initial.answered, 0);
    assert_eq!(initial.probes.len(), 3);
    let same = mcp::step(&state, &[]).unwrap();
    assert_eq!(same.exit_code(), 3);
    assert_eq!(same.probes, initial.probes);
    assert!(!pack_path.exists());
    let mut batches = 0;
    while initial.status == "needs-probes" {
        batches += 1;
        assert!(batches <= 4);
        if batches == 2 {
            assert!(initial.probes.iter().any(|probe| {
                probe["kind"] == "record-kind-facet" && probe["field"] == "#type"
            }));
            assert!(
                initial
                    .probes
                    .iter()
                    .any(|probe| probe["kind"] == "population")
            );
            assert!(
                initial
                    .probes
                    .iter()
                    .any(|probe| probe["kind"] == "tag-detect")
            );
        }
        if batches == 3 {
            assert!(initial.probes.iter().any(|probe| {
                probe["kind"] == "record-kind-facet" && probe["field"] == "#event_simpleName"
            }));
            assert!(
                initial
                    .probes
                    .iter()
                    .any(|probe| probe["kind"] == "value-facet")
            );
        }
        for probe in &initial.probes {
            let args = &probe["mcp"]["arguments"];
            let id = discovery::probe_id_for(
                args["repository"].as_str().unwrap(),
                args["query_string"].as_str().unwrap(),
                args["start"].as_str().unwrap(),
                args["end"].as_str().unwrap(),
            );
            assert_eq!(probe["probe_id"], id);
        }
        initial = answer_batch(dir.path(), &state, &initial);
    }
    assert_eq!(batches, 3);
    assert_eq!(initial.status, "complete", "{:?}", initial.diagnostics);
    assert_eq!(initial.exit_code(), 0);
    assert!(initial.active_pack_updated);
    assert_eq!(mcp::step(&state, &[]).unwrap().status, "complete");
    let saved = recorded(&state);
    assert_eq!(saved.format, "cqlmatic-mcp-state");
    assert_eq!(saved.schema_version, 2);
    assert_eq!(
        saved.request_sha256,
        sha256_hex(&serde_json::to_vec(&saved.request).unwrap())
    );
    assert_eq!(saved.status, "activated");
    assert_eq!(saved.answers.len(), 16);
    let mcp_pack = storage::read_pack(&pack_path).unwrap();
    assert_eq!(mcp_pack.data.values[0]["value"], "synthetic-user");
    assert_eq!(
        mcp_pack.data.sources[0]["relationship"]["record_kinds"],
        json!([
            {"field":"#type","values":["a","b"],"truncated":false},
            {"field":"#event_simpleName","values":["synthetic-event"],"truncated":false}
        ])
    );
    let mcp_provenance = &mcp_pack.data.provenance[0];
    assert_eq!(mcp_provenance["source_type"], "mcp-observation");
    assert_eq!(mcp_provenance["coverage_status"], "observed-only");
    assert!(
        mcp_provenance["id"]
            .as_str()
            .unwrap()
            .starts_with("prov:mcp:")
    );
    assert!(
        mcp_provenance["notes"]
            .as_str()
            .unwrap()
            .contains("no completeness attestation")
    );
    let receipt: Value = serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["mode"], "mcp");
    assert_eq!(receipt["status"], "complete");
    let captures = receipt["captures"].as_array().unwrap();
    assert_eq!(captures.len(), saved.answers.len());
    for capture in captures {
        assert_eq!(capture["receipt"]["cleanup"], "delegated");
        assert_eq!(capture["receipt"]["completion"], "complete");
        let p: Probe = serde_json::from_value(capture["probe"].clone()).unwrap();
        let answer = &saved.answers[&discovery::probe_id(&saved.request.environment.view, &p)];
        assert_eq!(capture["retrieved_at"], answer.ingested_at);
        assert_eq!(
            capture["receipt"]["metadata"]["mcp"]["raw_sha256"],
            answer.raw_sha256
        );
    }
    let mut api = FakeApi {
        answers: saved.answers,
        starts: vec![],
        active: None,
        stops: 0,
    };
    let (candidate, report) = discovery::execute(&saved.request, &mut api);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    assert_eq!(report.captures.len(), captures.len());
    assert_eq!(api.stops, captures.len());
    let mcp_set = captures
        .iter()
        .map(|capture| {
            let p: Probe = serde_json::from_value(capture["probe"].clone()).unwrap();
            emitted(&p, &saved.request.environment.view)
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(api.starts.into_iter().collect::<BTreeSet<_>>(), mcp_set);
    let api_pack = candidate.unwrap();
    assert_eq!(api_pack.data.sources.len(), mcp_pack.data.sources.len());
    assert_eq!(
        api_pack.data.sources[0]["selectors"],
        mcp_pack.data.sources[0]["selectors"]
    );
    assert_eq!(
        api_pack.data.sources[0]["relationship"],
        mcp_pack.data.sources[0]["relationship"]
    );
    let fields = |pack: &cqlmatic::environment::Pack| {
        pack.data
            .fields
            .iter()
            .map(|f| {
                (
                    f["path"].clone(),
                    f["coverage"].clone(),
                    f["is_tag"].clone(),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(fields(&api_pack), fields(&mcp_pack));
    assert_eq!(
        api_pack.data.values[0]["value"],
        mcp_pack.data.values[0]["value"]
    );
    assert_eq!(api_pack.data.provenance[0]["source_type"], "ngsiem-api");
    assert_eq!(api_pack.data.provenance[0]["coverage_status"], "complete");
    assert!(
        api_pack.data.provenance[0]["id"]
            .as_str()
            .unwrap()
            .starts_with("prov:api:")
    );
    let ambiguous: GenerateRequest = serde_json::from_value(json!({
        "schema_version":1,
        "input":{"mode":"plan","plan":{"source":{"repo":"synthetic_repo"},"steps":[{"op":"count"}]}}
    }))
    .unwrap();
    let result = app::generate(ambiguous.clone(), Some(&mcp_pack));
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_RECORD_KIND_NOTICE" && d.severity == Severity::Info)
    );
    assert!(result.query.is_some());
    let mut strict = ambiguous;
    strict.context.require_observed = true;
    let strict = app::generate(strict, Some(&mcp_pack));
    assert_eq!(strict.outcome, Outcome::NeedsContext);
    assert!(strict.query.is_none());
    for (field, outcome) in [("user", Outcome::Ready), ("made_up", Outcome::NeedsContext)] {
        let request: GenerateRequest = serde_json::from_value(json!({
            "schema_version":1,
            "input":{"mode":"plan","plan":{"source":{"repo":"synthetic_repo"},"filters":[{"op":"eq","field":field,"value":"synthetic-user"}],"steps":[{"op":"count"}]}},
            "context":{"require_observed":true,"accept_mixed_record_kinds":true}
        }))
        .unwrap();
        let result = app::generate(request, Some(&mcp_pack));
        assert_eq!(result.outcome, outcome, "{:?}", result.diagnostics);
        assert!(result.diagnostics.iter().any(|d| {
            d.code == "CQL_RECORD_KIND_AMBIGUOUS"
                && d.severity == cqlmatic::contracts::Severity::Warning
        }));
        if outcome == Outcome::NeedsContext {
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "CQL_CONTEXT_REQUIRED")
            );
        }
    }
}

#[test]
fn failed_record_kind_facet_answer_leaves_no_pack() {
    for (raw, expected) in [
        (
            r#"{"result":[],"error":"deadline exceeded"}"#,
            "MCP_DEADLINE",
        ),
        (r#"{"result":[],"warnings":["partial"]}"#, "MCP_INCOMPLETE"),
    ] {
        let dir = Temp::new();
        let (state, pack, receipt) = paths(dir.path());
        let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
        let second = answer_batch(dir.path(), &state, &first);
        let kind = second
            .probes
            .iter()
            .find(|probe| probe["kind"] == "record-kind-facet" && probe["field"] == "#type")
            .unwrap();
        let path = dir.path().join("failed-kind.json");
        fs::write(&path, raw).unwrap();
        let output =
            mcp::step(&state, &[(kind["probe_id"].as_str().unwrap().into(), path)]).unwrap();
        assert_eq!(output.status, "failed");
        assert_eq!(output.exit_code(), 2);
        assert_eq!(code(&output), expected);
        assert!(output.probes.is_empty());
        assert!(!pack.exists());
        assert!(!receipt.exists());
    }
}

#[test]
fn failed_mcp_normalizations_stop_without_activating_a_pack() {
    for (raw, expected) in [
        (
            r#"{"result":[],"error":"deadline exceeded"}"#,
            "MCP_DEADLINE",
        ),
        (
            r#"{"result":[],"metaData":{"errors":[{"message":"deadline exceeded"}]}}"#,
            "MCP_DEADLINE",
        ),
        (
            r#"{"result":[],"error":"synthetic tool error"}"#,
            "MCP_TOOL_ERROR",
        ),
        (
            r#"{"result":[],"metadata":{"error":{"message":"synthetic tool error"}}}"#,
            "MCP_TOOL_ERROR",
        ),
        (r#"{"result":[],"warnings":["partial"]}"#, "MCP_INCOMPLETE"),
        (
            r#"{"result":[],"metaData":{"extraData":{"eventsSkipped":1}}}"#,
            "MCP_INCOMPLETE",
        ),
        (
            r#"{"result":[],"metadata":{"filterQuery":{"isLive":true}}}"#,
            "MCP_INCOMPLETE",
        ),
        (r#"{"result":[null]}"#, "MCP_SHAPE"),
    ] {
        let dir = Temp::new();
        let (state, pack, receipt) = paths(dir.path());
        let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
        let path = dir.path().join("bad-answer.json");
        fs::write(&path, raw).unwrap();
        let output = mcp::step(
            &state,
            &[(first.probes[0]["probe_id"].as_str().unwrap().into(), path)],
        )
        .unwrap();
        assert_eq!(output.status, "failed");
        assert_eq!(output.exit_code(), 2);
        assert_eq!(code(&output), expected);
        let repeated = mcp::step(&state, &[]).unwrap();
        assert_eq!(repeated.status, "failed");
        assert_eq!(code(&repeated), expected);
        assert!(!pack.exists());
        assert!(!receipt.exists());
        let saved = recorded(&state);
        assert_eq!(saved.answers.len(), 1);
        assert_eq!(saved.status, "failed");
        assert_eq!(
            saved
                .answers
                .values()
                .next()
                .unwrap()
                .diagnostic
                .as_ref()
                .unwrap()
                .code,
            expected
        );
    }
}

#[test]
fn malformed_kind_facet_answer_does_not_replace_an_existing_pack() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let before = existing_pack(&pack, &request(), false);
    let mut output = mcp::init(request(), &state, &pack, &receipt).unwrap();
    let mut answered_bad_facet = false;
    while output.status == "needs-probes" {
        let answers = output
            .probes
            .iter()
            .enumerate()
            .map(|(i, probe)| {
                let path = dir.path().join(format!("kind-answer-{i}.json"));
                let kind = probe["kind"].as_str().unwrap();
                if kind == "record-kind-facet" && probe["field"] == "#type" {
                    fs::write(&path, r##"{"result":[{"#type":7,"events":5}]}"##).unwrap();
                    answered_bad_facet = true;
                } else {
                    fs::write(&path, fixture(kind, probe["field"].as_str())).unwrap();
                }
                (probe["probe_id"].as_str().unwrap().into(), path)
            })
            .collect::<Vec<_>>();
        output = mcp::step(&state, &answers).unwrap();
    }
    assert!(answered_bad_facet);
    assert_eq!(output.status, "failed");
    assert_eq!(output.exit_code(), 2);
    assert_eq!(code(&output), "DISCOVERY_EVIDENCE");
    assert_eq!(fs::read(&pack).unwrap(), before);
    assert!(!receipt.exists());
}

#[test]
fn non_renderable_kind_facet_answer_cannot_activate_a_pack() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let before = existing_pack(&pack, &request(), false);
    let mut output = mcp::init(request(), &state, &pack, &receipt).unwrap();
    let mut answered_bad_facet = false;
    while output.status == "needs-probes" {
        let answers = output
            .probes
            .iter()
            .enumerate()
            .map(|(i, probe)| {
                let path = dir.path().join(format!("kind-control-answer-{i}.json"));
                let kind = probe["kind"].as_str().unwrap();
                if kind == "record-kind-facet" && probe["field"] == "#type" {
                    fs::write(&path, r##"{"result":[{"#type":"a\u0000b","events":5}]}"##).unwrap();
                    answered_bad_facet = true;
                } else {
                    fs::write(&path, fixture(kind, probe["field"].as_str())).unwrap();
                }
                (probe["probe_id"].as_str().unwrap().into(), path)
            })
            .collect::<Vec<_>>();
        output = mcp::step(&state, &answers).unwrap();
    }
    assert!(answered_bad_facet);
    assert_eq!(output.status, "failed");
    assert_eq!(output.exit_code(), 2);
    assert_eq!(code(&output), "DISCOVERY_EVIDENCE");
    assert_eq!(fs::read(&pack).unwrap(), before);
    assert!(!receipt.exists());
}

#[test]
fn later_failed_answer_ends_run_without_emitting_unanswered_probes_or_changing_pack() {
    for (raw, expected) in [
        (
            r#"{"result":[],"error":"deadline exceeded"}"#,
            "MCP_DEADLINE",
        ),
        (
            r#"{"result":[],"error":"synthetic tool error"}"#,
            "MCP_TOOL_ERROR",
        ),
        (r#"{"result":[],"warnings":["partial"]}"#, "MCP_INCOMPLETE"),
        (r#"{"result":[null]}"#, "MCP_SHAPE"),
    ] {
        let dir = Temp::new();
        let (state, pack, receipt) = paths(dir.path());
        let before = existing_pack(&pack, &request(), false);
        let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
        assert_eq!(first.probes[0]["kind"], "volume");
        assert_eq!(first.probes[1]["kind"], "dataset-facet");
        let path = dir.path().join("bad-answer.json");
        fs::write(&path, raw).unwrap();
        let output = mcp::step(
            &state,
            &[(first.probes[1]["probe_id"].as_str().unwrap().into(), path)],
        )
        .unwrap();
        assert_eq!(output.status, "failed");
        assert_eq!(output.exit_code(), 2);
        assert_eq!(code(&output), expected);
        assert!(output.probes.is_empty());
        let again = mcp::step(&state, &[]).unwrap();
        assert_eq!(again.status, "failed");
        assert_eq!(again.exit_code(), 2);
        assert_eq!(code(&again), expected);
        assert!(again.probes.is_empty());
        let retried = mcp::step(
            &state,
            &[(
                first.probes[1]["probe_id"].as_str().unwrap().into(),
                dir.path().join("bad-answer.json"),
            )],
        )
        .unwrap();
        assert_eq!(retried.status, "failed");
        assert_eq!(code(&retried), expected);
        assert!(retried.probes.is_empty());
        assert_eq!(recorded(&state).status, "failed");
        assert_eq!(fs::read(&pack).unwrap(), before);
        assert!(!receipt.exists());
    }
}

#[test]
fn ten_thousand_object_rows_fail_immediately_even_when_earlier_probe_is_unanswered() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let before = existing_pack(&pack, &request(), false);
    let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
    let path = dir.path().join("ten-thousand.json");
    fs::write(&path, json!({"result":vec![json!({});10_000]}).to_string()).unwrap();
    let output = mcp::step(
        &state,
        &[(first.probes[1]["probe_id"].as_str().unwrap().into(), path)],
    )
    .unwrap();
    assert_eq!(output.status, "failed");
    assert_eq!(output.exit_code(), 2);
    assert_eq!(code(&output), "SEARCH_ROW_LIMIT");
    assert!(output.probes.is_empty());
    let repeated = mcp::step(&state, &[]).unwrap();
    assert_eq!(repeated.status, "failed");
    assert_eq!(code(&repeated), "SEARCH_ROW_LIMIT");
    assert_eq!(fs::read(&pack).unwrap(), before);
    assert!(!receipt.exists());
}

#[test]
fn persisted_rows_are_not_authoritative_and_raw_bytes_require_matching_sha256() {
    for edit in ["row", "raw"] {
        let dir = Temp::new();
        let (state, pack, receipt) = paths(dir.path());
        let before = existing_pack(&pack, &request(), false);
        let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
        let path = dir.path().join("count.json");
        fs::write(&path, COUNT).unwrap();
        mcp::step(
            &state,
            &[(first.probes[0]["probe_id"].as_str().unwrap().into(), path)],
        )
        .unwrap();
        let mut saved: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
        let id = first.probes[0]["probe_id"].as_str().unwrap();
        if edit == "row" {
            saved["answers"][id]["rows"] = json!([{"events":"999999"}]);
        } else {
            let raw = saved["answers"][id]["raw_hex"].as_str().unwrap();
            let changed = format!("00{}", &raw[2..]);
            saved["answers"][id]["raw_hex"] = json!(changed);
        }
        fs::write(&state, serde_json::to_vec(&saved).unwrap()).unwrap();
        assert_eq!(
            mcp::step(&state, &[]).unwrap_err().code,
            "MCP_STATE_TAMPERED"
        );
        assert_eq!(fs::read(&pack).unwrap(), before);
        assert!(!receipt.exists());
    }
}

#[test]
fn forged_activated_status_without_matching_pack_and_receipt_is_rejected() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    mcp::init(request(), &state, &pack, &receipt).unwrap();
    let mut saved: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    saved["status"] = json!("activated");
    fs::write(&state, serde_json::to_vec(&saved).unwrap()).unwrap();
    assert_eq!(
        mcp::step(&state, &[]).unwrap_err().code,
        "MCP_STATE_TAMPERED"
    );
    assert!(!pack.exists());
    assert!(!receipt.exists());
}

#[test]
fn activated_pack_digest_and_receipt_are_checked_on_every_step() {
    for remove_receipt in [false, true] {
        let dir = Temp::new();
        let (state, pack, receipt) = paths(dir.path());
        let mut output = mcp::init(request(), &state, &pack, &receipt).unwrap();
        while output.status == "needs-probes" {
            output = answer_batch(dir.path(), &state, &output);
        }
        assert_eq!(output.status, "complete");
        let saved: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
        assert_eq!(saved["pack_sha256"].as_str().map(str::len), Some(64));
        if remove_receipt {
            fs::remove_file(&receipt).unwrap();
        } else {
            let changed = existing_pack(&pack, &request(), false);
            assert_ne!(sha256_hex(&changed), saved["pack_sha256"].as_str().unwrap());
        }
        assert_eq!(
            mcp::step(&state, &[]).unwrap_err().code,
            "MCP_STATE_TAMPERED"
        );
    }
}

#[test]
fn duplicate_value_fields_are_rejected_by_both_api_and_mcp_validation() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let mut r = request();
    r.value_fields.push("user".into());
    assert_eq!(
        discovery::validate_request(&r).unwrap_err().code,
        "DISCOVERY_EVIDENCE"
    );
    assert_eq!(
        mcp::init(r.clone(), &state, &pack, &receipt)
            .unwrap_err()
            .code,
        "DISCOVERY_EVIDENCE"
    );
    let mut backend = FakeApi {
        answers: BTreeMap::new(),
        starts: vec![],
        active: None,
        stops: 0,
    };
    let (candidate, report) = discovery::execute(&r, &mut backend);
    assert!(candidate.is_none());
    assert_eq!(report.diagnostics[0].code, "DISCOVERY_EVIDENCE");
    assert!(backend.starts.is_empty());
    assert!(!state.exists());
    assert!(!pack.exists());
}

#[test]
fn empty_fieldset_zero_volume_and_exhausted_budget_fail_closed() {
    for (kind, raw, expected) in [
        ("fieldset", r#"{"result":[]}"#, "DISCOVERY_EVIDENCE"),
        (
            "volume",
            r#"{"result":[{"events":"0"}]}"#,
            "DISCOVERY_EVIDENCE",
        ),
    ] {
        let dir = Temp::new();
        let (state, pack, receipt) = paths(dir.path());
        let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
        let answers = first
            .probes
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let path = dir.path().join(format!("base-{i}.json"));
                fs::write(
                    &path,
                    if p["kind"] == kind {
                        raw
                    } else {
                        fixture(p["kind"].as_str().unwrap(), p["field"].as_str())
                    },
                )
                .unwrap();
                (p["probe_id"].as_str().unwrap().into(), path)
            })
            .collect::<Vec<_>>();
        let output = mcp::step(&state, &answers).unwrap();
        assert_eq!(output.status, "failed");
        assert_eq!(code(&output), expected);
        assert_eq!(code(&mcp::step(&state, &[]).unwrap()), expected);
        assert!(!pack.exists());
        assert!(!receipt.exists());
    }
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let mut r = request();
    r.max_queries = 1;
    let first = mcp::init(r, &state, &pack, &receipt).unwrap();
    assert_eq!(first.probes.len(), 1);
    let output = answer_batch(dir.path(), &state, &first);
    assert_eq!(output.status, "failed");
    assert_eq!(code(&output), "DISCOVERY_EVIDENCE");
    assert!(!pack.exists());
}

#[test]
fn unknown_future_duplicate_and_tampered_probes_are_rejected() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
    let path = dir.path().join("answer.json");
    fs::write(&path, COUNT).unwrap();
    let unknown = mcp::step(&state, &[("p-0000000000000000".into(), path.clone())]).unwrap_err();
    assert_eq!(unknown.code, "MCP_UNKNOWN_PROBE");
    let id = first.probes[0]["probe_id"].as_str().unwrap().to_string();
    let base = first
        .probes
        .iter()
        .map(|p| {
            let id = p["probe_id"].as_str().unwrap().into();
            let rows = mcp::normalize(
                fixture(p["kind"].as_str().unwrap(), p["field"].as_str()).as_bytes(),
            )
            .unwrap()
            .rows
            .into_iter()
            .map(Value::Object)
            .collect();
            (id, rows)
        })
        .collect();
    let future = discovery::frontier(&request(), &base).unwrap();
    assert_eq!(
        mcp::step(
            &state,
            &[(
                discovery::probe_id(&request().environment.view, &future[0]),
                path.clone()
            )],
        )
        .unwrap_err()
        .code,
        "MCP_UNKNOWN_PROBE"
    );
    let second = mcp::step(&state, &[(id.clone(), path.clone())]).unwrap();
    assert_eq!(second.status, "needs-probes");
    let duplicate = mcp::step(&state, &[(id, path)]).unwrap_err();
    assert_eq!(duplicate.code, "MCP_ALREADY_ANSWERED");
    assert!(!pack.exists());
    assert!(!receipt.exists());
    let mut raw: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    raw["request"]["environment"]["view"] = json!("changed-view");
    fs::write(&state, serde_json::to_vec(&raw).unwrap()).unwrap();
    let tampered = mcp::step(&state, &[]).unwrap_err();
    assert_eq!(tampered.code, "MCP_STATE_TAMPERED");
    assert!(!pack.exists());
}

#[test]
fn answer_after_wall_clock_budget_is_recorded_as_deadline() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
    let mut raw: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    raw["created_at"] = json!("2020-01-01T00:00:00Z");
    fs::write(&state, serde_json::to_vec(&raw).unwrap()).unwrap();
    let path = dir.path().join("late-answer.json");
    fs::write(&path, COUNT).unwrap();
    let output = mcp::step(
        &state,
        &[(first.probes[0]["probe_id"].as_str().unwrap().into(), path)],
    )
    .unwrap();
    assert_eq!(output.status, "failed");
    assert_eq!(code(&output), "MCP_DEADLINE");
    assert_eq!(recorded(&state).answers.len(), 1);
    assert!(!pack.exists());
}

#[test]
fn busy_activation_retries_without_receipt_or_repeating_answers() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
    let second = answer_batch(dir.path(), &state, &first);
    let third = answer_batch(dir.path(), &state, &second);
    assert_eq!(third.probes.len(), 2);
    assert!(third.probes.iter().any(|p| p["kind"] == "value-facet"));
    assert!(
        third
            .probes
            .iter()
            .any(|p| p["kind"] == "record-kind-facet")
    );
    let held = storage::Writer::acquire(&pack).unwrap();
    let blocked = answer_batch(dir.path(), &state, &third);
    assert_eq!(blocked.status, "activation-blocked");
    assert_eq!(blocked.exit_code(), 3);
    assert_eq!(code(&blocked), "PACK_BUSY");
    assert!(!pack.exists());
    assert!(!receipt.exists());
    assert_eq!(recorded(&state).status, "activation-blocked");
    drop(held);
    let complete = mcp::step(&state, &[]).unwrap();
    assert_eq!(complete.status, "complete", "{:?}", complete.diagnostics);
    assert_eq!(complete.exit_code(), 0);
    assert!(pack.exists());
    assert_eq!(recorded(&state).status, "activated");
}

#[test]
fn init_requires_distinct_new_paths_and_a_matching_existing_pack() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    assert_eq!(
        mcp::init(request(), &state, &state, &receipt)
            .unwrap_err()
            .code,
        "CLI_INPUT"
    );
    fs::write(&receipt, "{}").unwrap();
    assert_eq!(
        mcp::init(request(), &state, &pack, &receipt)
            .unwrap_err()
            .code,
        "CLI_INPUT"
    );
    fs::remove_file(&receipt).unwrap();
    let output = mcp::init(request(), &state, &pack, &receipt).unwrap();
    assert_eq!(output.status, "needs-probes");
    assert!(!receipt.exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn init_rejects_an_existing_pack_from_another_environment_without_creating_state() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let before = existing_pack(&pack, &request(), true);
    assert_eq!(
        mcp::init(request(), &state, &pack, &receipt)
            .unwrap_err()
            .code,
        "PACK_ENVIRONMENT_MISMATCH"
    );
    assert!(!state.exists());
    assert!(!receipt.exists());
    assert_eq!(fs::read(&pack).unwrap(), before);
}

#[test]
fn activation_rechecks_replaced_pack_environment_without_overwriting_it() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let mut output = mcp::init(request(), &state, &pack, &receipt).unwrap();
    while output.status == "needs-probes"
        && !output.probes.iter().any(|p| p["kind"] == "value-facet")
    {
        output = answer_batch(dir.path(), &state, &output);
    }
    assert!(output.probes.iter().any(|p| p["kind"] == "value-facet"));
    let replaced = existing_pack(&pack, &request(), true);
    let rejected = answer_batch(dir.path(), &state, &output);
    assert_eq!(rejected.status, "failed");
    assert_eq!(rejected.exit_code(), 2);
    assert_eq!(code(&rejected), "PACK_ENVIRONMENT_MISMATCH");
    assert_eq!(
        code(&mcp::step(&state, &[]).unwrap()),
        "PACK_ENVIRONMENT_MISMATCH"
    );
    assert_eq!(fs::read(&pack).unwrap(), replaced);
    assert!(!receipt.exists());
}

#[test]
fn invalid_replacement_pack_fails_activation_without_issuing_a_receipt() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let mut output = mcp::init(request(), &state, &pack, &receipt).unwrap();
    while output.status == "needs-probes"
        && !output.probes.iter().any(|p| p["kind"] == "value-facet")
    {
        output = answer_batch(dir.path(), &state, &output);
    }
    assert!(output.probes.iter().any(|p| p["kind"] == "value-facet"));
    fs::write(&pack, b"{").unwrap();
    let rejected = answer_batch(dir.path(), &state, &output);
    assert_eq!(rejected.status, "failed");
    assert_eq!(rejected.exit_code(), 2);
    assert_eq!(code(&rejected), "PACK_JSON");
    assert_eq!(fs::read(&pack).unwrap(), b"{");
    assert!(!receipt.exists());
}

#[test]
fn expired_run_without_new_answers_does_not_advertise_probes() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    mcp::init(request(), &state, &pack, &receipt).unwrap();
    let mut saved: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    saved["created_at"] = json!("2020-01-01T00:00:00Z");
    fs::write(&state, serde_json::to_vec(&saved).unwrap()).unwrap();
    let output = mcp::step(&state, &[]).unwrap();
    assert_eq!(output.status, "failed");
    assert_eq!(code(&output), "MCP_DEADLINE");
    assert!(output.probes.is_empty());
}

#[test]
fn expired_partially_answered_run_does_not_advertise_probes() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    let first = mcp::init(request(), &state, &pack, &receipt).unwrap();
    let path = dir.path().join("count.json");
    fs::write(&path, COUNT).unwrap();
    mcp::step(
        &state,
        &[(first.probes[0]["probe_id"].as_str().unwrap().into(), path)],
    )
    .unwrap();
    let mut saved: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    saved["created_at"] = json!("2020-01-01T00:00:00Z");
    let id = first.probes[0]["probe_id"].as_str().unwrap();
    saved["answers"][id]["ingested_at"] = json!("2020-01-01T00:00:01Z");
    fs::write(&state, serde_json::to_vec(&saved).unwrap()).unwrap();
    let output = mcp::step(&state, &[]).unwrap();
    assert_eq!(output.status, "failed");
    assert_eq!(code(&output), "MCP_DEADLINE");
    assert!(output.probes.is_empty());
}

#[test]
fn activated_receipt_requires_original_complete_bytes() {
    for replacement in ["", "{", "{}", "{\"active_pack_updated\":true}"] {
        let dir = Temp::new();
        let (state, pack, receipt) = paths(dir.path());
        let mut output = mcp::init(request(), &state, &pack, &receipt).unwrap();
        while output.status == "needs-probes" {
            output = answer_batch(dir.path(), &state, &output);
        }
        assert_eq!(output.status, "complete");
        fs::write(&receipt, replacement).unwrap();
        assert_eq!(
            mcp::step(&state, &[]).unwrap_err().code,
            "MCP_STATE_TAMPERED"
        );
    }
}

#[test]
fn incompatible_internal_state_requires_restart() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    mcp::init(request(), &state, &pack, &receipt).unwrap();
    let mut saved: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    saved["schema_version"] = json!(1);
    saved.as_object_mut().unwrap().remove("discovery_algorithm");
    fs::write(&state, serde_json::to_vec(&saved).unwrap()).unwrap();
    assert_eq!(
        mcp::step(&state, &[]).unwrap_err().code,
        "MCP_STATE_RESTART"
    );
    assert!(!pack.exists());
    assert!(!receipt.exists());
}

#[test]
fn bare_filenames_remain_bound_when_later_steps_use_another_directory() {
    let origin = Temp::new();
    let later = Temp::new();
    fs::write(origin.path().join("request.json"), REQUEST).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .current_dir(origin.path())
        .args([
            "env",
            "discover",
            "--mcp",
            "--request",
            "request.json",
            "--state",
            "state.json",
            "--pack",
            "pack.json",
            "--receipt",
            "receipt.json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let (state, pack, receipt) = paths(origin.path());
    let saved = recorded(&state);
    assert!(saved.pack.is_absolute());
    assert!(saved.receipt.is_absolute());
    assert_eq!(
        saved.pack,
        fs::canonicalize(origin.path()).unwrap().join("pack.json")
    );
    assert_eq!(
        saved.receipt,
        fs::canonicalize(origin.path())
            .unwrap()
            .join("receipt.json")
    );
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .current_dir(later.path())
        .args(["env", "discover", "--mcp", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!pack.exists());
    assert!(!receipt.exists());
    assert!(!later.path().join("pack.json").exists());
}

#[test]
fn path_resolution_failures_keep_their_local_diagnostic() {
    let dir = Temp::new();
    let (state, pack, receipt) = paths(dir.path());
    mcp::init(request(), &state, &pack, &receipt).unwrap();
    let parent_file = dir.path().join("parent-file");
    fs::write(&parent_file, b"regular file").unwrap();
    let mut saved: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    saved["pack"] = json!(parent_file.join("pack.json"));
    fs::write(&state, serde_json::to_vec(&saved).unwrap()).unwrap();
    assert_eq!(mcp::step(&state, &[]).unwrap_err().code, "LOCAL_PATH");
    assert!(!pack.exists());
    assert!(!receipt.exists());
}

#[test]
fn destination_cannot_alias_another_destinations_writer_lock() {
    let names: &[&str] = if cfg!(windows) {
        &[".pack.json.lock", ".PACK.json.lock"]
    } else {
        &[".pack.json.lock"]
    };
    for name in names {
        let dir = Temp::new();
        let (state, pack, _) = paths(dir.path());
        let receipt = dir.path().join(name);
        assert_eq!(
            mcp::init(request(), &state, &pack, &receipt)
                .unwrap_err()
                .code,
            "CLI_INPUT"
        );
        assert!(!state.exists());
        assert!(!pack.exists());
        assert!(!receipt.exists());
    }
}
