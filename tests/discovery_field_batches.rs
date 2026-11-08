use cqlmatic::{
    app,
    contracts::*,
    discovery::{self, DiscoverRequest, FieldSelection, Origin, Probe},
    environment::{self, Pack},
    mcp,
    network::SearchBackend,
    storage,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

fn request(fields: &[&str]) -> DiscoverRequest {
    serde_json::from_value(json!({
        "schema_version":1,
        "environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"},
        "start":"2026-01-01T00:00:00Z", "end":"2026-01-01T01:00:00Z",
        "sources":[{"repo":"synthetic","type":"literal,type"}],
        "population_fields":[{"source":{"repo":"synthetic","type":"literal,type"},"fields":fields}],
        "max_population_fields_per_source":2,
        "max_queries":100,"max_seconds":3600,
        "contracts":[{"source":{"repo":"synthetic","type":"literal,type"},"contract":{
            "source":"synthetic parser declaration", "parser":"synthetic", "retrieved_at":"2025-01-01T00:00:00Z",
            "declared_fields":["first","second","zero"],"fields_to_tag":[]
        }}]
    })).unwrap()
}

struct Scenario {
    sampled: Vec<String>,
    counts: BTreeMap<String, u64>,
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            sampled: vec!["recent".into()],
            counts: BTreeMap::new(),
        }
    }
}
impl Scenario {
    fn rows(&self, probe: &Probe) -> Vec<Value> {
        match probe.kind.as_str() {
            "volume" => vec![json!({"events":1000})],
            "dataset-facet" | "record-kind-facet" => vec![],
            "fieldset" => self
                .sampled
                .iter()
                .map(|field| json!({"field":field}))
                .collect(),
            "tag-detect" => vec![json!({"events":0})],
            "population" => {
                let mut row = json!({"total":1000});
                for (i, field) in probe.fields.iter().enumerate() {
                    row[format!("c{i}")] = json!(self.counts.get(field).copied().unwrap_or(0));
                }
                vec![row]
            }
            other => panic!("Unexpected probe {other}"),
        }
    }
    fn graph(&self, request: &DiscoverRequest) -> (Vec<Probe>, BTreeMap<String, Vec<Value>>) {
        let mut answers = BTreeMap::new();
        let mut probes = Vec::new();
        loop {
            let pending = discovery::frontier(request, &answers).unwrap();
            if pending.is_empty() {
                return (probes, answers);
            }
            assert!(probes.len() < 1000);
            for probe in pending {
                answers.insert(
                    discovery::probe_id(&request.environment.view, &probe),
                    self.rows(&probe),
                );
                probes.push(probe);
            }
        }
    }
}
struct Backend {
    answers: BTreeMap<String, Vec<Value>>,
    emitted: Vec<String>,
    observed_at: String,
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
            "Unexpected execution query {query}"
        );
        self.emitted.push(id.clone());
        Ok(id)
    }
    fn poll(&mut self, _: &str, job: &str, _: Instant) -> Result<Value, Diagnostic> {
        Ok(json!({"done":true,"cancelled":false,"metaData":{},"events":self.answers[job]}))
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        Ok(())
    }
    fn observed_at(&self, _: &str) -> Option<String> {
        Some(self.observed_at.clone())
    }
}
fn execute(
    r: &DiscoverRequest,
    scenario: &Scenario,
    day: u32,
    origin: Origin,
) -> (Pack, Vec<Probe>) {
    let date = format!("2026-02-{day:02}T00:00:00Z");
    let (probes, answers) = scenario.graph(r);
    let mut backend = Backend {
        answers,
        emitted: vec![],
        observed_at: date.clone(),
    };
    let (pack, report) =
        discovery::execute_with_origin(r, &mut backend, origin, Duration::from_secs(3600));
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    assert_eq!(backend.emitted.len(), probes.len());
    let pack = Pack::new(r.environment.clone(), date, pack.unwrap().data).unwrap();
    (pack, probes)
}
fn field<'a>(pack: &'a Pack, path: &str, tag: bool) -> &'a Value {
    pack.data
        .fields
        .iter()
        .find(|field| field["path"] == path && field["is_tag"] == tag)
        .unwrap()
}

