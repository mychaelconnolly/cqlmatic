use cqlmatic::{
    app,
    contracts::{CheckRequest, Cloud, Context, EnvironmentId, Outcome, Scope},
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

fn source_pack(end: &str, retrieved: &str, created: &str, observed: bool) -> Pack {
    let mut pack = pack(end, retrieved, created, true, None);
    pack.data.fields.clear();
    pack.data.sources[0]["evidence"]
        .as_object_mut()
        .unwrap()
        .remove("measured_fields");
    pack.data.sources[0]["evidence"]["observed"] = json!(observed);
    pack.content_digest = pack.digest().unwrap();
    pack.validate().unwrap();
    pack
}

#[test]
fn older_or_tied_opposite_source_measurements_fail_in_both_directions() {
    for (previous, incoming) in [(true, false), (false, true)] {
        let old = source_pack(
            "2026-01-03T00:00:00Z",
            "2026-01-04T00:00:00Z",
            "2026-01-04T00:00:00Z",
            previous,
        );
        let before = serde_json::to_vec(&old).unwrap();
        for (end, retrieved) in [
            ("2026-01-02T00:00:00Z", "2026-01-05T00:00:00Z"),
            ("2026-01-03T00:00:00Z", "2026-01-04T00:00:00Z"),
        ] {
            let candidate = source_pack(end, retrieved, "2026-01-06T00:00:00Z", incoming);
            let error = environment::merge(&old, &candidate).unwrap_err();
            assert!(
                error
                    .message
                    .starts_with("Source measurement ordering conflict")
            );
            assert_eq!(serde_json::to_vec(&old).unwrap(), before);
        }
    }
}

fn pack_with_user_sample() -> Pack {
    let mut old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-01-03T00:00:00Z",
        true,
        None,
    );
    old.data.fields[0]["id"] = json!("env:field:slot");
    old.data.fields[0]["path"] = json!("user");
    old.data.sources[0]["evidence"]["measured_fields"] = json!(["user"]);
    old.data.values.push(json!({
        "id":"env:value:user:alice","field_id":"env:field:slot","value":"alice",
        "value_kind":"sample","vocabulary":"sampled","evidence_kind":"observed",
        "provenance_ids":[old.data.fields[0]["provenance_ids"][0]]
    }));
    old.content_digest = old.digest().unwrap();
    old.validate().unwrap();
    old
}

#[test]
fn a_reused_field_id_cannot_rebind_a_retained_sample() {
    let old = pack_with_user_sample();
    let before = serde_json::to_vec(&old).unwrap();
    for (path, tag, different_source) in [
        ("host", false, false),
        ("user", true, false),
        ("user", false, true),
    ] {
        let mut candidate = pack(
            "2026-01-04T00:00:00Z",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
            true,
            None,
        );
        candidate.data.fields[0]["id"] = json!("env:field:slot");
        candidate.data.fields[0]["path"] = json!(path);
        candidate.data.fields[0]["is_tag"] = json!(tag);
        candidate.data.sources[0]["evidence"]["measured_fields"] =
            json!([if tag { format!("#{path}") } else { path.into() }]);
        if different_source {
            candidate.data.sources[0]["id"] = json!("env:repo:other");
            candidate.data.sources[0]["selectors"]["repo"] = json!("other");
            candidate.data.fields[0]["source_id"] = json!("env:repo:other");
        }
        candidate.content_digest = candidate.digest().unwrap();
        candidate.validate().unwrap();
        let candidate_before = serde_json::to_vec(&candidate).unwrap();
        let error = environment::merge(&old, &candidate).unwrap_err();
        assert!(error.message.contains("field ID"));
        assert_eq!(serde_json::to_vec(&old).unwrap(), before);
        assert_eq!(serde_json::to_vec(&candidate).unwrap(), candidate_before);
    }
}

