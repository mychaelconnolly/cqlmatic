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
            "tag-detect" => vec![
                json!({"events":self.counts.get(&format!("tag:#{}", probe.field.as_deref().unwrap())).copied().unwrap_or(0)}),
            ],
            "population" => {
                let mut row = json!({"total":1000});
                for (i, field) in probe.fields.iter().enumerate() {
                    row[format!("c{i}")] = json!(self.counts.get(field).copied().unwrap_or(0));
                }
                vec![row]
            }
            "value-facet" => self
                .counts
                .get(probe.field.as_ref().unwrap())
                .filter(|count| **count > 0)
                .map(|count| vec![json!({probe.field.as_ref().unwrap():"sample","events":count})])
                .unwrap_or_default(),
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
    retrievals: BTreeMap<String, String>,
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
    fn observed_at(&self, job: &str) -> Option<String> {
        Some(
            self.retrievals
                .get(job)
                .unwrap_or(&self.observed_at)
                .clone(),
        )
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
        retrievals: BTreeMap::new(),
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
            assert_eq!(zero["evidence_kind"], "observed");
            assert_eq!(zero["observation_window"], format!("{}/{}", r.start, r.end));
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
        assert!(pending["coverage"]["observed"].is_null());
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
            assert!(pending["coverage"]["observed"].is_null());
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
    assert_eq!(tag["evidence_kind"], "observed");
}

