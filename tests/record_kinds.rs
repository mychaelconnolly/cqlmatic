use cqlmatic::{
    app,
    contracts::*,
    environment::{self, Pack, PackData},
};
use serde_json::{Value, json};

fn pack(relationship: Option<Value>, created_at: &str) -> Pack {
    let mut source = json!({
        "id": "env:repo:fw_repo",
        "kind": "repo",
        "name": "fw_repo",
        "inventory_status": "observed-only",
        "selectors": {"repo": "fw_repo"},
        "provenance_ids": ["prov:synthetic"]
    });
    if let Some(relationship) = relationship {
        source["relationship"] = relationship;
    }
    Pack::new(
        EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "search-all".into(),
        },
        created_at.into(),
        PackData {
            sources: vec![source],
            provenance: vec![json!({
                "id": "prov:synthetic",
                "source_type": "ngsiem-api",
                "canonical_source": "synthetic record-kind test",
                "retrieved_at": "2000-01-01T00:00:00Z",
                "redaction_status": "none",
                "coverage_status": "complete"
            })],
            ..PackData::default()
        },
    )
    .unwrap()
}

fn datasets(values: &[&str]) -> Pack {
    pack(Some(json!({"datasets": values})), "2026-01-01T00:00:00Z")
}

fn named_kinds(field: &str, values: &[&str], truncated: bool) -> Pack {
    pack(
        Some(json!({"record_kinds": [
            {"field":field,"values":values,"truncated":truncated}
        ]})),
        "2026-01-01T00:00:00Z",
    )
}

fn check(query: &str, pack: Option<&Pack>, accept_mixed_record_kinds: bool) -> Analysis {
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context {
                accept_mixed_record_kinds,
                ..Context::default()
            },
        },
        pack,
    )
}

fn ambiguity(result: &Analysis) -> &Diagnostic {
    result
        .diagnostics
        .iter()
        .find(|diagnostic| {
            matches!(
                diagnostic.code.as_str(),
                "CQL_RECORD_KIND_AMBIGUOUS" | "CQL_RECORD_KIND_NOTICE"
            )
        })
        .expect("record-kind diagnostic")
}

fn assert_no_ambiguity(result: &Analysis) {
    assert!(
        result.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic.code.as_str(),
            "CQL_RECORD_KIND_AMBIGUOUS" | "CQL_RECORD_KIND_NOTICE"
        )),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn multi_dataset_top_proceeds_with_a_notice_without_strict_observation() {
    let pack = datasets(&["traffic", "event"]);
    let result = check("#repo=fw_repo | top(dst)", Some(&pack), false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(result.validation.status, Acceptance::Accepted);
    let evidence = &result.grounding.basis["env:repo:fw_repo:@record-kind"];
    assert!(
        evidence.contains("#event.dataset: event, traffic"),
        "{evidence}"
    );
    assert!(evidence.contains("filter or group"), "{evidence}");
    assert!(result.grounding.missing.is_empty());
    assert_eq!(ambiguity(&result).severity, Severity::Info);
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.code != "CQL_CONTEXT_REQUIRED")
    );
}

