use cqlmatic::{app, contracts::*, environment::Pack};
use serde_json::{Value, json};

fn check(query: &str, strict: bool) -> Analysis {
    let pack: Pack =
        serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context {
                require_observed: strict,
                ..Context::default()
            },
        },
        Some(&pack),
    )
}

#[test]
fn aggregate_fields_are_shared_by_grounding_and_projection() {
    for query in [
        "#repo=example | collect(fields=[user],multival=false) | user=*",
        "#repo=example | selectLast([user]) | user=*",
        "#repo=example | selectFromMax(user,include=[user]) | user=*",
        "#repo=example | selectFromMin(user,include=[user]) | user=*",
        "#repo=example | groupBy(user,function=[count(as=n),collect([user]),selectLast([user])],limit=max) | n=* | user=*",
        "#repo=example | groupBy(user,function=head(1),limit=max) | user=*",
        "#repo=example | stats() | _count=*",
        "#repo=example | top(user,percent=true) | _count=* | percent=*",
    ] {
        assert_eq!(
            check(query, true).outcome,
            Outcome::Ready,
            "{query}: {:?}",
            check(query, true).grounding
        );
    }
    for query in [
        "#repo=example | collect([user]) | _collect=*",
        "#repo=example | selectFromMax(user,include=[user]) | unmeasured=*",
        "#repo=example | stats() | user=*",
        "#repo=example | fieldset() | user=*",
        "#repo=example | groupBy(user,function=[head(1),count(as=x)]) | x=*",
    ] {
        assert_eq!(check(query, true).outcome, Outcome::NeedsContext, "{query}");
    }
}

#[test]
fn argument_types_and_context_do_not_silently_widen() {
    for query in [
        "stats()",
        "collect(fields=user,limit=10)",
        "select(user)",
        "top(user,max=user,limit=5,error=5,rest=other)",
        r#"formatted:=formatTime("%F",field=epoch+1,unit=milliseconds,timezone=UTC)"#,
        r#"formatTime("%F",as=formatted,unit=auto,locale=en_US)"#,
        "round(user,how=ceil,as=rounded)",
    ] {
        assert_eq!(
            check(query, false).outcome,
            Outcome::Ready,
            "{query}: {:?}",
            check(query, false).diagnostics
        );
    }
    for query in [
        "collect(field=user)",
        "collect([user],as=names)",
        "collect([user],limit=max)",
        "selectFromMax(user)",
        "selectLast(user,other)",
        "top(user,limit=max)",
        "sort(user,limit=max)",
        r#"formatTime("%F")"#,
        r#"formatTime("%F",as=x,unit=hours)"#,
        "round(user+1)",
        "round(user,how=truncate)",
    ] {
        assert_ne!(check(query, false).outcome, Outcome::Ready, "{query}");
    }
    assert_eq!(check("top(user,error=2.5)", false).outcome, Outcome::Ready);
    assert_eq!(
        check("top(user,sum=user,max=user)", false).outcome,
        Outcome::Unsupported
    );
}