#[test]
fn a_default_refresh_preserves_fields_outside_its_actual_measurements() {
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
    assert_eq!(
        candidate.data.sources[0]["evidence"]["measured_fields"],
        json!(["recent"])
    );
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(
        field(&merged, "previous", false),
        field(&old, "previous", false)
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
fn repeated_batches_preserve_earlier_observations_and_selected_zeros_are_current() {
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
    assert_eq!(field(&next, "first", false)["refresh_status"], Value::Null);
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
    assert_eq!(zero["evidence_kind"], "observed");
    assert_eq!(
        zero["observation_window"],
        format!(
            "{}/{}",
            request(&["not.declared"]).start,
            request(&["not.declared"]).end
        )
    );
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
    assert_eq!(field(&merged, "pair", false)["refresh_status"], Value::Null);
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

#[test]
fn type_only_population_batches_and_source_only_refresh_preserve_field_evidence() {
    let mut r = request(&["first"]);
    r.sources[0].repo = None;
    r.population_fields[0].source.repo = None;
    r.contracts[0].source.repo = None;
    let scenario = Scenario {
        counts: BTreeMap::from([("first".into(), 500)]),
        ..Scenario::default()
    };
    let (old, _) = execute(&r, &scenario, 1, Origin::Mcp);
    assert_eq!(old.data.sources[0]["selectors"]["repo"], Value::Null);
    assert_eq!(field(&old, "first", false)["coverage"]["observed"], true);
    let mut quick = r.clone();
    quick.depth = discovery::DiscoveryDepth::SourcesOnly;
    quick.population_fields.clear();
    quick.contracts.clear();
    quick.value_fields.clear();
    quick.kind_fields.clear();
    quick.max_population_fields_per_source = None;
    let (candidate, probes) = execute(&quick, &scenario, 2, Origin::Mcp);
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].kind, "volume");
    assert!(candidate.data.fields.is_empty());
    assert_eq!(
        candidate.data.sources[0]["evidence"]["discovery_depth"],
        "sources-only"
    );
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(merged.data.fields, old.data.fields);
    assert_eq!(merged.data.values, old.data.values);
}

#[test]
fn a_twenty_field_refresh_preserves_other_current_records_and_dates() {
    let names = (0..40)
        .map(|index| format!("f{index:02}"))
        .collect::<Vec<_>>();
    let mut r = request(&["placeholder"]);
    r.contracts.clear();
    r.population_fields[0].fields = names.clone();
    r.max_population_fields_per_source = Some(40);
    let scenario = Scenario {
        sampled: names.clone(),
        counts: names.iter().map(|name| (name.clone(), 1)).collect(),
    };
    let (old, _) = execute(&r, &scenario, 1, Origin::Mcp);
    r.population_fields.clear();
    r.max_population_fields_per_source = Some(20);
    let (candidate, _) = execute(&r, &scenario, 2, Origin::Mcp);
    assert_eq!(
        candidate.data.sources[0]["evidence"]["measured_fields"]
            .as_array()
            .unwrap()
            .len(),
        20
    );
    let merged = environment::merge(&old, &candidate).unwrap();
    for name in &names[20..] {
        assert_eq!(field(&merged, name, false), field(&old, name, false));
    }
    assert!(field(&merged, "f20", false)["refresh_status"].is_null());
}

#[test]
fn auxiliary_measurements_keep_exact_namespaces_and_zero_facets_outside_the_population_cap() {
    let mut r = request(&["first"]);
    r.contracts.clear();
    r.max_population_fields_per_source = Some(1);
    let scenario = Scenario {
        sampled: vec!["Vendor".into(), "event.kind".into()],
        counts: BTreeMap::from([("first".into(), 1), ("tag:#Vendor".into(), 7)]),
    };
    let (pack, _) = execute(&r, &scenario, 1, Origin::Mcp);
    assert_eq!(
        pack.data.sources[0]["evidence"]["measured_fields"],
        json!(["first"])
    );
    assert!(field(&pack, "Vendor", false)["coverage"]["observed"].is_null());
    assert_eq!(field(&pack, "Vendor", true)["coverage"]["observed"], true);
    for (path, tag) in [("event.dataset", true), ("event.kind", false)] {
        let zero = field(&pack, path, tag);
        assert_eq!(zero["evidence_kind"], "observed");
        assert_eq!(zero["coverage"]["observed"], false);
        assert!(zero["coverage"]["rate"].is_null());
        assert!(zero["observation_window"].is_string());
        assert_eq!(zero["provenance_ids"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn population_batches_near_the_limit_do_not_absorb_auxiliary_fields() {
    let mut r = request(&["placeholder"]);
    r.contracts.clear();
    r.max_population_fields_per_source = Some(4000);
    r.max_queries = 1000;
    r.population_fields[0].fields = (0..3998).map(|index| format!("f{index:04}")).collect();
    let (pack, _) = execute(&r, &Scenario::default(), 1, Origin::Api);
    assert_eq!(
        pack.data.sources[0]["evidence"]["measured_fields"]
            .as_array()
            .unwrap()
            .len(),
        3998
    );
    assert!(pack.data.fields.len() > 4000);
    assert_eq!(field(&pack, "Vendor", true)["coverage"]["observed"], false);
    pack.validate().unwrap();
}

#[test]
fn a_narrower_auxiliary_zero_controls_presence_without_borrowing_the_population_rate() {
    let mut r = request(&["#Vendor"]);
    r.contracts.clear();
    let scenario = Scenario {
        counts: BTreeMap::from([("#Vendor".into(), 10)]),
        ..Scenario::default()
    };
    let (pack, _) = execute(&r, &scenario, 1, Origin::Api);
    let vendor = field(&pack, "Vendor", true);
    assert_eq!(vendor["coverage"]["observed"], false);
    assert!(vendor["coverage"]["rate"].is_null());
    assert_eq!(
        vendor["observation_window"],
        "2026-01-01T00:45:00Z/2026-01-01T01:00:00Z"
    );
    let proof = pack
        .data
        .provenance
        .iter()
        .find(|proof| proof["id"] == vendor["provenance_ids"][0])
        .unwrap();
    assert_eq!(proof["observation_window"], vendor["observation_window"]);
    assert!(proof["id"].as_str().unwrap().contains("tag-detect"));
    assert!(
        pack.data
            .provenance
            .iter()
            .any(|proof| proof["id"].as_str().unwrap().contains("population"))
    );
}

#[test]
fn heterogeneous_empty_value_facets_succeed_but_nonempty_unknown_fields_are_rejected() {
    let mut r = request(&["first"]);
    r.contracts.clear();
    r.value_fields = vec!["absent".into()];
    let (pack, probes) = execute(&r, &Scenario::default(), 1, Origin::Api);
    assert!(pack.data.values.is_empty());
    assert!(probes.iter().any(|probe| probe.kind == "value-facet"));
    let scenario = Scenario {
        counts: BTreeMap::from([("absent".into(), 1)]),
        ..Scenario::default()
    };
    let (_, answers) = scenario.graph(&r);
    let mut backend = Backend {
        answers,
        emitted: vec![],
        observed_at: "2026-02-01T00:00:00Z".into(),
        retrievals: BTreeMap::new(),
    };
    let (pack, report) = discovery::execute(&r, &mut backend);
    assert!(pack.is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("value field"))
    );
}

#[test]
fn current_proofs_keep_each_winning_captures_retrieval_date() {
    let mut r = request(&["first"]);
    r.contracts.clear();
    r.value_fields = vec!["absent".into()];
    let scenario = Scenario {
        counts: BTreeMap::from([("first".into(), 5)]),
        ..Scenario::default()
    };
    let (probes, answers) = scenario.graph(&r);
    let retrievals = probes
        .iter()
        .map(|probe| {
            let date = match probe.kind.as_str() {
                "population" => "2026-02-02T00:00:00Z",
                "tag-detect" => "2026-02-03T00:00:00Z",
                "value-facet" => "2026-02-04T00:00:00Z",
                _ => "2026-02-01T00:00:00Z",
            };
            (discovery::probe_id(&r.environment.view, probe), date.into())
        })
        .collect();
    let mut backend = Backend {
        answers,
        emitted: vec![],
        observed_at: "2026-02-01T00:00:00Z".into(),
        retrievals,
    };
    let (pack, report) = discovery::execute(&r, &mut backend);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    let pack = pack.unwrap();
    for (path, tag, date) in [
        ("first", false, "2026-02-02T00:00:00Z"),
        ("Vendor", true, "2026-02-03T00:00:00Z"),
        ("recent", false, "2026-02-01T00:00:00Z"),
    ] {
        let record = field(&pack, path, tag);
        let proof = pack
            .data
            .provenance
            .iter()
            .find(|proof| proof["id"] == record["provenance_ids"][0])
            .unwrap();
        assert_eq!(proof["retrieved_at"], date);
        assert_eq!(proof["observation_window"], record["observation_window"]);
        assert_eq!(record["provenance_ids"].as_array().unwrap().len(), 1);
    }
}

#[test]
fn tied_native_capture_presence_conflicts_prevent_pack_construction() {
    let mut r = request(&["#Vendor"]);
    r.contracts.clear();
    r.start = "2026-01-01T00:45:00Z".into();
    let scenario = Scenario {
        counts: BTreeMap::from([("#Vendor".into(), 1)]),
        ..Scenario::default()
    };
    let (_, answers) = scenario.graph(&r);
    let mut backend = Backend {
        answers,
        emitted: vec![],
        observed_at: "2026-02-01T00:00:00Z".into(),
        retrievals: BTreeMap::new(),
    };
    let (pack, report) = discovery::execute(&r, &mut backend);
    assert!(pack.is_none());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("Tied field measurements"))
    );
}

#[test]
fn every_fieldset_alias_retains_unmeasured_sample_and_kind_capture_proof() {
    for alias in ["field", "fieldname", "name", "_field"] {
        for rare_kind in [false, true] {
            let mut r = request(&["first"]);
            r.contracts.clear();
            r.max_population_fields_per_source = Some(1);
            let scenario = Scenario {
                counts: BTreeMap::from([("first".into(), 1)]),
                ..Scenario::default()
            };
            let mut answers = BTreeMap::new();
            let mut probes = Vec::new();
            loop {
                let pending = discovery::frontier(&r, &answers).unwrap();
                if pending.is_empty() {
                    break;
                }
                assert!(probes.len() < 100);
                for probe in pending {
                    let rows = match probe.kind.as_str() {
                        "fieldset" => {
                            let names = if rare_kind {
                                vec!["recent", "event.kind"]
                            } else {
                                vec!["unmeasured"]
                            };
                            names.into_iter().map(|name| json!({alias:name})).collect()
                        }
                        "kind-fieldset" => vec![json!({alias:"unmeasured"})],
                        "record-kind-facet"
                            if rare_kind && probe.field.as_deref() == Some("event.kind") =>
                        {
                            vec![
                                json!({"event.kind":"alpha","events":1}),
                                json!({"event.kind":"beta","events":2}),
                            ]
                        }
                        _ => scenario.rows(&probe),
                    };
                    answers.insert(discovery::probe_id(&r.environment.view, &probe), rows);
                    probes.push(probe);
                }
            }
            let retrievals = probes
                .iter()
                .enumerate()
                .map(|(index, probe)| {
                    (
                        discovery::probe_id(&r.environment.view, probe),
                        format!("2026-02-01T00:00:{index:02}Z"),
                    )
                })
                .collect();
            let mut backend = Backend {
                answers,
                emitted: vec![],
                observed_at: "2026-02-01T00:00:00Z".into(),
                retrievals,
            };
            let (pack, report) = discovery::execute(&r, &mut backend);
            assert_eq!(
                report.status, "complete",
                "{alias}, rare={rare_kind}: {:?}",
                report.diagnostics
            );
            let pack = pack.unwrap();
            let unmeasured = field(&pack, "unmeasured", false);
            assert_eq!(unmeasured["evidence_kind"], "documented");
            assert!(unmeasured["coverage"]["observed"].is_null());
            assert!(unmeasured["coverage"]["rate"].is_null());
            assert_eq!(unmeasured["provenance_ids"].as_array().unwrap().len(), 1);
            assert_eq!(
                pack.data.sources[0]["evidence"]["measured_fields"],
                json!(["first"])
            );
            let kind = if rare_kind {
                "kind-fieldset"
            } else {
                "fieldset"
            };
            let capture =
                report
                    .captures
                    .iter()
                    .find(|capture| {
                        capture.probe.kind == kind
                            && capture.receipt.events.iter().any(|row| {
                                row.get(alias).and_then(Value::as_str) == Some("unmeasured")
                            })
                    })
                    .unwrap();
            let proof = pack
                .data
                .provenance
                .iter()
                .find(|proof| proof["id"] == unmeasured["provenance_ids"][0])
                .unwrap();
            assert_eq!(proof["retrieved_at"], capture.retrieved_at);
            assert_eq!(
                proof["observation_window"],
                format!("{}/{}", capture.probe.start, capture.probe.end)
            );
            assert_eq!(
                proof["observation_window"],
                unmeasured["observation_window"]
            );
        }
    }
}
