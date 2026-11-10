use cqlmatic::{
    contracts::{Cloud, EnvironmentId, Scope},
    discovery,
    environment::{Pack, PackData},
    inventory::*,
    sha256_hex,
};
use serde_json::json;

const NOW: &str = "2026-01-02T00:00:00Z";
const START: &str = "2026-01-01T00:00:00Z";
fn environment() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "00000000000000000000000000000000".into(),
        view: "search-all".into(),
    }
}
fn source() -> Scope {
    Scope {
        repo: Some("synthetic".into()),
        type_: Some("synthetic-type".into()),
        event: None,
    }
}
fn prepared() -> InventoryState {
    let mut state = InventoryState::new(NOW).unwrap();
    state
        .prepare_scout(environment(), START.into(), NOW.into())
        .unwrap();
    let args = state.scout_plan.as_ref().unwrap().arguments.clone();
    state
        .import_scout(
            args,
            br##"{"result":[{"#repo":"synthetic","#type":"synthetic-type","events":"10"}]}"##,
            NOW,
        )
        .unwrap();
    state
        .confirm(
            environment(),
            TargetScope::FullEnvironment,
            "Synthetic inventory test".into(),
            vec![],
            NOW,
        )
        .unwrap();
    state
}
fn parser(id: &str, script: &str) -> ParserSnapshot {
    let raw = serde_json::to_vec(&json!({"resources":[{"id":id,"name":"misleading-similar-source-name","script":script,"fields_to_tag":["event.kind"]}]})).unwrap();
    parse_parser(id, RawSnapshot::new(&raw, NOW).unwrap()).unwrap()
}
fn catalog_page(ids: &[&str], offset: u32, total: Option<u64>) -> ParserPage {
    let mut response = json!({"resources":ids,"meta":{"pagination":{"offset":offset}}});
    if let Some(total) = total {
        response["meta"]["pagination"]["total"] = json!(total);
    }
    ParserPage {
        offset,
        limit: 100,
        snapshot: RawSnapshot::new(&serde_json::to_vec(&response).unwrap(), NOW).unwrap(),
    }
}

#[test]
fn blank_canvas_is_offline_and_cannot_claim_full_map() {
    let status = assess(&InventoryState::new(NOW).unwrap(), &[]).unwrap();
    assert!(status.blank_canvas);
    assert!(!status.full_map_complete);
    assert_eq!(status.message, COMPLETION_MESSAGE);
    assert!(status.covered_queries_usable);
}