#[test]
fn changed_ids_for_the_same_semantic_field_preserve_and_remap_samples() {
    let old = pack_with_user_sample();
    let mut candidate = pack(
        "2026-01-04T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "2026-01-06T00:00:00Z",
        true,
        None,
    );
    candidate.data.sources[0]["id"] = json!("env:repo:fresh");
    candidate.data.sources[0]["evidence"]["measured_fields"] = json!(["user"]);
    candidate.data.fields[0]["id"] = json!("env:field:fresh");
    candidate.data.fields[0]["source_id"] = json!("env:repo:fresh");
    candidate.data.fields[0]["path"] = json!("user");
    let mut sample = old.data.values[0].clone();
    sample["id"] = json!("env:value:user:bob");
    sample["field_id"] = json!("env:field:fresh");
    sample["value"] = json!("bob");
    sample["provenance_ids"] = candidate.data.fields[0]["provenance_ids"].clone();
    candidate.data.values.push(sample);
    candidate.content_digest = candidate.digest().unwrap();
    candidate.validate().unwrap();
    let merged = environment::merge(&old, &candidate).unwrap();
    merged.validate().unwrap();
    assert_eq!(merged.data.fields.len(), 1);
    assert_eq!(field(&merged)["id"], "env:field:slot");
    assert_eq!(field(&merged)["path"], "user");
    assert_eq!(field(&merged)["source_id"], "env:repo:events");
    assert_eq!(merged.data.values[0], old.data.values[0]);
    assert_eq!(merged.data.values[1]["field_id"], "env:field:slot");
    assert_eq!(merged.data.values[1]["value"], "bob");
}

#[test]
fn older_or_tied_consistent_source_measurements_preserve_current_proof() {
    for observed in [true, false] {
        let old = source_pack(
            "2026-01-03T00:00:00Z",
            "2026-01-04T00:00:00Z",
            "2026-01-04T00:00:00Z",
            observed,
        );
        for (end, retrieved) in [
            ("2026-01-02T00:00:00Z", "2026-01-05T00:00:00Z"),
            ("2026-01-03T00:00:00Z", "2026-01-04T00:00:00Z"),
        ] {
            let candidate = source_pack(end, retrieved, "2026-01-06T00:00:00Z", observed);
            let merged = environment::merge(&old, &candidate).unwrap();
            assert_eq!(merged.data.sources[0], old.data.sources[0]);
            let history = &candidate.data.provenance[1];
            assert!(merged.data.provenance.contains(history));
            if end != "2026-01-03T00:00:00Z" {
                assert!(
                    !merged.data.sources[0]["provenance_ids"]
                        .as_array()
                        .unwrap()
                        .contains(&history["id"])
                );
            }
        }
    }
}

#[test]
fn newer_opposite_source_measurements_replace_current_proof_in_both_directions() {
    for (previous, incoming) in [(true, false), (false, true)] {
        let old = source_pack(
            "2026-01-02T00:00:00Z",
            "2026-01-03T00:00:00Z",
            "2026-06-01T00:00:00Z",
            previous,
        );
        let candidate = source_pack(
            "2026-01-04T00:00:00Z",
            "2026-07-02T00:00:00Z",
            "2026-07-03T00:00:00Z",
            incoming,
        );
        let merged = environment::merge(&old, &candidate).unwrap();
        assert_eq!(merged.data.sources[0], candidate.data.sources[0]);
        assert!(merged.data.provenance.contains(&old.data.provenance[1]));
        assert!(
            !merged.data.sources[0]["provenance_ids"]
                .as_array()
                .unwrap()
                .contains(&old.data.provenance[1]["id"])
        );
    }
}

#[test]
fn opposite_unorderable_source_decisions_fail_in_both_directions() {
    for (previous, incoming) in [(true, false), (false, true)] {
        for missing_window in ["retained", "incoming", "both"] {
            let mut old = source_pack(
                "2026-01-02T00:00:00Z",
                "2026-01-03T00:00:00Z",
                "2026-06-01T00:00:00Z",
                previous,
            );
            let mut candidate = source_pack(
                "2026-03-02T00:00:00Z",
                "2026-07-02T00:00:00Z",
                "2026-07-03T00:00:00Z",
                incoming,
            );
            if missing_window != "incoming" {
                old.data.sources[0]["evidence"]["observation_window"] = Value::Null;
                old.content_digest = old.digest().unwrap();
            }
            if missing_window != "retained" {
                candidate.data.sources[0]["evidence"]["observation_window"] = Value::Null;
                candidate.content_digest = candidate.digest().unwrap();
            }
            old.validate().unwrap();
            candidate.validate().unwrap();
            assert!(
                environment::merge(&old, &candidate)
                    .unwrap_err()
                    .message
                    .starts_with("Source measurement ordering conflict")
            );
        }
    }
}

