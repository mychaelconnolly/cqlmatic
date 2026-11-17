use cqlmatic::{
    app,
    contracts::{CheckRequest, Context},
};
fn check(query: &str) -> cqlmatic::contracts::Analysis {
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
fn unquoted_dollar_and_question_rules_preserve_literals_and_defaults() {
    for query in [
        "a=$x",
        "a=x$y",
        "a=x$",
        "a=$",
        "x=1 | foo$bar",
        "x=1 | $x",
        "a=x?",
        "a=?",
        "a=x*?",
        "a=*?*",
    ] {
        let result = check(query);
        assert!(
            result.diagnostics.iter().any(|d| d.code == "CQL_SYNTAX"),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for query in [
        "a=x?y",
        "a=1?2",
        "a=x_?y",
        "x=1 | foo?bar",
        "u=http://h/p?q",
    ] {
        let result = check(query);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_UNSUPPORTED_PARAMETER"),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for query in [
        r#"a="$x""#,
        "a=/x$/",
        "a=/x?y/",
        r#"a="x?y""#,
        "a=?{p=1}",
        "a=x?{y=1}",
        r#"a=x?{y="1"}z"#,
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            cqlmatic::contracts::Acceptance::Accepted,
            "{query}: {:?}",
            result.diagnostics
        );
    }
    assert!(
        check("x=1 | $foo()")
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_UNSUPPORTED_SYNTAX")
    );
}
#[test]
fn top_limit_respects_historical_map_ceiling() {
    for query in ["top(a, limit=24576)", "top([a, b], limit=24576)"] {
        assert_eq!(
            check(query).validation.status,
            cqlmatic::contracts::Acceptance::Accepted
        );
    }
    for query in ["top(a, limit=24577)", "top(a, limit=200000)"] {
        assert!(
            check(query)
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_ARGUMENT_RANGE"),
            "{query}"
        );
    }
    assert_ne!(
        check("top(a, limit=max)").validation.status,
        cqlmatic::contracts::Acceptance::Accepted
    );
}
