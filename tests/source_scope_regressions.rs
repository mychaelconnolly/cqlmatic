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
