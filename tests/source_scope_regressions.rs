use cqlmatic::{
    app,
    contracts::*,
    environment::{Pack, PackData},
};
use serde_json::{Value, json};

const REPO: &str = "scope_example";
const SOURCE: &str = "env:repo:scope_example";
const OBSERVED: &str = "observed in a bounded sample";

fn data() -> PackData {
    PackData {
        sources: vec![json!({
            "id": SOURCE,
            "kind": "repo",
            "name": REPO,
            "inventory_status": "observed-only",
            "selectors": {"repo": REPO},
            "evidence": {"observed": true},
            "provenance_ids": ["prov:synthetic"]
        })],
        fields: ["#event_simpleName", "aid", "Severity"]
            .into_iter()
            .map(|field| observation(SOURCE, field))
            .collect(),
        provenance: vec![json!({
            "id": "prov:synthetic",
            "source_type": "mcp-observation",
            "canonical_source": "synthetic source-scope regression",
            "retrieved_at": "2026-01-01T00:00:00Z",
            "redaction_status": "none",
            "coverage_status": "complete"
        })],
        ..PackData::default()
    }
}

fn observation(source: &str, field: &str) -> Value {
    json!({
        "id": format!("env:field:{source}:{field}"),
        "source_id": source,
        "path": field.trim_start_matches('#'),
        "data_type": "string",
        "is_tag": field.starts_with('#'),
        "evidence_kind": "observed",
        "coverage": {"observed": true},
        "provenance_ids": ["prov:synthetic"]
    })
}

fn pack(data: PackData) -> Pack {
    Pack::new(
        EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "search-all".into(),
        },
        "2026-01-02T00:00:00Z".into(),
        data,
    )
    .unwrap()
}

fn context() -> Context {
    Context {
        require_observed: true,
        ..Context::default()
    }
}

fn check(query: &str, pack: &Pack) -> Analysis {
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: context(),
        },
        Some(pack),
    )
}

fn generate(field: &str, predicate: Value, extra: &str, pack: &Pack) -> Analysis {
    let request = serde_json::from_value(json!({
        "schema_version": 1,
        "input": {"mode": "plan", "plan": {
            "source": {"repo": REPO},
            "filters": [predicate, {"op": "exists", "field": extra}],
            "steps": [{"op": "group", "keys": [field],
                "aggregates": [{"function": "count", "as": "_count"}]}]
        }},
        "context": {"require_observed": true}
    }))
    .unwrap();
    app::generate(request, Some(pack))
}

fn assert_ready(result: &Analysis) {
    assert_eq!(result.outcome, Outcome::Ready, "{result:?}");
    assert_eq!(result.validation.status, Acceptance::Accepted);
    assert_eq!(result.validation.execution, "not-run");
    assert!(result.grounding.missing.is_empty());
    assert!(result.grounding.stale.is_empty());
}

fn assert_unresolved(query: &str, pack: &Pack) {
    let result = check(query, pack);
    assert_eq!(result.validation.status, Acceptance::Accepted, "{result:?}");
    assert_eq!(result.outcome, Outcome::NeedsContext, "{query}: {result:?}");
    assert!(
        result
            .grounding
            .basis
            .values()
            .any(|basis| basis == "source or field selection unresolved; unverified"),
        "{query}: {:?}",
        result.grounding
    );
}

#[test]
fn anchored_discriminator_existence_preserves_repo_observation() {
    let pack = pack(data());
    for field in ["#event_simpleName", "#type"] {
        let generated = generate(field, json!({"op": "exists", "field": field}), "aid", &pack);
        assert_ready(&generated);
        let query = generated.query.as_ref().unwrap();
        assert!(query.contains(&format!("{field} = \"*\"")), "{query}");
        assert_ready(&check(query, &pack));
        assert_eq!(generated.grounding.basis["scope_example:*:aid"], OBSERVED);
        assert_eq!(
            generated.grounding.basis["scope_example:*:#repo"],
            "language-built-in"
        );
    }
}

