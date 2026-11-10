use cqlmatic::{
    app::{self, GroundingEvidence},
    contracts::*,
    discovery,
    environment::{Pack, PackData},
    inventory::{QueryObservation, QueryResultAssertion},
};
use serde_json::{Value, json};

fn environment() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "search-all".into(),
    }
}

fn source() -> Scope {
    Scope {
        repo: Some("synthetic".into()),
        ..Scope::default()
    }
}

fn observation() -> QueryObservation {
    observation_in_window(
        "2026-02-01T00:00:00Z",
        "2026-02-02T00:00:00Z",
        "2026-02-03T00:00:00Z",
    )
}

fn observation_in_window(start: &str, end: &str, retrieved_at: &str) -> QueryObservation {
    QueryObservation::from_result(
        br#"#repo="synthetic" | select([user]) | head(5)"#,
        br#"{"result":[{"user":"alice"}]}"#,
        environment(),
        source(),
        start.into(),
        end.into(),
        retrieved_at.into(),
        QueryResultAssertion::ExactQueryResultTargetWindow,
        std::env::temp_dir()
            .join("synthetic-query-observation-audit.json")
            .to_string_lossy()
            .into_owned(),
    )
    .unwrap()
}

fn data() -> PackData {
    let source_id = discovery::source_id(&source());
    PackData {
        sources: vec![json!({
            "id":source_id,"kind":"repo","name":"synthetic","inventory_status":"observed-only",
            "selectors":{"repo":"synthetic"},"evidence":{"observed":true},
            "provenance_ids":["prov:measurement"]
        })],
        fields: vec![json!({
            "id":"env:field:synthetic:user","source_id":source_id,"path":"user","data_type":"string",
            "is_tag":false,"evidence_kind":"observed","coverage":{"observed":true},
            "provenance_ids":["prov:measurement"]
        })],
        provenance: vec![json!({
            "id":"prov:measurement","source_type":"ngsiem-api","canonical_source":"Synthetic measurement",
            "retrieved_at":"2026-01-09T00:00:00Z","redaction_status":"none","coverage_status":"complete",
            "observation_window":"2026-01-01T00:00:00Z/2026-01-08T00:00:00Z"
        })],
        ..PackData::default()
    }
}

fn pack(data: PackData, created_at: &str) -> Pack {
    Pack::new(environment(), created_at.into(), data).unwrap()
}

fn checked(
    query: &str,
    explicit: Option<&Pack>,
    active: Option<&Pack>,
    observations: &[QueryObservation],
) -> Analysis {
    app::check_with_evidence(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context {
                require_observed: true,
                ..Context::default()
            },
        },
        explicit,
        Some(GroundingEvidence {
            environment: &environment(),
            active_pack: active,
            query_observations: observations,
        }),
    )
}

#[test]
fn exact_direct_field_qualifies_without_a_pack_and_supersedes_an_ordered_older_measurement() {
    let observation = observation();
    let mut old = data();
    old.fields[0]["coverage"]["observed"] = json!(false);
    let old = pack(old, "2026-01-10T00:00:00Z");
    for explicit in [None, Some(&old)] {
        let result = checked(
            "#repo=synthetic | user=* | count()",
            explicit,
            None,
            std::slice::from_ref(&observation),
        );
        assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.grounding);
        assert!(result.grounding.missing.is_empty());
        let basis = &result.grounding.basis["synthetic:*:user"];
        for detail in [
            "caller-attested query observation",
            "positive_rows=1",
            "retrieved_at=2026-02-03T00:00:00Z",
            "observation_window=2026-02-01T00:00:00Z/2026-02-02T00:00:00Z",
        ] {
            assert!(basis.contains(detail), "{basis}");
        }
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_QUERY_OBSERVATION" && d.severity == Severity::Warning)
        );
        assert!(result.grounding.policy.require_observed);
        assert_eq!(result.exit_code(false), 0);
        assert_eq!(result.exit_code(true), 1);
    }
    let request: GenerateRequest = serde_json::from_value(json!({
        "schema_version":1,"context":{"require_observed":true,"accept_partial_coverage":true},
        "input":{"mode":"plan","plan":{"source":{"repo":"synthetic"},"filters":[{"op":"exists","field":"user"}]}}
    })).unwrap();
    let result = app::generate_with_evidence(
        request,
        None,
        Some(GroundingEvidence {
            environment: &environment(),
            active_pack: None,
            query_observations: &[observation],
        }),
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.grounding);
    assert!(result.query.is_some());
    assert!(result.grounding.policy.accept_partial_coverage);
}

