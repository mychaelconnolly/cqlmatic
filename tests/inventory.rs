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
