use cqlmatic::{
    app,
    contracts::*,
    discovery::{self, DiscoverRequest},
    environment::{self, Pack, PackData},
    language,
    network::{JobPolicy, SearchBackend, run_job},
    storage,
};
use proptest::prelude::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

fn identity() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "search-all".into(),
    }
}
fn pack() -> Pack {
    Pack::new(identity(), "2026-01-01T00:00:00Z".into(), PackData {
        provenance: vec![json!({"id":"prov:old","source_type":"manual-curation","canonical_source":"test fixture","retrieved_at":"2025-12-01","redaction_status":"none","coverage_status":"reconciled"})],
        sources: ["selected", "untouched"].iter().map(|n| json!({"id":format!("env:repo:{n}"),"kind":"repo","name":n,"inventory_status":"reconciled","selectors":{"repo":n},"relationship":{"datasets":["historical"],"parsers":["custom"]},"parser_contract":{"source":"operator","parser":"custom","retrieved_at":"2025-12-01","declared_fields":["user","quiet"],"fields_to_tag":[]},"evidence":{"declared":true,"observed":true},"provenance_ids":["prov:old"]})).collect(),
        fields: ["user", "quiet"].iter().map(|f| json!({"id":format!("env:field:{f}"),"source_id":"env:repo:selected","path":f,"data_type":"string","is_tag":false,"is_array":false,"description":"operator meaning","evidence_kind":"observed","coverage":{"declared":true,"observed":true},"provenance_ids":["prov:old"]})).collect(),
        values: vec![json!({"id":"env:value:old","field_id":"env:field:user","value":"alice","value_kind":"sample","vocabulary":"sampled","evidence_kind":"observed","provenance_ids":["prov:old"]})],
        annotations: vec![json!({"source_id":"env:repo:selected","reviewed_at":"2025-12-01","notes":"operator review"})],
    }).unwrap()
}

#[test]
fn refresh_preserves_history_without_relabelling_it_as_new_observation() {
    let old = pack();
    let fresh = Pack::new(identity(), "2026-02-01T00:00:00Z".into(), PackData {
        provenance: vec![json!({"id":"prov:new","source_type":"ngsiem-api","canonical_source":"test API","retrieved_at":"2026-02-01","redaction_status":"none","coverage_status":"observed-only"})],
        sources: vec![json!({"id":"env:repo:new-id","kind":"repo","name":"generated name","inventory_status":"observed-only","selectors":{"repo":"selected"},"relationship":{"datasets":["current"]},"evidence":{"declared":false,"observed":true},"provenance_ids":["prov:new"]})],
        fields: vec![json!({"id":"env:field:new-user","source_id":"env:repo:new-id","path":"user","data_type":"unknown","is_tag":false,"is_array":null,"evidence_kind":"observed","coverage":{"declared":null,"observed":true},"provenance_ids":["prov:new"]})],
        ..PackData::default()
    }).unwrap();
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(merged.data.sources[1], old.data.sources[1]);
    let s = &merged.data.sources[0];
    assert_eq!(s["id"], "env:repo:selected");
    assert_eq!(s["name"], "selected");
    assert_eq!(s["parser_contract"]["retrieved_at"], "2025-12-01");
    assert_eq!(s["evidence"]["declared"], true);
    assert_eq!(s["relationship"]["parsers"], json!(["custom"]));
    assert_eq!(
        s["relationship"]["datasets"],
        json!(["current", "historical"])
    );
    assert_eq!(merged.data.fields[0]["data_type"], "string");
    assert_eq!(merged.data.fields[0]["is_array"], false);
    assert_eq!(merged.data.fields[0]["coverage"]["declared"], true);
    assert_eq!(
        merged.data.fields[0]["retained_context"]["evidence_retrieved_at"],
        json!(["2025-12-01"])
    );
    assert_eq!(merged.data.fields[1]["refresh_status"], "not-reobserved");
    assert_eq!(merged.data.fields[1]["provenance_ids"], json!(["prov:old"]));
    assert_eq!(merged.data.values, old.data.values);
    assert_eq!(merged.data.annotations, old.data.annotations);
    let mut other = fresh.clone();
    other.environment.cloud = Cloud::Eu1;
    other.content_digest = other.digest().unwrap();
    assert_eq!(
        environment::merge(&old, &other).unwrap_err().code,
        "PACK_ENVIRONMENT_MISMATCH"
    );
    let mut corrupt = old.clone();
    corrupt.data.fields[0]["path"] = json!("tampered");
    assert_eq!(corrupt.validate().unwrap_err().code, "PACK_DIGEST");
}

