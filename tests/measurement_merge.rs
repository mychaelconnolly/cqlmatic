use cqlmatic::{
    contracts::{Cloud, EnvironmentId, Scope},
    environment::{self, Pack, PackData},
    inventory::{self, InventoryState, PackReference, TargetScope},
};
use serde_json::{Value, json};

fn pack(end: &str, retrieved: &str, created: &str, observed: bool, rate: Option<f64>) -> Pack {
    let window = format!("2026-01-01T00:00:00Z/{end}");
    let proof = format!("prov:field:{end}:{retrieved}");
    let volume = format!("prov:volume:{end}:{retrieved}");
    Pack::new(
        EnvironmentId { cloud: Cloud::Us1, tenant: "0".repeat(32), view: "search-all".into() },
        created.into(),
        PackData {
            sources: vec![json!({"id":"env:repo:events","kind":"repo","name":"events",
                "inventory_status":"observed-only","selectors":{"repo":"events"},
                "evidence":{"observed":true,"observation_window":window,"measured_fields":["key"]},
                "provenance_ids":[volume]})],
            fields: vec![json!({"id":"env:field:key","source_id":"env:repo:events","path":"key",
                "data_type":"unknown","is_tag":false,"evidence_kind":"observed",
                "observation_window":window,"coverage":{"observed":observed,"rate":rate},"provenance_ids":[proof]})],
            provenance: [proof,volume].into_iter().map(|id| json!({"id":id,"source_type":"mcp-observation",
                "canonical_source":"Synthetic measurement","retrieved_at":retrieved,"observation_window":window,
                "redaction_status":"none","coverage_status":"observed-only"})).collect(),
            ..PackData::default()
        },
    ).unwrap()
}

fn field(pack: &Pack) -> &Value {
    &pack.data.fields[0]
}

#[test]
fn a_backdated_opposite_measurement_cannot_replace_a_newer_negative() {
    let old = pack(
        "2026-01-03T00:00:00Z",
        "2026-01-04T00:00:00Z",
        "2026-01-04T00:00:00Z",
        false,
        Some(0.0),
    );
    let candidate = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "2026-01-06T00:00:00Z",
        true,
        Some(0.5),
    );
    let before = old.content_digest.clone();
    let error = environment::merge(&old, &candidate).unwrap_err();
    assert!(error.message.contains("ordering conflict"));
    assert_eq!(old.content_digest, before);
}

#[test]
fn an_older_consistent_measurement_is_history_without_refreshing_the_field() {
    let old = pack(
        "2026-01-03T00:00:00Z",
        "2026-01-04T00:00:00Z",
        "2026-01-04T00:00:00Z",
        false,
        None,
    );
    let candidate = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "2026-01-06T00:00:00Z",
        false,
        Some(0.0),
    );
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(field(&merged), field(&old));
    let losing = &candidate.data.fields[0]["provenance_ids"][0];
    assert!(
        merged
            .data
            .provenance
            .iter()
            .any(|proof| proof["id"] == *losing)
    );
    assert!(
        !field(&merged)["provenance_ids"]
            .as_array()
            .unwrap()
            .contains(losing)
    );
}

#[test]
fn tied_presence_and_nonnull_rate_conflicts_reject_without_treating_missing_rate_as_conflict() {
    let old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-01-04T00:00:00Z",
        true,
        Some(0.5),
    );
    for (observed, rate) in [(false, Some(0.0)), (true, Some(0.7))] {
        let candidate = pack(
            "2026-01-02T00:00:00Z",
            "2026-01-03T00:00:00Z",
            "2026-01-05T00:00:00Z",
            observed,
            rate,
        );
        assert!(environment::merge(&old, &candidate).is_err());
    }
    let candidate = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-01-05T00:00:00Z",
        true,
        None,
    );
    assert_eq!(
        field(&environment::merge(&old, &candidate).unwrap()),
        field(&old)
    );
}

#[test]
fn a_newer_opposite_measurement_replaces_current_proof_without_relinking_history() {
    let old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-01-03T00:00:00Z",
        false,
        None,
    );
    let candidate = pack(
        "2026-01-04T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "2026-01-06T00:00:00Z",
        true,
        Some(0.5),
    );
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(field(&merged), field(&candidate));
    let historical = &field(&old)["provenance_ids"][0];
    assert!(
        merged
            .data
            .provenance
            .iter()
            .any(|proof| proof["id"] == *historical)
    );
    assert!(
        !field(&merged)["provenance_ids"]
            .as_array()
            .unwrap()
            .contains(historical)
    );
}

#[test]
fn snapshot_monotonicity_remains_required_in_addition_to_field_measurement_ordering() {
    let old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-07-01T00:00:00Z",
        false,
        None,
    );
    let candidate = pack(
        "2026-05-02T00:00:00Z",
        "2026-06-01T00:00:00Z",
        "2026-06-02T00:00:00Z",
        true,
        None,
    );
    let before = old.content_digest.clone();
    assert!(
        environment::merge(&old, &candidate)
            .unwrap_err()
            .message
            .contains("older or same-time")
    );
    assert_eq!(old.content_digest, before);
}

