use cqlmatic::{
    contracts::{Diagnostic, EnvironmentId},
    discovery::{self, DiscoverRequest, Probe},
    environment::{Pack, PackData},
    mcp,
    network::SearchBackend,
    storage,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

fn request() -> DiscoverRequest {
    serde_json::from_value(json!({
        "schema_version":1,
        "environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"},
        "start":"2026-01-01T00:00:00Z",
        "end":"2026-01-01T01:00:00Z",
        "sources":[{"repo":"synthetic"}],
        "max_population_fields_per_source":128,
        "max_queries":40,
        "max_seconds":3600
    }))
    .unwrap()
}
struct Scenario {
    sampled: Vec<String>,
    facets: BTreeMap<String, Vec<Value>>,
    datasets: Vec<Value>,
    rare_fields: Vec<String>,
}
impl Default for Scenario {
    fn default() -> Self {
        Self {
            sampled: vec!["#type".into(), "recent".into()],
            facets: BTreeMap::from([(
                "#type".into(),
                vec![
                    json!({"#type":"rare","events":10}),
                    json!({"#type":"common","events":999990}),
                ],
            )]),
            datasets: vec![json!({"#event.dataset":"synthetic","events":1000000})],
            rare_fields: vec!["rare.new".into(), "recent".into(), "rare.new".into()],
        }
    }
}
impl Scenario {
    fn rows(&self, request: &DiscoverRequest, p: &Probe) -> Vec<Value> {
        match p.kind.as_str() {
            "volume" => vec![json!({"events":1000000})],
            "dataset-facet" => self.datasets.clone(),
            "fieldset" => self
                .sampled
                .iter()
                .map(|field| json!({"field":field}))
                .collect(),
            "tag-detect" => vec![json!({"events":0})],
            "record-kind-facet" => self.facets[p.field.as_ref().unwrap()].clone(),
            "kind-fieldset" => self
                .rare_fields
                .iter()
                .map(|field| json!({"field":field}))
                .collect(),
            "population" => {
                let (count, total) = if p.start == request.start {
                    (10, 1_000_000)
                } else {
                    (50, 100)
                };
                let mut row = json!({"total":total});
                for (index, _) in p.fields.iter().enumerate() {
                    row[format!("c{index}")] = json!(count);
                }
                vec![row]
            }
            "value-facet" => vec![json!({p.field.as_ref().unwrap():"synthetic","events":1})],
            _ => panic!("unexpected probe: {p:?}"),
        }
    }
    fn graph(&self, request: &DiscoverRequest) -> (Vec<Probe>, BTreeMap<String, Vec<Value>>) {
        let mut answered = BTreeMap::new();
        let mut probes = Vec::new();
        loop {
            let pending = discovery::frontier(request, &answered).unwrap();
            if pending.is_empty() {
                return (probes, answered);
            }
            assert!(probes.len() < 1000);
            for p in pending {
                answered.insert(
                    discovery::probe_id(&request.environment.view, &p),
                    self.rows(request, &p),
                );
                probes.push(p);
            }
        }
    }
}
struct Backend {
    answers: BTreeMap<String, Vec<Value>>,
    emitted: Vec<(String, String, String, String)>,
    stops: usize,
    timeout: Option<String>,
}
impl Backend {
    fn new(answers: BTreeMap<String, Vec<Value>>) -> Self {
        Self {
            answers,
            emitted: vec![],
            stops: 0,
            timeout: None,
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
        assert!(self.answers.contains_key(&id), "unexpected query: {query}");
        self.emitted
            .push((view.into(), query.into(), start.into(), end.into()));
        Ok(id)
    }
    fn poll(&mut self, _: &str, id: &str, _: Instant) -> Result<Value, Diagnostic> {
        if self.timeout.as_deref() == Some(id) {
            return Err(Diagnostic::error("TEST_TIMEOUT", "synthetic timeout"));
        }
        Ok(json!({"done":true,"cancelled":false,"metaData":{},"events":self.answers[id]}))
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        self.stops += 1;
        Ok(())
    }
}
fn execute(request: &DiscoverRequest, scenario: &Scenario) -> (Pack, Vec<Probe>) {
    let (probes, answers) = scenario.graph(request);
    let mut backend = Backend::new(answers);
    let (candidate, report) = discovery::execute(request, &mut backend);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    assert!(!report.active_pack_updated);
    assert_eq!(backend.emitted.len(), backend.stops);
    assert_eq!(backend.emitted.len(), probes.len());
    (candidate.unwrap(), probes)
}
fn field<'a>(pack: &'a Pack, path: &str) -> &'a Value {
    pack.data
        .fields
        .iter()
        .find(|field| field["path"] == path && field["is_tag"] == false)
        .unwrap()
}