#[test]
fn max_row_limits_are_supported_only_for_head_tail_and_table() {
    for query in [
        "head(max)",
        "tail(max)",
        "table([user], limit=max)",
        "table([user], limit=10)",
        "head(10)",
    ] {
        let result = check(query, false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for query in [
        "sort(user,limit=max)",
        "top(user,limit=max)",
        "collect([user],limit=max)",
    ] {
        let result = check(query, false);
        assert_eq!(
            result.outcome,
            Outcome::Invalid,
            "{query}: {:?}",
            result.diagnostics
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| { d.code == "CQL_ARGUMENT_TYPE" && d.severity == Severity::Error }),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn group_by_limit_max_is_info_but_other_limits_warn() {
    for (query, severity, exit_code) in [
        (
            "groupBy([user], function=count(), limit=max)",
            Severity::Info,
            0,
        ),
        ("groupBy([user], function=count())", Severity::Warning, 1),
        (
            "groupBy([user], function=count(), limit=100)",
            Severity::Warning,
            1,
        ),
    ] {
        let result = check(query, false);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        let diagnostic = result
            .diagnostics
            .iter()
            .find(|d| d.code == "CQL_GROUPBY_LIMIT")
            .unwrap_or_else(|| panic!("{query}: missing CQL_GROUPBY_LIMIT"));
        assert_eq!(diagnostic.severity, severity, "{query}");
        assert_eq!(result.exit_code(true), exit_code, "{query}");
    }
}

#[test]
fn approximate_and_bounded_results_keep_their_diagnostics() {
    for (query, code) in [
        ("top(user)", "CQL_APPROXIMATE_TOP"),
        ("collect([user])", "CQL_COLLECT_LIMIT"),
        ("selectFromMin(user,include=user)", "CQL_SELECTOR_TIES"),
        ("selectLast([user])", "CQL_LAST_EVENT_ORDER"),
    ] {
        let r = check(query, false);
        assert_eq!(r.outcome, Outcome::Ready);
        assert!(r.diagnostics.iter().any(|d| d.code == code), "{query}");
    }
    assert!(
        check("groupBy(user,limit=max)", false)
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_GROUPBY_LIMIT")
    );
}

#[test]
fn public_calls_compose_conditions_aggregation_rates_and_presentation() {
    let lit = |value: Value| json!({"kind":"literal","value":value});
    let arg = |name: &str, value: Value| json!({"name":name,"value":value});
    let expr_call =
        |name: &str, args: Vec<Value>| json!({"kind":"call","function":name,"args":args});
    let plan = json!({"source":{},"steps":[
        {"op":"assign","field":"hit","expression":expr_call("if",vec![arg("condition",expr_call("regex",vec![arg("regex",lit(json!("fail"))),arg("field",lit(json!("status")))])),arg("then",lit(json!(1))),arg("else",lit(json!(0)))])},
        {"op":"call","function":"groupBy","args":[arg("field",json!({"kind":"array","items":[lit(json!("host"))]})),arg("limit",json!({"kind":"field","name":"max"})),arg("function",json!({"kind":"array","items":[expr_call("count",vec![arg("as",lit(json!("n")))]),expr_call("sum",vec![arg("field",lit(json!("hit"))),arg("as",lit(json!("failed")))]),expr_call("collect",vec![arg("fields",json!({"kind":"array","items":[lit(json!("user"))]}))])]}))]},
        {"op":"assign","field":"rate","expression":{"kind":"binary","operator":"divide","left":{"kind":"field","name":"failed"},"right":{"kind":"field","name":"n"}}},
        {"op":"filter","predicate":{"op":"compare","field":"rate","operator":"ge","value":0.5}},
        {"op":"sort","fields":["rate"],"order":"desc","type":"number","limit":10},
        {"op":"select","fields":["host","n","failed","rate","user"]}
    ]});
    let request: GenerateRequest =
        serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":plan}}))
            .unwrap();
    let r = app::generate(request, None);
    assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
    let q = r.query.unwrap();
    assert!(q.contains("collect(fields="));
    assert!(q.contains("sum(field="));
    assert!(!q.contains("top("));
}

#[test]
fn nested_calls_keep_accuracy_requirements_and_ordering_expires() {
    for q in ["count(distinct=true)", "stats(count(distinct=true))"] {
        assert_eq!(check(q, false).outcome, Outcome::Invalid, "{q}");
    }
    for (q, code) in [
        (
            "stats(count(user,distinct=true))",
            "CQL_APPROXIMATE_DISTINCT",
        ),
        ("stats(groupBy(user))", "CQL_GROUPBY_LIMIT"),
        (
            "stats(groupBy([],function=count()))",
            "CQL_EMPTY_GROUP_KEYS",
        ),
    ] {
        let r = check(q, false);
        assert_eq!(r.outcome, Outcome::Ready);
        assert!(r.diagnostics.iter().any(|d| d.code == code), "{q}");
    }
    for aggregate in [
        "collect([user],multival=false)",
        "groupBy(user)",
        "stats([collect([user],multival=false)])",
    ] {
        let q = format!("sort(user,type=string) | {aggregate} | neighbor(user)");
        let r = check(&q, false);
        assert_eq!(r.outcome, Outcome::Ready, "{q}");
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == "CQL_NEIGHBOR_UNORDERED")
        );
        let q = format!(
            "sort(user,type=string) | {aggregate} | sort(user,type=string) | neighbor(user)"
        );
        assert_eq!(check(&q, false).outcome, Outcome::Ready, "{q}");
    }
    for q in [
        r#"formatTime("%F",as=t,field=1.5,unit=seconds)"#,
        r#"t:=formatTime("%F",field=-1.5,unit=milliseconds)"#,
    ] {
        let r = check(q, false);
        assert_eq!(r.outcome, Outcome::Invalid);
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == "CQL_TIME_INTEGER_INPUT")
        );
    }
    let r = check(
        r#"t:=formatTime("%F",field=epoch,unit=milliseconds)"#,
        false,
    );
    assert_eq!(r.outcome, Outcome::Ready);
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.code == "CQL_TIME_INTEGER_INPUT")
    );
    for (function, args, expected, code) in [
        (
            "count",
            json!([{"name":"distinct","value":{"kind":"literal","value":true}}]),
            Outcome::Invalid,
            "CQL_ARGUMENT_REQUIRED",
        ),
        (
            "stats",
            json!([{"value":{"kind":"call","function":"count","args":[{"name":"field","value":{"kind":"literal","value":"user"}},{"name":"distinct","value":{"kind":"literal","value":true}}]}}]),
            Outcome::Ready,
            "CQL_APPROXIMATE_DISTINCT",
        ),
        (
            "formatTime",
            json!([{"value":{"kind":"literal","value":"%F"}},{"name":"as","value":{"kind":"literal","value":"t"}},{"name":"field","value":{"kind":"literal","value":1.5}},{"name":"unit","value":{"kind":"literal","value":"seconds"}}]),
            Outcome::Invalid,
            "CQL_TIME_INTEGER_INPUT",
        ),
    ] {
        let req:GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"call","function":function,"args":args}]}}})).unwrap();
        let r = app::generate(req, None);
        assert_eq!(r.outcome, expected);
        assert!(r.diagnostics.iter().any(|d| d.code == code));
    }
}

#[test]
fn timestamp_literal_checks_keep_decimal_precision_and_signed_long_bounds() {
    for literal in [
        "1700000000000.00001",
        "1e-400",
        "9223372036854775808",
        "-9223372036854775809",
        "1e999",
        "-1e-999",
    ] {
        let query = format!("formatTime(\"%F\",as=t,field={literal},unit=milliseconds)");
        let r = check(&query, false);
        assert_eq!(r.outcome, Outcome::Invalid, "{query}");
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == "CQL_TIME_INTEGER_INPUT")
        );
    }
    for literal in [
        "9223372036854775807",
        "-9223372036854775808",
        "+1",
        "1e3",
        "1000e-3",
        "1.0000",
        "0.0",
        "0e-400",
    ] {
        let query = format!("t:=formatTime(\"%F\",field={literal},unit=seconds)");
        assert_eq!(
            check(&query, false).outcome,
            Outcome::Ready,
            "{query}: {:?}",
            check(&query, false).diagnostics
        );
    }
}