#[test]
fn an_unorderable_legacy_negative_cannot_be_promoted_by_snapshot_creation_time() {
    let mut old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-01-03T00:00:00Z",
        false,
        None,
    );
    old.data.fields[0]["observation_window"] = Value::Null;
    old.content_digest = old.digest().unwrap();
    let candidate = pack(
        "2026-01-04T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "2026-01-06T00:00:00Z",
        true,
        None,
    );
    assert!(
        environment::merge(&old, &candidate)
            .unwrap_err()
            .message
            .contains("orderable")
    );
}

#[test]
fn a_zero_facet_is_measured_without_a_population_rate_or_full_inventory_completion() {
    let pack = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-01-03T00:00:00Z",
        false,
        None,
    );
    let mut state = InventoryState::new("2026-01-03T00:00:00Z").unwrap();
    state
        .confirm(
            pack.environment.clone(),
            TargetScope::SelectedSources,
            "Synthetic scope".into(),
            vec![Scope {
                repo: Some("events".into()),
                ..Scope::default()
            }],
            "2026-01-03T00:00:00Z",
        )
        .unwrap();
    let reference = PackReference {
        path: "/synthetic/pack.json".into(),
        content_digest: pack.content_digest.clone(),
        imported_at: "2026-01-03T00:00:00Z".into(),
        discovery_run: None,
    };
    state.active_pack = Some(reference.clone());
    state.packs.push(reference);
    let status = inventory::assess(&state, &[pack]).unwrap();
    assert!(status.sources[0].fields[0].measured);
    assert!(!status.sources[0].fields[0].observed);
    assert!(!status.full_map_complete);
}

fn set_start(pack: &mut Pack, start: &str) {
    let end = pack.data.fields[0]["observation_window"]
        .as_str()
        .unwrap()
        .split_once('/')
        .unwrap()
        .1;
    let window = format!("{start}/{end}");
    pack.data.fields[0]["observation_window"] = json!(window);
    pack.data.sources[0]["evidence"]["observation_window"] = json!(window);
    for proof in &mut pack.data.provenance {
        proof["observation_window"] = json!(window);
    }
    pack.content_digest = pack.digest().unwrap();
}

#[test]
fn undated_historical_markers_require_a_measurement_window_after_the_retained_snapshot() {
    let mut old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-06-01T00:00:00Z",
        false,
        None,
    );
    old.data.fields[0]["refresh_status"] = json!("not-reobserved");
    old.content_digest = old.digest().unwrap();
    let candidate = pack(
        "2026-03-02T00:00:00Z",
        "2026-07-02T00:00:00Z",
        "2026-07-03T00:00:00Z",
        true,
        None,
    );
    assert!(environment::merge(&old, &candidate).is_err());
    let mut candidate = pack(
        "2026-07-02T00:00:00Z",
        "2026-07-02T00:00:00Z",
        "2026-07-03T00:00:00Z",
        true,
        None,
    );
    set_start(&mut candidate, "2026-07-01T00:00:00Z");
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(field(&merged)["coverage"]["observed"], true);
    assert!(field(&merged)["refresh_status"].is_null());
}

#[test]
fn stale_source_marker_dates_cannot_be_replaced_by_an_older_window_repacked_later() {
    let mut old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-06-01T00:00:00Z",
        true,
        None,
    );
    old.data.sources[0]["inventory_status"] = json!("stale");
    old.data.sources[0]["evidence"]
        .as_object_mut()
        .unwrap()
        .remove("measured_fields");
    old.data.provenance[1]["coverage_status"] = json!("stale");
    old.content_digest = old.digest().unwrap();
    let candidate = pack(
        "2026-03-02T00:00:00Z",
        "2026-07-02T00:00:00Z",
        "2026-07-03T00:00:00Z",
        true,
        None,
    );
    assert!(
        environment::merge(&old, &candidate)
            .unwrap_err()
            .message
            .contains("Source measurement ordering")
    );
    let mut candidate = pack(
        "2026-07-02T00:00:00Z",
        "2026-07-02T00:00:00Z",
        "2026-07-03T00:00:00Z",
        true,
        None,
    );
    set_start(&mut candidate, "2026-07-01T00:00:00Z");
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(
        merged.data.sources[0]["provenance_ids"],
        candidate.data.sources[0]["provenance_ids"]
    );
    assert!(
        merged
            .data
            .provenance
            .iter()
            .any(|proof| proof["coverage_status"] == "stale")
    );
}

#[test]
fn source_refresh_cannot_inherit_narrower_selector_restrictions() {
    let mut old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-01-03T00:00:00Z",
        true,
        None,
    );
    old.data.sources[0]["selectors"]["event_family"] = json!("one-kind");
    old.content_digest = old.digest().unwrap();
    let candidate = pack(
        "2026-01-04T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "2026-01-06T00:00:00Z",
        true,
        None,
    );
    assert!(
        environment::merge(&old, &candidate)
            .unwrap_err()
            .message
            .contains("selector restrictions")
    );
}