#[test]
fn explicit_fields_are_source_bound_bounded_unique_and_optional_on_the_wire() {
    let mut r = request(&["first"]);
    discovery::validate_request(&r).unwrap();
    r.population_fields.clear();
    assert!(
        serde_json::to_value(&r)
            .unwrap()
            .get("population_fields")
            .is_none()
    );
    let restored: DiscoverRequest =
        serde_json::from_value(serde_json::to_value(&r).unwrap()).unwrap();
    assert!(restored.population_fields.is_empty());
    for fields in [
        vec![],
        vec!["first", "first"],
        vec!["first", "second", "zero"],
        vec![""],
        vec!["bad\0field"],
    ] {
        let mut value = serde_json::to_value(request(&["first"])).unwrap();
        value["population_fields"][0]["fields"] = json!(fields);
        assert!(discovery::validate_request(&serde_json::from_value(value).unwrap()).is_err());
    }
    let mut r = request(&["first"]);
    r.population_fields.push(r.population_fields[0].clone());
    assert!(discovery::validate_request(&r).is_err());
    let mut r = request(&["first"]);
    r.population_fields[0].source.type_ = Some("different,type".into());
    assert!(discovery::validate_request(&r).is_err());
    r.sources.clear();
    r.census = true;
    assert!(discovery::validate_request(&r).is_err());
    let mut r = request(&["first"]);
    r.max_population_fields_per_source = None;
    r.population_fields[0].fields = (0..4001).map(|n| format!("f{n}")).collect();
    assert!(discovery::validate_request(&r).is_err());
}