#[test]
fn an_unqualified_source_refresh_preserves_current_positive_proof() {
    let old = source_pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-01-03T00:00:00Z",
        true,
    );
    for observed in [json!(true), Value::Null] {
        let mut candidate = source_pack(
            "2026-01-04T00:00:00Z",
            "2026-01-05T00:00:00Z",
            "2026-01-06T00:00:00Z",
            true,
        );
        candidate.data.sources[0]["evidence"]["observed"] = observed;
        candidate.data.provenance[1]["coverage_status"] = json!("partial");
        candidate.content_digest = candidate.digest().unwrap();
        candidate.validate().unwrap();
        let merged = environment::merge(&old, &candidate).unwrap();
        assert_eq!(merged.data.sources[0], old.data.sources[0]);
        assert!(
            merged
                .data
                .provenance
                .contains(&candidate.data.provenance[1])
        );
    }
}

#[test]
fn an_older_source_refresh_cannot_demote_an_omitted_newer_field() {
    let old = pack(
        "2026-01-03T00:00:00Z",
        "2026-01-04T00:00:00Z",
        "2026-01-04T00:00:00Z",
        true,
        None,
    );
    let mut candidate = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "2026-01-06T00:00:00Z",
        true,
        None,
    );
    candidate.data.fields.clear();
    candidate.data.sources[0]["evidence"]
        .as_object_mut()
        .unwrap()
        .remove("measured_fields");
    candidate.content_digest = candidate.digest().unwrap();
    candidate.validate().unwrap();
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(merged.data.sources[0], old.data.sources[0]);
    assert_eq!(field(&merged), field(&old));
}

#[test]
fn a_retained_source_measurement_does_not_block_a_newer_explicit_field_measurement() {
    let old = pack(
        "2026-01-03T00:00:00Z",
        "2026-01-04T00:00:00Z",
        "2026-01-04T00:00:00Z",
        true,
        Some(0.5),
    );
    let mut candidate = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-05T00:00:00Z",
        "2026-01-06T00:00:00Z",
        true,
        Some(0.7),
    );
    let window = "2026-01-01T00:00:00Z/2026-01-04T00:00:00Z";
    candidate.data.fields[0]["observation_window"] = json!(window);
    candidate.data.provenance[0]["observation_window"] = json!(window);
    candidate.content_digest = candidate.digest().unwrap();
    candidate.validate().unwrap();
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(merged.data.sources[0], old.data.sources[0]);
    assert_eq!(field(&merged), field(&candidate));
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

fn undated_source_negative() -> Pack {
    let mut old = pack(
        "2026-01-02T00:00:00Z",
        "2026-01-03T00:00:00Z",
        "2026-06-01T00:00:00Z",
        false,
        None,
    );
    old.data.sources[0]["evidence"] = json!({"observed":false});
    old.content_digest = old.digest().unwrap();
    old
}

#[test]
fn an_undated_source_negative_requires_qualified_post_snapshot_measurement() {
    let old = undated_source_negative();
    let before = serde_json::to_vec(&old).unwrap();
    for (start, end, proof) in [
        ("2026-03-01T00:00:00Z", "2026-03-02T00:00:00Z", "qualified"),
        ("2026-06-01T00:00:00Z", "2026-07-02T00:00:00Z", "qualified"),
        ("2026-07-01T00:00:00Z", "2026-07-02T00:00:00Z", "qualified"),
        ("2026-07-01T00:00:00Z", "2026-07-02T00:00:00Z", "partial"),
        ("2026-07-01T00:00:00Z", "2026-07-02T00:00:00Z", "mismatched"),
    ] {
        let mut candidate = pack(
            end,
            "2026-07-02T00:00:00Z",
            "2026-07-03T00:00:00Z",
            true,
            None,
        );
        set_start(&mut candidate, start);
        if proof != "qualified" {
            candidate.data.sources[0]["evidence"]
                .as_object_mut()
                .unwrap()
                .remove("measured_fields");
            if proof == "partial" {
                candidate.data.provenance[1]["coverage_status"] = json!("partial");
            } else {
                candidate.data.provenance[1]["observation_window"] =
                    json!("2026-01-01T00:00:00Z/2026-01-02T00:00:00Z");
            }
            candidate.content_digest = candidate.digest().unwrap();
        }
        old.validate().unwrap();
        candidate.validate().unwrap();
        if start == "2026-07-01T00:00:00Z" && proof == "qualified" {
            let merged = environment::merge(&old, &candidate).unwrap();
            merged.validate().unwrap();
            assert_eq!(merged.data.sources[0], candidate.data.sources[0]);
            assert_eq!(field(&merged), field(&candidate));
            let result = app::check(
                CheckRequest {
                    schema_version: 1,
                    id: None,
                    query: "#repo=events | key=* | count()".into(),
                    context: Context {
                        require_observed: true,
                        ..Context::default()
                    },
                },
                Some(&merged),
            );
            assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.grounding);
        } else {
            let error = environment::merge(&old, &candidate).unwrap_err();
            assert!(
                error
                    .message
                    .starts_with("Source measurement ordering conflict"),
                "{start} {proof}: {}",
                error.message
            );
        }
        assert_eq!(serde_json::to_vec(&old).unwrap(), before);
    }
}

