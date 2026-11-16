use std::collections::BTreeSet;

use cqlmatic::{
    app, catalog,
    contracts::*,
    language::{ast, parser},
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixtures {
    schema_version: u32,
    qualification: String,
    cases: Vec<Case>,
    controls: Vec<Control>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    function: String,
    primary: bool,
    forms: Vec<String>,
    placement: String,
    query: String,
    canonical: String,
    inputs: Option<Vec<String>>,
    outputs: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    id: String,
    function: Option<String>,
    #[serde(default)]
    forms: Vec<String>,
    query: String,
    outcome: Outcome,
    diagnostic: Option<String>,
    #[serde(default)]
    typed: bool,
}

fn fixtures() -> Fixtures {
    serde_json::from_str(include_str!("fixtures/function-coverage.json")).unwrap()
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

fn generate(pipeline: &ast::Pipeline) -> Result<Analysis, String> {
    Ok(app::generate(
        GenerateRequest {
            schema_version: 1,
            id: None,
            context: Context::default(),
            input: GenerateInput::Plan {
                plan: typed_plan(pipeline)?,
            },
        },
        None,
    ))
}

// This converter is limited to the fixture syntax and still uses public typed lowering.
fn typed_plan(pipeline: &ast::Pipeline) -> Result<Plan, String> {
    let mut plan = Plan::default();
    for step in &pipeline.steps {
        plan.steps.push(match step {
            ast::Step::Call(call) => Stage::Call {
                function: call.name.clone(),
                args: typed_args(call)?,
            },
            ast::Step::Assign { field, value } => Stage::Assign {
                field: field.clone(),
                expression: typed_value(value, catalog::ArgumentRole::Expression)?,
            },
            ast::Step::Filter(filter) => Stage::Filter {
                predicate: typed_filter(filter)?,
            },
            ast::Step::Link { field, target } => {
                let (to, key) = target
                    .split_once('.')
                    .ok_or("fixture link needs label.key")?;
                Stage::Link {
                    field: field.clone(),
                    to: to.into(),
                    key: key.into(),
                }
            }
            _ => return Err(format!("fixture has an unhandled stage: {step:?}")),
        });
    }
    Ok(plan)
}

fn typed_args(call: &ast::Call) -> Result<Vec<Argument>, String> {
    let resolved =
        catalog::resolve(call).ok_or_else(|| format!("unknown fixture function {}", call.name))?;
    call.args
        .iter()
        .map(|arg| {
            let name = resolved
                .ordered
                .iter()
                .find(|(_, value)| std::ptr::eq(*value, &arg.value))
                .map(|(name, _)| name.as_str())
                .unwrap_or("query");
            let value = if resolved.function.argument_scope(name)
                == catalog::ArgumentScope::Aggregate
                && let ast::Value::Word(function) = &arg.value
                && catalog::aggregate_value_outputs(&arg.value).is_some()
            {
                Expression::Call {
                    function: function.clone(),
                    args: vec![],
                }
            } else {
                typed_value(&arg.value, resolved.function.argument_role(name))?
            };
            Ok(Argument {
                name: arg.name.clone(),
                value,
            })
        })
        .collect()
}

fn typed_value(value: &ast::Value, role: catalog::ArgumentRole) -> Result<Expression, String> {
    let literal = |value| Expression::Literal { value };
    Ok(match value {
        ast::Value::Word(name)
            if matches!(
                role,
                catalog::ArgumentRole::Expression | catalog::ArgumentRole::InputField
            ) =>
        {
            Expression::Field { name: name.clone() }
        }
        ast::Value::Word(text) | ast::Value::String(text) => literal(Scalar::Text(text.clone())),
        ast::Value::Number(number) => literal(Scalar::Number(
            number.parse().map_err(|_| "fixture number is invalid")?,
        )),
        ast::Value::Bool(value) => literal(Scalar::Boolean(*value)),
        ast::Value::Array(values) => Expression::Array {
            items: values
                .iter()
                .map(|value| typed_value(value, role))
                .collect::<Result<_, _>>()?,
        },
        ast::Value::Call(call) => Expression::Call {
            function: call.name.clone(),
            args: typed_args(call)?,
        },
        ast::Value::Query {
            label: None,
            pipeline,
            attributes,
        } if attributes.is_empty() => Expression::Query {
            plan: Box::new(typed_plan(pipeline)?),
        },
        ast::Value::Query {
            label: Some(name),
            pipeline,
            attributes,
        } => {
            let mut include = None;
            for attribute in attributes {
                match attribute.name.as_deref() {
                    Some("include") if catalog::query_all_fields(&attribute.value) => {
                        include = Some(QueryFields::All)
                    }
                    Some("include") => {
                        include = Some(QueryFields::Fields {
                            fields: catalog::fields(&attribute.value).into_iter().collect(),
                        })
                    }
                    _ => {
                        return Err(
                            "fixture named-query attribute needs an explicit typed conversion"
                                .into(),
                        );
                    }
                }
            }
            Expression::NamedQuery {
                name: name.clone(),
                plan: Box::new(typed_plan(pipeline)?),
                include,
                after: None,
                before: None,
            }
        }
        ast::Value::Binary { left, op, right } => {
            let left = Box::new(typed_value(left, catalog::ArgumentRole::Expression)?);
            let right = Box::new(typed_value(right, catalog::ArgumentRole::Expression)?);
            if matches!(op.as_str(), "==" | "!=" | "<" | "<=" | ">" | ">=") {
                Expression::Compare {
                    left,
                    operator: comparison(op)?,
                    right,
                }
            } else {
                Expression::Binary {
                    left,
                    operator: match op.as_str() {
                        "+" => Arithmetic::Add,
                        "-" => Arithmetic::Subtract,
                        "*" => Arithmetic::Multiply,
                        "/" => Arithmetic::Divide,
                        "%" => Arithmetic::Remainder,
                        _ => return Err("fixture arithmetic is outside converter".into()),
                    },
                    right,
                }
            }
        }
        ast::Value::Unary { op, value } => Expression::Unary {
            operator: match op.as_str() {
                "+" => Unary::Positive,
                "-" => Unary::Negative,
                "!" => Unary::Not,
                _ => return Err("fixture unary operation is outside converter".into()),
            },
            value: Box::new(typed_value(value, catalog::ArgumentRole::Expression)?),
        },
        _ => return Err("fixture value is outside the explicit converter".into()),
    })
}

fn comparison(operator: &str) -> Result<Comparison, String> {
    Ok(match operator {
        "=" | "==" => Comparison::Eq,
        "!=" => Comparison::Ne,
        "<" => Comparison::Lt,
        "<=" => Comparison::Le,
        ">" => Comparison::Gt,
        ">=" => Comparison::Ge,
        _ => return Err("fixture comparison is outside converter".into()),
    })
}

fn typed_filter(filter: &ast::Filter) -> Result<Predicate, String> {
    Ok(match filter {
        ast::Filter::Call(call) => Predicate::Call {
            function: call.name.clone(),
            args: typed_args(call)?,
            negate: false,
        },
        ast::Filter::Not(filter) => {
            let ast::Filter::Call(call) = filter.as_ref() else {
                return Err("fixture negation must be a function predicate".into());
            };
            Predicate::Call {
                function: call.name.clone(),
                args: typed_args(call)?,
                negate: true,
            }
        }
        ast::Filter::Compare {
            field,
            op,
            value: ast::Pattern::Text(value),
        } if op == "=" => Predicate::Eq {
            field: field.clone(),
            value: Scalar::Text(value.clone()),
        },
        ast::Filter::And(filters) => Predicate::All {
            items: filters.iter().map(typed_filter).collect::<Result<_, _>>()?,
        },
        ast::Filter::Or(filters) => Predicate::Any {
            items: filters.iter().map(typed_filter).collect::<Result<_, _>>()?,
        },
        _ => return Err("fixture filter is outside the explicit converter".into()),
    })
}

fn calls<'a>(pipeline: &'a ast::Pipeline, found: &mut Vec<&'a ast::Call>) {
    fn value<'a>(item: &'a ast::Value, found: &mut Vec<&'a ast::Call>) {
        match item {
            ast::Value::Call(call) => visit(call, found),
            ast::Value::Query { pipeline, .. } => calls(pipeline, found),
            ast::Value::Array(values) => {
                for item in values {
                    value(item, found);
                }
            }
            ast::Value::Binary { left, right, .. } => {
                value(left, found);
                value(right, found);
            }
            ast::Value::Unary { value: item, .. } => value(item, found),
            _ => {}
        }
    }
    fn visit<'a>(call: &'a ast::Call, found: &mut Vec<&'a ast::Call>) {
        found.push(call);
        for arg in &call.args {
            value(&arg.value, found);
        }
    }
    fn filter<'a>(item: &'a ast::Filter, found: &mut Vec<&'a ast::Call>) {
        match item {
            ast::Filter::Call(call) => visit(call, found),
            ast::Filter::And(items) | ast::Filter::Or(items) => {
                for item in items {
                    filter(item, found);
                }
            }
            ast::Filter::Not(item) => filter(item, found),
            _ => {}
        }
    }
    for step in &pipeline.steps {
        match step {
            ast::Step::Call(call) => visit(call, found),
            ast::Step::Assign { value: item, .. } => value(item, found),
            ast::Step::Filter(item) => filter(item, found),
            ast::Step::Case(branches) => {
                for branch in branches {
                    calls(branch, found);
                }
            }
            ast::Step::Match { arms, .. } => {
                for (_, branch) in arms {
                    calls(branch, found);
                }
            }
            _ => {}
        }
    }
}