#[test]
fn event_filters_preserve_repo_inventory_with_type_and_sourcetype() {
    let mut data = data();
    data.sources[0]["selectors"]["type"] = json!("native-stream");
    data.sources[0]["selectors"]["sourcetype"] = json!("synthetic:native-stream");
    let pack = pack(data);
    for predicate in [
        json!({"op": "exists", "field": "#event_simpleName"}),
        json!({"op": "any", "items": [
            {"op": "eq", "field": "#event_simpleName", "value": "KindA"},
            {"op": "eq", "field": "#event_simpleName", "value": "KindB"}
        ]}),
    ] {
        let generated = generate("#event_simpleName", predicate, "aid", &pack);
        assert_ready(&generated);
        assert_ready(&check(generated.query.as_ref().unwrap(), &pack));
        assert_eq!(generated.grounding.basis["scope_example:*:aid"], OBSERVED);
    }
    assert_ready(&check(
        "#repo=scope_example | #type=native-stream | #event_simpleName=* | aid=*",
        &pack,
    ));
    let other = check(
        "#repo=scope_example | #type=other-stream | #event_simpleName=* | aid=*",
        &pack,
    );
    assert_eq!(other.outcome, Outcome::NeedsContext);
}

#[test]
fn repo_inventory_keeps_comma_bearing_types_as_exact_selectors() {
    let mut data = data();
    data.sources[0]["selectors"]["type"] = json!("KindA,KindB");
    data.sources[0]["selectors"]["sourcetype"] = json!("synthetic:inventory");
    let pack = pack(data);
    assert_ready(&check("#repo=scope_example | aid=*", &pack));
    assert_ready(&check(
        "#repo=scope_example | #type=\"KindA,KindB\" | aid=*",
        &pack,
    ));
    for filter in [
        "#type=*",
        "(#type=KindA OR #type=KindB)",
        "#type=KindA",
        "#type=KindB",
    ] {
        let result = check(&format!("#repo={REPO} | {filter} | aid=*"), &pack);
        assert_eq!(
            result.outcome,
            Outcome::NeedsContext,
            "{filter}: {result:?}"
        );
    }
}

#[test]
fn anchored_positive_alternatives_keep_all_branches_and_repo_fields() {
    let pack = pack(data());
    for (field, extra) in [
        ("#event_simpleName", "aid"),
        ("#type", "aid"),
        ("#type", "Severity"),
    ] {
        let generated = generate(
            field,
            json!({"op": "any", "items": [
                {"op": "eq", "field": field, "value": "KindA"},
                {"op": "eq", "field": field, "value": "KindB"}
            ]}),
            extra,
            &pack,
        );
        assert_ready(&generated);
        let query = generated.query.as_ref().unwrap();
        assert!(query.contains("\"KindA\" OR"), "{query}");
        assert!(query.contains("\"KindB\""), "{query}");
        assert!(query.contains(&format!("{extra} = \"*\"")), "{query}");
        assert_ready(&check(query, &pack));
        assert_eq!(
            generated.grounding.basis[&format!("{REPO}:*:{extra}")],
            OBSERVED
        );
    }
}

#[test]
fn nested_boolean_filters_preserve_only_the_conjunctive_repo_anchor() {
    let pack = pack(data());
    for query in [
        "(#type=KindA OR (#type=KindB #event_simpleName=*)) aid=* #repo=scope_example | groupBy([#type], limit=max)",
        "#repo=scope_example | ((#type=KindA aid=*) OR (#type=KindB Severity=*)) | groupBy([#type], limit=max)",
        "#repo=scope_example | (#type=KindA OR (#type=KindB OR #type=KindC)) | aid=*",
    ] {
        let result = check(query, &pack);
        assert_ready(&result);
        assert_eq!(result.grounding.basis["scope_example:*:aid"], OBSERVED);
        assert!(
            !result
                .grounding
                .basis
                .keys()
                .any(|key| key.contains(":Kind"))
        );
    }
}

