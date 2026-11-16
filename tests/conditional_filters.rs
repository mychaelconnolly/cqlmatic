use cqlmatic::{app, contracts::*};

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

fn rejected(query: &str, code: &str) {
    let result = check(query);
    assert_eq!(
        result.validation.status,
        Acceptance::Rejected,
        "{query}: {result:?}"
    );
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == code && d.severity == Severity::Error),
        "{query}: {result:?}"
    );
}

fn accepted(query: &str) {
    let result = check(query);
    assert_eq!(
        result.validation.status,
        Acceptance::Accepted,
        "{query}: {result:?}"
    );
}

#[test]
fn conditional_branches_are_filter_queries() {
    for query in [
        "x=1 | case { count(); * }",
        "x=1 | k match { a => count(); * => * }",
        "x=1 | case { y=1 | n := count(); * }",
        "x=1 | case { groupBy(k); * }",
        "x=1 | case { head(10); * }",
        "x=1 | case { y=1 | sort(z); * }",
        r#"case { createEvents(["a=1"]); * }"#,
        "x=1 | case { y=1 | case { count(); * }; * }",
        "x=1 | case { join({y=1}, field=k); * }",
        "x=1 | case { !join({y=1}, field=k); * }",
        "x=1 | k match { a => join({y=1}, field=k); * => * }",
        "x=1 | case { selfJoinFilter([k], where=[{a=1},{b=2}]); * }",
        "x=1 | case { selfJoin([k], where=[{a=1},{b=2}]); * }",
        "x=1 | case { k =~ join({y=1}, field=k); * }",
        r#"case { readFile("a.csv"); * }"#,
        r#"case { readPersistedAggregation("a"); * }"#,
        "case { correlate(A:{x=1}); * }",
        "case { k match { a => count(); * => * }; * }",
    ] {
        rejected(query, "CQL_BRANCH_FUNCTION");
        let result = check(query);
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|d| d.code == "CQL_BRANCH_FUNCTION")
                .count(),
            1,
            "{query}: {result:?}"
        );
    }
    for query in [
        "x=1 | case { y=1 | v := 1; * }",
        "x=1 | k match { a => v := 1; * => * }",
        "x=1 | case { y=1 | lower(z); * } | count()",
        r#"x=1 | count() | case { _count>1 | v := "many"; * | v := "few" }"#,
        r#"x=1 | case { wildcard("a*", field=y); * }"#,
        r#"x=1 | case { in(y, values=["a"]); * }"#,
        "x=1 | case { y=1 | case { z=1 | v := 1; * }; * }",
        "x=1 | groupBy(k) | join({y=1}, field=k)",
        r#"x=1 | case { regex("a(?<b>.)", field=y); * }"#,
        "case { array:reduceRow(\"v[]\", var=v, function=sum(v)); * }",
    ] {
        accepted(query);
    }
    let result = check("case { count() | n:=count(); * }");
    assert_eq!(
        result
            .diagnostics
            .iter()
            .filter(|d| d.code == "CQL_BRANCH_FUNCTION")
            .count(),
        2
    );
}

#[test]
fn quoted_star_is_free_text() {
    for query in [
        r#"count() | "*""#,
        r#"x=1 | groupBy(y) | "*""#,
        r#"count() | NOT "*""#,
    ] {
        rejected(query, "CQL_FREE_TEXT_AFTER_AGGREGATE");
    }
    for query in [
        "count() | *",
        r#""*" | count()"#,
        "* | count()",
        r#"count() | _count="*""#,
    ] {
        accepted(query);
    }
}

#[test]
fn modulo_requires_named_divisor() {
    for query in ["x=1 | math:mod(y, 3)", "x=1 | math:mod(y, 3, as=r)"] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Rejected,
            "{query}: {result:?}"
        );
    }
    for query in [
        "x=1 | math:mod(y, divisor=3)",
        "x=1 | math:mod(field=y, divisor=3, as=r)",
        "x=1 | r := math:mod(y, divisor=3)",
    ] {
        accepted(query);
    }
}

#[test]
fn string_arrays_accept_numeric_literals() {
    for query in [
        "in(a, values=[3, 6])",
        r#"in(a, values=[3, "x", y])"#,
        "in(a, values=[3.5, -2])",
        "not in(a, values=[1]) | b=2",
        "x=1 | !in(a, values=[1, 2])",
        r#"in(a, values=["500", "404"])"#,
        "in(a, values=[x0, x1])",
        "parseJson(removePrefixes=[3])",
        "createEvents([3])",
    ] {
        accepted(query);
    }
    rejected("in(a, values=[count()])", "CQL_ARGUMENT_TYPE");
    rejected("ioc:lookup(a, include=[3])", "CQL_ARGUMENT_VALUE");
    rejected("ioc:lookup(a, include=[-2])", "CQL_ARGUMENT_VALUE");
    assert_eq!(
        check(r#"in(a, values="alice")"#).outcome,
        Outcome::Unsupported
    );
}

#[test]
fn match_all_preserves_filter_cost_and_assessment() {
    use cqlmatic::{
        assess::{self, AssessRequest},
        language::parser,
    };
    use serde_json::json;
    for query in ["*", "count() | *", "* | count()"] {
        let pipeline = parser::parse(query).unwrap();
        let canonical = pipeline.render();
        assert_eq!(canonical, query);
        accepted(&canonical);
        assert!(
            !check(query)
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_RAW_SCAN_COST")
        );
        let result = assess::assess(
            AssessRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                result: json!({"result":[{"_count":1}]}),
                context: Context::default(),
            },
            None,
        );
        assert_eq!(
            result.outcome,
            assess::AssessOutcome::NoFindings,
            "{query}: {result:?}"
        );
    }
    assert!(
        check("* | regex(\"needle\")")
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_RAW_SCAN_COST")
    );
    assert!(
        !check("#repo=example | * | regex(\"needle\")")
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_RAW_SCAN_COST")
    );
}

#[test]
fn numeric_membership_generation_and_assessment_share_the_contract() {
    use cqlmatic::assess::{self, AssessRequest};
    use serde_json::json;
    let request: GenerateRequest = serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"call","function":"in","args":[{"value":{"kind":"literal","value":"a"}},{"name":"values","value":{"kind":"array","items":[{"kind":"literal","value":3},{"kind":"literal","value":-2}]}}]}]}}})).unwrap();
    let generated = app::generate(request, None);
    assert_eq!(generated.outcome, Outcome::Ready, "{generated:?}");
    let query = generated.query.unwrap();
    accepted(&query);
    let result = assess::assess(
        AssessRequest {
            schema_version: 1,
            id: None,
            query,
            result: json!({"result":[{"a":3}]}),
            context: Context::default(),
        },
        None,
    );
    assert_eq!(
        result.outcome,
        assess::AssessOutcome::NoFindings,
        "{result:?}"
    );
}
