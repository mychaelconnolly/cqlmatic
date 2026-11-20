use cqlmatic::{app, contracts::*};
use serde_json::{Value, json};

fn generate(steps: Value) -> Analysis {
    app::generate(
        serde_json::from_value(json!({
            "schema_version": 1,
            "input": {"mode": "plan", "plan": {"source": {}, "steps": steps}}
        }))
        .unwrap(),
        None,
    )
}

fn check(query: &str) -> Analysis {
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context::default(),
        },
        None,
    )
}

#[test]
fn generate_uses_head_limit_for_adjacent_typed_sort() {
    let result = generate(json!([
        {"op": "sort", "fields": ["score", "host"], "order": "asc", "type": "string"},
        {"op": "head", "limit": 300},
        {"op": "select", "fields": ["host"]}
    ]));
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(
        result.query.as_deref(),
        Some(
            r#"sort(["score", "host"], order="asc", type="string", limit=200) | select(["host"])"#
        )
    );
}

#[test]
fn generate_keeps_the_smallest_adjacent_row_limit() {
    for (steps, expected) in [
        (
            json!([{"op": "sort", "fields": ["score"], "limit": 3}, {"op": "head", "limit": 300}]),
            3,
        ),
        (
            json!([{"op": "sort", "fields": ["score"], "limit": 1}, {"op": "head", "limit": 3}]),
            1,
        ),
        (
            json!([{"op": "sort", "fields": ["score"]}, {"op": "head", "limit": 3}]),
            3,
        ),
    ] {
        let result = generate(steps);
        assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
        assert_eq!(
            result.query,
            Some(format!(
                r#"sort(["score"], order="desc", type="number", limit={expected})"#
            ))
        );
    }
}

#[test]
fn generate_folds_consecutive_heads_into_the_sort() {
    let result = generate(json!([
        {"op": "sort", "fields": ["total"]},
        {"op": "head", "limit": 200},
        {"op": "head", "limit": 3},
        {"op": "head", "limit": 5}
    ]));
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert_eq!(
        result.query.as_deref(),
        Some(r#"sort(["total"], order="desc", type="number", limit=3)"#)
    );
    assert!(result.diagnostics.iter().all(|d| d.code != "CQL_SORT_HEAD"));
}

#[test]
fn generate_reports_an_out_of_range_head_against_head() {
    let result = generate(json!([
        {"op": "sort", "fields": ["score"], "limit": 1},
        {"op": "head", "limit": 200001}
    ]));
    assert_eq!(result.outcome, Outcome::Invalid, "{:?}", result.diagnostics);
    let range = result
        .diagnostics
        .iter()
        .find(|d| d.code == "CQL_ARGUMENT_RANGE")
        .expect("range diagnostic");
    assert_eq!(range.reference.as_deref(), Some("cql:function:head"));
}

#[test]
fn generate_preserves_standalone_head_and_rejects_zero_limits() {
    let result = generate(json!([{"op": "head", "limit": 3}]));
    assert_eq!(result.query.as_deref(), Some("head(3)"));

    for (sort, head, code) in [(200, 0, "CQL_LIMIT"), (0, 3, "CQL_SORT")] {
        let result = generate(json!([
            {"op": "sort", "fields": ["score"], "limit": sort},
            {"op": "head", "limit": head}
        ]));
        assert_eq!(result.outcome, Outcome::Invalid);
        assert_eq!(result.diagnostics[0].code, code);
    }
}

#[test]
fn generate_checks_both_typed_limits_against_the_row_ceiling() {
    for (sort, head) in [(200001, 3), (200, 200001)] {
        let result = generate(json!([
            {"op": "sort", "fields": ["score"], "limit": sort},
            {"op": "head", "limit": head}
        ]));
        assert_eq!(result.outcome, Outcome::Invalid, "{:?}", result.diagnostics);
    }
}

#[test]
fn check_warns_on_sort_followed_by_head() {
    for query in [
        "sort(score, order=desc) | head(3)",
        "groupBy(host) | sort(_count, limit=200) | head(3)",
        "sort(score, limit=200) | head(3)",
        "sort(score, limit=max) | head(3)",
        "sort(score, limit=300) | head()",
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "CQL_SORT_HEAD" && diagnostic.severity == Severity::Warning
            }),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn check_leaves_other_row_caps_quiet() {
    for query in [
        "head(3)",
        "sort(score, limit=3)",
        "sort(score) | tail(3)",
        "sort(score) | score > 0 | head(3)",
        "sort(score, limit=1) | head(3)",
        "sort(score, limit=3) | head(3)",
        "sort(score, limit=2) | head(limit=3)",
        "sort(score, limit=5) | head()",
        "sort(score, limit=5) | head(max)",
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
        assert!(
            result.diagnostics.iter().all(|d| d.code != "CQL_SORT_HEAD"),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}