#[test]
fn source_or_negation_globs_and_conflicts_remain_unresolved() {
    let pack = pack(data());
    for query in [
        "(#repo=scope_example OR #repo=other) | aid=*",
        "#repo=scope_example | (#repo=scope_example OR #repo=other) | aid=*",
        "(#repo=scope_example OR #type=KindA) | aid=*",
        "#repo=* | aid=*",
        "#repo=scope* | aid=*",
        "#repo!=other | aid=*",
        "!#repo=other | aid=*",
        "#type=* | aid=*",
        "(#type=KindA OR #type=KindB) | aid=*",
        "#event_simpleName=* | aid=*",
        "#repo=scope_example | #type!=KindA | aid=*",
        "#repo=scope_example | !#event_simpleName=KindA | aid=*",
        "#repo=scope_example | #type=Kind* | aid=*",
        "#repo=scope_example | (#type=KindA OR #type=Kind*) | aid=*",
        "#repo=scope_example #repo=other #type=* | aid=*",
        "#repo=scope_example #type=KindA #type=KindB | aid=*",
        "#repo=scope_example #event_simpleName=KindA #event_simpleName=KindB | aid=*",
    ] {
        assert_unresolved(query, &pack);
    }
}

#[test]
fn alternatives_cannot_promote_a_single_source_specific_observation() {
    for (selector, field) in [
        ("type", "#type"),
        ("event_family", "#event_simpleName"),
        ("event_family", "#type"),
        ("sourcetype", "#type"),
    ] {
        let mut data = data();
        data.sources[0]["kind"] = json!("source");
        data.sources[0]["selectors"][selector] = json!("KindA");
        let pack = pack(data);
        for filter in [
            format!("{field}=*"),
            format!("({field}=KindA OR {field}=KindB)"),
        ] {
            let result = check(&format!("#repo={REPO} | {filter} | aid=*"), &pack);
            assert_eq!(
                result.outcome,
                Outcome::NeedsContext,
                "{selector}: {result:?}"
            );
            assert_eq!(
                result.grounding.basis["scope_example:*:aid"],
                "query-supplied; not measured in selected pack"
            );
        }
    }
}

#[test]
fn parent_repo_evidence_is_used_without_combining_child_namespaces() {
    let mut data = data();
    let mut child = data.sources[0].clone();
    child["id"] = json!("env:source:KindA");
    child["kind"] = json!("source");
    child["relationship"] = json!({"parent_repo_id": SOURCE});
    child["selectors"]["type"] = json!("KindA");
    data.sources.push(child);
    data.fields.push(observation("env:source:KindA", "aid"));
    let pack = pack(data);
    let result = check("#repo=scope_example | #type=* | aid=*", &pack);
    assert_ready(&result);
    assert_eq!(result.grounding.basis["scope_example:*:aid"], OBSERVED);

    let mut data = pack.data.clone();
    data.fields
        .retain(|field| field["source_id"] != SOURCE || field["path"] != "aid");
    let mut other = data.sources[1].clone();
    other["id"] = json!("env:source:KindB");
    other["selectors"]["type"] = json!("KindB");
    data.sources.push(other);
    data.fields.push(observation("env:source:KindB", "aid"));
    let result = check("#repo=scope_example | #type=* | aid=*", &self::pack(data));
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert_eq!(
        result.grounding.basis["scope_example:*:aid"],
        "query-supplied; not measured in selected pack"
    );
}

#[test]
fn exact_type_selectors_keep_the_existing_namespace() {
    let mut data = data();
    data.sources[0]["selectors"]["type"] = json!("KindA");
    let pack = pack(data);
    for filter in ["#type=KindA", "#type=KindA | #event_simpleName=*"] {
        let result = check(&format!("#repo={REPO} | {filter} | aid=*"), &pack);
        assert_ready(&result);
        assert_eq!(result.grounding.basis["scope_example:KindA:aid"], OBSERVED);
    }
    let other = check("#repo=scope_example | #type=KindB | aid=*", &pack);
    assert_eq!(other.outcome, Outcome::NeedsContext);
}

#[test]
fn relaxed_source_restrictions_survive_branches_and_exact_type_narrowing() {
    let mut data = data();
    data.sources[0]["selectors"]["type"] = json!("KindA");
    let pack = pack(data);
    let result = check("#repo=scope_example | case { #type=*; * } | aid=*", &pack);
    assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
    assert_eq!(
        result.grounding.basis["scope_example:*:aid"],
        "query-supplied; not measured in selected pack"
    );

    let mut data = pack.data.clone();
    data.sources[0]["selectors"]["event_family"] = json!("KindA");
    let result = check(
        "#repo=scope_example | #type=KindA | (#event_simpleName=KindA OR #event_simpleName=KindB) | aid=*",
        &self::pack(data),
    );
    assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
    assert_eq!(
        result.grounding.basis["scope_example:KindA:aid"],
        "query-supplied; not measured in selected pack"
    );
}