#[test]
fn observation_does_not_qualify_other_fields_sources_targets_or_shapes() {
    let observation = observation();
    for query in [
        "#repo=synthetic | other=*",
        "#repo=other | user=*",
        "#repo=synthetic #type=other | user=*",
        "(#repo=synthetic OR #repo=other) | user=*",
        "#repo=synthetic | readFile(\"events.csv\") | user=*",
        "#repo=synthetic | select([other]) | user=*",
        "#repo=synthetic | array:length(\"user[]\")",
    ] {
        let result = checked(query, None, None, std::slice::from_ref(&observation));
        assert_eq!(
            result.outcome,
            Outcome::NeedsContext,
            "{query}: {:?}",
            result.grounding
        );
    }
    let mut other_environment = environment();
    other_environment.view = "other-target".into();
    let result = app::check_with_evidence(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: "#repo=synthetic | user=*".into(),
            context: Context {
                environment: Some(other_environment),
                require_observed: true,
                ..Context::default()
            },
        },
        None,
        Some(GroundingEvidence {
            environment: &environment(),
            active_pack: None,
            query_observations: &[observation],
        }),
    );
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert!(!result.grounding.conflicting.is_empty());
}

#[test]
fn later_zero_stale_and_not_reobserved_active_measurements_block_even_with_an_older_pack() {
    let old = pack(data(), "2026-01-10T00:00:00Z");
    let observation = observation();
    for state in [
        "zero",
        "stale-source",
        "stale-provenance",
        "not-reobserved",
        "source-zero",
        "measured-absent",
    ] {
        let mut later = data();
        later.provenance[0]["retrieved_at"] = json!("2026-03-03T00:00:00Z");
        later.provenance[0]["observation_window"] =
            json!("2026-03-01T00:00:00Z/2026-03-02T00:00:00Z");
        match state {
            "zero" => later.fields[0]["coverage"]["observed"] = json!(false),
            "stale-source" => later.sources[0]["inventory_status"] = json!("stale"),
            "stale-provenance" => later.provenance[0]["coverage_status"] = json!("stale"),
            "not-reobserved" => later.fields[0]["refresh_status"] = json!("not-reobserved"),
            "source-zero" => later.sources[0]["evidence"]["observed"] = json!(false),
            "measured-absent" => {
                later.fields.clear();
                later.sources[0]["evidence"]["measured_fields"] = json!(["user"]);
                later.sources[0]["evidence"]["observation_window"] =
                    later.provenance[0]["observation_window"].clone();
            }
            _ => unreachable!(),
        }
        let active = pack(later, "2026-03-04T00:00:00Z");
        for explicit in [None, Some(&old)] {
            for observations in [std::slice::from_ref(&observation), &[]] {
                let result = checked(
                    "#repo=synthetic | user=*",
                    explicit,
                    Some(&active),
                    observations,
                );
                assert_eq!(
                    result.outcome,
                    Outcome::NeedsContext,
                    "{state}: {:?}",
                    result.grounding
                );
                assert!(
                    result
                        .grounding
                        .missing
                        .iter()
                        .any(|field| field == "synthetic:*:user")
                );
            }
        }
    }
}