#[test]
fn declared_unsampled_fields_are_counted_without_promoting_zero_declarations() {
    for cap in [Some(2), None] {
        let mut r = request(&["first", "zero"]);
        r.max_population_fields_per_source = cap;
        let scenario = Scenario {
            counts: BTreeMap::from([("first".into(), 7)]),
            ..Scenario::default()
        };
        for origin in [Origin::Api, Origin::Mcp] {
            let (pack, probes) = execute(&r, &scenario, 1, origin);
            let measured = probes
                .iter()
                .filter(|p| p.kind == "population")
                .collect::<Vec<_>>();
            assert_eq!(measured.len(), 1);
            assert_eq!(measured[0].fields, vec!["first", "zero"]);
            assert_eq!(measured[0].start, r.start);
            assert!(
                measured[0]
                    .query
                    .starts_with("#repo = \"synthetic\" | #type = \"literal,type\" |")
            );
            let positive = field(&pack, "first", false);
            assert_eq!(positive["coverage"]["observed"], true);
            assert_eq!(positive["coverage"]["rate"], 0.007);
            assert_eq!(positive["data_type"], "unknown");
            assert_eq!(
                positive["observation_window"],
                format!("{}/{}", r.start, r.end)
            );
            let zero = field(&pack, "zero", false);
            assert_eq!(zero["coverage"]["observed"], false);
            assert_eq!(zero["coverage"]["rate"], 0.0);
            assert_eq!(zero["evidence_kind"], "declared");
            assert!(zero["observation_window"].is_null());
            let provenance = zero["provenance_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|id| {
                    pack.data
                        .provenance
                        .iter()
                        .find(|p| p["id"] == *id)
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(provenance.len(), 2);
            assert!(provenance.iter().any(|p| {
                p["coverage_status"] == "declared-only"
                    && p["retrieved_at"] == "2025-01-01T00:00:00Z"
            }));
            assert!(provenance.iter().any(|p| {
                p["coverage_status"] != "declared-only"
                    && p["observation_window"] == format!("{}/{}", r.start, r.end)
            }));
            let assessment = app::check(
                CheckRequest {
                    schema_version: 1,
                    id: None,
                    query: "#repo=synthetic | #type=\"literal,type\" | zero=*".into(),
                    context: Context {
                        require_observed: true,
                        ..Context::default()
                    },
                },
                Some(&pack),
            );
            assert_eq!(assessment.validation.status, Acceptance::Accepted);
            assert!(
                assessment
                    .grounding
                    .missing
                    .iter()
                    .any(|field| field.contains("zero"))
            );
            assert_ne!(assessment.outcome, Outcome::Ready);
        }
    }
}

#[test]
fn batches_do_not_refill_the_first_fields_or_cross_source_and_tag_namespaces() {
    let mut r = request(&["next", "#next"]);
    let other = Scope {
        repo: Some("other".into()),
        ..Scope::default()
    };
    r.sources.push(other.clone());
    r.population_fields.push(FieldSelection {
        source: other.clone(),
        fields: vec!["remaining".into()],
    });
    let scenario = Scenario {
        sampled: (0..300).map(|i| format!("f{i:04}")).collect(),
        counts: BTreeMap::from([
            ("next".into(), 1),
            ("#next".into(), 2),
            ("remaining".into(), 3),
        ]),
    };
    let (pack, probes) = execute(&r, &scenario, 1, Origin::Api);
    assert_eq!(probes.iter().filter(|p| p.kind == "population").count(), 2);
    assert_eq!(field(&pack, "next", false)["coverage"]["rate"], 0.001);
    assert_eq!(field(&pack, "next", true)["coverage"]["rate"], 0.002);
    assert!(field(&pack, "next", false)["is_array"].is_null());
    for probe in probes.iter().filter(|p| p.kind == "population") {
        assert_eq!(
            probe.fields,
            if probe.source == other {
                vec!["remaining"]
            } else {
                vec!["#next", "next"]
            }
        );
    }
    for path in ["f0000", "f0299"] {
        let pending = field(&pack, path, false);
        assert_eq!(pending["coverage"]["observed"], false);
        assert!(pending["coverage"]["rate"].is_null());
    }
}

#[test]
fn native_batches_retain_unmeasured_fieldset_namespaces_without_grounding_them() {
    for cap in [Some(2), None] {
        for origin in [Origin::Api, Origin::Mcp] {
            let mut r = request(&["pair"]);
            r.contracts.clear();
            r.max_population_fields_per_source = cap;
            let scenario = Scenario {
                sampled: vec!["pair".into(), "#pair".into()],
                counts: BTreeMap::from([("pair".into(), 1), ("#pair".into(), 2)]),
            };
            let (first, probes) = execute(&r, &scenario, 1, origin);
            let populations = probes
                .iter()
                .filter(|p| p.kind == "population")
                .collect::<Vec<_>>();
            assert_eq!(populations.len(), 1);
            assert_eq!(populations[0].fields, vec!["pair"]);
            let pending = field(&first, "pair", true);
            assert_eq!(pending["coverage"]["observed"], false);
            assert!(pending["coverage"]["rate"].is_null());
            assert!(pending["coverage"]["declared"].is_null());
            let result = app::check(
                CheckRequest {
                    schema_version: 1,
                    id: None,
                    query: "#repo=synthetic | #type=\"literal,type\" | #pair=*".into(),
                    context: Context {
                        require_observed: true,
                        ..Context::default()
                    },
                },
                Some(&first),
            );
            assert_ne!(result.outcome, Outcome::Ready);
            assert!(result.grounding.missing.iter().any(|f| f.contains("pair")));

            r.population_fields[0].fields = vec!["#pair".into()];
            let (second, probes) = execute(&r, &scenario, 2, origin);
            let populations = probes
                .iter()
                .filter(|p| p.kind == "population")
                .collect::<Vec<_>>();
            assert_eq!(populations.len(), 1);
            assert_eq!(populations[0].fields, vec!["#pair"]);
            let merged = environment::merge(&first, &second).unwrap();
            assert_eq!(field(&merged, "pair", false), field(&first, "pair", false));
            assert_eq!(field(&merged, "pair", true)["coverage"]["observed"], true);
            assert_eq!(field(&merged, "pair", true)["coverage"]["rate"], 0.002);
        }
    }
}

#[test]
fn parser_declarations_keep_tag_and_event_namespaces_separate() {
    let mut r = request(&["tagged", "#tagged"]);
    r.contracts[0].contract.declared_fields = vec!["#tagged".into()];
    r.contracts[0].contract.array_fields = vec!["#tagged".into()];
    let scenario = Scenario {
        counts: BTreeMap::from([("tagged".into(), 1)]),
        ..Scenario::default()
    };
    let (pack, _) = execute(&r, &scenario, 1, Origin::Api);
    let event = field(&pack, "tagged", false);
    let tag = field(&pack, "tagged", true);
    assert_eq!(event["coverage"]["declared"], false);
    assert!(event["is_array"].is_null());
    assert_eq!(tag["coverage"]["declared"], true);
    assert_eq!(tag["coverage"]["observed"], false);
    assert_eq!(tag["is_array"], true);
    assert_eq!(tag["evidence_kind"], "declared");
}

#[test]
fn a_default_refresh_still_marks_omitted_fields_historical() {
    let mut r = request(&["previous"]);
    r.contracts.clear();
    let scenario = Scenario {
        counts: BTreeMap::from([("previous".into(), 1)]),
        ..Scenario::default()
    };
    let (old, _) = execute(&r, &scenario, 1, Origin::Api);
    r.population_fields.clear();
    r.max_population_fields_per_source = None;
    let scenario = Scenario {
        counts: BTreeMap::from([("recent".into(), 1)]),
        ..Scenario::default()
    };
    let (candidate, _) = execute(&r, &scenario, 2, Origin::Api);
    assert!(
        candidate.data.sources[0]["evidence"]
            .get("measured_fields")
            .is_none()
    );
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(
        field(&merged, "previous", false)["refresh_status"],
        "not-reobserved"
    );
}

#[test]
fn explicit_bounded_selections_cannot_omit_required_observed_fields() {
    let mut r = request(&["first"]);
    r.value_fields.push("recent".into());
    let scenario = Scenario::default();
    let mut answered = BTreeMap::new();
    for _ in 0..20 {
        match discovery::frontier(&r, &answered) {
            Err(diagnostic) => {
                assert!(diagnostic.message.contains("requested kind/value"));
                return;
            }
            Ok(probes) => {
                assert!(!probes.is_empty());
                for probe in probes {
                    answered.insert(
                        discovery::probe_id(&r.environment.view, &probe),
                        scenario.rows(&probe),
                    );
                }
            }
        }
    }
    panic!("Expected an incomplete mandatory field selection rejection");
}

#[test]
fn selected_and_sampled_population_windows_remain_distinct() {
    let r = request(&["first", "recent"]);
    let scenario = Scenario {
        counts: BTreeMap::from([("first".into(), 1), ("recent".into(), 2)]),
        ..Scenario::default()
    };
    let (pack, probes) = execute(&r, &scenario, 1, Origin::Api);
    assert_eq!(
        field(&pack, "first", false)["observation_window"],
        format!("{}/{}", r.start, r.end)
    );
    assert_eq!(
        field(&pack, "recent", false)["observation_window"],
        "2026-01-01T00:45:00Z/2026-01-01T01:00:00Z"
    );
    assert_eq!(probes.iter().filter(|p| p.kind == "population").count(), 2);
}

#[test]
fn repeated_batches_preserve_earlier_observations_and_only_selected_zero_fields_become_historical()
{
    let first = request(&["first"]);
    let scenario = Scenario {
        counts: BTreeMap::from([("first".into(), 1), ("second".into(), 2)]),
        ..Scenario::default()
    };
    let (old, _) = execute(&first, &scenario, 1, Origin::Api);
    let (candidate, _) = execute(&request(&["second"]), &scenario, 2, Origin::Api);
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(field(&merged, "first", false), field(&old, "first", false));
    assert_eq!(
        field(&merged, "second", false)["coverage"]["observed"],
        true
    );
    let scenario = Scenario::default();
    let (candidate, _) = execute(&first, &scenario, 3, Origin::Api);
    let next = environment::merge(&merged, &candidate).unwrap();
    assert_eq!(
        field(&next, "second", false),
        field(&merged, "second", false)
    );
    assert_eq!(
        field(&next, "first", false)["refresh_status"],
        "not-reobserved"
    );
    assert_eq!(field(&next, "first", false)["coverage"]["observed"], false);
    assert_eq!(
        next.data.sources[0]["evidence"]["measured_fields"],
        json!(["first"])
    );
    assert!(
        next.data
            .provenance
            .iter()
            .any(|p| p["retrieved_at"] == "2025-01-01T00:00:00Z")
    );
}

#[test]
fn unobserved_undeclared_selected_fields_record_zero_measurements() {
    let (pack, _) = execute(
        &request(&["not.declared"]),
        &Scenario::default(),
        1,
        Origin::Api,
    );
    let zero = field(&pack, "not.declared", false);
    assert_eq!(zero["coverage"]["declared"], false);
    assert_eq!(zero["coverage"]["observed"], false);
    assert_eq!(zero["coverage"]["rate"], 0.0);
    assert_eq!(zero["evidence_kind"], "declared");
    assert!(zero["observation_window"].is_null());
    assert_eq!(zero["provenance_ids"].as_array().unwrap().len(), 1);
    let provenance = pack
        .data
        .provenance
        .iter()
        .find(|p| p["id"] == zero["provenance_ids"][0])
        .unwrap();
    assert_ne!(provenance["coverage_status"], "declared-only");
    assert_eq!(
        provenance["observation_window"],
        "2026-01-01T00:00:00Z/2026-01-01T01:00:00Z"
    );
    assert_eq!(
        pack.data.sources[0]["evidence"]["measured_fields"],
        json!(["not.declared"])
    );
}

#[test]
fn selecting_an_ordinary_zero_keeps_the_prior_tag_observation_and_stable_ids() {
    let mut r = request(&["pair", "#pair"]);
    r.contracts.clear();
    let scenario = Scenario {
        counts: BTreeMap::from([("pair".into(), 1), ("#pair".into(), 2)]),
        ..Scenario::default()
    };
    let (mut old, _) = execute(&r, &scenario, 1, Origin::Api);
    old.data.sources[0]["id"] = json!("env:repo:stable");
    for f in &mut old.data.fields {
        f["source_id"] = json!("env:repo:stable");
        f["id"] = json!(format!(
            "env:field:stable:{}{}",
            if f["is_tag"] == true { "#" } else { "" },
            f["path"].as_str().unwrap()
        ));
    }
    old.content_digest = old.digest().unwrap();
    old.validate().unwrap();
    r.population_fields[0].fields = vec!["pair".into()];
    let (candidate, _) = execute(&r, &Scenario::default(), 2, Origin::Api);
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(
        field(&merged, "pair", false)["refresh_status"],
        "not-reobserved"
    );
    assert_eq!(field(&merged, "pair", true), field(&old, "pair", true));
    assert_eq!(merged.data.sources[0]["id"], "env:repo:stable");
    assert!(
        merged
            .data
            .fields
            .iter()
            .all(|f| f["source_id"] == "env:repo:stable")
    );
}

#[test]
fn batch_membership_requires_source_observation_provenance() {
    let (pack, _) = execute(&request(&["first"]), &Scenario::default(), 1, Origin::Api);
    for change in [
        "duplicate",
        "empty",
        "too-many",
        "unscoped",
        "unobserved",
        "no-window",
        "declaration-only",
    ] {
        let mut invalid = pack.clone();
        match change {
            "duplicate" => {
                invalid.data.sources[0]["evidence"]["measured_fields"] = json!(["first", "first"])
            }
            "empty" => invalid.data.sources[0]["evidence"]["measured_fields"] = json!([]),
            "too-many" => {
                invalid.data.sources[0]["evidence"]["measured_fields"] =
                    json!((0..4001).map(|i| format!("f{i}")).collect::<Vec<_>>())
            }
            "unscoped" => invalid.data.sources[0]["selectors"] = Value::Null,
            "unobserved" => invalid.data.sources[0]["evidence"]["observed"] = json!(false),
            "no-window" => invalid.data.sources[0]["evidence"]["observation_window"] = Value::Null,
            "declaration-only" => {
                for p in &mut invalid.data.provenance {
                    p["coverage_status"] = json!("declared-only");
                }
            }
            _ => unreachable!(),
        }
        invalid.content_digest = invalid.digest().unwrap();
        assert!(invalid.validate().is_err(), "Accepted {change}");
    }
}

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".local")
            .join(format!("field-batch-relay-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn explicit_batches_relay_the_same_api_frontier_without_plan_drift() {
    let directory = TestDir::new();
    let r = request(&["first", "zero"]);
    let scenario = Scenario {
        counts: BTreeMap::from([("first".into(), 7)]),
        ..Scenario::default()
    };
    let state = directory.0.join("state.json");
    let pack_path = directory.0.join("pack.json");
    let receipt = directory.0.join("receipt.json");
    let mut result = mcp::init(r.clone(), &state, &pack_path, &receipt).unwrap();
    let mut relayed = Vec::new();
    let mut round = 0;
    while result.status == "needs-probes" {
        round += 1;
        assert!(round < 10);
        let mut answers = Vec::new();
        for (index, wire) in result.probes.iter().enumerate() {
            assert_eq!(wire["mcp"]["arguments"]["repository"], "search-all");
            let arguments = &wire["mcp"]["arguments"];
            let probe = Probe {
                kind: wire["kind"].as_str().unwrap().into(),
                source: serde_json::from_value(wire["source"].clone()).unwrap(),
                query: arguments["query_string"].as_str().unwrap().into(),
                start: arguments["start"].as_str().unwrap().into(),
                end: arguments["end"].as_str().unwrap().into(),
                fields: serde_json::from_value(wire["fields"].clone()).unwrap(),
                field: serde_json::from_value(wire["field"].clone()).unwrap(),
            };
            let path = directory.0.join(format!("answer-{round}-{index}.json"));
            fs::write(&path, json!({"result":scenario.rows(&probe)}).to_string()).unwrap();
            answers.push((wire["probe_id"].as_str().unwrap().to_string(), path));
            relayed.push(probe);
        }
        result = mcp::step(&state, &answers).unwrap();
    }
    assert_eq!(result.status, "complete", "{:?}", result.diagnostics);
    let (api, api_probes) = execute(&r, &scenario, 1, Origin::Api);
    assert_eq!(relayed, api_probes);
    let relayed = storage::read_pack(&pack_path).unwrap();
    for path in ["first", "zero"] {
        assert_eq!(
            field(&relayed, path, false)["coverage"],
            field(&api, path, false)["coverage"]
        );
    }
    assert_eq!(
        relayed.data.sources[0]["evidence"]["measured_fields"],
        json!(["first", "zero"])
    );
}