struct Fake {
    rows: VecDeque<Vec<Value>>,
    starts: usize,
    stops: usize,
    stop_error: bool,
}
impl SearchBackend for Fake {
    fn start(
        &mut self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: Instant,
    ) -> Result<String, Diagnostic> {
        self.starts += 1;
        Ok(format!("job-{}", self.starts))
    }
    fn poll(&mut self, _: &str, _: &str, _: Instant) -> Result<Value, Diagnostic> {
        let rows = self.rows.pop_front().expect("unexpected discovery query");
        Ok(json!({"done":true,"cancelled":false,"metaData":{},"events":rows}))
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        self.stops += 1;
        if self.stop_error {
            Err(Diagnostic::error("TEST_STOP", "stop failed"))
        } else {
            Ok(())
        }
    }
}
fn discovery_request() -> DiscoverRequest {
    serde_json::from_value(json!({"schema_version":1,"environment":identity(),"start":"2026-01-01T00:00:00Z","end":"2026-01-01T01:00:00Z","sources":[{"repo":"selected"}],"value_fields":["peer","user"]})).unwrap()
}
fn kind_fake(field: &str, facet_rows: Vec<Value>) -> Fake {
    let mut rows = vec![
        vec![json!({"events":"3"})],
        vec![json!({"#event.dataset":"synthetic","events":"3"})],
        vec![json!({"field":field})],
        vec![json!({"total":"3","c0":"3"})],
    ];
    rows.extend((0..9).map(|_| vec![json!({"events":"0"})]));
    rows.push(facet_rows);
    Fake {
        rows: rows.into(),
        starts: 0,
        stops: 0,
        stop_error: false,
    }
}

#[test]
fn dataset_facets_reject_malformed_rows_in_execution_and_frontier() {
    let request = discovery_request();
    for row in [
        json!({"events":3}),
        json!({"#event.dataset":null,"events":3}),
        json!({"#event.dataset":7,"events":3}),
        json!({"#event.dataset":true,"events":3}),
        json!({"#event.dataset":[],"events":3}),
        json!({"#event.dataset":{},"events":3}),
        json!({"#event.dataset":"synthetic"}),
        json!({"#event.dataset":"synthetic","events":-1}),
        json!({"#event.dataset":"synthetic","events":1.5}),
        json!({"#event.dataset":"synthetic","events":"unknown"}),
        json!({"#event.dataset":"bad\u{0000}value","events":3}),
        json!({"#event.dataset":"x".repeat(MAX_QUERY_UTF16 + 1),"events":3}),
    ] {
        let mut fake = Fake {
            rows: vec![vec![json!({"events":3})], vec![row.clone()]].into(),
            starts: 0,
            stops: 0,
            stop_error: false,
        };
        let (candidate, report) = discovery::execute(&request, &mut fake);
        assert!(candidate.is_none(), "{row}");
        assert_eq!(report.status, "error");
        assert_eq!(report.diagnostics[0].code, "DISCOVERY_EVIDENCE");
        assert!(!report.active_pack_updated);
        assert_eq!(fake.starts, 2);
        assert_eq!(fake.stops, fake.starts);

        let initial = discovery::frontier(&request, &BTreeMap::new()).unwrap();
        let dataset = initial.iter().find(|p| p.kind == "dataset-facet").unwrap();
        let answered = BTreeMap::from([(
            discovery::probe_id(&request.environment.view, dataset),
            vec![row],
        )]);
        assert_eq!(
            discovery::frontier(&request, &answered).unwrap_err().code,
            "DISCOVERY_EVIDENCE"
        );
    }
}
mod poll_snapshot_tests {
    use super::*;

