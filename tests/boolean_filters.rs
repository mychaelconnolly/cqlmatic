use cqlmatic::{app, contracts::*};
use serde_json::{Value, json};

fn generate(filters: Value) -> Analysis {
    app::generate(serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"filters":filters}}})).unwrap(),None)
}
#[test]
fn conjunctive_function_predicates_use_valid_pipeline_contexts() {
    let result = generate(
        json!([{"op":"all","items":[{"op":"eq","field":"category","value":"p"},{"op":"fields","left":"a","operator":"eq","right":"b"}]}]),
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    let q = result.query.unwrap();
    assert!(q.contains("| test("));
    assert!(!q.contains("AND"));
}
#[test]
fn keyword_names_remain_valid_field_expressions() {
    for field in ["not", "NOT", "and", "AND", "or", "OR"] {
        let result = generate(json!([{"op":"fields","left":field,"operator":"eq","right":"peer"}]));
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{field}: {:?}",
            result.diagnostics
        );
    }
}
#[test]
fn literal_stars_and_missing_policies_remain_boolean_filter_expressions() {
    for missing in ["include", "exclude"] {
        let result = generate(
            json!([{"op":"all","items":[{"op":"not-eq","field":"name","value":"*","missing":missing},{"op":"compare","field":"n","operator":"ge","value":5}]}]),
        );
        assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
        let q = result.query.unwrap();
        assert!(q.contains("/\\A\\*\\z/"));
        assert!(!q.contains("regex("));
    }
}
#[test]
fn function_or_is_refused_and_standalone_negation_is_preserved() {
    let pred = json!({"op":"regex","field":"name","pattern":"a.*","flags":""});
    let result =
        generate(json!([{"op":"any","items":[{"op":"eq","field":"category","value":"p"},pred]}]));
    assert_eq!(result.outcome, Outcome::Unsupported);
    assert!(result.query.is_none());
    let result = generate(
        json!([{"op":"regex","field":"name","pattern":"a.*","flags":"","negate":true,"missing":"exclude"}]),
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    assert!(result.query.unwrap().contains("| !regex("));
    for query in [
        "not (user=alice OR user=bob)",
        "NOT(user=alice)",
        "NOT user=alice",
        "user=bob NOT peer=alice",
        "!(user=alice OR user=bob)",
        "!regex(\"a\", field=name)",
    ] {
        let r = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                context: Context::default(),
            },
            None,
        );
        assert_eq!(r.outcome, Outcome::Ready, "{query}: {:?}", r.diagnostics);
    }
}
#[test]
fn an_unknown_english_condition_operator_is_unsupported() {
    let r:GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"english","text":"find events where login happens before download within five minutes","source":{}}})).unwrap();
    assert_eq!(app::generate(r, None).outcome, Outcome::Unsupported);
}

#[test]
fn raw_function_calls_cannot_be_joined_with_boolean_operators() {
    for query in [
        "lower(y) OR z=2",
        "z=2 OR lower(y)",
        "lower(y) AND z=2",
        "x=1 AND lower(y)",
        "x=1 lower(y)",
        "lower(y) OR z=2 AND a=1",
        "lower(y) AND z=2 OR a=1",
    ] {
        let result = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                context: Context::default(),
            },
            None,
        );
        assert_eq!(
            result.validation.status,
            Acceptance::Rejected,
            "{query}: {result:?}"
        );
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "CQL_FUNCTION_FILTER_COMPOSITION"
                    && diagnostic.message.contains("pipe")
                    && diagnostic.message.contains("case")
            }),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for (query, status) in [
        ("lower(x) | a=b OR c=d", Acceptance::Accepted),
        ("lower(y) extra", Acceptance::Unknown),
        ("lower(y) OR", Acceptance::Unknown),
        ("lower(y) AND", Acceptance::Unknown),
        ("lower(y) OR z=", Acceptance::Unknown),
        ("lower(y) OR z=2 |", Acceptance::Rejected),
        ("lower(y) OR $helper()", Acceptance::Unknown),
        ("lower(y) OR `unsupported`", Acceptance::Rejected),
        ("lower(y) OR z=2 | lower(x) extra", Acceptance::Unknown),
    ] {
        let result = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                context: Context::default(),
            },
            None,
        );
        assert_eq!(result.validation.status, status, "{query}: {result:?}");
    }
}
