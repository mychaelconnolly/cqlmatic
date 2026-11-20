use cqlmatic::{
    app,
    contracts::*,
    environment::{Pack, PackData},
};
use serde_json::{Value, json};

const ALPHA: &str = "#repo=alpha_repo #type=alpha-json";
const BETA: &str = "#repo=beta_repo #type=beta-syslog";

fn source(id: &str, repo: &str, type_: &str) -> Value {
    json!({
        "id": id,
        "kind": "repo",
        "name": repo,
        "inventory_status": "observed-only",
        "selectors": {"repo": repo, "type": type_},
        "evidence": {"observed": true},
        "provenance_ids": ["prov:synthetic"]
    })
}

fn field(source: &str, path: &str, observed: bool) -> Value {
    json!({
        "id": format!("env:field:{source}:{path}"),
        "source_id": source,
        "path": path.trim_start_matches('#'),
        "data_type": "string",
        "is_tag": path.starts_with('#'),
        "evidence_kind": if observed { "observed" } else { "documented" },
        "coverage": {"observed": if observed { json!(true) } else { Value::Null }},
        "provenance_ids": ["prov:synthetic"]
    })
}

/// Two mapped sources: one observed field, a documented field, a tag and an array element in
/// alpha, and documented fields only in beta.
fn pack() -> Pack {
    Pack::new(
        EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "synthetic-view".into(),
        },
        "2026-01-02T00:00:00Z".into(),
        PackData {
            sources: vec![
                source("env:repo:alpha", "alpha_repo", "alpha-json"),
                source("env:repo:beta", "beta_repo", "beta-syslog"),
            ],
            fields: vec![
                field("env:repo:alpha", "src.ip", true),
                field("env:repo:alpha", "#Vendor", true),
                field("env:repo:alpha", "user.name", false),
                field("env:repo:alpha", "event.category[0]", false),
                field("env:repo:beta", "host.name", false),
                field("env:repo:beta", "process.name", false),
            ],
            provenance: vec![json!({
                "id": "prov:synthetic",
                "source_type": "mcp-observation",
                "canonical_source": "synthetic unmapped grounding regression",
                "retrieved_at": "2026-01-01T00:00:00Z",
                "redaction_status": "none",
                "coverage_status": "complete"
            })],
            ..PackData::default()
        },
    )
    .unwrap()
}

fn check(query: &str, pack: Option<&Pack>, require_observed: bool) -> Analysis {
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context {
                require_observed,
                ..Context::default()
            },
        },
        pack,
    )
}

fn warnings<'a>(result: &'a Analysis, code: &str) -> Vec<&'a str> {
    result
        .diagnostics
        .iter()
        .filter(|d| d.code == code && d.severity == Severity::Warning)
        .map(|d| d.message.as_str())
        .collect()
}

fn assert_no_warning(query: &str) {
    let result = check(query, Some(&pack()), false);
    assert_eq!(
        result.outcome,
        Outcome::Ready,
        "{query}: {:?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Warning),
        "{query}: {:?}",
        result.diagnostics
    );
    assert_eq!(result.exit_code(true), 0, "{query}");
}