    struct PollSnapshots {
        snapshots: VecDeque<Value>,
        stops: usize,
    }
    impl SearchBackend for PollSnapshots {
        fn start(
            &mut self,
            _: &str,
            _: &str,
            _: &str,
            _: &str,
            _: Instant,
        ) -> Result<String, Diagnostic> {
            Ok("synthetic-job".into())
        }
        fn poll(&mut self, _: &str, _: &str, _: Instant) -> Result<Value, Diagnostic> {
            Ok(self.snapshots.pop_front().unwrap())
        }
        fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
            self.stops += 1;
            Ok(())
        }
    }
    #[test]
    fn earlier_incomplete_poll_invalidates_a_later_clean_snapshot() {
        for (indicator, expected_issue) in [
            (
                json!({"metaData": {"extraData": {"eventsSkipped": 5}}}),
                "metaData.extraData.eventsSkipped",
            ),
            (json!({"truncated": true}), "truncated"),
            (json!({"metaData": {"partial": true}}), "metaData.partial"),
            (json!({"limitReached": true}), "limitReached"),
            (json!({"metaData": {"timedOut": true}}), "metaData.timedOut"),
            (
                json!({"metaData": {"filterQuery": {"isLive": true}}}),
                "metaData.filterQuery.isLive",
            ),
        ] {
            let mut first =
                json!({"done": false, "cancelled": false, "metaData": {}, "events": []});
            for (key, value) in indicator.as_object().unwrap() {
                first[key] = value.clone();
            }
            let mut backend = PollSnapshots {
                snapshots: vec![
                    first,
                    json!({"done": true, "cancelled": false, "metaData": {}, "events": [{"events": 7}]}),
                ]
                .into(),
                stops: 0,
            };
            let receipt = run_job(
                &mut backend,
                "search-all",
                "count(as=events)",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:15:00Z",
                &JobPolicy {
                    timeout: Duration::from_secs(2),
                    poll_interval: Duration::ZERO,
                    max_polls: 2,
                },
            );
            assert_eq!(receipt.cleanup, "confirmed");
            assert_eq!(backend.stops, 1);
            assert_eq!(receipt.polls, 2);
            assert_eq!(receipt.completion, "error", "{receipt:?}");
            assert_eq!(
                receipt.diagnostic.as_ref().map(|d| d.code.as_str()),
                Some("SEARCH_INCOMPLETE")
            );
            assert!(receipt.issues.contains_key(expected_issue), "{receipt:?}");
            assert!(receipt.events.is_empty());
        }
    }

    #[test]
    fn running_poll_with_more_events_does_not_invalidate_a_clean_completion() {
        let mut backend = PollSnapshots {
            snapshots: vec![
                json!({"done": false, "cancelled": false, "metaData": {"extraData": {"hasMoreEvents": true}}, "events": []}),
                json!({"done": true, "cancelled": false, "metaData": {}, "events": [{"events": 7}]}),
            ]
            .into(),
            stops: 0,
        };
        let receipt = run_job(
            &mut backend,
            "search-all",
            "count(as=events)",
            "2026-01-01T00:00:00Z",
            "2026-01-01T00:15:00Z",
            &JobPolicy {
                timeout: Duration::from_secs(2),
                poll_interval: Duration::ZERO,
                max_polls: 2,
            },
        );
        assert_eq!(receipt.cleanup, "confirmed");
        assert_eq!(backend.stops, 1);
        assert_eq!(receipt.completion, "complete", "{receipt:?}");
        assert!(receipt.diagnostic.is_none());
        assert!(receipt.issues.is_empty());
        assert_eq!(receipt.events, vec![json!({"events": 7})]);
    }
}
#[test]
fn discovery_keeps_namespaces_unknown_shapes_and_duplicate_values_distinct() {
    let mut rows = vec![
        vec![json!({"events":"3"})],
        vec![json!({"#event.dataset":"example","events":"3"})],
        vec![
            json!({"field":"user"}),
            json!({"field":"peer"}),
            json!({"field":"only_seen"}),
        ],
        vec![json!({"total":"3","c0":"0","c1":"3","c2":"2"})],
        vec![json!({"events":"3"})],
    ];
    rows.extend((0..8).map(|_| vec![json!({"events":"0"})]));
    rows.push(vec![json!({"#event_simpleName":"synthetic","events":"3"})]);
    rows.push(vec![json!({"peer":"same","events":"3"})]);
    rows.push(vec![json!({"user":"same","events":"2"})]);
    let mut fake = Fake {
        rows: rows.into(),
        starts: 0,
        stops: 0,
        stop_error: false,
    };
    let (candidate, report) = discovery::execute(&discovery_request(), &mut fake);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    let p = candidate.unwrap();
    assert_eq!(fake.starts, fake.stops);
    assert!(fake.rows.is_empty());
    assert_eq!(p.data.values.len(), 2);
    assert_ne!(p.data.values[0]["id"], p.data.values[1]["id"]);
    let f = p
        .data
        .fields
        .iter()
        .find(|f| f["path"] == "only_seen")
        .unwrap();
    assert_eq!(f["coverage"]["observed"], false);
    assert_eq!(f["evidence_kind"], "observed");
    assert_eq!(f["is_array"], Value::Null);
    assert_eq!(f["data_type"], "unknown");
    assert!(
        p.data
            .fields
            .iter()
            .any(|f| f["path"] == "event_simpleName" && f["is_tag"] == true)
    );
    assert_eq!(
        p.data.sources[0]["relationship"]["record_kinds"],
        json!([{"field":"#event_simpleName","values":["synthetic"],"truncated":false}])
    );
    assert!(!report.active_pack_updated);
}
#[test]
fn discovered_type_kinds_require_context_for_strict_generated_counts() {
    let mut request = discovery_request();
    request.value_fields.clear();
    let mut fake = kind_fake(
        "#type",
        vec![
            json!({"#type":"a","events":"2"}),
            json!({"#type":"b","events":"1"}),
        ],
    );
    let (candidate, report) = discovery::execute(&request, &mut fake);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    assert!(report.diagnostics.is_empty());
    assert!(fake.rows.is_empty());
    assert_eq!(fake.starts, fake.stops);
    let kind_probes = report
        .captures
        .iter()
        .filter(|capture| capture.probe.kind == "record-kind-facet")
        .collect::<Vec<_>>();
    assert_eq!(kind_probes.len(), 1);
    assert_eq!(kind_probes[0].probe.field.as_deref(), Some("#type"));
    assert_eq!(kind_probes[0].probe.start, request.start);
    assert_eq!(kind_probes[0].probe.end, request.end);
    assert!(kind_probes[0].probe.query.contains(
        "groupBy([\"#type\"], function=count(as=events), limit=max) | sort(events, order=desc, limit=50)"
    ));
    let pack = candidate.unwrap();
    assert_eq!(
        pack.data.sources[0]["relationship"]["record_kinds"],
        json!([{"field":"#type","values":["a","b"],"truncated":false}])
    );
    for (type_, expected) in [(None, Outcome::NeedsContext), (Some("a"), Outcome::Ready)] {
        let filters = type_
            .map(|value| vec![json!({"op":"eq","field":"#type","value":value})])
            .unwrap_or_default();
        let generation: GenerateRequest = serde_json::from_value(json!({
            "schema_version":1,
            "context":{"require_observed":true},
            "input":{"mode":"plan","plan":{"source":{"repo":"selected"},"filters":filters,"steps":[{"op":"count"}]}}
        }))
        .unwrap();
        let result = app::generate(generation, Some(&pack));
        assert_eq!(result.outcome, expected, "{:?}", result.diagnostics);
        assert_eq!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_RECORD_KIND_AMBIGUOUS"),
            type_.is_none(),
            "{:?}",
            result.diagnostics
        );
        if type_.is_none() {
            assert!(result.query.is_none());
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "CQL_CONTEXT_REQUIRED")
            );
        } else {
            assert!(result.query.is_some());
        }
    }
    for (query, expected) in [
        ("#repo=selected | count()", Outcome::NeedsContext),
        ("#repo=selected #type=a | count()", Outcome::Ready),
    ] {
        let result = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                context: Context {
                    require_observed: true,
                    ..Context::default()
                },
            },
            Some(&pack),
        );
        assert_eq!(result.outcome, expected, "{:?}", result.diagnostics);
        assert_eq!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_RECORD_KIND_AMBIGUOUS"),
            expected == Outcome::NeedsContext
        );
    }
}
#[test]
fn fifty_record_kind_rows_mark_the_facet_truncated() {
    let mut request = discovery_request();
    request.value_fields.clear();
    let mut fake = kind_fake(
        "#type",
        (0..50)
            .map(|i| json!({"#type":format!("kind-{i}"),"events":"1"}))
            .collect(),
    );
    let (candidate, report) = discovery::execute(&request, &mut fake);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    let pack = candidate.unwrap();
    let kinds = &pack.data.sources[0]["relationship"]["record_kinds"][0];
    assert_eq!(kinds["field"], "#type");
    assert_eq!(kinds["truncated"], true);
    assert_eq!(kinds["values"].as_array().unwrap().len(), 50);
    assert_eq!(kinds["values"][0], "kind-0");
    assert_eq!(kinds["values"][49], "kind-49");
}
#[test]
fn kind_fields_adds_custom_discriminators_and_rejects_invalid_fields() {
    let mut request = discovery_request();
    request.value_fields.clear();
    request.kind_fields = vec!["custom.kind".into(), "custom.kind".into()];
    let mut fake = kind_fake(
        "custom.kind",
        vec![
            json!({"custom.kind":"","events":"1"}),
            json!({"custom.kind":"first","events":"1"}),
            json!({"custom.kind":"second","events":"1"}),
        ],
    );
    let (candidate, report) = discovery::execute(&request, &mut fake);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    assert!(fake.rows.is_empty());
    let pack = candidate.unwrap();
    assert_eq!(
        pack.data.sources[0]["relationship"]["record_kinds"],
        json!([{"field":"custom.kind","values":["","first","second"],"truncated":false}])
    );
    request.kind_fields = vec!["".into()];
    assert_eq!(
        discovery::validate_request(&request).unwrap_err().code,
        "CQL_FIELD"
    );
    let mut backend = kind_fake("custom.kind", vec![]);
    let (candidate, report) = discovery::execute(&request, &mut backend);
    assert!(candidate.is_none());
    assert_eq!(report.status, "error");
    assert_eq!(report.diagnostics[0].code, "CQL_FIELD");
    assert_eq!(backend.starts, 0);
}
#[test]
fn malformed_kind_facet_must_not_produce_an_activatable_pack() {
    let mut request = discovery_request();
    request.value_fields.clear();
    for row in [
        json!({"#type":7,"events":5}),
        json!({"#type":true,"events":5}),
        json!({"#type":{},"events":5}),
        json!({"#type":[],"events":5}),
        json!({"#type":null,"events":5}),
        json!({"events":5}),
    ] {
        let mut fake = kind_fake("#type", vec![json!({"#type":"synthetic","events":5}), row]);
        let (candidate, report) = discovery::execute(&request, &mut fake);
        assert_eq!(report.status, "error", "{:?}", report.diagnostics);
        assert!(!report.active_pack_updated);
        assert_eq!(report.diagnostics[0].code, "DISCOVERY_EVIDENCE");
        assert!(candidate.is_none());
        assert_eq!(fake.starts, fake.stops);
        assert!(fake.rows.is_empty());
    }
}
#[test]
fn empty_string_kind_value_keeps_an_unscoped_count_ambiguous() {
    let mut request = discovery_request();
    request.value_fields.clear();
    let mut fake = kind_fake(
        "#type",
        vec![
            json!({"#type":"","events":5}),
            json!({"#type":"synthetic","events":5}),
        ],
    );
    let (candidate, report) = discovery::execute(&request, &mut fake);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    let pack = candidate.unwrap();
    assert_eq!(
        pack.data.sources[0]["relationship"]["record_kinds"][0]["values"],
        json!(["", "synthetic"])
    );
    let generation: GenerateRequest = serde_json::from_value(json!({
        "schema_version": 1,
        "input": {
            "mode": "plan",
            "plan": {
                "source": {"repo": "selected"},
                "steps": [{"op": "count"}]
            }
        }
    }))
    .unwrap();
    let result = app::generate(generation.clone(), Some(&pack));
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_RECORD_KIND_NOTICE" && d.severity == Severity::Info)
    );
    assert!(result.query.is_some());
    let mut strict = generation;
    strict.context.require_observed = true;
    let strict = app::generate(strict, Some(&pack));
    assert_eq!(strict.outcome, Outcome::NeedsContext);
    assert!(strict.query.is_none());
}
#[test]
fn kind_facet_value_forbidden_in_cql_must_not_activate_a_pack() {
    let mut request = discovery_request();
    request.value_fields.clear();
    for kind in ["a\u{0000}b", "a\u{0001}b"] {
        let mut fake = kind_fake("#type", vec![json!({"#type":kind,"events":2})]);
        let (candidate, report) = discovery::execute(&request, &mut fake);
        assert_eq!(report.status, "error", "{:?}", report.diagnostics);
        assert_eq!(report.diagnostics[0].code, "DISCOVERY_EVIDENCE");
        assert!(!report.active_pack_updated);
        assert!(candidate.is_none());
        assert_eq!(fake.starts, fake.stops);
    }
}
#[test]
fn frontier_skips_absent_kinds_and_emits_tag_kinds_with_value_probes() {
    let request = discovery_request();
    let view = &request.environment.view;
    let initial = discovery::frontier(&request, &BTreeMap::new()).unwrap();
    assert_eq!(initial.len(), 3);
    let mut answered = BTreeMap::new();
    for probe in &initial {
        let rows = match probe.kind.as_str() {
            "volume" => vec![json!({"events":"3"})],
            "dataset-facet" => vec![json!({"#event.dataset":"synthetic","events":"3"})],
            "fieldset" => vec![json!({"field":"user"})],
            _ => panic!("unexpected initial probe"),
        };
        answered.insert(discovery::probe_id(view, probe), rows);
    }
    let dependent = discovery::frontier(&request, &answered).unwrap();
    assert_eq!(
        dependent
            .iter()
            .filter(|probe| probe.kind == "record-kind-facet")
            .count(),
        0
    );
    assert_eq!(
        dependent.iter().filter(|p| p.kind == "tag-detect").count(),
        9
    );
    for probe in &dependent {
        let rows = match probe.kind.as_str() {
            "population" => vec![json!({"total":"3","c0":"3"})],
            "tag-detect" => vec![json!({
                "events": if probe.field.as_deref() == Some("event.module") {"3"} else {"0"}
            })],
            _ => panic!("unexpected dependent probe"),
        };
        answered.insert(discovery::probe_id(view, probe), rows);
    }
    let final_batch = discovery::frontier(&request, &answered).unwrap();
    assert_eq!(
        final_batch
            .iter()
            .filter(|probe| probe.kind == "value-facet")
            .count(),
        2
    );
    let kinds = final_batch
        .iter()
        .filter(|probe| probe.kind == "record-kind-facet")
        .collect::<Vec<_>>();
    assert_eq!(kinds.len(), 1);
    assert_eq!(kinds[0].field.as_deref(), Some("#event.module"));
    assert_eq!(kinds[0].start, request.start);
    assert!(kinds[0].query.contains(
        "groupBy([\"#event.module\"], function=count(as=events), limit=max) | sort(events, order=desc, limit=50)"
    ));
}
#[test]
fn empty_or_budget_exhausted_discovery_never_yields_an_active_candidate() {
    let mut request = discovery_request();
    request.sources.clear();
    request.census = true;
    let mut fake = Fake {
        rows: vec![vec![]].into(),
        starts: 0,
        stops: 0,
        stop_error: false,
    };
    let (candidate, report) = discovery::execute(&request, &mut fake);
    assert!(candidate.is_none());
    assert_eq!(report.status, "error");
    assert!(!report.active_pack_updated);
    assert_eq!(fake.stops, 1);
    request = discovery_request();
    request.max_queries = 1;
    let mut fake = Fake {
        rows: vec![vec![json!({"events":3})]].into(),
        starts: 0,
        stops: 0,
        stop_error: false,
    };
    let (candidate, report) = discovery::execute(&request, &mut fake);
    assert!(candidate.is_none());
    assert_eq!(report.captures.len(), 1);
    assert_eq!(fake.starts, fake.stops);
}

