use cqlmatic::{
    app,
    contracts::{Analysis, CheckRequest, Context, Outcome},
};

fn expect_check(query: &str, outcome: Outcome, code: Option<&str>) -> Analysis {
    let result = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context::default(),
        },
        None,
    );
    assert_eq!(result.outcome, outcome, "{query}: {:?}", result.diagnostics);
    if let Some(code) = code {
        assert!(
            result.diagnostics.iter().any(|d| d.code == code),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    result
}

#[test]
fn aggregate_function_lists_accept_assignment_aliases() {
    for query in [
        "groupBy([category], function=[total:=sum(points), hits:=count()])",
        "bucket(span=1h, function=[total:=sum(points), hits:=count()])",
        "timeChart(span=1h, function=[total:=sum(points), hits:=count()])",
        "stats(function=[total:=sum(points), hits:=count()])",
        r#"stats(["total points":=sum(points), count()])"#,
        "groupBy([category], function=[sum(points), count()])",
    ] {
        expect_check(query, Outcome::Ready, None);
    }
    let result = expect_check(
        "groupBy([category], function=[total:=sum(points)]) | sort(total)",
        Outcome::Ready,
        None,
    );
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_DROPPED_FIELD"),
        "{:?}",
        result.diagnostics
    );
    for (query, code) in [
        (
            "groupBy([category], function=total:=sum(points))",
            "CQL_SYNTAX",
        ),
        ("stats(function=total:=sum(points))", "CQL_SYNTAX"),
        ("stats(function=[total:=1])", "CQL_SYNTAX"),
        ("stats(function=[total:={count()}])", "CQL_SYNTAX"),
        ("stats(function=[total:=(count())])", "CQL_SYNTAX"),
        ("stats(function=[total:=count])", "CQL_SYNTAX"),
        (
            "stats(function=[total:=lower(category)])",
            "CQL_AGGREGATE_REQUIRED",
        ),
        ("stats(function=[total:=sum()])", "CQL_ARGUMENT_REQUIRED"),
        (
            "stats(function=[total:=sum(points, as=other)])",
            "CQL_DUPLICATE_ARGUMENT",
        ),
    ] {
        expect_check(query, Outcome::Invalid, Some(code));
    }
}

#[test]
fn format_time_requires_an_output_field() {
    for (query, code) in [
        (r#"eval(hour=formatTime("%H"))"#, "CQL_ARGUMENT_REQUIRED"),
        (r#"formatTime("%H")"#, "CQL_ARGUMENT_REQUIRED"),
        (
            r#"eval(hour=formatTime("%H") + "h")"#,
            "CQL_ARGUMENT_REQUIRED",
        ),
        (r#"hour:=formatTime("%H") + "h""#, "CQL_ARGUMENT_REQUIRED"),
        (r#"hour:=formatTime("%H", as=other)"#, "CQL_ASSIGN_ALIAS"),
        (r#"length(formatTime("%H"), as=size)"#, "CQL_ARGUMENT_TYPE"),
        (r#"size:=length(formatTime("%H"))"#, "CQL_ARGUMENT_TYPE"),
        (
            r#"length(formatTime("%H", as=formatted), as=size)"#,
            "CQL_ARGUMENT_TYPE",
        ),
    ] {
        expect_check(query, Outcome::Invalid, Some(code));
    }
    for query in [
        r#"formatTime("%H", as=formatted)"#,
        r#"eval(hour=formatTime("%H", as=formatted))"#,
        r#"eval(hour=formatTime("%H", as=formatted) + "h")"#,
        r#"hour:=formatTime("%H")"#,
        r#"hour:=(formatTime("%H"))"#,
        r#"hour:=formatTime("%H", as=formatted) + "h""#,
    ] {
        expect_check(query, Outcome::Ready, None);
    }
}