#[test]
fn an_older_consistent_source_zero_does_not_backdate_the_current_negative() {
    let mut old = pack(
        "2026-01-03T00:00:00Z",
        "2026-01-04T00:00:00Z",
        "2026-01-04T00:00:00Z",
        false,
        None,
    );
    let mut candidate = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "2026-01-06T00:00:00Z",
        false,
        None,
    );
    for pack in [&mut old, &mut candidate] {
        pack.data.sources[0]["evidence"]["observed"] = json!(false);
        pack.data.sources[0]["evidence"]
            .as_object_mut()
            .unwrap()
            .remove("measured_fields");
        pack.content_digest = pack.digest().unwrap();
    }
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(
        merged.data.sources[0]["evidence"],
        old.data.sources[0]["evidence"]
    );
    assert_eq!(
        merged.data.sources[0]["provenance_ids"],
        old.data.sources[0]["provenance_ids"]
    );
    assert!(
        merged
            .data
            .provenance
            .iter()
            .any(|proof| proof["id"] == candidate.data.sources[0]["provenance_ids"][0])
    );
    assert!(
        !merged.data.sources[0]["provenance_ids"]
            .as_array()
            .unwrap()
            .contains(&candidate.data.sources[0]["provenance_ids"][0])
    );
}

#[test]
fn a_compatible_old_zero_cannot_remove_a_historical_boundary_before_a_second_refresh() {
    for marker in ["not-reobserved", "stale", "unknown-window"] {
        let mut old = pack(
            "2026-01-02T00:00:00Z",
            "2026-01-03T00:00:00Z",
            "2026-06-01T00:00:00Z",
            false,
            None,
        );
        match marker {
            "not-reobserved" => old.data.fields[0]["refresh_status"] = json!("not-reobserved"),
            "stale" => old.data.provenance[0]["coverage_status"] = json!("stale"),
            _ => old.data.fields[0]["observation_window"] = Value::Null,
        }
        old.content_digest = old.digest().unwrap();
        let mut zero = pack(
            "2026-03-02T00:00:00Z",
            "2026-07-02T00:00:00Z",
            "2026-07-03T00:00:00Z",
            false,
            None,
        );
        set_start(&mut zero, "2026-03-01T00:00:00Z");
        let retained = environment::merge(&old, &zero).unwrap();
        assert_eq!(field(&retained), field(&old));
        assert!(
            retained
                .data
                .provenance
                .iter()
                .any(|proof| proof["id"] == field(&zero)["provenance_ids"][0])
        );
        assert!(
            !field(&retained)["provenance_ids"]
                .as_array()
                .unwrap()
                .contains(&field(&zero)["provenance_ids"][0])
        );
        let mut positive = pack(
            "2026-05-02T00:00:00Z",
            "2026-08-02T00:00:00Z",
            "2026-08-03T00:00:00Z",
            true,
            None,
        );
        set_start(&mut positive, "2026-05-01T00:00:00Z");
        assert!(environment::merge(&retained, &positive).is_err());
        assert_eq!(field(&retained), field(&old));
    }
}

#[test]
fn opposite_unorderable_field_decisions_fail_in_both_directions() {
    for (previous, incoming) in [(false, true), (true, false)] {
        let mut old = pack(
            "2026-01-02T00:00:00Z",
            "2026-01-03T00:00:00Z",
            "2026-06-01T00:00:00Z",
            previous,
            None,
        );
        let mut candidate = pack(
            "2026-03-02T00:00:00Z",
            "2026-07-02T00:00:00Z",
            "2026-07-03T00:00:00Z",
            incoming,
            None,
        );
        for pack in [&mut old, &mut candidate] {
            pack.data.fields[0]["observation_window"] = Value::Null;
            pack.content_digest = pack.digest().unwrap();
        }
        assert!(
            environment::merge(&old, &candidate)
                .unwrap_err()
                .message
                .contains("ordering conflict")
        );
    }
}

#[test]
fn qualified_post_snapshot_zero_and_positive_measurements_can_clear_historical_markers() {
    let mut old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-06-01T00:00:00Z",
        false,
        None,
    );
    old.data.fields[0]["refresh_status"] = json!("not-reobserved");
    old.content_digest = old.digest().unwrap();
    for observed in [false, true] {
        let mut candidate = pack(
            "2026-07-02T00:00:00Z",
            "2026-07-02T00:00:00Z",
            "2026-07-03T00:00:00Z",
            observed,
            None,
        );
        set_start(&mut candidate, "2026-07-01T00:00:00Z");
        let merged = environment::merge(&old, &candidate).unwrap();
        assert_eq!(field(&merged), field(&candidate));
        assert!(field(&merged)["refresh_status"].is_null());
    }
}