#[test]
fn anchored_filters_preserve_missing_stale_and_tag_evidence_checks() {
    let query = "#repo=scope_example | #event_simpleName=* | aid=*";
    let mut missing = data();
    missing.fields.retain(|field| field["path"] != "aid");
    let result = check(query, &pack(missing));
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert!(
        result
            .grounding
            .missing
            .contains(&"scope_example:*:aid".into())
    );

    let mut stale = data();
    stale.provenance[0]["coverage_status"] = json!("stale");
    let result = check(query, &pack(stale));
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert!(
        result
            .grounding
            .stale
            .contains(&"scope_example:*:aid".into())
    );

    let mut ordinary_event_name = data();
    ordinary_event_name.fields[0]["is_tag"] = json!(false);
    let result = check(query, &pack(ordinary_event_name));
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert!(
        result
            .grounding
            .missing
            .contains(&"scope_example:*:#event_simpleName".into())
    );
}

#[test]
fn replacement_and_independent_subqueries_cannot_inherit_the_repo_anchor() {
    let pack = pack(data());
    for query in [
        "#repo=scope_example | readFile(\"synthetic.csv\") | #repo=scope_example #type=* aid=*",
        "#repo=scope_example | join({#type=* | aid=*}, field=aid)",
        "#repo=scope_example | join({#repo=other | #type=* | aid=*}, field=aid)",
        "#repo=scope_example | join({#repo=scope_example | #type=* | aid=*}, field=aid, view=other)",
    ] {
        let result = check(query, &pack);
        assert_eq!(result.validation.status, Acceptance::Accepted, "{result:?}");
        assert_eq!(result.outcome, Outcome::NeedsContext, "{query}: {result:?}");
    }
    assert_ready(&check(
        "#repo=scope_example | join({#repo=scope_example | #type=* | aid=*}, field=aid)",
        &pack,
    ));
}

#[test]
fn mixed_record_kind_policy_still_applies_to_anchored_discriminator_filters() {
    let mut data = data();
    data.sources[0]["relationship"] = json!({"record_kinds": [{
        "field": "#type", "values": ["KindA", "KindB"], "truncated": false
    }]});
    let pack = pack(data);
    let query = "#repo=scope_example | #type=* | aid=* | count()";
    let result = check(query, &pack);
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_RECORD_KIND_AMBIGUOUS")
    );
    assert_eq!(result.grounding.basis["scope_example:*:aid"], OBSERVED);
    assert_ready(&check(
        "#repo=scope_example | #type=* | aid=* | groupBy([#type], limit=max)",
        &pack,
    ));
}
fn check_with_packs(query: &str, primary: &Pack, active: Option<&Pack>) -> Analysis {
    app::check_with_evidence(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: context(),
        },
        Some(primary),
        Some(app::GroundingEvidence {
            environment: &primary.environment,
            active_pack: active,
            query_observations: &[],
            captured_observations: &[],
            capabilities: None,
        }),
    )
}

fn check_both(query: &str, pack: &Pack) -> [Analysis; 2] {
    [
        check(query, pack),
        check_with_packs(query, pack, Some(pack)),
    ]
}

fn add_scoped_field(data: &mut PackData, kind: &str, field: &str) {
    let id = format!("env:source:{kind}");
    let mut source = data.sources[0].clone();
    source["id"] = json!(id);
    source["kind"] = json!("source");
    source["selectors"] = json!({"repo":REPO,"type":kind,"sourcetype":kind});
    data.sources.push(source);
    data.fields.push(observation(&id, field));
}

#[test]
fn exact_repo_and_type_observations_resolve_overlapping_dataset_records() {
    let mut data = data();
    data.fields.push(observation(SOURCE, "#event.dataset"));
    add_scoped_field(&mut data, "logs", "#event.dataset");
    data.fields.push(observation("env:source:logs", "user"));
    let pack = pack(data);
    for query in [
        "#repo=scope_example user=* | count()",
        "#repo=scope_example #event.dataset=example user=* | count()",
        "#repo=scope_example #type=logs #event.dataset=example user=* | count()",
    ] {
        for result in check_both(query, &pack) {
            assert_ready(&result);
        }
    }
}