#[test]
fn bounded_mode_is_opt_in_and_checks_the_population_cap() {
    let mut r = request();
    for cap in [0, 4001] {
        r.max_population_fields_per_source = Some(cap);
        assert!(discovery::validate_request(&r).is_err());
    }
    for cap in [1, 128, 4000] {
        r.max_population_fields_per_source = Some(cap);
        discovery::validate_request(&r).unwrap();
    }
    let mut old = serde_json::to_value(&r).unwrap();
    old.as_object_mut()
        .unwrap()
        .remove("max_population_fields_per_source");
    let old: DiscoverRequest = serde_json::from_value(old).unwrap();
    assert!(old.max_population_fields_per_source.is_none());
    assert!(
        serde_json::to_value(&old)
            .unwrap()
            .get("max_population_fields_per_source")
            .is_none()
    );
    let (probes, _) = Scenario::default().graph(&old);
    assert!(probes.iter().all(|p| p.kind != "kind-fieldset"));
    assert!(
        probes
            .iter()
            .filter(|p| p.kind == "population")
            .all(|p| p.start == "2026-01-01T00:45:00Z")
    );
}

#[test]
fn rare_kind_cutoff_is_inclusive_and_zero_and_larger_groups_are_skipped() {
    let mut scenario = Scenario::default();
    scenario.facets.insert(
        "#type".into(),
        vec![
            json!({"#type":"above","events":50001}),
            json!({"#type":"at","events":"50000"}),
            json!({"#type":"below","events":49999}),
            json!({"#type":"zero","events":0}),
        ],
    );
    let (probes, _) = scenario.graph(&request());
    let rare = probes
        .iter()
        .filter(|p| p.kind == "kind-fieldset")
        .collect::<Vec<_>>();
    assert_eq!(rare.len(), 2);
    assert_eq!(
        rare[0].query,
        "#repo = \"synthetic\" | #type = \"below\" | fieldset()"
    );
    assert_eq!(
        rare[1].query,
        "#repo = \"synthetic\" | #type = \"at\" | fieldset()"
    );
    assert!(rare.iter().all(|p| p.start == request().start));
}

#[test]
fn rare_values_sort_by_count_then_value_and_stop_at_ten() {
    let mut scenario = Scenario::default();
    scenario.facets.insert(
        "#type".into(),
        (0..12)
            .rev()
            .map(|i| json!({"#type":format!("k{i:02}"),"events":if i < 2 {2} else {1}}))
            .collect(),
    );
    let (probes, _) = scenario.graph(&request());
    let rare = probes
        .iter()
        .filter(|p| p.kind == "kind-fieldset")
        .collect::<Vec<_>>();
    assert_eq!(rare.len(), 10);
    for (index, p) in rare.iter().enumerate() {
        assert_eq!(
            p.query,
            format!(
                "#repo = \"synthetic\" | #type = \"k{:02}\" | fieldset()",
                index + 2
            )
        );
    }
}