fn expression_inputs(value: &ast::Value, found: &mut BTreeSet<String>) {
    match value {
        ast::Value::Word(field) => {
            found.insert(field.clone());
        }
        ast::Value::Call(call) => found.extend(input_names(call)),
        ast::Value::Array(values) => {
            for item in values {
                expression_inputs(item, found);
            }
        }
        ast::Value::Binary { left, right, .. } => {
            expression_inputs(left, found);
            expression_inputs(right, found);
        }
        ast::Value::Unary { value, .. } => expression_inputs(value, found),
        _ => {}
    }
}

fn input_names(call: &ast::Call) -> BTreeSet<String> {
    let resolved = catalog::resolve(call).unwrap();
    let mut inputs = resolved
        .function
        .default_read_fields(&resolved.arguments)
        .iter()
        .map(|field| (*field).into())
        .collect::<BTreeSet<_>>();
    for (name, value) in &resolved.ordered {
        match resolved.function.argument_role(name) {
            catalog::ArgumentRole::InputField => inputs.extend(catalog::fields(value)),
            catalog::ArgumentRole::Expression => expression_inputs(value, &mut inputs),
            catalog::ArgumentRole::Renames => inputs.extend(
                catalog::rename_bindings(&resolved.arguments)
                    .into_iter()
                    .map(|(old, _)| old.into()),
            ),
            _ => {}
        }
    }
    if resolved.function.name == "getField"
        && let Some(ast::Value::String(field)) = resolved.arguments.get("source").copied()
    {
        inputs.insert(field.clone());
    }
    inputs
}