#[test]
fn a_field_the_selected_source_lacks_warns_and_names_the_source() {
    let result = check(
        r#"#repo=alpha_repo | url.domain="x" | count()"#,
        Some(&pack()),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    let found = warnings(&result, "CQL_UNMAPPED_FIELD");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(found[0].contains("url.domain"), "{}", found[0]);
    assert!(found[0].contains(ALPHA), "{}", found[0]);
    assert!(!found[0].contains(BETA), "{}", found[0]);
    assert_eq!(
        result.grounding.basis["alpha_repo:*:url.domain"],
        "query-supplied; not measured in selected pack"
    );
    assert_eq!(result.exit_code(true), 1);
}

#[test]
fn a_field_no_mapped_source_has_names_every_source_when_the_query_selects_none() {
    let result = check(r#"winlog.event_id=4624 | count()"#, Some(&pack()), false);
    let found = warnings(&result, "CQL_UNMAPPED_FIELD");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(found[0].contains("winlog.event_id"), "{}", found[0]);
    assert!(
        found[0].contains(ALPHA) && found[0].contains(BETA),
        "{}",
        found[0]
    );
    // A field one mapped source documents is quiet when the query selects no source.
    assert_no_warning(r#"process.name="x" | count()"#);
}

#[test]
fn documented_produced_tag_array_and_metadata_fields_are_quiet() {
    for query in [
        r#"#repo=alpha_repo | user.name="x" | count()"#,
        r#"#repo=alpha_repo | n := length(src.ip) | n > 3 | count()"#,
        r#"#repo=alpha_repo | #Vendor="x" | count()"#,
        r#"#repo=alpha_repo | event.category[0]="network" | count()"#,
        r#"#repo=alpha_repo | array:contains(array="event.category[]", value="network") | count()"#,
        r#"#repo=alpha_repo | @ingesttimestamp > 0 | count()"#,
    ] {
        let result = check(query, Some(&pack()), false);
        assert!(
            warnings(&result, "CQL_UNMAPPED_FIELD").is_empty(),
            "{query}: {:?}",
            result.diagnostics
        );
        assert!(
            result
                .diagnostics
                .iter()
                .all(|d| d.code != "CQL_GROUNDING_LENIENT"),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn a_correct_query_with_a_map_gets_no_grounding_warning() {
    assert_no_warning(r#"#repo=alpha_repo | src.ip="10.0.0.1" | groupBy([user.name], limit=max)"#);
    assert_no_warning(r#"#repo=beta_repo #type=beta-syslog | host.name="h" | count()"#);
}

#[test]
fn a_repository_no_mapped_source_has_warns_and_names_the_mapped_selectors() {
    let result = check(
        r#"#repo=gamma_repo | src.ip=* | count()"#,
        Some(&pack()),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    let found = warnings(&result, "CQL_UNMAPPED_SOURCE");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(found[0].contains("#repo=gamma_repo"), "{}", found[0]);
    assert!(
        found[0].contains(ALPHA) && found[0].contains(BETA),
        "{}",
        found[0]
    );
    assert!(!found[0].contains("view"), "{}", found[0]);
    // The selection is reported once; its fields are not reported again.
    assert!(
        warnings(&result, "CQL_UNMAPPED_FIELD").is_empty(),
        "{:?}",
        result.diagnostics
    );
    assert_eq!(result.exit_code(true), 1);

    let result = check(
        r#"#repo=alpha_repo #type=beta-syslog | src.ip=* | count()"#,
        Some(&pack()),
        false,
    );
    let found = warnings(&result, "CQL_UNMAPPED_SOURCE");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(
        found[0].contains("#repo=alpha_repo #type=beta-syslog"),
        "{}",
        found[0]
    );
}

#[test]
fn the_target_view_name_used_as_a_repository_is_called_the_view() {
    for query in [
        r#"#repo=synthetic-view | src.ip="10.0.0.1" | count()"#,
        r#"#repo="synthetic-view" | url.domain="x" | count()"#,
    ] {
        let result = check(query, Some(&pack()), false);
        let found = warnings(&result, "CQL_UNMAPPED_SOURCE");
        assert_eq!(found.len(), 1, "{query}: {:?}", result.diagnostics);
        assert!(found[0].contains("#repo=synthetic-view"), "{}", found[0]);
        assert!(found[0].contains("target view"), "{}", found[0]);
        assert!(
            found[0].contains(ALPHA) && found[0].contains(BETA),
            "{}",
            found[0]
        );
        assert!(
            warnings(&result, "CQL_UNMAPPED_FIELD").is_empty(),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    // With a mapped #type, the warning names the source that has it, not every source.
    let result = check(
        r#"#repo=synthetic-view #type=beta-syslog | host.name=* | count()"#,
        Some(&pack()),
        false,
    );
    let found = warnings(&result, "CQL_UNMAPPED_SOURCE");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(found[0].contains("target view"), "{}", found[0]);
    assert!(found[0].contains(BETA), "{}", found[0]);
    assert!(!found[0].contains(ALPHA), "{}", found[0]);
}

#[test]
fn strict_mode_output_is_unchanged() {
    for (query, missing) in [
        (
            r#"#repo=alpha_repo | url.domain="x" | count()"#,
            vec!["alpha_repo:*:url.domain"],
        ),
        (
            r#"#repo=synthetic-view | src.ip="10.0.0.1" | count()"#,
            vec!["synthetic-view:*:src.ip"],
        ),
        (
            r#"#repo=alpha_repo | user.name="x" | count()"#,
            vec!["alpha_repo:*:user.name"],
        ),
    ] {
        let result = check(query, Some(&pack()), true);
        assert_eq!(result.outcome, Outcome::NeedsContext, "{query}");
        assert_eq!(result.grounding.missing, missing, "{query}");
        let codes: Vec<_> = result.diagnostics.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(codes, ["CQL_CONTEXT_REQUIRED"], "{query}");
    }
    let result = check(
        r#"#repo=alpha_repo | src.ip="10.0.0.1" | count()"#,
        Some(&pack()),
        true,
    );
    assert_eq!(result.outcome, Outcome::Ready);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn without_a_map_check_adds_no_grounding_warning() {
    // An informational grouping ceiling alone leaves --fail-on-warning at exit 0.
    let result = check("groupBy([user], function=count(), limit=max)", None, false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    let codes: Vec<_> = result
        .diagnostics
        .iter()
        .map(|d| (d.code.as_str(), d.severity.clone()))
        .collect();
    assert_eq!(codes, [("CQL_GROUPBY_LIMIT", Severity::Info)]);
    assert_eq!(result.exit_code(true), 0);
    assert_eq!(
        result.grounding.basis["*:*:user"],
        "query-supplied; deployment unverified"
    );
}