#[test]
fn selection_prioritizes_requested_then_new_fields_and_retains_pending_fields() {
    let mut r = request();
    r.max_population_fields_per_source = Some(4);
    r.kind_fields = vec!["custom.kind".into()];
    r.value_fields = vec!["requested.value".into()];
    let scenario = Scenario {
        sampled: vec![
            "old.z".into(),
            "requested.value".into(),
            "custom.kind".into(),
            "old.a".into(),
            "old.a".into(),
        ],
        facets: BTreeMap::from([(
            "custom.kind".into(),
            vec![
                json!({"custom.kind":"a","events":1}),
                json!({"custom.kind":"b","events":2}),
            ],
        )]),
        rare_fields: vec![
            "new.z".into(),
            "old.a".into(),
            "new.a".into(),
            "new.a".into(),
        ],
        ..Scenario::default()
    };
    let (pack, probes) = execute(&r, &scenario);
    let selected = probes
        .iter()
        .filter(|p| p.kind == "population")
        .flat_map(|p| p.fields.iter().cloned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        selected,
        BTreeSet::from([
            "custom.kind".into(),
            "requested.value".into(),
            "new.a".into(),
            "new.z".into()
        ])
    );
    for path in ["old.a", "old.z"] {
        let pending = field(&pack, path);
        assert!(pending["coverage"]["observed"].is_null());
        assert!(pending["coverage"]["rate"].is_null());
    }
    assert_eq!(
        pack.data
            .fields
            .iter()
            .filter(|f| f["path"] == "new.a")
            .count(),
        1
    );
    assert!(
        probes
            .iter()
            .filter(|p| p.kind == "kind-fieldset")
            .all(|p| p.field.as_deref() == Some("custom.kind"))
    );

    r.max_population_fields_per_source = Some(3);
    let (pack, probes) = execute(&r, &scenario);
    let selected = probes
        .iter()
        .filter(|p| p.kind == "population")
        .flat_map(|p| p.fields.iter().cloned())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        selected,
        BTreeSet::from([
            "custom.kind".into(),
            "requested.value".into(),
            "new.a".into()
        ])
    );
    let pending = field(&pack, "new.z");
    assert!(pending["coverage"]["observed"].is_null());
    assert!(pending["coverage"]["rate"].is_null());
    let full_window = format!("{}/{}", r.start, r.end);
    assert_eq!(pending["observation_window"], full_window);
    let provenance = pack
        .data
        .provenance
        .iter()
        .find(|p| p["id"] == pending["provenance_ids"][0])
        .unwrap();
    assert_eq!(provenance["observation_window"], full_window);

    r.max_population_fields_per_source = Some(1);
    let mut answered = BTreeMap::new();
    loop {
        let pending = match discovery::frontier(&r, &answered) {
            Ok(pending) => pending,
            Err(diagnostic) => {
                assert!(diagnostic.message.contains("requested kind/value fields"));
                break;
            }
        };
        assert!(!pending.is_empty());
        for p in pending {
            assert_ne!(p.kind, "population");
            answered.insert(
                discovery::probe_id(&r.environment.view, &p),
                scenario.rows(&r, &p),
            );
        }
    }
}

#[test]
fn new_field_population_uses_source_full_window_and_its_own_denominator() {
    let r = request();
    let (pack, probes) = execute(&r, &Scenario::default());
    let rare = field(&pack, "rare.new");
    let recent = field(&pack, "recent");
    assert_eq!(rare["coverage"]["rate"], 10.0 / 1_000_000.0);
    assert_eq!(recent["coverage"]["rate"], 0.5);
    let full_window = format!("{}/{}", r.start, r.end);
    assert_eq!(rare["observation_window"], full_window);
    assert_eq!(
        recent["observation_window"],
        "2026-01-01T00:45:00Z/2026-01-01T01:00:00Z"
    );
    for item in [rare, recent] {
        let provenance = pack
            .data
            .provenance
            .iter()
            .find(|p| p["id"] == item["provenance_ids"][0])
            .unwrap();
        assert_eq!(provenance["observation_window"], item["observation_window"]);
    }
    let source = &pack.data.sources[0];
    let provenance = pack
        .data
        .provenance
        .iter()
        .find(|p| p["id"] == source["provenance_ids"][0])
        .unwrap();
    assert_eq!(source["evidence"]["observation_window"], full_window);
    assert_eq!(provenance["observation_window"], full_window);
    let population = probes
        .iter()
        .find(|p| p.kind == "population" && p.fields.contains(&"rare.new".into()))
        .unwrap();
    assert_eq!(population.start, r.start);
    assert!(
        population
            .query
            .starts_with("#repo = \"synthetic\" | stats([")
    );
    assert!(!population.query.contains("#type ="));
}

