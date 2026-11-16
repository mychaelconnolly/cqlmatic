use cqlmatic::{app, catalog, contracts::*, environment::Pack};
use serde_json::{Value, json};

const TIME: &[&str] = &[
    "dayOfMonth",
    "dayOfWeek",
    "dayOfWeekName",
    "dayOfYear",
    "hour",
    "millisecond",
    "minute",
    "month",
    "monthName",
    "second",
    "weekOfYear",
    "year",
];

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

fn generate(function: &str, args: Vec<Value>, scalar: bool) -> Analysis {
    let step = if scalar {
        json!({"op":"assign","field":"result","expression":{"kind":"call","function":function,"args":args}})
    } else {
        json!({"op":"call","function":function,"args":args})
    };
    let request = serde_json::from_value(
        json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[step]}}}),
    )
    .unwrap();
    app::generate(request, None)
}

fn arg(name: Option<&str>, value: Value) -> Value {
    let mut argument = json!({"value":value});
    if let Some(name) = name {
        argument["name"] = name.into();
    }
    argument
}

fn literal(value: Value) -> Value {
    json!({"kind":"literal","value":value})
}
fn field(name: &str) -> Value {
    json!({"kind":"field","name":name})
}
fn ready(result: Analysis) {
    assert_eq!(result.outcome, Outcome::Ready, "{result:?}");
    assert_eq!(result.validation.execution, "not-run");
}