#[test]
fn broader_active_scopes_block_narrower_observations_and_kind_ambiguity() {
    let narrow = Scope {
        repo: Some("synthetic".into()),
        type_: Some("kind".into()),
        ..Scope::default()
    };
    let observation = QueryObservation::from_result(
        br#"#repo="synthetic" #type="kind" | select([user]) | head(5)"#,
        br#"{"result":[{"user":"alice"}]}"#,
        environment(),
        narrow,
        "2026-02-01T00:00:00Z".into(),
        "2026-02-02T00:00:00Z".into(),
        "2026-02-03T00:00:00Z".into(),
        QueryResultAssertion::ExactQueryResultTargetWindow,
        std::env::temp_dir()
            .join("synthetic-narrow-audit.json")
            .to_string_lossy()
            .into_owned(),
    )
    .unwrap();
    let active = |scope: Scope, zero: bool, mixed: bool| {
        let mut d = data();
        let id = discovery::source_id(&scope);
        d.sources[0]["id"] = json!(id);
        d.sources[0]["selectors"] = serde_json::to_value(&scope).unwrap();
        d.fields[0]["source_id"] = json!(id);
        d.provenance[0]["retrieved_at"] = json!("2026-03-03T00:00:00Z");
        d.provenance[0]["observation_window"] = json!("2026-03-01T00:00:00Z/2026-03-02T00:00:00Z");
        if zero {
            d.fields[0]["coverage"]["observed"] = json!(false);
        }
        if mixed {
            d.sources[0]["relationship"] = json!({"datasets":["first","second"]});
        }
        pack(d, "2026-03-04T00:00:00Z")
    };
    for broad in [
        Scope {
            repo: Some("synthetic".into()),
            ..Scope::default()
        },
        Scope {
            type_: Some("kind".into()),
            ..Scope::default()
        },
    ] {
        let zero = active(broad.clone(), true, false);
        let result = checked(
            "#repo=synthetic #type=kind | user=*",
            None,
            Some(&zero),
            std::slice::from_ref(&observation),
        );
        assert_eq!(
            result.outcome,
            Outcome::NeedsContext,
            "{broad:?}: {:?}",
            result.grounding
        );
        let mixed = active(broad.clone(), false, true);
        let result = checked(
            "#repo=synthetic #type=kind | top(user)",
            None,
            Some(&mixed),
            std::slice::from_ref(&observation),
        );
        assert_eq!(
            result.outcome,
            Outcome::NeedsContext,
            "{broad:?}: {:?}",
            result.grounding
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_RECORD_KIND_AMBIGUOUS")
        );
    }
    for unrelated in [
        Scope {
            repo: Some("other".into()),
            type_: Some("kind".into()),
            ..Scope::default()
        },
        Scope {
            repo: Some("synthetic".into()),
            type_: Some("other".into()),
            ..Scope::default()
        },
    ] {
        let zero = active(unrelated.clone(), true, false);
        let result = checked(
            "#repo=synthetic #type=kind | user=*",
            None,
            Some(&zero),
            std::slice::from_ref(&observation),
        );
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{unrelated:?}: {:?}",
            result.grounding
        );
        let mixed = active(unrelated.clone(), false, true);
        let result = checked(
            "#repo=synthetic #type=kind | top(user)",
            None,
            Some(&mixed),
            std::slice::from_ref(&observation),
        );
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{unrelated:?}: {:?}",
            result.grounding
        );
    }
}

#[test]
fn repackaging_and_unrelated_source_captures_do_not_refresh_a_field_negative() {
    let observation = observation();
    let mut d = data();
    d.fields[0]["coverage"]["observed"] = json!(false);
    d.fields[0]["observation_window"] = json!("2026-01-01T00:00:00Z/2026-01-08T00:00:00Z");
    let mut later_source = d.provenance[0].clone();
    later_source["id"] = json!("prov:later-source");
    later_source["retrieved_at"] = json!("2026-03-05T00:00:00Z");
    later_source["observation_window"] = json!("2026-03-01T00:00:00Z/2026-03-04T00:00:00Z");
    d.provenance.push(later_source);
    d.sources[0]["provenance_ids"] = json!(["prov:later-source"]);
    d.sources[0]["evidence"]["observation_window"] =
        json!("2026-03-01T00:00:00Z/2026-03-04T00:00:00Z");
    let active = pack(d, "2026-03-06T00:00:00Z");
    let result = checked(
        "#repo=synthetic | user=*",
        None,
        Some(&active),
        &[observation],
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.grounding);
    assert!(
        result.grounding.basis["synthetic:*:user"]
            .starts_with("caller-attested query observation;")
    );
}