#[test]
fn ambiguity_names_the_competing_scopes() {
    let mut data = data();
    add_scoped_field(&mut data, "logs", "aid");
    add_scoped_field(&mut data, "other", "aid");
    data.fields
        .retain(|f| f["path"] != "aid" || f["source_id"] != SOURCE);
    let pack = pack(data);
    for result in check_both("#repo=scope_example aid=* | count()", &pack) {
        assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
        let basis = &result.grounding.basis["scope_example:*:aid"];
        assert!(
            basis.contains("#type=logs") && basis.contains("#type=other"),
            "{basis}"
        );
        assert!(
            basis.ends_with("Narrow the query to one of these selectors, or measure the field at the query's own scope."),
            "{basis}"
        );
    }
}

#[test]
fn two_exact_namespace_variants_still_refuse() {
    let mut primary = data();
    primary.sources[0]["selectors"] = json!({"repo":REPO,"type":"logs","sourcetype":"logs"});
    let mut active = primary.clone();
    active.sources[0]["selectors"] = json!({"repo":REPO,"type":"logs"});
    let primary = pack(primary);
    let active = pack(active);
    let query = "#repo=scope_example #type=logs aid=* | count()";
    let result = check_with_packs(query, &primary, Some(&active));
    assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
    let basis = &result.grounding.basis["scope_example:logs:aid"];
    assert!(basis.contains("#repo=scope_example #type=logs;"), "{basis}");
    assert!(basis.contains(r#"@sourcetype="logs""#), "{basis}");
    assert!(
        basis.ends_with("Select the snapshot you intend, or update the other."),
        "{basis}"
    );
    assert_ready(&check_with_packs(query, &active, Some(&active)));
}

#[test]
fn exact_scalar_namespace_cannot_borrow_narrow_array() {
    let mut data = data();
    data.fields.push(observation(SOURCE, "parts[]"));
    let exact_only = pack(data.clone());
    add_scoped_field(&mut data, "logs", "parts[]");
    let array = data.fields.last_mut().unwrap();
    array["data_type"] = json!("array");
    array["is_array"] = json!(true);
    let mixed = pack(data);
    let query = r#"#repo=scope_example | array:length("parts[]")"#;
    let result = check_with_packs(query, &mixed, None);
    let control = check(query, &exact_only);
    assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
    assert_eq!(result.grounding.basis, control.grounding.basis);
}

#[test]
fn exact_documented_active_namespace_cannot_borrow_narrow_primary_measurement() {
    let mut documented = data();
    let field = documented
        .fields
        .iter_mut()
        .find(|f| f["path"] == "aid")
        .unwrap();
    field["evidence_kind"] = json!("documented");
    field.as_object_mut().unwrap().remove("coverage");
    let active = pack(documented);
    let mut narrow = data();
    narrow.sources[0]["selectors"]["type"] = json!("logs");
    let narrow = pack(narrow);
    let query = "#repo=scope_example aid=* | count()";
    assert_ready(&check(query, &narrow));
    let result = check_with_packs(query, &narrow, Some(&active));
    assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
    assert_eq!(
        result.grounding.basis["scope_example:*:aid"],
        "query-supplied; not measured in selected pack"
    );
}

#[test]
fn exact_namespace_keeps_stale_negative_and_measured_absent_outcomes() {
    let query = "#repo=scope_example aid=* | count()";
    for condition in ["stale", "negative", "measured-absent"] {
        let mut data = data();
        match condition {
            "stale" => data.sources[0]["inventory_status"] = json!("stale"),
            "negative" => data.fields[1]["coverage"]["observed"] = json!(false),
            "measured-absent" => {
                data.fields.remove(1);
                let window = json!("2025-12-31T00:00:00Z/2026-01-01T00:00:00Z");
                data.sources[0]["evidence"]["measured_fields"] = json!(["aid"]);
                data.sources[0]["evidence"]["observation_window"] = window.clone();
                data.provenance[0]["observation_window"] = window;
            }
            _ => unreachable!(),
        }
        let exact = pack(data.clone());
        add_scoped_field(&mut data, "logs", "aid");
        data.sources[1]["inventory_status"] = json!("observed-only");
        let mixed = pack(data);
        let control = check_with_packs(query, &exact, Some(&exact));
        let result = check_with_packs(query, &mixed, Some(&mixed));
        assert_eq!(
            result.outcome,
            Outcome::NeedsContext,
            "{condition}: {result:?}"
        );
        assert_eq!(
            result.grounding.basis["scope_example:*:aid"],
            control.grounding.basis["scope_example:*:aid"],
            "{condition}"
        );
        if condition != "measured-absent" {
            let result = check(query, &mixed);
            assert_eq!(
                result.outcome,
                Outcome::NeedsContext,
                "{condition}: {result:?}"
            );
            assert_eq!(result.grounding.basis, check(query, &exact).grounding.basis);
        }
    }
}

#[test]
fn contradictory_literal_event_family_does_not_supply_grounding() {
    let mut data = data();
    data.sources[0]["selectors"]["event_family"] = json!("ProcessEvent");
    let pack = pack(data);
    let result_key = "scope_example:*:aid";
    for result in check_both(
        "#repo=scope_example #event_simpleName=FileEvent aid=* | count()",
        &pack,
    ) {
        assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
        assert_eq!(
            result.grounding.basis[result_key],
            "query-supplied; not measured in selected pack"
        );
    }
    for result in check_both(
        "#repo=scope_example #event_simpleName=ProcessEvent aid=* | count()",
        &pack,
    ) {
        assert_ready(&result);
        assert_eq!(result.grounding.basis[result_key], OBSERVED);
    }
    for (name, outcome) in [
        ("FileEvent", Outcome::NeedsContext),
        ("ProcessEvent", Outcome::Ready),
    ] {
        let result = app::generate(
            GenerateRequest {
                schema_version: 1,
                id: None,
                input: GenerateInput::Recipe {
                    request: Recipe::CountEvents {
                        source: Scope {
                            repo: Some(REPO.into()),
                            event: Some(name.into()),
                            ..Scope::default()
                        },
                        filters: vec![Predicate::Exists {
                            field: "aid".into(),
                            present: true,
                        }],
                    },
                },
                context: context(),
            },
            Some(&pack),
        );
        assert_eq!(result.outcome, outcome, "{result:?}");
        assert_eq!(result.query.is_some(), outcome == Outcome::Ready);
    }
}

#[test]
fn nonliteral_and_unrestricted_event_queries_keep_their_basis() {
    let mut data = data();
    data.sources[0]["selectors"]["event_family"] = json!("ProcessEvent");
    let pack = pack(data);
    // These outcomes and basis strings are from the released checker.
    for (filter, outcome, basis) in [
        ("", Outcome::Ready, OBSERVED),
        (
            r##"in(field="#event_simpleName", values=["FileEvent"]) | "##,
            Outcome::Ready,
            OBSERVED,
        ),
        (
            r##"in(field="#event_simpleName", values=["FileEvent","NetworkEvent"]) | "##,
            Outcome::Ready,
            OBSERVED,
        ),
        (
            r##"in(field="#event_simpleName", values=["fileevent"], ignoreCase=true) | "##,
            Outcome::Ready,
            OBSERVED,
        ),
        (
            r##"case { in(#event_simpleName, values=["FileEvent"]); in(#event_simpleName, values=["NetworkEvent"]) } | "##,
            Outcome::Ready,
            OBSERVED,
        ),
        (
            r##"!in(field="#event_simpleName", values=["ProcessEvent"]) | "##,
            Outcome::Ready,
            OBSERVED,
        ),
        (
            "#event_simpleName=/^FileEvent$/ | ",
            Outcome::NeedsContext,
            "source or field selection unresolved; unverified",
        ),
        (
            "#event_simpleName!=ProcessEvent | ",
            Outcome::NeedsContext,
            "source or field selection unresolved; unverified",
        ),
    ] {
        let query = format!("#repo=scope_example | {filter}aid=* | count()");
        for result in check_both(&query, &pack) {
            assert_eq!(result.outcome, outcome, "{query}: {result:?}");
            assert_eq!(
                result.grounding.basis["scope_example:*:aid"], basis,
                "{query}"
            );
        }
    }
}
