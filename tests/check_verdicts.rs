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

#[test]
fn aggregate_alias_advice_only_applies_to_aggregate_arguments() {
    let result = expect_check(
        "stats(function=total:=count())",
        Outcome::Invalid,
        Some("CQL_SYNTAX"),
    );
    assert!(
        result.diagnostics[0]
            .message
            .contains("aggregate function list")
    );

    for query in [
        "eval(total := 1)",
        "if(score == 1, then=total:=1, else=2)",
        "test(total := score)",
        "concat([total:=score, suffix], as=label)",
    ] {
        let result = expect_check(query, Outcome::Invalid, Some("CQL_SYNTAX"));
        assert_eq!(result.diagnostics[0].message, "Expected '{'.", "{query}");
    }
}

#[test]
fn assigned_format_time_preserves_existing_value_for_grounding() {
    let pack = serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    let result = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: r#"formatted := "unknown" | formatted := formatTime("%H", field=0) | label := formatted | select([label])"#.into(),
            context: Context {
                require_observed: true,
                ..Context::default()
            },
        },
        Some(&pack),
    );
    assert_eq!(
        result.outcome,
        Outcome::Ready,
        "{:?}",
        result.grounding.missing
    );
}

#[test]
fn assigned_format_time_preserves_the_timestamp_for_tail() {
    for query in [
        r#"createEvents(["sample"]) | select([@timestamp]) | formatted := formatTime("%H", field=@timestamp) | tail(1)"#,
        r#"createEvents(["sample"]) | select([@timestamp]) | formatTime("%H", field=@timestamp, as=formatted) | tail(1)"#,
    ] {
        let result = expect_check(query, Outcome::Ready, None);
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_TAIL_WITHOUT_TIMESTAMP"),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn format_time_as_a_computed_string_selector_is_unsupported() {
    expect_check(
        r#"callFunction(function=formatTime("sum", field=0, as=selector), field=points)"#,
        Outcome::Unsupported,
        Some("CQL_UNSUPPORTED_ARGUMENT_EXPRESSION"),
    );
}

#[test]
fn aggregate_aliases_follow_function_name_quoting() {
    expect_check(
        r#"stats([total:="count"()])"#,
        Outcome::Invalid,
        Some("CQL_SYNTAX"),
    );
}

#[test]
fn aggregate_aliases_follow_assignment_target_quoting() {
    expect_check(
        "stats([OR:=count()])",
        Outcome::Unsupported,
        Some("CQL_UNSUPPORTED_SYNTAX"),
    );
    expect_check(r#"stats(["OR":=count()])"#, Outcome::Ready, None);
}

#[test]
fn saved_calls_in_aggregate_aliases_require_resolution() {
    expect_check(
        "stats([total:=$example()])",
        Outcome::Unsupported,
        Some("CQL_UNSUPPORTED_SYNTAX"),
    );
}