fn cli(args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("FALCON_CLIENT_ID")
        .env_remove("FALCON_CLIENT_SECRET")
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
#[test]
fn jsonl_recovers_after_malformed_and_oversized_records() {
    let request = |id| {
        json!({"schema_version":1,"id":id,"input":{"mode":"english","text":"count events","source":{}}}).to_string()
    };
    let mut input = format!("{}\n{{broken\n", request("first")).into_bytes();
    input.extend(vec![b'x'; MAX_REQUEST_BYTES + 1]);
    input.extend(format!("\n{}\n", request("last")).as_bytes());
    let out = cli(&["generate", "--jsonl"], &input);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stderr.is_empty());
    let rows = out
        .stdout
        .split(|b| *b == b'\n')
        .filter(|r| !r.is_empty())
        .map(|r| serde_json::from_slice::<Value>(r).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 4);
    assert_eq!(rows[0]["id"], "first");
    assert_eq!(rows[1]["outcome"], "error");
    assert_eq!(rows[2]["outcome"], "error");
    assert_eq!(rows[3]["id"], "last");
    assert_eq!(rows[3]["outcome"], "ready");
    assert_eq!(
        cli(&["--schema", "generate", "catalog"], b"").status.code(),
        Some(2)
    );
    assert!(cli(&["--schema", "generate"], b"").status.success());
}
#[test]
fn jsonl_recovers_after_expression_depth_limits() {
    let query = format!("x := {}", vec!["a"; 14_000].join("+"));
    assert!(query.encode_utf16().count() < MAX_QUERY_UTF16);
    let input = format!(
        "{}\n{}\n",
        json!({"schema_version":1,"id":"deep","query":query}),
        json!({"schema_version":1,"id":"after","query":"count()"})
    );
    let out = cli(&["check", "--jsonl"], input.as_bytes());
    assert_eq!(out.status.code(), Some(1), "{:?}", out.stderr);
    assert!(out.stderr.is_empty());
    let rows = out
        .stdout
        .split(|b| *b == b'\n')
        .filter(|r| !r.is_empty())
        .map(|r| serde_json::from_slice::<Value>(r).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "deep");
    assert_eq!(rows[0]["outcome"], "unsupported");
    assert_eq!(rows[1]["id"], "after");
    assert_eq!(rows[1]["outcome"], "ready");
}
#[test]
fn strict_requests_and_unicode_diagnostics_survive_the_cli_boundary() {
    for raw in [
        r#"{"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"filters":[],"steps":[]}},"unknown":true}"#,
        r#"{"schema_version":1,"schema_version":1,"input":{"mode":"english","text":"count events","source":{}}}"#,
    ] {
        assert_eq!(
            cli(&["generate", "--request", "-", "--json"], raw.as_bytes())
                .status
                .code(),
            Some(2)
        );
    }
    let text = "user=\"é\"\n| count(no_such_arg=1)";
    let result = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: text.into(),
            context: Context::default(),
        },
        None,
    );
    assert_eq!(result.outcome, Outcome::Invalid);
    let span = result
        .diagnostics
        .iter()
        .find_map(|d| d.span.as_ref())
        .unwrap();
    assert!(text.is_char_boundary(span.start));
    assert!(text.is_char_boundary(span.end));
    assert_eq!(span.line, 2);
    assert!(span.column >= 3);
}
#[test]
fn effects_and_expression_names_cannot_change_the_requested_operation() {
    for query in [
        "x := count()",
        "x := createEvents([\"injected\"])",
        "eval(x=readFile(\"file\"))",
        "groupBy([user], function=unknownAggregate())",
    ] {
        let result = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                context: Context::default(),
            },
            None,
        );
        assert_eq!(result.outcome, Outcome::Unsupported, "{query}");
    }
    for query in [
        "count(field=\"\\u0000\")",
        "eval(\"#type\"=\"changed\")",
        "count(as=\"#tag\")",
        "split(field=items,by=\",\")",
    ] {
        let result = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                context: Context::default(),
            },
            None,
        );
        assert_eq!(result.outcome, Outcome::Invalid, "{query}");
    }
    let request:GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"assign","field":"x","expression":{"kind":"field","name":"true"}}]}}})).unwrap();
    let result = app::generate(request, None);
    assert_eq!(result.outcome, Outcome::Unsupported);
    assert!(result.query.is_none());
    for f in cqlmatic::catalog::FUNCTIONS {
        assert!(
            cqlmatic::catalog::records()
                .iter()
                .any(|r| r.id == f.reference_id),
            "{}",
            f.name
        );
    }
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-invariants-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn writer_process() {
    let Some(path) = std::env::var_os("CQLMATIC_TEST_LOCK_PATH") else {
        return;
    };
    let _writer = storage::Writer::acquire(&PathBuf::from(path)).unwrap();
    println!("LOCKED");
    std::io::stdout().flush().unwrap();
    loop {
        std::thread::park();
    }
}
#[test]
fn killed_writer_releases_os_lock_and_leaves_committed_snapshot_readable() {
    let dir = Temp::new();
    let path = dir.0.join("active.json");
    {
        storage::Writer::acquire(&path)
            .unwrap()
            .commit(&pack())
            .unwrap();
    }
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "writer_process", "--nocapture"])
        .env("CQLMATIC_TEST_LOCK_PATH", &path)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        assert!(output.read_line(&mut line).unwrap() > 0);
        if line.contains("LOCKED") {
            break;
        }
        line.clear();
    }
    assert!(storage::Writer::acquire(&path).is_err());
    assert_eq!(
        storage::read_pack(&path).unwrap().content_digest,
        pack().content_digest
    );
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(storage::Writer::acquire(&path).is_ok());
}
#[test]
fn identity_is_offline_and_bound_to_the_client_cloud_and_view() {
    let dir = Temp::new();
    let path = dir.0.join("fixture.credentials.json");
    fs::write(&path,r#"{"client_id":"fixture-client","client_secret":"fixture-secret","base_url":"https://api.us-2.crowdstrike.com"}"#).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let out = cli(
        &[
            "env",
            "identity",
            "--cloud",
            "us2",
            "--view",
            "search-all",
            "--credentials-file",
            path.to_str().unwrap(),
        ],
        b"",
    );
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("fixture-client"));
    assert!(!text.contains("fixture-secret"));
    let e: EnvironmentId = serde_json::from_str(&text).unwrap();
    assert!(e.tenant.starts_with("api-client:"));
    assert!(cqlmatic::network::Credentials::load(Some(&path), &e).is_ok());
    let mut other = e;
    other.cloud = Cloud::Us1;
    assert!(cqlmatic::network::Credentials::load(Some(&path), &other).is_err());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(128))]
    #[test]
    fn literal_data_cannot_add_pipeline_stages(s in "[^\\x00-\\x1f]{0,120}") {
        let request: GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"filters":[{"op":"eq","field":"user","value":s}],"steps":[{"op":"count"}]}}})).unwrap();
        let result=app::generate(request,None);
        if s.chars().any(char::is_control) {
            prop_assert_eq!(result.outcome,Outcome::Invalid);
            prop_assert!(result.query.is_none());
        } else {
            prop_assert_eq!(result.outcome,Outcome::Ready);
            let parsed=language::parser::parse(result.query.as_ref().unwrap()).unwrap();
            prop_assert_eq!(parsed.steps.len(),2);
        }
    }
    #[test]
    fn arbitrary_short_cql_never_panics(s in ".{0,300}") {
        let _=app::check(CheckRequest {schema_version:1,id:None,query:s,context:Context::default()},None);
    }
}