#[test]
fn an_unknown_source_observation_cannot_erase_a_negative() {
    for undated in [false, true] {
        let mut old = undated_source_negative();
        if !undated {
            old.data.sources[0]["evidence"]["observation_window"] =
                json!("2026-01-01T00:00:00Z/2026-01-02T00:00:00Z");
            old.content_digest = old.digest().unwrap();
        }
        let before = serde_json::to_vec(&old).unwrap();
        for explicit_null in [false, true] {
            let mut candidate = pack(
                "2026-03-02T00:00:00Z",
                "2026-07-02T00:00:00Z",
                "2026-07-03T00:00:00Z",
                true,
                None,
            );
            set_start(&mut candidate, "2026-03-01T00:00:00Z");
            let evidence = candidate.data.sources[0]["evidence"]
                .as_object_mut()
                .unwrap();
            evidence.remove("measured_fields");
            if explicit_null {
                evidence.insert("observed".into(), Value::Null);
            } else {
                evidence.remove("observed");
            }
            candidate.content_digest = candidate.digest().unwrap();
            old.validate().unwrap();
            candidate.validate().unwrap();
            let error = match environment::merge(&old, &candidate) {
                Err(error) => error,
                Ok(merged) => {
                    let result = app::check(
                        CheckRequest {
                            schema_version: 1,
                            id: None,
                            query: "#repo=events | key=* | count()".into(),
                            context: Context {
                                require_observed: true,
                                ..Context::default()
                            },
                        },
                        Some(&merged),
                    );
                    panic!(
                        "unknown source observation erased negative evidence (undated={undated}, null={explicit_null}); strict grounding returned {:?}",
                        result.outcome
                    );
                }
            };
            assert!(
                error
                    .message
                    .starts_with("Source measurement ordering conflict"),
                "{}",
                error.message
            );
            assert_eq!(serde_json::to_vec(&old).unwrap(), before);
            let result = app::check(
                CheckRequest {
                    schema_version: 1,
                    id: None,
                    query: "#repo=events | key=* | count()".into(),
                    context: Context {
                        require_observed: true,
                        ..Context::default()
                    },
                },
                Some(&old),
            );
            assert_eq!(result.outcome, Outcome::NeedsContext);
        }
    }
}

#[test]
fn a_backdated_consistent_zero_preserves_an_undated_source_negative() {
    let old = undated_source_negative();
    let mut candidate = pack(
        "2026-03-02T00:00:00Z",
        "2026-07-02T00:00:00Z",
        "2026-07-03T00:00:00Z",
        false,
        None,
    );
    set_start(&mut candidate, "2026-03-01T00:00:00Z");
    candidate.data.sources[0]["evidence"]["observed"] = json!(false);
    candidate.data.sources[0]["evidence"]
        .as_object_mut()
        .unwrap()
        .remove("measured_fields");
    candidate.content_digest = candidate.digest().unwrap();
    old.validate().unwrap();
    candidate.validate().unwrap();
    let merged = environment::merge(&old, &candidate).unwrap();
    assert_eq!(merged.data.sources[0], old.data.sources[0]);
    let retained_proof = &old.data.provenance[1];
    assert!(merged.data.provenance.contains(retained_proof));
    assert!(
        merged
            .data
            .provenance
            .contains(&candidate.data.provenance[1])
    );
    assert!(
        !merged.data.sources[0]["provenance_ids"]
            .as_array()
            .unwrap()
            .contains(&candidate.data.provenance[1]["id"])
    );
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