#[test]
fn fixture_inventory_covers_every_query_candidate_and_links_to_qualified_forms() {
    let fixture = fixtures();
    assert_eq!(fixture.schema_version, 1);
    assert!(fixture.qualification.contains("offline"));
    let ledger: Value = serde_json::from_str(include_str!("../data/function-scope.json")).unwrap();
    let candidates = ledger["functions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|function| function["status"] == "query_candidate")
        .collect::<Vec<_>>();
    let expected = candidates
        .iter()
        .map(|function| function["canonical_name"].as_str().unwrap())
        .collect::<BTreeSet<_>>();
    let primary = fixture
        .cases
        .iter()
        .filter(|case| case.primary)
        .map(|case| case.function.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(primary, expected);
    assert_eq!(
        fixture.cases.iter().filter(|case| case.primary).count(),
        194
    );
    assert!(
        fixture
            .controls
            .iter()
            .any(|control| control.outcome == Outcome::Invalid)
    );
    assert!(
        fixture
            .controls
            .iter()
            .any(|control| control.outcome == Outcome::Unsupported)
    );
    let mut ids = BTreeSet::new();
    for case in &fixture.cases {
        assert!(ids.insert(&case.id), "duplicate fixture {}", case.id);
        assert!(!case.placement.is_empty());
        assert!(!case.forms.is_empty());
        let function = candidates
            .iter()
            .find(|function| function["canonical_name"] == case.function)
            .unwrap();
        for form in &case.forms {
            assert!(
                function["forms"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|row| row["id"] == *form),
                "{}: unknown qualified form {form}",
                case.id
            );
        }
        let contract = catalog::function(&case.function).unwrap();
        assert!(
            catalog::search(contract.reference_id, 100, true)
                .iter()
                .any(|record| record["id"] == contract.reference_id),
            "{}: lookup missing",
            case.id
        );
    }
    for control in &fixture.controls {
        assert!(ids.insert(&control.id), "duplicate control {}", control.id);
        if let Some(name) = &control.function {
            let function = candidates
                .iter()
                .find(|function| function["canonical_name"] == *name)
                .unwrap();
            assert!(!control.forms.is_empty());
            for form in &control.forms {
                assert!(
                    function["forms"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|row| row["id"] == *form),
                    "{}: unknown control form {form}",
                    control.id
                );
            }
        } else {
            assert!(control.forms.is_empty());
        }
    }
}

#[test]
fn qualified_synthetic_forms_check_and_round_trip_through_public_typed_lowering() {
    let mut failures = Vec::new();
    for case in fixtures().cases {
        let checked = check(&case.query);
        if checked.outcome != Outcome::Ready {
            failures.push(format!(
                "{} raw {:?}: {:?}",
                case.id, checked.outcome, checked.diagnostics
            ));
        }
        let pipeline = match parser::parse(&case.query) {
            Ok(pipeline) => pipeline,
            Err(error) => {
                failures.push(format!("{} parse: {error:?}", case.id));
                continue;
            }
        };
        match generate(&pipeline) {
            Ok(generated) if generated.outcome == Outcome::Ready => {
                if generated.query.as_deref() != Some(case.canonical.as_str()) {
                    failures.push(format!(
                        "{} canonical: expected {:?}, got {:?}",
                        case.id, case.canonical, generated.query
                    ));
                }
                let rechecked = check(generated.query.as_deref().unwrap());
                if rechecked.outcome != Outcome::Ready {
                    failures.push(format!(
                        "{} generated recheck: {:?}",
                        case.id, rechecked.diagnostics
                    ));
                }
                assert_eq!(generated.validation.execution, "not-run");
            }
            Ok(generated) => failures.push(format!(
                "{} typed {:?}: {:?}",
                case.id, generated.outcome, generated.diagnostics
            )),
            Err(error) => failures.push(format!("{} converter: {error}", case.id)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn field_and_output_names_follow_the_qualified_argument_roles() {
    let mut failures = Vec::new();
    for case in fixtures().cases {
        let Ok(pipeline) = parser::parse(&case.query) else {
            continue;
        };
        let mut found = Vec::new();
        calls(&pipeline, &mut found);
        let Some(call) = found
            .into_iter()
            .find(|call| call.name.eq_ignore_ascii_case(&case.function))
        else {
            failures.push(format!("{}: target call absent", case.id));
            continue;
        };
        if let Some(expected) = case.inputs {
            let actual = input_names(call);
            let expected = expected.into_iter().collect::<BTreeSet<_>>();
            if actual != expected {
                failures.push(format!(
                    "{} inputs: expected {expected:?}, got {actual:?}",
                    case.id
                ));
            }
        }
        if let Some(expected) = case.outputs {
            let mut actual = catalog::output_writes(call)
                .map(|writes| writes.fields)
                .unwrap_or_default();
            if let Some((outputs, _)) = catalog::aggregate_outputs(call) {
                actual.extend(outputs);
            }
            for output in expected {
                if !actual.contains(&output) {
                    failures.push(format!(
                        "{}: expected output {output:?} missing from {actual:?}",
                        case.id
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn invalid_categories_and_unresolved_forms_keep_their_distinct_outcomes() {
    let mut failures = Vec::new();
    for control in fixtures().controls {
        let checked = check(&control.query);
        if checked.outcome != control.outcome
            || control.diagnostic.as_ref().is_some_and(|expected| {
                !checked
                    .diagnostics
                    .iter()
                    .any(|diagnostic| &diagnostic.code == expected)
            })
        {
            failures.push(format!(
                "{}: expected {:?}/{:?}; got {:?}: {:?}",
                control.id,
                control.outcome,
                control.diagnostic,
                checked.outcome,
                checked.diagnostics
            ));
        }
        if control.typed {
            match parser::parse(&control.query)
                .map_err(|error| format!("{error:?}"))
                .and_then(|pipeline| generate(&pipeline))
            {
                Ok(generated) if generated.outcome == control.outcome => {
                    assert!(generated.query.is_none());
                }
                other => failures.push(format!("{} typed: {other:?}", control.id)),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