#[test]
fn undated_stale_markers_require_an_observation_window_after_the_snapshot() {
    for marker in ["source", "field", "provenance"] {
        let mut d = data();
        match marker {
            "source" => d.sources[0]["inventory_status"] = json!("stale"),
            "field" => d.fields[0]["refresh_status"] = json!("not-reobserved"),
            _ => d.provenance[0]["coverage_status"] = json!("stale"),
        }
        let active = pack(d, "2026-03-06T00:00:00Z");
        for (observed, expected) in [
            (observation(), Outcome::NeedsContext),
            (
                observation_in_window(
                    "2026-04-01T00:00:00Z",
                    "2026-04-02T00:00:00Z",
                    "2026-04-03T00:00:00Z",
                ),
                Outcome::Ready,
            ),
        ] {
            let result = checked("#repo=synthetic | user=*", None, Some(&active), &[observed]);
            assert_eq!(result.outcome, expected, "{marker}: {:?}", result.grounding);
        }
    }
}

#[test]
fn a_negative_with_qualified_and_stale_proof_keeps_the_snapshot_bound() {
    for target in ["field", "source"] {
        let mut d = data();
        let mut stale = d.provenance[0].clone();
        stale["id"] = json!("prov:stale");
        stale["coverage_status"] = json!("stale");
        d.provenance.push(stale);
        if target == "field" {
            d.fields[0]["coverage"]["observed"] = json!(false);
            d.fields[0]["provenance_ids"] = json!(["prov:measurement", "prov:stale"]);
        } else {
            d.sources[0]["evidence"]["observed"] = json!(false);
            d.sources[0]["provenance_ids"] = json!(["prov:measurement", "prov:stale"]);
        }
        let active = pack(d.clone(), "2026-06-01T00:00:00Z");
        for (observed, expected) in [
            (observation(), Outcome::NeedsContext),
            (
                observation_in_window(
                    "2026-07-01T00:00:00Z",
                    "2026-07-02T00:00:00Z",
                    "2026-07-03T00:00:00Z",
                ),
                Outcome::Ready,
            ),
        ] {
            let result = checked("#repo=synthetic | user=*", None, Some(&active), &[observed]);
            assert_eq!(result.outcome, expected, "{target}: {:?}", result.grounding);
        }
        d.fields[0]["coverage"]["observed"] = json!(true);
        d.sources[0]["evidence"]["observed"] = json!(true);
        let positive = pack(d, "2026-06-01T00:00:00Z");
        let result = checked("#repo=synthetic | user=*", Some(&positive), None, &[]);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{target}: {:?}",
            result.grounding
        );
    }
}

#[test]
fn a_listed_unmeasured_namespace_does_not_contradict_a_saved_observation() {
    let observation = observation();
    let mut d = data();
    d.fields[0]["evidence_kind"] = json!("documented");
    d.fields[0]["coverage"]["observed"] = Value::Null;
    let active = pack(d, "2026-03-06T00:00:00Z");
    let result = checked(
        "#repo=synthetic | user=*",
        None,
        Some(&active),
        &[observation],
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.grounding);
}

#[test]
fn unordered_or_overlapping_measurements_cannot_be_revived() {
    let observation = observation();
    for (created_at, retrieved_at, window) in [
        (
            "2026-01-10T00:00:00Z",
            "2026-01-09",
            "2026-01-01T00:00:00Z/2026-01-08T00:00:00Z",
        ),
        (
            "2026-01-10T00:00:00Z",
            "2026-02-04T00:00:00Z",
            "2026-01-01T00:00:00Z/2026-01-08T00:00:00Z",
        ),
        (
            "2026-01-10T00:00:00Z",
            "2026-01-09T00:00:00Z",
            "2026-01-01T00:00:00Z/2026-02-02T00:00:00Z",
        ),
        (
            "2026-03-06T00:00:00Z",
            "2026-02-03T00:00:00Z",
            "2026-01-01T00:00:00Z/2026-01-08T00:00:00Z",
        ),
    ] {
        let mut d = data();
        d.fields[0]["coverage"]["observed"] = json!(false);
        d.provenance[0]["retrieved_at"] = json!(retrieved_at);
        d.provenance[0]["observation_window"] = json!(window);
        let active = pack(d, created_at);
        let result = checked(
            "#repo=synthetic | user=*",
            None,
            Some(&active),
            std::slice::from_ref(&observation),
        );
        assert_eq!(
            result.outcome,
            Outcome::NeedsContext,
            "{:?}",
            result.grounding
        );
        assert!(result.grounding.basis["synthetic:*:user"].contains("cannot be ordered"));
    }
}