#[test]
fn scouting_retains_exact_saved_result_and_rejects_different_query() {
    let mut state = prepared();
    let capture = &state.scouting[0];
    assert_eq!(
        capture.plan.query_sha256,
        sha256_hex(SCOUT_QUERY.as_bytes())
    );
    assert_eq!(
        sha256_hex(&capture.snapshot.bytes().unwrap()),
        capture.snapshot.sha256
    );
    assert_eq!(state.sources[0].scouted_events, Some(10));
    assert!(state.sources[0].review.is_none());
    let mut args = state.scout_plan.as_ref().unwrap().arguments.clone();
    args.query_string.push(' ');
    assert!(state.import_scout(args, br#"{"result":[]}"#, NOW).is_err());
    let mut corrupt = state.scouting[0].snapshot.clone();
    corrupt.raw_hex.push_str("00");
    assert!(corrupt.bytes().is_err());
}

#[test]
fn failed_or_capped_scouting_cannot_become_observation() {
    let mut state = InventoryState::new(NOW).unwrap();
    state
        .prepare_scout(environment(), START.into(), NOW.into())
        .unwrap();
    let args = state.scout_plan.as_ref().unwrap().arguments.clone();
    assert!(
        state
            .import_scout(args.clone(), br#"{"result":[],"error":"timeout"}"#, NOW)
            .is_err()
    );
    assert!(
        state
            .import_scout(args, br#"{"result":[],"truncated":true}"#, NOW)
            .is_err()
    );
    assert!(state.scouting.is_empty());
}

#[test]
fn parser_inventory_requires_all_pages_details_and_explicit_binding() {
    let mut state = prepared();
    let p = parser("p1", "user := \"synthetic\"");
    state
        .import_parser_inventory(vec![catalog_page(&["p1"], 0, Some(2))], vec![p.clone()])
        .unwrap();
    assert_eq!(
        assess(&state, &[]).unwrap().parser_catalog,
        Progress::Pending
    );
    state
        .import_parser_inventory(vec![catalog_page(&["p1"], 0, Some(1))], vec![p])
        .unwrap();
    let status = assess(&state, &[]).unwrap();
    assert_eq!(status.parser_catalog, Progress::Done);
    assert_eq!(status.unclassified_parser_ids, ["p1"]);
    assert!(state.sources[0].binding.is_none());
    assert!(!status.full_map_complete);
}

#[test]
fn catalog_without_total_or_missing_first_page_is_not_complete() {
    let mut state = prepared();
    let p = parser("p1", "user := \"synthetic\"");
    state
        .import_parser_inventory(vec![catalog_page(&["p1"], 0, None)], vec![p.clone()])
        .unwrap();
    assert_eq!(
        assess(&state, &[]).unwrap().parser_catalog,
        Progress::Pending
    );
    state
        .import_parser_inventory(vec![catalog_page(&["p1"], 1, Some(1))], vec![p])
        .unwrap();
    assert_eq!(
        assess(&state, &[]).unwrap().parser_catalog,
        Progress::Pending
    );
}

#[test]
fn native_uppercase_catalog_ids_are_explicitly_supported() {
    let raw=serde_json::to_vec(&json!({"resources":[{"ID":"native-id","Name":"Synthetic Native Name"}],"meta":{"pagination":{"offset":0,"total":1}}})).unwrap();
    let page = ParserPage {
        offset: 0,
        limit: 100,
        snapshot: RawSnapshot::new(&raw, NOW).unwrap(),
    };
    assert_eq!(parse_catalog_page(&page).unwrap().ids, ["native-id"]);
}

#[test]
fn declaration_extraction_never_executes_and_tracks_unrecognized_lines() {
    let script = "// fake := comment\nuser := \"embedded := value\"\nitems[] := split(raw, by=\",\")\ncase {\n  status = \"ok\" | result := \"yes\";\n}\nparseJson()";
    let declaration = extract_declarations(script, &["event.kind".into()]).unwrap();
    assert_eq!(
        declaration.fields,
        ["event.kind", "items[]", "result", "user"]
    );
    assert_eq!(declaration.array_fields, ["items[]"]);
    assert_eq!(declaration.review_lines, [3, 4, 6, 7]);
    assert!(!declaration.fields.contains(&"embedded".into()));
}

#[test]
fn parser_changes_invalidate_snapshot_binding() {
    let mut state = prepared();
    let p = parser("p1", "user := \"a\"");
    state
        .import_parser_inventory(vec![catalog_page(&["p1"], 0, Some(1))], vec![p.clone()])
        .unwrap();
    state.sources[0].binding = Some(ParserBinding::Parser {
        parser_id: p.id,
        parser_sha256: p.snapshot.sha256,
        reviewed_at: NOW.into(),
        evidence: "Explicit synthetic source assignment".into(),
        semantics: vec![],
    });
    state
        .import_parser_inventory(
            vec![catalog_page(&["p1"], 0, Some(1))],
            vec![parser("p1", "user := \"b\"")],
        )
        .unwrap();
    assert!(state.sources[0].binding.is_none());
}

#[test]
fn source_only_pack_and_reconciliation_cannot_claim_full_map() {
    let mut state = prepared();
    let sid = discovery::source_id(&source());
    let pack = Pack::new(environment(),NOW.into(),PackData {
        sources:vec![json!({"id":sid,"kind":"repo","name":"synthetic","inventory_status":"inventoried","selectors":{"repo":"synthetic","type":"synthetic-type"},"provenance_ids":["prov:synthetic"]})],
        provenance:vec![json!({"id":"prov:synthetic","source_type":"manual-curation","canonical_source":"synthetic","retrieved_at":NOW,"redaction_status":"sanitized","coverage_status":"declared-only"})],
        ..Default::default()
    }).unwrap();
    state.packs.push(PackReference {
        path: "synthetic-pack.json".into(),
        content_digest: pack.content_digest.clone(),
        imported_at: NOW.into(),
        discovery_run: None,
    });
    state.reconciled_evidence_sha256 = Some(state.evidence_digest().unwrap());
    state.reconciled_at = Some(NOW.into());
    let status = assess(&state, &[pack]).unwrap();
    assert_eq!(status.reconciliation, Progress::Done);
    assert_eq!(status.sources[0].measurements, Progress::Unknown);
    assert_eq!(status.sources[0].observation, Progress::Unknown);
    assert!(!status.full_map_complete);
}

#[test]
fn declared_parser_fields_are_not_observed_and_mismatched_pack_digest_fails() {
    let mut state = prepared();
    let p = parser("p1", "quiet := \"declared\"");
    state
        .import_parser_inventory(vec![catalog_page(&["p1"], 0, Some(1))], vec![p.clone()])
        .unwrap();
    state.sources[0].binding = Some(ParserBinding::Parser {
        parser_id: p.id,
        parser_sha256: p.snapshot.sha256,
        reviewed_at: NOW.into(),
        evidence: "Explicit synthetic binding".into(),
        semantics: vec![],
    });
    let status = assess(&state, &[]).unwrap();
    let quiet = status.sources[0]
        .fields
        .iter()
        .find(|f| f.field == "quiet")
        .unwrap();
    assert!(quiet.declared);
    assert!(!quiet.observed);
    assert!(!quiet.measured);
    assert_eq!(quiet.values, Progress::NotApplicable);
    assert!(!quiet.values_requested);
    state.packs.push(PackReference {
        path: "changed.json".into(),
        content_digest: "f".repeat(64),
        imported_at: NOW.into(),
        discovery_run: None,
    });
    assert!(assess(&state, &[]).is_err());
}

#[test]
fn strict_requests_reject_unknown_fields_and_no_completion_checkbox_exists() {
    assert!(
        serde_json::from_value::<InventoryRequest>(
            json!({"schema_version":1,"action":{"kind":"init","complete":true}})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<InventoryRequest>(
            json!({"schema_version":1,"action":{"kind":"set-complete"}})
        )
        .is_err()
    );
}

#[test]
fn selection_scope_never_becomes_full_map_and_discovery_carries_explicit_fields() {
    let mut state = prepared();
    state
        .confirm(
            environment(),
            TargetScope::SelectedSources,
            "Synthetic selection".into(),
            vec![source()],
            NOW,
        )
        .unwrap();
    let request = state
        .discovery_request(
            START.into(),
            NOW.into(),
            &[],
            vec![],
            vec![],
            vec![discovery::FieldSelection {
                source: source(),
                fields: vec!["quiet".into()],
            }],
            30,
            90,
        )
        .unwrap();
    assert_eq!(request.population_fields[0].fields, ["quiet"]);
    assert!(!assess(&state, &[]).unwrap().full_map_complete);
}

fn measured_pack(date: &str, coverage: &str, observed: bool) -> Pack {
    let sid = discovery::source_id(&source());
    Pack::new(environment(), date.into(), PackData {
        sources: vec![json!({"id":sid,"kind":"repo","name":"synthetic","inventory_status":"observed-only","selectors":{"repo":"synthetic","type":"synthetic-type"},"evidence":{"observed":observed},"provenance_ids":["prov:measurement"]})],
        fields: vec![json!({"id":"env:field:synthetic:user","source_id":sid,"path":"user","data_type":"string","evidence_kind":"observed","coverage":{"observed":observed,"rate":if observed {1.0} else {0.0}},"provenance_ids":["prov:measurement"]})],
        provenance: vec![json!({"id":"prov:measurement","source_type":"mcp-observation","canonical_source":"Synthetic offline measurement","retrieved_at":date,"redaction_status":"sanitized","coverage_status":coverage})],
        ..Default::default()
    }).unwrap()
}
fn retain(state: &mut InventoryState, pack: &Pack, path: &str) -> PackReference {
    let reference = PackReference {
        path: path.into(),
        content_digest: pack.content_digest.clone(),
        imported_at: pack.created_at.clone(),
        discovery_run: None,
    };
    state.packs.push(reference.clone());
    reference
}

#[test]
fn unavailable_error_and_stale_provenance_cannot_promote_observations() {
    for coverage in ["unavailable", "error", "stale", "partial", "incomplete"] {
        let mut state = prepared();
        let pack = measured_pack(NOW, coverage, true);
        retain(&mut state, &pack, "synthetic.json");
        let status = assess(&state, &[pack]).unwrap();
        assert_eq!(
            status.sources[0].observation,
            Progress::Unknown,
            "{coverage}"
        );
        assert!(!status.sources[0].fields[0].observed, "{coverage}");
        assert!(!status.sources[0].fields[0].measured, "{coverage}");
    }
}

#[test]
fn current_zero_overrides_older_observed_source_and_field() {
    let mut state = prepared();
    let old = measured_pack(NOW, "observed-only", true);
    let current = measured_pack("2026-01-01T23:30:00-01:00", "observed-only", false);
    retain(&mut state, &current, "current.json");
    // Deliberately retain in reverse order: timestamps establish evidence precedence.
    retain(&mut state, &old, "old.json");
    let status = assess(&state, &[old, current]).unwrap();
    let source = &status.sources[0];
    assert_eq!(source.observation, Progress::Unknown);
    assert!(!source.fields[0].observed);
    assert!(source.fields[0].measured);
}

#[test]
fn active_cumulative_snapshot_preserves_untargeted_fields_and_historical_status() {
    let mut state = prepared();
    let old = measured_pack(NOW, "observed-only", true);
    retain(&mut state, &old, "old.json");
    let mut data = old.data.clone();
    data.fields[0]["refresh_status"] = json!("not-reobserved");
    let mut retained = data.fields[0].clone();
    retained["id"] = json!("env:field:synthetic:retained");
    retained["path"] = json!("retained");
    retained.as_object_mut().unwrap().remove("refresh_status");
    data.fields.push(retained);
    let current = Pack::new(environment(), "2026-01-03T00:00:00Z".into(), data).unwrap();
    state.active_pack = Some(retain(&mut state, &current, "current.json"));
    let status = assess(&state, &[current, old]).unwrap();
    assert!(status.active_pack.is_some());
    let fields = &status.sources[0].fields;
    assert!(
        fields
            .iter()
            .find(|f| f.field == "retained")
            .unwrap()
            .observed
    );
    let historical = fields.iter().find(|f| f.field == "user").unwrap();
    assert!(!historical.observed);
    assert!(historical.not_reobserved);
    assert!(!historical.measured);
}

#[test]
fn dynamic_parser_semantics_require_review_of_each_exact_snapshot_line() {
    let mut state = prepared();
    let p = parser("p1", "user := \"synthetic\"\nparseJson()");
    state
        .import_parser_inventory(vec![catalog_page(&["p1"], 0, Some(1))], vec![p.clone()])
        .unwrap();
    state.sources[0].binding = Some(ParserBinding::Parser {
        parser_id: p.id.clone(),
        parser_sha256: p.snapshot.sha256.clone(),
        reviewed_at: NOW.into(),
        evidence: "Explicit synthetic assignment".into(),
        semantics: vec![],
    });
    assert_eq!(
        assess(&state, &[]).unwrap().sources[0].parser,
        Progress::Review
    );
    if let Some(ParserBinding::Parser { semantics, .. }) = &mut state.sources[0].binding {
        *semantics = p
            .declarations
            .review_lines
            .iter()
            .map(|line| SemanticReview {
                line: *line,
                interpretation: "Dynamic keys need measured field inventory".into(),
                evidence: "Reviewed exact saved definition".into(),
            })
            .collect();
    }
    assert_eq!(
        assess(&state, &[]).unwrap().sources[0].parser,
        Progress::Done
    );
    let mut stale = state.sources[0].binding.clone().unwrap();
    if let ParserBinding::Parser { parser_sha256, .. } = &mut stale {
        *parser_sha256 = "0".repeat(64);
    }
    assert!(state.validate_binding(&stale).is_err());
}

fn imported_pack(selectors: Scope, label: &str, date: &str) -> Pack {
    let sid = discovery::source_id(&selectors);
    Pack::new(environment(), date.into(), PackData {
        sources: vec![json!({"id":sid,"kind":"source","name":label,"inventory_status":"observed-only",
            "selectors":{"repo":selectors.repo,"type":selectors.type_},
            "evidence":{"observed":true,"observation_window":format!("{START}/{NOW}")},
            "provenance_ids":[format!("prov:{label}")]})],
        fields:vec![json!({"id":format!("env:field:{label}"),"source_id":sid,"path":"direct",
            "data_type":"unknown","evidence_kind":"observed","coverage":{"observed":true},"provenance_ids":[format!("prov:{label}")]})],
        provenance: vec![json!({"id":format!("prov:{label}"),"source_type":"mcp-observation","canonical_source":"Synthetic saved response",
            "retrieved_at":NOW,"redaction_status":"none","coverage_status":"observed-only"})],
        ..PackData::default()
    }).unwrap()
}

#[test]
fn type_only_sources_and_historical_imports_are_visible_but_unconfirmed() {
    let mut state = prepared();
    let selectors = Scope {
        type_: Some("type-only".into()),
        ..Scope::default()
    };
    let old = imported_pack(source(), "old", NOW);
    let historical = imported_pack(selectors.clone(), "historical", START);
    let merged = cqlmatic::environment::import_history(&old, &historical).unwrap();
    assert_eq!(merged.data.fields.len(), 2);
    assert_eq!(merged.created_at, NOW);
    assert_eq!(
        cqlmatic::environment::import_history(&merged, &historical)
            .unwrap()
            .content_digest,
        merged.content_digest
    );
    state.register_pack_sources(&historical).unwrap();
    let registered = state
        .sources
        .iter()
        .find(|s| s.selectors == selectors)
        .unwrap();
    assert!(!registered.expected);
    assert!(registered.review.is_none());
    assert!(registered.scouted_events.is_none());
    assert!(
        assess(&state, &[])
            .unwrap()
            .sources
            .iter()
            .any(|s| s.selectors == selectors && !s.expected)
    );
    state
        .confirm(
            environment(),
            TargetScope::SelectedSources,
            "Selected synthetic source".into(),
            vec![selectors],
            NOW,
        )
        .unwrap();
    let request = state
        .discovery_request(
            START.into(),
            NOW.into(),
            &[],
            vec![],
            vec![],
            vec![],
            10,
            120,
        )
        .unwrap();
    let plan = discovery::initial_plan(&request).unwrap();
    assert!(
        plan["initial_probes"][0]["query"]
            .as_str()
            .unwrap()
            .starts_with("#type")
    );
}

#[test]
fn conflicting_import_ids_reject_without_mutating_retained_pack() {
    let old = imported_pack(source(), "conflict", NOW);
    let before = old.content_digest.clone();
    let mut conflicting = old.clone();
    conflicting.data.fields[0]["data_type"] = json!("string");
    conflicting.content_digest = conflicting.digest().unwrap();
    assert!(cqlmatic::environment::import_history(&old, &conflicting).is_err());
    assert_eq!(old.content_digest, before);
    old.validate().unwrap();
}

#[test]
fn quick_mode_is_explicit_and_sources_only_probes_never_measure_fields() {
    let mut state = prepared();
    state.discovery_mode = DiscoveryMode::Quick;
    state.quick_depth = QuickDepth::SourcesOnly;
    state
        .confirm(
            environment(),
            TargetScope::SelectedSources,
            "Quick synthetic source".into(),
            vec![source()],
            NOW,
        )
        .unwrap();
    let request = state
        .discovery_request(
            START.into(),
            NOW.into(),
            &[],
            vec![],
            vec![],
            vec![],
            10,
            120,
        )
        .unwrap();
    let probes = discovery::frontier(&request, &Default::default()).unwrap();
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0].kind, "volume");
    let mut answered = std::collections::BTreeMap::new();
    answered.insert(
        discovery::probe_id(&request.environment.view, &probes[0]),
        vec![json!({"events":5})],
    );
    assert!(discovery::frontier(&request, &answered).unwrap().is_empty());
    answered.insert(
        discovery::probe_id(&request.environment.view, &probes[0]),
        vec![json!({"events":0})],
    );
    assert!(discovery::frontier(&request, &answered).is_err());
    assert!(
        state
            .discovery_request(
                START.into(),
                NOW.into(),
                &[],
                vec!["direct".into()],
                vec![],
                vec![],
                10,
                120
            )
            .is_err()
    );
    assert!(!assess(&state, &[]).unwrap().full_map_complete);
}

fn query_observation(
    query: &str,
    result: serde_json::Value,
) -> Result<QueryObservation, cqlmatic::contracts::Diagnostic> {
    QueryObservation::from_result(
        query.as_bytes(),
        &serde_json::to_vec(&result).unwrap(),
        environment(),
        source(),
        START.into(),
        NOW.into(),
        NOW.into(),
        QueryResultAssertion::ExactQueryResultTargetWindow,
        std::env::temp_dir()
            .join("synthetic-audit.json")
            .to_string_lossy()
            .into_owned(),
    )
}

#[test]
fn saved_query_observations_keep_only_direct_positive_fields_with_exact_binding() {
    let observation = query_observation(r##"#repo="synthetic" #type="synthetic-type" | select([present, zero, flag, empty, null_value]) | head(10)"##,
        json!({"result":[{"present":"yes","zero":0,"flag":false,"empty":"","null_value":null}]})).unwrap();
    let fields: Vec<_> = observation
        .fields
        .iter()
        .map(|f| f.field.as_str())
        .collect();
    assert_eq!(fields, ["flag", "present", "zero"]);
    assert!(observation.fields.iter().all(|f| f.positive_rows == 1));
    let state_text = serde_json::to_string(&observation).unwrap();
    assert!(!state_text.contains("synthetic-type"));
    assert!(!state_text.contains("select("));
    let mut state = prepared();
    state.record_query_observation(observation.clone()).unwrap();
    state.record_query_observation(observation).unwrap();
    assert_eq!(state.query_observations.len(), 1);
    assert!(state.packs.is_empty());
    state
        .add_unverified_leads(vec![
            UnverifiedFieldLead {
                source: source(),
                field: "lead".into(),
            },
            UnverifiedFieldLead {
                source: source(),
                field: "lead".into(),
            },
        ])
        .unwrap();
    assert_eq!(state.unverified_leads.len(), 1);
    let full = assess(&state, &[]).unwrap();
    assert_eq!(full.unverified_leads.len(), 1);
    assert_eq!(full.query_observations.len(), 1);
    assert_eq!(
        full.query_observations[0].caller_assertion,
        QueryResultAssertion::ExactQueryResultTargetWindow
    );
    let compact = summary(&full);
    assert!(compact.get("unverified_leads").is_none());
    assert!(compact.get("query_observations").is_none());
    assert_eq!(compact["counts"]["query_observations"], 1);
    assert!(state.sources[0].review.is_none());
}

#[test]
fn saved_query_observations_reject_derived_aliased_aggregate_join_and_other_sources() {
    for query in [
        r##"#repo="synthetic" #type="synthetic-type" | /(?<derived>.+)/ | select(derived)"##,
        r##"#repo="synthetic" #type="synthetic-type" | direct=/(?<derived>.+)/ | select(derived)"##,
    ] {
        let error =
            query_observation(query, json!({"result":[{"derived":"captured"}]})).unwrap_err();
        assert!(
            error.message.contains("Regex capture"),
            "{query}: {error:?}"
        );
    }
    for query in [
        r##"#repo="other" #type="synthetic-type" | head(10)"##,
        r##"(#repo="synthetic" OR #repo="other") #type="synthetic-type""##,
        r##"#repo="synthetic" #type="synthetic-type" | n := 1"##,
        r##"#repo="synthetic" #type="synthetic-type" | rename(field=direct,as=alias)"##,
        r##"#repo="synthetic" #type="synthetic-type" | count(as=events)"##,
        r##"#repo="synthetic" #type="synthetic-type" | join({#repo="other"},field=direct)"##,
        r##"#repo="synthetic" | head(10)"##,
    ] {
        assert!(
            query_observation(query, json!({"result":[{"direct":true}]})).is_err(),
            "{query}"
        );
    }
    for result in [
        json!({"result":[]}),
        json!({"result":[{"direct":null}]}),
        json!({"result":[{"direct":[] }]}),
        json!({"result":[{"direct":"yes"}],"truncated":true}),
        json!({"result":[{"#repo":"other","direct":1}]}),
    ] {
        assert!(
            query_observation(
                r##"#repo="synthetic" #type="synthetic-type" | head(10)"##,
                result
            )
            .is_err()
        );
    }
}

#[test]
fn version_one_state_defaults_to_full_and_compact_status_omits_field_histories() {
    let original = InventoryState::new(NOW).unwrap();
    let mut old = serde_json::to_value(original).unwrap();
    old["schema_version"] = json!(1);
    for field in [
        "discovery_mode",
        "quick_depth",
        "unverified_leads",
        "query_observations",
    ] {
        old.as_object_mut().unwrap().remove(field);
    }
    let restored: InventoryState = serde_json::from_value(old).unwrap();
    restored.validate().unwrap();
    assert_eq!(restored.discovery_mode, DiscoveryMode::Full);
    let full = assess(&restored, &[]).unwrap();
    let compact = summary(&full);
    assert_eq!(compact["summary"], true);
    assert_eq!(compact["counts"]["sources"], 0);
    assert!(compact.get("scouting_evidence").is_none());
}

#[test]
fn saved_query_observations_accept_scalar_and_lowered_literal_projections_only() {
    let scalar = query_observation(
        r##"#repo="synthetic" #type="synthetic-type" | select(direct)"##,
        json!({"result":[{"direct":true}]}),
    )
    .unwrap();
    assert_eq!(scalar.fields[0].field, "direct");
    let lowered = cqlmatic::language::plan::lower(&cqlmatic::contracts::Plan {
        source: source(),
        filters: vec![],
        steps: vec![],
    })
    .unwrap()
    .render();
    assert!(query_observation(&lowered, json!({"result":[{"direct":true}]})).is_ok());
    assert!(
        query_observation(
            r##"#repo="synthetic" #type="synthetic-type" | select([direct])"##,
            json!({"result":[{"other":true}]})
        )
        .is_err()
    );
    assert!(
        query_observation(
            r##"#repo="synthetic" #type="synthetic-type" | select([direct]) | select([other])"##,
            json!({"result":[{"other":true}]})
        )
        .is_err()
    );
}

#[test]
fn full_environment_completion_requires_newly_imported_selectors_to_be_confirmed() {
    let mut state = prepared();
    state.sources[0].review = Some(SourceReview {
        purpose: "Synthetic source".into(),
        data_model: "Synthetic events".into(),
        reviewed_at: NOW.into(),
        evidence: "Synthetic source contract".into(),
    });
    state.sources[0].binding = Some(ParserBinding::NotApplicable {
        reason: NativeSourceReason::FalconSensorNative,
        reviewed_at: NOW.into(),
        evidence: "Synthetic native source".into(),
    });
    state
        .import_parser_inventory(vec![catalog_page(&[], 0, Some(0))], vec![])
        .unwrap();
    let pack = imported_pack(source(), "complete", NOW);
    let reference = PackReference {
        path: "synthetic-pack.json".into(),
        content_digest: pack.content_digest.clone(),
        imported_at: NOW.into(),
        discovery_run: Some("synthetic-run.json".into()),
    };
    state.packs.push(reference.clone());
    state.active_pack = Some(reference);
    state.discovery_runs.push(serde_json::from_value(json!({"state_path":"synthetic-run.json","pack_path":"synthetic-pack.json","receipt_path":"synthetic-receipt.json","start":START,"end":NOW,"source_ids":[state.sources[0].id],"value_fields":[],"max_queries":20,"max_seconds":300,"answered":5,"elapsed_seconds":1.0,"status":"complete"})).unwrap());
    state.reconciled_evidence_sha256 = Some(state.evidence_digest().unwrap());
    state.reconciled_at = Some(NOW.into());
    assert!(
        assess(&state, std::slice::from_ref(&pack))
            .unwrap()
            .full_map_complete
    );
    let imported = imported_pack(
        Scope {
            type_: Some("unconfirmed".into()),
            ..Scope::default()
        },
        "new",
        START,
    );
    state.register_pack_sources(&imported).unwrap();
    state.reconciled_evidence_sha256 = Some(state.evidence_digest().unwrap());
    let status = assess(&state, &[pack]).unwrap();
    assert!(!status.full_map_complete);
    assert!(
        status
            .next_actions
            .iter()
            .any(|action| action.contains("known source selectors"))
    );
}

#[test]
fn direct_data_filter_alternatives_do_not_broaden_an_exact_saved_query_source() {
    assert!(query_observation(r##"#repo="synthetic" #type="synthetic-type" | (value="a" OR value="b") | select(direct)"##,
        json!({"result":[{"direct":true}]})).is_ok());
    assert!(
        query_observation(
            r##"#repo="synthetic" #type="synthetic-type" | !(#type="other") | select(direct)"##,
            json!({"result":[{"direct":true}]})
        )
        .is_err()
    );
}

#[test]
fn source_selector_extension_keeps_empty_selector_validation() {
    for selectors in [
        Scope {
            repo: Some("".into()),
            type_: Some("valid-type".into()),
            event: None,
        },
        Scope {
            type_: Some(" ".into()),
            ..Scope::default()
        },
        Scope::default(),
    ] {
        let mut state = InventoryState::new(NOW).unwrap();
        assert!(
            state
                .confirm(
                    environment(),
                    TargetScope::SelectedSources,
                    "Synthetic source".into(),
                    vec![selectors.clone()],
                    NOW
                )
                .is_err()
        );
        let request:cqlmatic::discovery::DiscoverRequest=serde_json::from_value(json!({"schema_version":1,"environment":environment(),"start":START,"end":NOW,"sources":[selectors]})).unwrap();
        assert!(discovery::validate_request(&request).is_err());
    }
}

#[test]
fn quick_scout_requires_explicit_target_and_finite_host_budget_without_breaking_full_requests() {
    let mut state = InventoryState::new(NOW).unwrap();
    let legacy = state
        .prepare_scout(environment(), START.into(), NOW.into())
        .unwrap();
    assert_eq!(legacy.arguments.repository, "search-all");
    assert!(
        serde_json::to_value(&legacy)
            .unwrap()
            .get("max_seconds")
            .is_none()
    );
    state.discovery_mode = DiscoveryMode::Quick;
    let legacy_arguments = legacy.arguments;
    assert!(
        state
            .import_scout(
                legacy_arguments,
                br##"{"result":[{"#type":"synthetic","events":1}]}"##,
                NOW
            )
            .is_err()
    );
    state.scout_plan = None;
    for max_seconds in [None, Some(0), Some(601)] {
        assert!(
            state
                .prepare_scout_with_budget(environment(), START.into(), NOW.into(), max_seconds)
                .is_err()
        );
        assert!(state.scout_plan.is_none());
    }
    let mut selected = environment();
    selected.view = "selected-view".into();
    let plan = state
        .prepare_scout_with_budget(selected.clone(), START.into(), NOW.into(), Some(120))
        .unwrap();
    assert_eq!(plan.arguments.repository, "selected-view");
    assert_eq!(plan.max_seconds, Some(120));
    selected.view.clear();
    assert!(
        state
            .prepare_scout_with_budget(selected, START.into(), NOW.into(), Some(120))
            .is_err()
    );
    assert!(
        state
            .prepare_scout_with_budget(
                environment(),
                "2026-01-01T00:00:00-05:00".into(),
                NOW.into(),
                Some(120)
            )
            .is_err()
    );
}