#[test]
fn positive_leading_dataset_filter_satisfies_record_kind_requirement() {
    let pack = datasets(&["traffic", "event"]);
    let result = check(
        "#repo=fw_repo | #event.dataset=traffic | top(dst)",
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert!(result.grounding.missing.is_empty());
    assert_no_ambiguity(&result);
}

#[test]
fn exact_event_selector_explains_dataset_ambiguity() {
    let pack = datasets(&["traffic", "event"]);
    let result = check(
        "#repo=fw_repo #event_simpleName=FirewallEvent | top(dst)",
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    let message = &ambiguity(&result).message;
    assert!(message.contains("#event_simpleName selector does not establish #event.dataset"));
    assert!(message.contains("exact #event.dataset filter"));
    assert!(message.contains("group by #event.dataset"));
    assert!(message.contains("this query can include these kinds"));
}

#[test]
fn groupby_dataset_key_satisfies_record_kind_requirement() {
    let pack = datasets(&["traffic", "event"]);
    let result = check(
        "#repo=fw_repo | groupBy([#event.dataset, dst])",
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_no_ambiguity(&result);
}

#[test]
fn normal_explicit_acceptance_retains_a_calm_notice() {
    let pack = datasets(&["traffic", "event"]);
    let result = check("#repo=fw_repo | top(dst)", Some(&pack), true);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert!(result.grounding.missing.is_empty());
    assert_eq!(ambiguity(&result).severity, Severity::Info);
    assert!(
        ambiguity(&result)
            .message
            .contains("#event.dataset: event, traffic")
    );
}

#[test]
fn single_kind_unscoped_and_unmatched_source_do_not_warn() {
    let single = datasets(&["traffic"]);
    let result = check("#repo=fw_repo | top(dst)", Some(&single), false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_no_ambiguity(&result);

    let mixed = datasets(&["traffic", "event"]);
    for query in ["top(dst)", "#repo=other_repo | top(dst)"] {
        let result = check(query, Some(&mixed), false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_no_ambiguity(&result);
    }
}

#[test]
fn type_only_scope_matches_pack_and_no_pack_does_not_assert_ambiguity() {
    let mut typed = datasets(&["traffic", "event"]);
    typed.data.sources[0]["selectors"] = json!({"type": "firewall"});
    typed.content_digest = typed.digest().unwrap();
    typed.validate().unwrap();

    let result = check("#type=firewall | top(dst)", Some(&typed), false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);

    let no_pack = check("#type=firewall | top(dst)", None, false);
    assert_eq!(no_pack.outcome, Outcome::Ready, "{:?}", no_pack.diagnostics);
    assert_no_ambiguity(&no_pack);
}

#[test]
fn kind_unrelated_filter_before_source_selector_retains_notice() {
    let pack = datasets(&["traffic", "event"]);
    let result = check(
        r#"in(dst, values=["x"]) | #repo=fw_repo | top(dst)"#,
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);
}

#[test]
fn unrelated_field_basis_cannot_create_record_kind_diagnostic() {
    let pack = datasets(&["traffic"]);
    let result = check(
        r#"#repo=fw_repo | "@record-kind"=* | top(dst)"#,
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_no_ambiguity(&result);
}

#[test]
fn generation_returns_a_query_and_notice_for_mixed_record_kinds() {
    let pack = datasets(&["traffic", "event"]);
    let request = GenerateRequest {
        schema_version: 1,
        id: None,
        input: GenerateInput::Recipe {
            request: Recipe::TopValues {
                source: Scope {
                    repo: Some("fw_repo".into()),
                    ..Scope::default()
                },
                filters: vec![],
                field: "dst".into(),
                limit: 10,
            },
        },
        context: Context::default(),
    };
    let generated = app::generate(request.clone(), Some(&pack));
    assert_eq!(
        generated.outcome,
        Outcome::Ready,
        "{:?}",
        generated.diagnostics
    );
    assert!(generated.query.is_some());
    assert_eq!(ambiguity(&generated).severity, Severity::Info);

    let accepted = app::generate(
        GenerateRequest {
            context: Context {
                accept_mixed_record_kinds: true,
                ..Context::default()
            },
            ..request
        },
        Some(&pack),
    );
    assert_eq!(
        accepted.outcome,
        Outcome::Ready,
        "{:?}",
        accepted.diagnostics
    );
    assert!(accepted.query.is_some());
    assert_eq!(ambiguity(&accepted).severity, Severity::Info);
}

#[test]
fn explicit_record_kinds_field_triggers_ambiguity_and_groupby_resolves_it() {
    let pack = pack(
        Some(
            json!({"record_kinds": [{"field": "#type", "values": ["a", "b"], "truncated": false}]}),
        ),
        "2026-01-01T00:00:00Z",
    );
    let result = check("#repo=fw_repo | top(dst)", Some(&pack), false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);
    assert!(ambiguity(&result).message.contains("#type: a, b"));

    let grouped = check("#repo=fw_repo | groupBy([#type, dst])", Some(&pack), false);
    assert_eq!(grouped.outcome, Outcome::Ready, "{:?}", grouped.diagnostics);
    assert_no_ambiguity(&grouped);
}

#[test]
fn merge_unions_record_kind_values_per_field_and_preserves_truncation() {
    let old = pack(
        Some(json!({"record_kinds": [
            {"field": "#type", "values": ["a", "b"], "truncated": true},
            {"field": "kind", "values": ["old"], "truncated": false}
        ]})),
        "2026-01-01T00:00:00Z",
    );
    let fresh = pack(
        Some(json!({"record_kinds": [
            {"field": "#type", "values": ["b", "c"], "truncated": false},
            {"field": "other", "values": ["new"], "truncated": true}
        ]})),
        "2026-02-01T00:00:00Z",
    );
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(
        merged.data.sources[0]["relationship"]["record_kinds"],
        json!([
            {"field": "#type", "values": ["b", "c", "a"], "truncated": true},
            {"field": "other", "values": ["new"], "truncated": true},
            {"field": "kind", "values": ["old"], "truncated": false}
        ])
    );
    let result = check("#repo=fw_repo | top(dst)", Some(&merged), false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);
}

#[test]
fn merge_folds_duplicate_entries_for_one_field_on_both_sides() {
    let old = pack(
        Some(json!({"record_kinds": [
            {"field": "#type", "values": ["a"], "truncated": false},
            {"field": "#type", "values": ["b"], "truncated": true}
        ]})),
        "2026-01-01T00:00:00Z",
    );
    let fresh = pack(
        Some(json!({"record_kinds": [
            {"field": "#type", "values": ["c"], "truncated": false},
            {"field": "#type", "values": ["a", "d"], "truncated": false}
        ]})),
        "2026-02-01T00:00:00Z",
    );
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(
        merged.data.sources[0]["relationship"]["record_kinds"],
        json!([{"field": "#type", "values": ["c", "a", "d", "b"], "truncated": true}])
    );
}

#[test]
fn pack_json_without_record_kinds_still_loads() {
    let original = datasets(&["traffic"]);
    assert!(
        original.data.sources[0]["relationship"]
            .get("record_kinds")
            .is_none()
    );
    let decoded: Pack = serde_json::from_value(serde_json::to_value(&original).unwrap()).unwrap();
    decoded.validate().unwrap();
    let source = environment::source(&decoded.data.sources[0]).unwrap();
    assert!(source.relationship.unwrap().record_kinds.is_empty());
    let result = check("#repo=fw_repo | top(dst)", Some(&decoded), false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_no_ambiguity(&result);
}

#[test]
fn negated_disjunction_and_presence_do_not_constrain_record_kind() {
    let pack = datasets(&["traffic", "event"]);
    for query in [
        "#repo=fw_repo | !(#event.dataset=traffic) | top(dst)",
        "#repo=fw_repo | (#event.dataset=traffic OR dst=x) | top(dst)",
        "#repo=fw_repo | #event.dataset=* | top(dst)",
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(ambiguity(&result).severity, Severity::Info);
    }
}

#[test]
fn single_literal_in_and_early_match_or_case_separate_record_kinds() {
    let pack = datasets(&["traffic", "event"]);
    for query in [
        r#"#repo=fw_repo | in(#event.dataset, values=["traffic"]) | top(dst)"#,
        r#"#repo=fw_repo | #event.dataset match { traffic => top(dst); event => top(dst) }"#,
        r#"#repo=fw_repo | case { #event.dataset=traffic | top(dst); #event.dataset=event | top(dst) }"#,
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_no_ambiguity(&result);
    }
}

#[test]
fn catch_all_match_arm_does_not_separate_record_kinds() {
    let pack = datasets(&["traffic", "event"]);
    for query in [
        "#repo=fw_repo | #event.dataset match { * => top(dst) }",
        "#repo=fw_repo | #event.dataset match { traffic => top(dst); * => top(dst) }",
        "#repo=fw_repo | #event.dataset match { tra* => top(dst); event => top(dst) }",
        "#repo=fw_repo | #event.dataset match { /traffic/ => top(dst); event => top(dst) }",
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(result.validation.status, Acceptance::Accepted, "{query}");
        assert_eq!(ambiguity(&result).severity, Severity::Info, "{query}");
    }
}

#[test]
fn multi_value_in_does_not_constrain_a_single_record_kind() {
    let pack = datasets(&["traffic", "event"]);
    let result = check(
        r#"#repo=fw_repo | in(#event.dataset, values=["traffic", "event"]) | top(dst)"#,
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);
}

#[test]
fn regex_kind_filters_do_not_prove_a_single_kind() {
    let pack = datasets(&["traffic", "event"]);
    for query in [
        "#repo=fw_repo | #event.dataset=/.*/ | top(dst)",
        "#repo=fw_repo | #event.dataset=/traffic/ | top(dst)",
        r#"#repo=fw_repo | regex("^traffic$", field=#event.dataset) | top(dst)"#,
        "#repo=fw_repo | #event.dataset=tra* | top(dst)",
        "#repo=fw_repo | (#event.dataset=traffic OR #event.dataset=event) | top(dst)",
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(ambiguity(&result).severity, Severity::Info, "{query}");
    }
}

#[test]
fn named_regex_capture_before_kind_grouping_does_not_separate_kinds() {
    let pack = pack(
        Some(json!({"record_kinds": [
            {"field": "kind", "values": ["a", "b"], "truncated": false}
        ]})),
        "2026-01-01T00:00:00Z",
    );
    let result = check(
        r#"#repo=fw_repo | regex("(?<kind>.*)", field=src) | groupBy([kind, dst])"#,
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);
}

#[test]
fn strict_observation_does_not_make_overwritten_kind_a_record_kind() {
    let mut pack = pack(
        Some(json!({"record_kinds": [
            {"field": "kind", "values": ["a", "b"], "truncated": false}
        ]})),
        "2026-01-01T00:00:00Z",
    );
    pack.data.fields.push(json!({
        "id": "env:field:fw_repo:src",
        "source_id": "env:repo:fw_repo",
        "path": "src",
        "data_type": "string",
        "evidence_kind": "observed",
        "is_tag": false,
        "coverage": {"observed": true},
        "provenance_ids": ["prov:synthetic"]
    }));
    pack.content_digest = pack.digest().unwrap();
    pack.validate().unwrap();
    let result = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: r#"#repo=fw_repo | regex("^(?<kind>.+)$", field=src) | groupBy([kind])"#.into(),
            context: Context {
                require_observed: true,
                ..Context::default()
            },
        },
        Some(&pack),
    );
    assert_eq!(
        result.outcome,
        Outcome::NeedsContext,
        "{:?}",
        result.diagnostics
    );
    assert_eq!(ambiguity(&result).severity, Severity::Error);
}

#[test]
fn truncated_single_value_kind_facet_gets_a_notice() {
    let pack = pack(
        Some(json!({"record_kinds": [
            {"field": "kind", "values": ["a"], "truncated": true}
        ]})),
        "2026-01-01T00:00:00Z",
    );
    let result = check("#repo=fw_repo | top(dst)", Some(&pack), false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);
    assert!(ambiguity(&result).message.contains("truncated"));
}

#[test]
fn projection_or_rewrite_before_kind_grouping_cannot_clear_ambiguity() {
    let pack = datasets(&["traffic", "event"]);
    for query in [
        "#repo=fw_repo | select([dst]) | groupBy([#event.dataset, dst])",
        "#repo=fw_repo | drop([#event.dataset]) | groupBy([#event.dataset, dst])",
        "#repo=fw_repo | rename(field=src, as=dst) | groupBy([#event.dataset, dst])",
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(ambiguity(&result).severity, Severity::Info, "{query}");
    }
}

#[test]
fn aggregation_before_kind_grouping_cannot_clear_ambiguity() {
    let pack = datasets(&["traffic", "event"]);
    let late = check(
        "#repo=fw_repo | top(dst) | groupBy([#event.dataset, dst])",
        Some(&pack),
        false,
    );
    assert_eq!(late.outcome, Outcome::Ready, "{:?}", late.diagnostics);
    assert_eq!(ambiguity(&late).severity, Severity::Info);
    assert!(late.grounding.basis["env:repo:fw_repo:@record-kind"].contains("#event.dataset"));

    let early = check(
        "#repo=fw_repo | groupBy([#event.dataset, dst])",
        Some(&pack),
        false,
    );
    assert_eq!(early.outcome, Outcome::Ready, "{:?}", early.diagnostics);
    assert_no_ambiguity(&early);
}

#[test]
fn aggregation_before_kind_case_match_or_top_cannot_clear_ambiguity() {
    let pack = datasets(&["traffic", "event"]);
    for query in [
        "#repo=fw_repo | top(dst) | #event.dataset match { traffic => top(dst); event => top(dst) }",
        "#repo=fw_repo | top(dst) | case { #event.dataset=traffic | top(dst); #event.dataset=event | top(dst) }",
        "#repo=fw_repo | top(dst) | top(#event.dataset)",
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(ambiguity(&result).severity, Severity::Info);
    }

    let early_top = check("#repo=fw_repo | top(#event.dataset)", Some(&pack), false);
    assert_eq!(
        early_top.outcome,
        Outcome::Ready,
        "{:?}",
        early_top.diagnostics
    );
    assert_no_ambiguity(&early_top);
}

#[test]
fn independently_scoped_table_and_join_subqueries_retain_kind_notices() {
    let pack = datasets(&["traffic", "event"]);
    for query in [
        "defineTable(name=t, query={ #repo=fw_repo | top(dst) }, include=[dst]) | readFile(t)",
        "join({ #repo=fw_repo | top(dst) }, field=dst, key=dst, include=[dst])",
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(ambiguity(&result).severity, Severity::Info);
        assert!(
            ambiguity(&result)
                .message
                .contains("nested query scope #repo=fw_repo"),
            "{query}: {:?}",
            result.diagnostics
        );
        assert!(
            result.grounding.basis["env:repo:fw_repo:@record-kind"].contains("#event.dataset"),
            "{query}: {:?}",
            result.grounding
        );
    }
}

#[test]
fn kind_values_union_across_sources_selected_by_repo_only() {
    let mut pack = pack(
        Some(json!({
            "datasets": ["traffic"],
            "record_kinds": [{"field": "#type", "values": ["a"], "truncated": false}]
        })),
        "2026-01-01T00:00:00Z",
    );
    pack.data.sources[0]["id"] = json!("env:repo:fw_repo:type:a");
    pack.data.sources[0]["selectors"] = json!({"repo": "fw_repo", "type": "a"});
    let mut second = pack.data.sources[0].clone();
    second["id"] = json!("env:repo:fw_repo:type:b");
    second["selectors"] = json!({"repo": "fw_repo", "type": "b"});
    second["relationship"] = json!({
        "datasets": ["event"],
        "record_kinds": [{"field": "#type", "values": ["b"], "truncated": false}]
    });
    pack.data.sources.push(second);
    pack.content_digest = pack.digest().unwrap();
    pack.validate().unwrap();

    let result = check("#repo=fw_repo | top(dst)", Some(&pack), false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    for id in ["env:repo:fw_repo:type:a", "env:repo:fw_repo:type:b"] {
        let evidence = &result.grounding.basis[&format!("{id}:@record-kind")];
        assert!(evidence.contains("#type: a, b"), "{evidence}");
        assert!(
            evidence.contains("#event.dataset: event, traffic"),
            "{evidence}"
        );
    }
    assert_eq!(ambiguity(&result).severity, Severity::Info);

    let narrowed = check("#repo=fw_repo #type=a | top(dst)", Some(&pack), false);
    assert_eq!(
        narrowed.outcome,
        Outcome::Ready,
        "{:?}",
        narrowed.diagnostics
    );
    assert_no_ambiguity(&narrowed);
}

#[test]
fn distinct_type_selectors_are_kind_evidence_without_relationship_entries() {
    let mut pack = pack(None, "2026-01-01T00:00:00Z");
    pack.data.sources[0]["id"] = json!("env:repo:fw_repo:type:a");
    pack.data.sources[0]["selectors"] = json!({"repo": "fw_repo", "type": "a"});
    let mut second = pack.data.sources[0].clone();
    second["id"] = json!("env:repo:fw_repo:type:b");
    second["selectors"] = json!({"repo": "fw_repo", "type": "b"});
    pack.data.sources.push(second);
    pack.content_digest = pack.digest().unwrap();
    pack.validate().unwrap();

    let result = check("#repo=fw_repo | top(dst)", Some(&pack), false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert!(ambiguity(&result).message.contains("#type: a, b"));
    let grouped = check("#repo=fw_repo | groupBy([#type, dst])", Some(&pack), false);
    assert_eq!(grouped.outcome, Outcome::Ready, "{:?}", grouped.diagnostics);
    assert_no_ambiguity(&grouped);
}

#[test]
fn case_insensitive_single_value_in_does_not_constrain_a_single_kind() {
    let pack = datasets(&["TRAFFIC", "traffic"]);
    let result = check(
        r#"#repo=fw_repo | in(#event.dataset, values=["traffic"], ignoreCase=true) | top(dst)"#,
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);
    let exact = check(
        r#"#repo=fw_repo | in(#event.dataset, values=["traffic"]) | top(dst)"#,
        Some(&pack),
        false,
    );
    assert_eq!(exact.outcome, Outcome::Ready, "{:?}", exact.diagnostics);
    assert_no_ambiguity(&exact);
}

#[test]
fn lookup_enrichment_of_kind_must_not_count_as_original_kind_grouping() {
    let pack = named_kinds("kind", &["a", "b"], false);
    let result = check(
        r#"#repo=fw_repo | match(file="labels.csv", field=user, include=[kind]) | groupBy([kind])"#,
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);
}

#[test]
fn lookup_enrichment_of_kind_must_not_count_as_original_kind_filter() {
    let pack = named_kinds("kind", &["a", "b"], false);
    let result = check(
        r#"#repo=fw_repo | match(file="labels.csv", field=user, include=[kind]) | kind=a | top(dst)"#,
        Some(&pack),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(ambiguity(&result).severity, Severity::Info);
}

#[test]
fn only_non_assigning_filter_calls_can_precede_kind_separation() {
    let pack = named_kinds("kind", &["a", "b"], false);
    for query in [
        r#"#repo=fw_repo | regex("(?<kind>.*)", field=src) | kind=a | top(dst)"#,
        r#"#repo=fw_repo | regex("a", field=src, strict=false) | groupBy([kind])"#,
        r#"#repo=fw_repo | test(src == "a") | groupBy([kind])"#,
        r#"#repo=fw_repo | match(file="labels.csv", field=user) | groupBy([kind])"#,
        r#"#repo=fw_repo | eval(kind="a") | kind=a | top(dst)"#,
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(ambiguity(&result).severity, Severity::Info, "{query}");
    }
    let filtered = check(
        r#"#repo=fw_repo | in(src, values=["a"]) | regex("a", field=src) | wildcard("a", field=src) | kind=a | top(dst)"#,
        Some(&pack),
        false,
    );
    assert_eq!(
        filtered.outcome,
        Outcome::Ready,
        "{:?}",
        filtered.diagnostics
    );
    assert_no_ambiguity(&filtered);
}

#[test]
fn strict_mode_requires_kind_acceptance_and_keeps_warning_exit_codes() {
    let pack = datasets(&["traffic", "event"]);
    for accept in [false, true] {
        let context = Context {
            require_observed: true,
            accept_mixed_record_kinds: accept,
            ..Context::default()
        };
        let checked = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: "#repo=fw_repo | count()".into(),
                context: context.clone(),
            },
            Some(&pack),
        );
        let generated = app::generate(
            GenerateRequest {
                schema_version: 1,
                id: None,
                context,
                input: GenerateInput::Recipe {
                    request: Recipe::CountEvents {
                        source: Scope {
                            repo: Some("fw_repo".into()),
                            ..Scope::default()
                        },
                        filters: vec![],
                    },
                },
            },
            Some(&pack),
        );
        for result in [&checked, &generated] {
            assert_eq!(
                result.outcome,
                if accept {
                    Outcome::Ready
                } else {
                    Outcome::NeedsContext
                }
            );
            assert_eq!(ambiguity(result).code, "CQL_RECORD_KIND_AMBIGUOUS");
            assert_eq!(
                ambiguity(result).severity,
                if accept {
                    Severity::Warning
                } else {
                    Severity::Error
                }
            );
            assert_eq!(result.exit_code(false), if accept { 0 } else { 1 });
            assert_ne!(result.exit_code(true), 0);
        }
        assert_eq!(generated.query.is_some(), accept);
    }
    let normal = check("#repo=fw_repo | count()", Some(&pack), false);
    assert_eq!(normal.outcome, Outcome::Ready);
    assert_eq!(ambiguity(&normal).code, "CQL_RECORD_KIND_NOTICE");
    assert_eq!(ambiguity(&normal).severity, Severity::Info);
    assert_eq!(normal.exit_code(true), 0);
}

#[test]
fn multi_value_truncated_kind_facet_discloses_unknown_kinds_in_both_outcomes() {
    let pack = named_kinds("kind", &["a", "b"], true);
    for (accept, outcome, severity) in [
        (false, Outcome::Ready, Severity::Info),
        (true, Outcome::Ready, Severity::Info),
    ] {
        let result = check("#repo=fw_repo | top(dst)", Some(&pack), accept);
        assert_eq!(result.outcome, outcome, "{:?}", result.diagnostics);
        let diagnostic = ambiguity(&result);
        assert_eq!(diagnostic.severity, severity);
        assert!(
            diagnostic.message.contains("truncated"),
            "{}",
            diagnostic.message
        );
        assert!(
            diagnostic.message.contains("other kinds may exist"),
            "{}",
            diagnostic.message
        );
    }
}
