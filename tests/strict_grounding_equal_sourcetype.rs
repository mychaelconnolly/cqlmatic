use cqlmatic::{
    app,
    contracts::{Acceptance, Analysis, CheckRequest, Cloud, Context, EnvironmentId, Outcome},
    environment::{Pack, PackData},
};
use serde_json::json;

const TYPE: &str = "example_type";
const USER_FIELD: &str = "example_repo:example_type:user";
const QUERY: &str = "#repo=example_repo #type=example_type | #type=* | user=* | count()";

fn check(sourcetype: Option<&str>) -> Analysis {
    let mut selectors = json!({"repo": "example_repo", "type": TYPE});
    if let Some(sourcetype) = sourcetype {
        selectors["sourcetype"] = json!(sourcetype);
    }
    let pack = Pack::new(
        EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "search-all".into(),
        },
        "2026-01-02T00:00:00Z".into(),
        PackData {
            sources: vec![json!({
                "id": "env:repo:example",
                "kind": "repo",
                "name": "example_repo",
                "inventory_status": "observed-only",
                "selectors": selectors,
                "evidence": {"observed": true},
                "provenance_ids": ["prov:synthetic"]
            })],
            fields: vec![json!({
                "id": "env:field:example:user",
                "source_id": "env:repo:example",
                "path": "user",
                "data_type": "string",
                "is_tag": false,
                "evidence_kind": "observed",
                "coverage": {"observed": true},
                "provenance_ids": ["prov:synthetic"]
            })],
            provenance: vec![json!({
                "id": "prov:synthetic",
                "source_type": "mcp-observation",
                "canonical_source": "synthetic grounding test",
                "retrieved_at": "2026-01-01T00:00:00Z",
                "redaction_status": "none",
                "coverage_status": "complete"
            })],
            ..PackData::default()
        },
    )
    .unwrap();
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: QUERY.into(),
            context: Context {
                require_observed: true,
                ..Context::default()
            },
        },
        Some(&pack),
    )
}

#[test]
fn repeated_sourcetype_grounds_like_an_omitted_sourcetype() {
    let omitted = check(None);
    assert_eq!(omitted.outcome, Outcome::Ready, "{:?}", omitted.diagnostics);
    assert_eq!(
        omitted.grounding.basis[USER_FIELD],
        "observed in a bounded sample"
    );

    let repeated = check(Some(TYPE));
    assert_eq!(
        repeated.outcome, omitted.outcome,
        "{:?}",
        repeated.diagnostics
    );
    assert_eq!(repeated.grounding.basis, omitted.grounding.basis);
    assert_eq!(json!(repeated.diagnostics), json!(omitted.diagnostics));
}

#[test]
fn distinct_sourcetype_still_requires_context() {
    let distinct = check(Some("example_variant"));
    assert_eq!(distinct.validation.status, Acceptance::Accepted);
    assert_eq!(distinct.outcome, Outcome::NeedsContext);
    assert_eq!(
        distinct.grounding.basis[USER_FIELD],
        "query-supplied; not measured in selected pack"
    );
    assert_eq!(distinct.grounding.missing, [USER_FIELD]);
    assert!(
        distinct
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_CONTEXT_REQUIRED")
    );
}
