use cqlmatic::{app, catalog, contracts::*, environment::Pack};
use serde_json::{Value, json};

const MATH: &[&str] = &[
    "abs", "arccos", "arcsin", "arctan", "arctan2", "ceil", "cos", "cosh", "deg2rad", "exp",
    "expm1", "floor", "log", "log10", "log1p", "log2", "mod", "pow", "rad2deg", "sin", "sinh",
    "sqrt", "tan", "tanh",
];

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

fn args(name: &str, named: bool) -> String {
    match name {
        "arctan2" => "x=user,y=user".into(),
        "mod" if !named => "user,3".into(),
        "mod" => "field=user,divisor=3".into(),
        "pow" => "field=user,exponent=3".into(),
        _ if named => "field=user".into(),
        _ => "user".into(),
    }
}

#[test]
fn every_math_function_has_lookup_and_checks_named_positional_scalar_and_output_forms() {
    assert_eq!(
        catalog::FUNCTIONS
            .iter()
            .filter(|f| f.name.starts_with("math:"))
            .count(),
        24
    );
    for name in MATH {
        let function = format!("math:{name}");
        let spec = catalog::function(&function).unwrap();
        assert!(spec.scalar_computation());
        assert!(
            catalog::search(&function, 100, true)
                .iter()
                .any(|r| r["id"] == spec.reference_id)
        );
        for named in [false, true] {
            let args = args(name, named);
            for query in [
                format!("#repo=example | select([user]) | {function}({args}) | _{name}=*"),
                format!("#repo=example | select([user]) | {function}({args},as=result) | result=*"),
                format!("#repo=example | select([user]) | result:={function}({args}) | result=*"),
                format!("#repo=example | select([user]) | result:={function}({args})+1 | result=*"),
            ] {
                let result = check(&query, true);
                assert_eq!(
                    result.outcome,
                    Outcome::Ready,
                    "{query}: {:?} {:?}",
                    result.diagnostics,
                    result.grounding
                );
            }
            for query in [
                format!("{function}({args},unknown=true)"),
                format!("{function}({args},as=\"#tag\")"),
                format!("stats({function}({args}))"),
                format!("!{function}({args})"),
                format!("{function}()"),
            ] {
                assert_ne!(check(&query, false).outcome, Outcome::Ready, "{query}");
            }
        }
    }
    assert_eq!(
        check(
            "math:spherical2cartesian(azimuth=user,polar=user,radius=user)",
            false
        )
        .outcome,
        Outcome::Unsupported
    );
}

#[test]
fn typed_generation_uses_the_same_contract_and_rejects_predicate_context() {
    let lit = |value: Value| json!({"kind":"literal","value":value});
    let arg = |name: &str, value: Value| json!({"name":name,"value":value});
    for name in MATH {
        let function = format!("math:{name}");
        let args = match *name {
            "arctan2" => vec![arg("x", lit(json!("user"))), arg("y", lit(json!("user")))],
            "mod" => vec![
                arg("field", lit(json!("user"))),
                arg("divisor", lit(json!(3))),
            ],
            "pow" => vec![
                arg("field", lit(json!("user"))),
                arg("exponent", lit(json!(3))),
            ],
            _ => vec![arg("field", lit(json!("user")))],
        };
        for step in [
            json!({"op":"call","function":function,"args":args}),
            json!({"op":"assign","field":"result","expression":{"kind":"call","function":function,"args":args}}),
        ] {
            let request: GenerateRequest = serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[step]}}})).unwrap();
            let result = app::generate(request, None);
            assert_eq!(
                result.outcome,
                Outcome::Ready,
                "{function}: {:?}",
                result.diagnostics
            );
            assert!(result.query.unwrap().contains(&function));
        }
        let request: GenerateRequest = serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"filters":[{"op":"call","function":function,"args":args}]}}})).unwrap();
        let result = app::generate(request, None);
        assert_eq!(
            result.outcome,
            Outcome::Invalid,
            "{function}: {:?}",
            result.diagnostics
        );
        assert!(result.query.is_none());
    }
}

#[test]
fn numeric_operands_and_grounding_remain_explicit() {
    for query in [
        "math:pow(user,exponent=-2)",
        "math:pow(user,exponent=0.5)",
        "math:mod(user,divisor=-3)",
        "math:mod(user,divisor=2.5)",
        "math:pow(user,exponent=user+1)",
        "math:mod(user,divisor=user)",
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
        "math:sqrt(user+1)",
        "math:sqrt([user])",
        "math:sqrt(4)",
        "math:arctan2(user,user)",
        "math:arctan2(x=user)",
        "math:pow(user,2)",
        "math:pow(user,exponent=true)",
        "math:pow(user,exponent=\"bad\")",
        "math:mod(user,divisor=[2])",
        "math:mod(user,divisor=true+1)",
        "math:pow(user,exponent=user==1)",
        "math:sqrt(user,field=user)",
    ] {
        assert_ne!(check(query, false).outcome, Outcome::Ready, "{query}");
    }
    for query in [
        "#repo=example | math:sqrt(user) | _sqrt=*",
        "#repo=example | math:sqrt(unmeasured) | _sqrt=*",
        "#repo=example | math:arctan2(x=user,y=unmeasured)",
        "#repo=example | math:pow(user,exponent=unmeasured)",
        "#repo=example | math:mod(user,divisor=unmeasured)",
        "#repo=example | math:sqrt(user) | select([_sqrt]) | user=*",
        "#repo=example | result:=math:sqrt(user) | _sqrt=*",
    ] {
        assert_eq!(check(query, true).outcome, Outcome::NeedsContext, "{query}");
    }
    let result = check("math:pow(user,exponent=0.5)", false);
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_MATH_PARAMETER_CONTRACT")
    );
    assert!(
        !check("math:pow(user,exponent=-2)", false)
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_MATH_PARAMETER_CONTRACT")
    );
}