#[test]
fn requested_discriminator_precedes_generic_and_truncated_facets_are_not_stratified() {
    let mut r = request();
    r.kind_fields = vec!["custom.kind".into()];
    let mut scenario = Scenario::default();
    scenario.sampled.push("custom.kind".into());
    scenario.facets.insert(
        "custom.kind".into(),
        vec![
            json!({"custom.kind":"first","events":1}),
            json!({"custom.kind":"second","events":2}),
        ],
    );
    let (probes, _) = scenario.graph(&r);
    assert!(
        probes
            .iter()
            .filter(|p| p.kind == "kind-fieldset")
            .all(|p| p.field.as_deref() == Some("custom.kind"))
    );
    scenario.facets.insert(
        "custom.kind".into(),
        (0..50)
            .map(|i| json!({"custom.kind":format!("k{i}"),"events":1}))
            .collect(),
    );
    let (pack, probes) = execute(&r, &scenario);
    assert!(
        probes
            .iter()
            .filter(|p| p.kind == "kind-fieldset")
            .all(|p| p.field.as_deref() == Some("#type"))
    );
    assert!(
        pack.data.sources[0]["relationship"]["record_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|facet| facet["field"] == "custom.kind" && facet["truncated"] == true)
    );
    scenario.facets.insert(
        "#type".into(),
        (0..50)
            .map(|i| json!({"#type":format!("k{i}"),"events":1}))
            .collect(),
    );
    let (probes, _) = scenario.graph(&r);
    assert!(probes.iter().all(|p| p.kind != "kind-fieldset"));
}

#[test]
fn generic_discriminators_follow_the_documented_order_with_dataset_as_fallback() {
    let candidates = [
        "#event_simpleName",
        "#type",
        "#event.kind",
        "event.kind",
        "#event.module",
        "#event.dataset",
    ];
    for index in 0..candidates.len() {
        let scenario = Scenario {
            sampled: candidates[index..]
                .iter()
                .map(|field| field.to_string())
                .collect(),
            facets: candidates
                .iter()
                .map(|field| {
                    (
                        field.to_string(),
                        vec![
                            json!({*field:"a","events":1}),
                            json!({*field:"b","events":2}),
                        ],
                    )
                })
                .collect(),
            datasets: vec![
                json!({"#event.dataset":"a","events":1}),
                json!({"#event.dataset":"b","events":2}),
            ],
            ..Scenario::default()
        };
        let (probes, _) = scenario.graph(&request());
        let rare = probes
            .iter()
            .filter(|p| p.kind == "kind-fieldset")
            .collect::<Vec<_>>();
        assert_eq!(rare.len(), 2);
        assert!(
            rare.iter()
                .all(|p| p.field.as_deref() == Some(candidates[index]))
        );
        assert!(
            probes
                .iter()
                .all(|p| p.kind != "record-kind-facet"
                    || p.field.as_deref() != Some("#event.dataset"))
        );
    }
}

#[test]
fn repeated_fieldsets_do_not_merge_tag_and_ordinary_namespaces() {
    let scenario = Scenario {
        sampled: vec![
            "event.kind".into(),
            "#event.kind".into(),
            "event.kind".into(),
        ],
        facets: BTreeMap::from([
            (
                "event.kind".into(),
                vec![
                    json!({"event.kind":"a","events":1}),
                    json!({"event.kind":"b","events":2}),
                ],
            ),
            (
                "#event.kind".into(),
                vec![
                    json!({"#event.kind":"a","events":1}),
                    json!({"#event.kind":"b","events":2}),
                ],
            ),
        ]),
        ..Scenario::default()
    };
    let (pack, probes) = execute(&request(), &scenario);
    let fields = pack
        .data
        .fields
        .iter()
        .filter(|f| f["path"] == "event.kind")
        .collect::<Vec<_>>();
    assert_eq!(fields.len(), 2);
    assert_ne!(fields[0]["is_tag"], fields[1]["is_tag"]);
    assert_ne!(fields[0]["id"], fields[1]["id"]);
    let selected = probes
        .iter()
        .filter(|p| p.kind == "population")
        .flat_map(|p| &p.fields)
        .collect::<Vec<_>>();
    assert_eq!(
        selected
            .iter()
            .filter(|field| field.as_str() == "event.kind")
            .count(),
        1
    );
    assert_eq!(
        selected
            .iter()
            .filter(|field| field.as_str() == "#event.kind")
            .count(),
        1
    );
}

#[test]
fn invalid_population_counts_cannot_produce_a_candidate() {
    let r = request();
    for row in [
        json!({"total":0,"c0":0}),
        json!({"total":10,"c0":11}),
        json!({"total":10}),
        json!({"total":"unknown","c0":1}),
    ] {
        let (probes, mut answers) = Scenario::default().graph(&r);
        let full = probes
            .iter()
            .find(|p| p.kind == "population" && p.start == r.start)
            .unwrap();
        answers.insert(discovery::probe_id(&r.environment.view, full), vec![row]);
        let mut backend = Backend::new(answers);
        let (candidate, report) = discovery::execute(&r, &mut backend);
        assert!(candidate.is_none());
        assert_eq!(report.status, "error");
        assert_eq!(report.diagnostics[0].code, "DISCOVERY_EVIDENCE");
        assert!(!report.active_pack_updated);
        assert_eq!(backend.emitted.len(), backend.stops);
    }
}

#[test]
fn rare_fieldset_filters_escape_literals_without_expanding_globs_or_injecting_stages() {
    let mut scenario = Scenario::default();
    scenario.facets.insert(
        "#type".into(),
        vec![
            json!({"#type":"literal*kind","events":1}),
            json!({"#type":"a\" | count() | x=\"b\\\n","events":2}),
        ],
    );
    let (probes, _) = scenario.graph(&request());
    let rare = probes
        .iter()
        .filter(|p| p.kind == "kind-fieldset")
        .collect::<Vec<_>>();
    assert_eq!(rare.len(), 2);
    assert_eq!(
        rare[0].query,
        "#repo = \"synthetic\" | #type = /\\Aliteral\\*kind\\z/ | fieldset()"
    );
    assert_eq!(
        rare[1].query,
        "#repo = \"synthetic\" | #type = \"a\\\" | count() | x=\\\"b\\\\\\n\" | fieldset()"
    );
}

#[test]
fn one_hundred_twenty_eight_fields_fit_the_forty_query_budget() {
    let r = request();
    let candidates = [
        "#event_simpleName",
        "#type",
        "#event.kind",
        "event.kind",
        "#event.module",
    ];
    let scenario = Scenario {
        sampled: candidates
            .iter()
            .map(|s| s.to_string())
            .chain((0..500).map(|i| format!("sampled.{i:03}")))
            .collect(),
        rare_fields: (0..200).map(|i| format!("rare.{i:03}")).collect(),
        facets: candidates
            .iter()
            .map(|field| {
                (
                    field.to_string(),
                    (0..12)
                        .map(|i| json!({*field:format!("kind-{i:02}"),"events":i+1}))
                        .collect(),
                )
            })
            .collect(),
        ..Scenario::default()
    };
    let (_, probes) = execute(&r, &scenario);
    assert!(probes.len() <= 40, "{}", probes.len());
    assert_eq!(
        probes.iter().filter(|p| p.kind == "kind-fieldset").count(),
        10
    );
    assert_eq!(
        probes
            .iter()
            .filter(|p| p.kind == "population")
            .map(|p| p.fields.len())
            .sum::<usize>(),
        128
    );
    assert!(probes.iter().filter(|p| p.kind == "population").count() <= 8);
    let kinds_end = probes
        .iter()
        .rposition(|p| matches!(p.kind.as_str(), "tag-detect" | "record-kind-facet"))
        .unwrap();
    let rare_start = probes
        .iter()
        .position(|p| p.kind == "kind-fieldset")
        .unwrap();
    assert!(kinds_end < rare_start);
}

#[test]
fn sampled_selection_is_lexical_and_direct_kind_metadata_does_not_consume_the_cap() {
    let mut r = request();
    r.max_population_fields_per_source = Some(2);
    let scenario = Scenario {
        sampled: vec!["z".into(), "b".into(), "a".into(), "b".into()],
        facets: BTreeMap::new(),
        rare_fields: vec![],
        ..Scenario::default()
    };
    let (pack, probes) = execute(&r, &scenario);
    let selected = probes
        .iter()
        .filter(|p| p.kind == "population")
        .flat_map(|p| p.fields.iter().map(String::as_str))
        .collect::<Vec<_>>();
    assert_eq!(selected, vec!["a", "b"]);
    assert_eq!(
        pack.data
            .fields
            .iter()
            .filter(|field| field["is_tag"] == false)
            .count(),
        3
    );
    let pending = field(&pack, "z");
    assert!(pending["coverage"]["observed"].is_null());
    assert!(pending["coverage"]["rate"].is_null());
    let dataset = pack
        .data
        .fields
        .iter()
        .find(|field| field["path"] == "event.dataset")
        .unwrap();
    assert_eq!(dataset["is_tag"], true);
    assert_eq!(dataset["coverage"]["observed"], false);
    assert!(dataset["coverage"]["rate"].is_null());
}

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".local")
            .join(format!(
                "stratified-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn wire_probe(wire: &Value) -> Probe {
    let args = &wire["mcp"]["arguments"];
    Probe {
        kind: wire["kind"].as_str().unwrap().into(),
        source: serde_json::from_value(wire["source"].clone()).unwrap(),
        query: args["query_string"].as_str().unwrap().into(),
        start: args["start"].as_str().unwrap().into(),
        end: args["end"].as_str().unwrap().into(),
        fields: serde_json::from_value(wire["fields"].clone()).unwrap(),
        field: serde_json::from_value(wire["field"].clone()).unwrap(),
    }
}
fn existing_pack(path: &Path, environment: EnvironmentId) -> Vec<u8> {
    let pack = Pack::new(environment, discovery::timestamp(), PackData::default()).unwrap();
    storage::Writer::acquire(path)
        .unwrap()
        .commit(&pack)
        .unwrap();
    fs::read(path).unwrap()
}

#[test]
fn api_and_mcp_emit_identical_stratified_probes_and_population_rates() {
    let dir = TestDir::new();
    let r = request();
    let scenario = Scenario::default();
    let state = dir.0.join("state.json");
    let pack_path = dir.0.join("pack.json");
    let receipt = dir.0.join("receipt.json");
    let mut output = mcp::init(r.clone(), &state, &pack_path, &receipt).unwrap();
    let mut relayed = BTreeSet::new();
    let mut rounds = 0;
    while output.status == "needs-probes" {
        rounds += 1;
        assert!(rounds <= 6);
        let answers = output
            .probes
            .iter()
            .enumerate()
            .map(|(index, wire)| {
                assert_eq!(wire["mcp"]["arguments"]["repository"], "search-all");
                let p = wire_probe(wire);
                relayed.insert((
                    "search-all".to_string(),
                    p.query.clone(),
                    p.start.clone(),
                    p.end.clone(),
                ));
                let path = dir.0.join(format!("answer-{rounds}-{index}.json"));
                fs::write(&path, json!({"result":scenario.rows(&r,&p)}).to_string()).unwrap();
                (wire["probe_id"].as_str().unwrap().to_string(), path)
            })
            .collect::<Vec<_>>();
        output = mcp::step(&state, &answers).unwrap();
    }
    assert_eq!(output.status, "complete", "{:?}", output.diagnostics);
    let relayed_pack = storage::read_pack(&pack_path).unwrap();
    let (_, answers) = scenario.graph(&r);
    let mut backend = Backend::new(answers);
    let (candidate, report) = discovery::execute(&r, &mut backend);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    let api = candidate.unwrap();
    assert_eq!(relayed, backend.emitted.into_iter().collect());
    assert_eq!(
        field(&relayed_pack, "rare.new")["coverage"],
        field(&api, "rare.new")["coverage"]
    );
    assert_eq!(
        field(&relayed_pack, "recent")["coverage"],
        field(&api, "recent")["coverage"]
    );
}

#[test]
fn malformed_facets_budgets_and_timeouts_leave_the_active_pack_untouched() {
    for failure in ["malformed", "truncated", "timeout", "budget"] {
        let dir = TestDir::new();
        let mut r = request();
        if failure == "budget" {
            r.max_queries = 3;
        }
        let scenario = Scenario::default();
        let state = dir.0.join("state.json");
        let pack = dir.0.join("pack.json");
        let receipt = dir.0.join("receipt.json");
        let old = existing_pack(&pack, r.environment.clone());
        let output = mcp::init(r, &state, &pack, &receipt).unwrap();
        let answers = output
            .probes
            .iter()
            .enumerate()
            .map(|(index, wire)| {
                let p = wire_probe(wire);
                let payload = if p.kind == "dataset-facet" && failure != "budget" {
                    match failure {
                        "malformed" => json!({"result":[{"#event.dataset":7,"events":1}]}),
                        "truncated" => json!({"result":scenario.datasets,"partial":true}),
                        "timeout" => json!({"result":[],"error":"synthetic timeout"}),
                        _ => unreachable!(),
                    }
                } else {
                    json!({"result":scenario.rows(&request(), &p)})
                };
                let path = dir.0.join(format!("answer-{index}.json"));
                fs::write(&path, payload.to_string()).unwrap();
                (wire["probe_id"].as_str().unwrap().to_string(), path)
            })
            .collect::<Vec<_>>();
        let output = mcp::step(&state, &answers).unwrap();
        assert_eq!(
            output.status, "failed",
            "{failure}: {:?}",
            output.diagnostics
        );
        assert!(!output.active_pack_updated);
        assert_eq!(fs::read(&pack).unwrap(), old);
    }
    let r = request();
    let (probes, answers) = Scenario::default().graph(&r);
    let mut backend = Backend::new(answers);
    backend.timeout = Some(discovery::probe_id(
        &r.environment.view,
        probes.iter().find(|p| p.kind == "kind-fieldset").unwrap(),
    ));
    let (candidate, report) = discovery::execute(&r, &mut backend);
    assert!(candidate.is_none());
    assert_eq!(report.status, "error");
    assert!(!report.active_pack_updated);
    assert_eq!(backend.emitted.len(), backend.stops);
}