#[test]
fn discovery_stops_on_unconfirmed_cleanup_before_measuring_more_or_activating() {
    let mut backend = Fake {
        rows: vec![vec![json!({"events":"3"})]].into(),
        starts: 0,
        stops: 0,
        stop_error: true,
    };
    let (candidate, report) = discovery::execute(&discovery_request(), &mut backend);
    assert!(candidate.is_none());
    assert_eq!(report.status, "error");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|d| d.message.contains("cleanup"))
    );
    assert_eq!(backend.starts, 1);
    assert_eq!(backend.stops, 1);
    assert_eq!(report.captures.len(), 1);
    assert_eq!(report.captures[0].receipt.completion, "complete");
    assert_eq!(report.captures[0].receipt.cleanup, "unconfirmed");
}

#[test]
fn exact_catalog_ids_precede_cross_references_and_workflow_is_bundled() {
    for (term, id) in [
        ("cql:function:join", "cql:function:join"),
        ("cql:function:array:eval", "cql:function:array.eval"),
        ("cql:workflow:analyst", "cql:workflow:analyst"),
    ] {
        let rows = cqlmatic::catalog::search(term, 1, true);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], id);
    }
    let guide = cqlmatic::catalog::search("cql:workflow:analyst", 1, true);
    assert!(
        guide[0]["body"]
            .as_str()
            .unwrap()
            .contains("Execute and inspect the exact query")
    );
}
