use cqlmatic::{
    app,
    assess::{self, AssessRequest},
    contracts::*,
    language::parser,
};
use serde_json::json;

fn check(q: &str) -> Analysis {
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: q.into(),
            context: Context::default(),
        },
        None,
    )
}
fn equivalent(a: &str, b: &str) {
    let left = check(a);
    let right = check(b);
    assert_eq!(
        left.validation.status,
        Acceptance::Accepted,
        "{a}: {:?}",
        left.diagnostics
    );
    assert_eq!(
        right.validation.status,
        Acceptance::Accepted,
        "{b}: {:?}",
        right.diagnostics
    );
    assert_eq!(
        serde_json::to_value(&left.grounding).unwrap(),
        serde_json::to_value(&right.grounding).unwrap(),
        "{a} / {b}"
    );
    let diagnostics = |r: Analysis| {
        r.diagnostics
            .into_iter()
            .filter(|d| {
                !matches!(
                    d.code.as_str(),
                    "CQL_EMPTY_STAGE" | "CQL_NEGATION_NO_EFFECT"
                )
            })
            .map(|d| (d.code, d.message))
            .collect::<Vec<_>>()
    };
    assert_eq!(diagnostics(left), diagnostics(right), "{a} / {b}");
    assert_eq!(
        serde_json::to_value(app::inventory_hints(a).unwrap()).unwrap(),
        serde_json::to_value(app::inventory_hints(b).unwrap()).unwrap(),
        "{a} / {b}"
    );
    let assess = |q: &str| {
        let r = assess::assess(
            AssessRequest {
                schema_version: 1,
                id: None,
                query: q.into(),
                context: Context::default(),
                result: json!({"result":[{"a":"1","x":"1","_count":1}]}),
            },
            None,
        );
        (
            serde_json::to_value(r.findings).unwrap(),
            serde_json::to_value(r.caps).unwrap(),
        )
    };
    assert_eq!(assess(a), assess(b), "{a} / {b}");
    for q in [a, b] {
        let p = parser::parse(q).unwrap();
        let rendered = p.render();
        assert_eq!(parser::parse(&rendered).unwrap().render(), rendered, "{q}");
    }
}
#[test]
fn converted_arguments_have_consistent_consumers() {
    for (a, b) in [
        ("head(10)", "head(\"10\")"),
        ("tail(10)", "tail(\"10\")"),
        ("sort(a, limit=max)", "sort(a, limit=\"max\")"),
        ("table([a], limit=5)", "table([a], limit=\"5\")"),
        ("top(a, limit=5)", "top(a, limit=\"5\")"),
        ("groupBy(a, limit=5)", "groupBy(a, limit=\"5\")"),
        ("collect(a, limit=5)", "collect(a, limit=\"5\")"),
        (
            "join({a=1}, field=a, limit=5)",
            "join({a=1}, field=a, limit=\"5\")",
        ),
        ("bucket(limit=5)", "bucket(limit=\"5\")"),
        ("series(a, maxduration=1h)", "series(a, maxduration=\"1h\")"),
        ("count(a, distinct=true)", "count(a, distinct=\"TRUE\")"),
        (
            "collect(a, multival=false)",
            "collect(a, multival=\"FALSE\")",
        ),
        ("top(a, rest=false)", "top(a, rest=\"FALSE\")"),
        ("in(a, values=[1])", "in(a, values=1)"),
        (
            "in(a, values=[1], ignoreCase=true)",
            "in(a, values=1, ignoreCase=\"TRUE\")",
        ),
        (
            "regex(\"x\", field=a, strict=false)",
            "regex(\"x\", field=a, strict=\"FALSE\")",
        ),
        (
            "\"a[0].x\":=1 | split(a, strip=true) | x=*",
            "\"a[0].x\":=1 | split(a, strip=\"true\") | x=*",
        ),
        ("lower(a, as=123)", "lower(a, as=\"123\")"),
    ] {
        equivalent(a, b);
    }
}
#[test]
fn accepted_forms_have_consistent_consumers() {
    for (a, b) in [
        ("a=\"1\"", "a=?{p=\"1\"}"),
        ("a=x1z", "a=x?{y=1}z"),
        ("a=1", "| a=1"),
        ("a=1", "a=1 |"),
        ("count()", "count() | *"),
        ("a=/x/i", "a=/x/gi"),
        ("a=/(?<x>foo)/i | x=*", "a=/(?<x>foo)/gi | x=*"),
        (r#"a="x\\q""#, r#"a="x\q""#),
        ("in(a, values=[\"1\", \"true\"])", "in(a, values=[1, true])"),
        ("sort(a, order=asc)", "sort(a, order=ascending)"),
        ("sort(a, order=asc)", "sort(a, order=ASCENDING)"),
    ] {
        equivalent(a, b);
    }
    equivalent("in(a, values=[])", "in(a, values=[])");
    assert_eq!(
        parser::parse(r#"a="x\q""#).unwrap().render(),
        r#"a = "x\\q""#
    );
}

#[test]
fn all_transform_steps_share_negation_interpretation() {
    use cqlmatic::{
        catalog::{self, Effect},
        language::ast::{Call, Filter, Step},
    };
    for function in catalog::FUNCTIONS {
        let call = Call::new(function.name, vec![]);
        let step = Step::Filter(Filter::Not(Box::new(Filter::Call(call.clone()))));
        if function.effect == Effect::Transform && !matches!(function.name, "eval" | "ioc:lookup") {
            assert_eq!(
                step.semantic().as_ref(),
                &Step::Call(call),
                "{}",
                function.name
            );
        } else {
            assert_eq!(step.semantic().as_ref(), &step, "{}", function.name);
        }
        assert!(step.render().starts_with('!'));
    }
}