#[test]
fn text_filters_keep_expression_inputs_and_filter_only_contexts() {
    for name in ["contains", "startsWith", "endsWith"] {
        let function = format!("text:{name}");
        ready(check(&format!(r#"{function}(user, substring="a")"#)));
        ready(check(&format!(
            r#"!{function}(text:trim(user), substring=other)"#
        )));
        let args = vec![
            arg(None, field("user")),
            arg(Some("substring"), literal(json!("a"))),
        ];
        ready(generate(&function, args.clone(), false));
        for result in [
            check(&format!(r#"x := {function}(user, substring="a")"#)),
            generate(&function, args.clone(), true),
        ] {
            assert_eq!(result.outcome, Outcome::Invalid, "{result:?}");
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "CQL_ASSIGN_ALIAS")
            );
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "CQL_UNSUPPORTED_SCALAR_FUNCTION")
            );
        }
        for query in [
            format!("{function}(user)"),
            format!(r#"{function}(user, "a")"#),
            format!(r#"{function}(user, substring="a", ignoreCase=true)"#),
        ] {
            assert_eq!(check(&query).outcome, Outcome::Invalid, "{query}");
        }
    }
}

#[test]
fn text_computations_share_typed_raw_and_nested_contexts() {
    for (function, args, raw, output) in [
        (
            "text:length",
            vec![arg(None, field("user"))],
            "text:length(user)",
            "_length",
        ),
        (
            "text:trim",
            vec![
                arg(None, literal(json!(" x "))),
                arg(Some("mode"), literal(json!("start"))),
            ],
            r#"text:trim(" x ", mode=start)"#,
            "_trim",
        ),
        (
            "text:substring",
            vec![
                arg(None, field("user")),
                arg(Some("begin"), field("offset")),
            ],
            "text:substring(user, begin=offset)",
            "_string",
        ),
        (
            "text:positionOf",
            vec![
                arg(None, field("user")),
                arg(Some("character"), literal(json!("@"))),
            ],
            r#"text:positionOf(user, character="@")"#,
            "_position",
        ),
        (
            "text:editDistance",
            vec![
                arg(Some("target"), field("user")),
                arg(Some("reference"), literal(json!("alice"))),
                arg(Some("maxDistance"), literal(json!(5))),
            ],
            r#"text:editDistance(target=user, reference="alice", maxDistance=5)"#,
            "_distance",
        ),
    ] {
        ready(check(raw));
        ready(check(&format!("result := {raw}")));
        ready(generate(function, args.clone(), false));
        ready(generate(function, args, true));
        let function = catalog::function(function).unwrap();
        assert_eq!(
            function.default_output(&Default::default()).as_deref(),
            Some(output)
        );
    }
    ready(check(
        r#"text:substring(user, begin=text:positionOf(user,character=",")+1, end=text:length(user)-1,as=part) | text:trim(part,mode=end,as=clean)"#,
    ));
    ready(check(
        r#"text:editDistance(target=text:trim(user),reference=other,maxDistance=100,allowTranspositions=false,ignoreCase=true,as=distance)"#,
    ));
    for query in [
        "text:trim(user,mode=middle)",
        "text:length()",
        "text:positionOf(user)",
        r#"text:editDistance(target=user,reference="x",maxDistance=0)"#,
        r#"text:editDistance(target=user,reference="x",maxDistance=101)"#,
        r#"text:editDistance(target=user,reference="x",maxDistance=1.5)"#,
        r#"text:editDistance(user,reference="x",maxDistance=5)"#,
    ] {
        assert_eq!(check(query).outcome, Outcome::Invalid, "{query}");
    }
}

#[test]
fn object_array_distances_have_bounded_expression_lists_and_no_scalar_result() {
    let args = vec![
        arg(Some("target"), field("user")),
        arg(
            Some("references"),
            json!({"kind":"array","items":[literal(json!("alice")),field("other")]}),
        ),
        arg(Some("maxDistance"), literal(json!(5))),
        arg(Some("asArray"), literal(json!("distances[]"))),
    ];
    ready(generate("text:editDistanceAsArray", args.clone(), false));
    ready(check(
        r#"text:editDistanceAsArray(target=user,references=["alice",text:trim(other)],maxDistance=5,asArray="distances[]",ignoreCase=true,allowTranspositions=false) | distances[0].distance=*"#,
    ));
    let scalar = generate("text:editDistanceAsArray", args, true);
    assert_eq!(scalar.outcome, Outcome::Invalid, "{scalar:?}");
    assert!(
        scalar
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_ASSIGN_ALIAS")
    );
    for references in ["[]", "[{}]", "[[\"alice\"]]", "\"alice\""] {
        assert_ne!(
            check(&format!(
                "text:editDistanceAsArray(target=user,references={references},maxDistance=5)"
            ))
            .outcome,
            Outcome::Ready
        );
    }
    let references = std::iter::repeat_n("\"alice\"", 101)
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        check(&format!(
            "text:editDistanceAsArray(target=user,references=[{references}],maxDistance=5)"
        ))
        .outcome,
        Outcome::Invalid
    );
    for prefix in ["#tag", ""] {
        assert_eq!(check(&format!(r#"text:editDistanceAsArray(target=user,references=["alice"],maxDistance=5,asArray="{prefix}")"#)).outcome,Outcome::Invalid);
    }
    ready(check(
        r#"text:editDistanceAsArray(target=user,references=["alice"],maxDistance=5,asArray="out[]")"#,
    ));
}

#[test]
fn calendar_functions_cover_defaults_options_and_scalar_assignments() {
    for name in TIME {
        let function = format!("time:{name}");
        ready(check(&format!("{function}(@timestamp)")));
        ready(check(&format!(
            "result := {function}(field=stamp,timezone=\"-01:00\",timezoneField=zone)"
        )));
        let mut args = vec![
            arg(None, literal(json!("stamp"))),
            arg(Some("timezoneField"), literal(json!("zone"))),
            arg(Some("as"), literal(json!("component"))),
        ];
        ready(generate(&function, args.clone(), false));
        let conflict = generate(&function, args.clone(), true);
        assert_eq!(conflict.outcome, Outcome::Invalid);
        assert!(
            conflict
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_ASSIGN_ALIAS")
        );
        args.retain(|argument| argument["name"] != "as");
        ready(generate(&function, args, true));
        if *name == "dayOfWeek" {
            assert_eq!(check(&format!("{function}()")).outcome, Outcome::Invalid);
        } else {
            ready(check(&format!("{function}()")));
            ready(generate(&function, vec![], true));
        }
        assert_eq!(
            catalog::function(&function)
                .unwrap()
                .default_output(&Default::default()),
            Some(format!("_{name}"))
        );
        assert_eq!(
            check(&format!("{function}(field=1)")).outcome,
            Outcome::Invalid
        );
        assert_eq!(
            check(&format!("{function}(field=stamp,timezoneField=1)")).outcome,
            Outcome::Invalid
        );
        assert_eq!(
            check(&format!("{function}(@timestamp,unit=seconds)")).outcome,
            Outcome::Invalid
        );
    }
}

#[test]
fn default_dependencies_timezone_precedence_and_output_members_keep_grounding() {
    let pack: Pack =
        serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    let strict = |query: &str| {
        app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                context: Context {
                    require_observed: true,
                    ..Context::default()
                },
            },
            Some(&pack),
        )
    };
    for query in [
        r#"#repo=example | select([user]) | text:length(user) | _length=*"#,
        r#"#repo=example | select([user]) | text:editDistanceAsArray(target=user,references=["alice"],maxDistance=5) | _distance[0].distance=* | _distance[0].reference=*"#,
        r#"#repo=example | select([@timestamp]) | time:hour(timezone="UTC",timezoneField=unobserved) | _hour=*"#,
    ] {
        ready(strict(query));
    }
    let result = strict(r#"#repo=example | time:hour(timezoneField=unobserved)"#);
    assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
    assert!(
        result
            .grounding
            .basis
            .keys()
            .any(|key| key.ends_with(":unobserved"))
    );
    let result = strict(
        r#"#repo=example | text:editDistanceAsArray(target=unobserved,references=["alice"],maxDistance=5) | _distance[0].distance=*"#,
    );
    assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
    for name in TIME {
        let function = catalog::function(&format!("time:{name}")).unwrap();
        let fields = function.default_read_fields(&Default::default());
        assert!(fields.contains(&"@timestamp"));
        assert_eq!(fields.contains(&"@timezone"), *name != "dayOfMonth");
    }
}

#[test]
fn every_text_time_function_has_public_catalog_attribution() {
    for function in catalog::FUNCTIONS
        .iter()
        .filter(|f| f.name.starts_with("text:") || f.name.starts_with("time:"))
    {
        let record = catalog::records()
            .iter()
            .find(|record| record.id == function.reference_id)
            .unwrap();
        assert_eq!(record.sources.len(), 1);
        assert!(record.reference.starts_with("https://library.humio.com/"));
        assert!(
            catalog::search(function.reference_id, 100, false)
                .iter()
                .any(|record| record["id"] == function.reference_id && record["supported"] == true)
        );
    }
}

#[test]
fn filters_and_computations_preserve_nested_query_effect_boundaries() {
    ready(check("!text:trim(user)"));
    ready(check(
        r#"text:editDistance(target=user,reference="x",maxDistance=5,ignoreCase="true")"#,
    ));
    ready(check(
        r#"selfJoinFilter(user,where=[{text:contains(user,substring="a")}])"#,
    ));
    let request = serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"filters":[{"op":"call","function":"text:startsWith","negate":true,"args":[arg(None,field("user")),arg(Some("substring"),literal(json!("admin")))]}]}}})).unwrap();
    let generated = app::generate(request, None);
    assert_eq!(generated.outcome, Outcome::Ready, "{generated:?}");
    ready(check(generated.query.as_deref().unwrap()));
    for query in [
        "selfJoinFilter(user,where=[{time:hour(@timestamp)}])",
        r#"groupBy(user,function=text:length(user))"#,
    ] {
        assert_ne!(check(query).outcome, Outcome::Ready, "{query}");
    }
}