#[test]
fn record_kind_guard_unions_active_and_explicit_evidence_in_both_directions() {
    let with_kinds = |kinds: Value, time| {
        let mut d = data();
        d.sources[0]["relationship"] = json!({"datasets":kinds});
        pack(d, time)
    };
    let first = with_kinds(json!(["first"]), "2026-01-10T00:00:00Z");
    let second = with_kinds(json!(["second"]), "2026-03-04T00:00:00Z");
    let mixed = with_kinds(json!(["first", "second"]), "2026-03-04T00:00:00Z");
    let observation = observation();
    for (explicit, active) in [
        (None, &mixed),
        (Some(&first), &mixed),
        (Some(&mixed), &first),
        (Some(&first), &second),
        (Some(&second), &first),
    ] {
        let result = checked(
            "#repo=synthetic | top(user)",
            explicit,
            Some(active),
            std::slice::from_ref(&observation),
        );
        assert_eq!(
            result.outcome,
            Outcome::NeedsContext,
            "{:?}",
            result.grounding
        );
        let diagnostic = result
            .diagnostics
            .iter()
            .find(|d| d.code == "CQL_RECORD_KIND_AMBIGUOUS")
            .unwrap();
        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(diagnostic.message.contains("first, second"));
    }
    let result = app::check_with_evidence(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: "#repo=synthetic | top(user)".into(),
            context: Context {
                require_observed: true,
                accept_mixed_record_kinds: true,
                ..Context::default()
            },
        },
        None,
        Some(GroundingEvidence {
            environment: &environment(),
            active_pack: Some(&mixed),
            query_observations: &[observation],
        }),
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.grounding);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_RECORD_KIND_AMBIGUOUS" && d.severity == Severity::Warning)
    );
}

#[test]
fn active_source_ambiguity_and_invalid_observations_fail_closed() {
    let mut d = data();
    let mut other_source = d.sources[0].clone();
    other_source["id"] = json!("env:repo:other-type");
    other_source["selectors"]["type"] = json!("other-type");
    d.sources.push(other_source);
    let mut other_field = d.fields[0].clone();
    other_field["id"] = json!("env:field:other-type:user");
    other_field["source_id"] = json!("env:repo:other-type");
    d.fields.push(other_field);
    let active = pack(d, "2026-01-10T00:00:00Z");
    let observation = observation();
    let result = checked(
        "#repo=synthetic | user=*",
        None,
        Some(&active),
        std::slice::from_ref(&observation),
    );
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert!(result.grounding.basis["synthetic:*:user"].contains("not interchangeable"));
    let mut invalid = observation;
    invalid.fields[0].positive_rows = 0;
    let result = checked("#repo=synthetic | user=*", None, None, &[invalid]);
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert!(!result.grounding.conflicting.is_empty());
}

#[test]
fn predicted_leads_include_only_scoped_input_fields_and_are_bounded() {
    let leads = app::unverified_leads("#repo=synthetic | status=ok | label:=\"literal-value\" | lower(user,as=normalized) | normalized=alice | select([user,label])").unwrap();
    assert_eq!(
        leads
            .iter()
            .map(|lead| lead.field.as_str())
            .collect::<Vec<_>>(),
        ["status", "user"]
    );
    assert!(leads.iter().all(|lead| lead.source == source()));
    let leads =
        app::unverified_leads("#repo=synthetic | regex(\"(?<derived>.+)\",field=user) | derived=*")
            .unwrap();
    assert_eq!(
        leads
            .iter()
            .map(|lead| lead.field.as_str())
            .collect::<Vec<_>>(),
        ["user"]
    );
    for query in [
        "#repo=synthetic | readFile(\"events.csv\") | user=*",
        "(#repo=synthetic OR #repo=other) | user=*",
        "user=alice",
        "count()",
    ] {
        assert!(app::unverified_leads(query).unwrap().is_empty(), "{query}");
    }
    assert!(app::unverified_leads("unknownFunction()").is_err());
    let fields = (0..1001)
        .map(|index| format!("field{index}"))
        .collect::<Vec<_>>()
        .join(",");
    let error =
        app::unverified_leads(&format!("#repo=synthetic | select([{fields}])")).unwrap_err();
    assert_eq!(error.code, "CQL_LEAD_BUDGET");
}
