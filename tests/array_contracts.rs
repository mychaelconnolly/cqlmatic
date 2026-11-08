use cqlmatic::{app, contracts::*};

fn context() -> Context {
    Context {
        confirmed_fields: ["user", "kind", "aid"]
            .into_iter()
            .map(str::to_string)
            .collect(),
        ..Context::default()
    }
}

fn check(query: &str) -> Analysis {
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: context(),
        },
        None,
    )
}

fn generate(steps: Vec<Stage>) -> Analysis {
    app::generate(
        GenerateRequest {
            schema_version: 1,
            id: None,
            input: GenerateInput::Plan {
                plan: Plan {
                    steps,
                    ..Plan::default()
                },
            },
            context: context(),
        },
        None,
    )
}

fn text(value: &str) -> Expression {
    Expression::Literal {
        value: Scalar::Text(value.into()),
    }
}

fn named(name: &str, value: Expression) -> Argument {
    Argument {
        name: Some(name.into()),
        value,
    }
}

fn call(function: &str, args: Vec<Argument>) -> Stage {
    Stage::Call {
        function: function.into(),
        args,
    }
}

fn query(steps: Vec<Stage>) -> Expression {
    Expression::Query {
        plan: Box::new(Plan {
            filters: vec![Predicate::Eq {
                field: "kind".into(),
                value: Scalar::Text("login".into()),
            }],
            steps,
            ..Plan::default()
        }),
    }
}

fn array_call(function: &str, argument: &str, value: Expression) -> Stage {
    let mut args = vec![named(argument, value)];
    if matches!(function, "in" | "selfJoinFilter") {
        args.push(named("field", text("user")));
    }
    call(function, args)
}

#[test]
fn checked_string_arrays_require_string_elements() {
    for (function, prefix) in [
        ("createEvents", ""),
        ("in", "field=user, values="),
        ("parseJson", "removePrefixes="),
    ] {
        let valid = format!("{function}({prefix}[\"alice\", bob])");
        assert_eq!(check(&valid).outcome, Outcome::Ready, "{valid}");
        for element in [
            "{ count() }",
            "count()",
            "1 + 2",
            "[\"nested\"]",
            "1",
            "true",
            "A: { user=alice }",
        ] {
            let query = format!("{function}({prefix}[\"valid\", {element}])");
            let result = check(&query);
            assert_eq!(result.outcome, Outcome::Invalid, "{query}");
            assert_eq!(result.validation.status, Acceptance::Rejected);
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "CQL_ARGUMENT_TYPE")
            );
        }
        let scalar = format!("{function}({prefix}\"alice\")");
        assert_eq!(check(&scalar).outcome, Outcome::Invalid, "{scalar}");
    }
    assert_eq!(
        check(
            r#"createEvents(["{\"user\":\"alice\"}"]) | parseJson() | in(user, values=["alice"])"#
        )
        .outcome,
        Outcome::Ready
    );
    assert_eq!(
        check("parseJson(removePrefixes=[])").outcome,
        Outcome::Ready
    );
}

#[test]
fn create_events_uses_the_documented_named_argument() {
    for argument in [None, Some("rawstring")] {
        let prefix = argument.map(|name| format!("{name}=")).unwrap_or_default();
        let checked = check(&format!("createEvents({prefix}[\"user=alice\"])"));
        assert_eq!(checked.outcome, Outcome::Ready);
        let generated = generate(vec![call(
            "createEvents",
            vec![Argument {
                name: argument.map(str::to_string),
                value: Expression::Array {
                    items: vec![text("user=alice")],
                },
            }],
        )]);
        assert_eq!(generated.outcome, Outcome::Ready);
        assert!(generated.query.is_some());
    }
    let checked = check(r#"createEvents(raw=["user=alice"])"#);
    let generated = generate(vec![array_call(
        "createEvents",
        "raw",
        Expression::Array {
            items: vec![text("user=alice")],
        },
    )]);
    for result in [checked, generated] {
        assert_eq!(result.outcome, Outcome::Invalid);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_ARGUMENT_UNKNOWN")
        );
        assert!(result.query.is_none());
    }
}

#[test]
fn typed_string_arrays_cannot_emit_nested_executable_values_or_coercions() {
    let malformed = [
        query(vec![call("count", vec![])]),
        Expression::Call {
            function: "count".into(),
            args: vec![],
        },
        Expression::Binary {
            left: Box::new(text("left")),
            operator: Arithmetic::Add,
            right: Box::new(text("right")),
        },
        Expression::Array {
            items: vec![text("nested")],
        },
        Expression::Literal {
            value: Scalar::Number(1.into()),
        },
        Expression::Literal {
            value: Scalar::Boolean(true),
        },
    ];
    for (function, argument) in [
        ("createEvents", "rawstring"),
        ("in", "values"),
        ("parseJson", "removePrefixes"),
    ] {
        let valid = generate(vec![array_call(
            function,
            argument,
            Expression::Array {
                items: vec![text("alice"), text("bob")],
            },
        )]);
        assert_eq!(valid.outcome, Outcome::Ready, "{function}");
        assert!(valid.query.is_some());
        for value in &malformed {
            let result = generate(vec![array_call(
                function,
                argument,
                Expression::Array {
                    items: vec![text("valid"), value.clone()],
                },
            )]);
            assert_eq!(result.outcome, Outcome::Invalid, "{function}: {value:?}");
            assert_eq!(result.validation.status, Acceptance::Rejected);
            assert!(result.query.is_none());
        }
    }
}

#[test]
fn checked_self_join_requires_an_array_of_unlabeled_filter_queries() {
    for clauses in [
        r#"[{ kind=login }, { in(user, values=["alice"]) | test(true) }]"#,
        r#"[{ user=alice OR user=bob }, { not wildcard("admin*", field=user) }]"#,
        r#"[{ selfJoinFilter(user, where=[{ kind=login }]) }]"#,
    ] {
        let query = format!("selfJoinFilter(field=[user], where={clauses})");
        assert_eq!(check(&query).outcome, Outcome::Ready, "{query}");
    }
    for clauses in [
        "[1,2]",
        r#"["kind=login"]"#,
        r#"[in(user, values=["alice"])]"#,
        "[[{ kind=login }]]",
        "[{ kind=login }, true]",
        "{ kind=login }",
        "[A: { kind=login }]",
    ] {
        let query = format!("selfJoinFilter(field=[user], where={clauses})");
        assert_eq!(check(&query).outcome, Outcome::Invalid, "{query}");
    }
    for branch in [
        "count()",
        "parseJson()",
        r#"createEvents(["a=1"])"#,
        r#"defineTable(name="t", query={ user=alice })"#,
        r#"user := "alice""#,
        "case { user=alice; user=bob }",
        "user match { alice => true; * => true }",
        "aid <=> A.aid",
        "count() AND true",
        "selfJoinFilter(user, where=[{ count() }])",
    ] {
        let query = format!("selfJoinFilter(field=[user], where=[{{ {branch} }}])");
        let result = check(&query);
        assert_eq!(result.outcome, Outcome::Invalid, "{query}");
        assert_eq!(result.validation.status, Acceptance::Rejected);
    }
    assert_eq!(
        check("selfJoinFilter(user, where=[{ unknownFilter() }])").outcome,
        Outcome::Unsupported
    );
}

#[test]
fn typed_self_join_queries_preserve_the_filter_context() {
    let filter = query(vec![call(
        "in",
        vec![
            named("field", text("user")),
            named(
                "values",
                Expression::Array {
                    items: vec![text("alice")],
                },
            ),
        ],
    )]);
    let valid = generate(vec![array_call(
        "selfJoinFilter",
        "where",
        Expression::Array {
            items: vec![filter],
        },
    )]);
    assert_eq!(valid.outcome, Outcome::Ready);
    assert!(valid.query.is_some());
    let declaration = call(
        "defineTable",
        vec![named("name", text("t")), named("query", query(vec![]))],
    );
    for step in [
        call("count", vec![]),
        call("parseJson", vec![]),
        array_call(
            "createEvents",
            "rawstring",
            Expression::Array {
                items: vec![text("a=1")],
            },
        ),
        declaration,
        Stage::Assign {
            field: "user".into(),
            expression: text("alice"),
        },
    ] {
        let result = generate(vec![array_call(
            "selfJoinFilter",
            "where",
            Expression::Array {
                items: vec![query(vec![step.clone()])],
            },
        )]);
        assert_eq!(result.outcome, Outcome::Invalid, "{step:?}");
        assert!(result.query.is_none());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_FILTER_QUERY_REQUIRED")
        );
    }
    let result = generate(vec![array_call(
        "selfJoinFilter",
        "where",
        Expression::Array {
            items: vec![Expression::Literal {
                value: Scalar::Number(1.into()),
            }],
        },
    )]);
    assert_eq!(result.outcome, Outcome::Invalid);
    assert!(result.query.is_none());
}

#[test]
fn correlation_aggregation_requires_named_query_elements() {
    assert_eq!(
        check("correlate(A:{true},aggregation=[])").outcome,
        Outcome::Ready
    );
    for aggregation in ["[count()]", "[{ count() }]"] {
        let query = format!(
            "correlate(A: {{ user=alice }}, B: {{ user=bob | aid <=> A.aid }}, aggregation={aggregation})"
        );
        let result = check(&query);
        assert_eq!(result.outcome, Outcome::Invalid, "{query}");
        assert_eq!(result.validation.status, Acceptance::Rejected);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_ARGUMENT_TYPE")
        );
    }
}

#[test]
fn ordinary_subqueries_and_correlation_labels_keep_their_contexts() {
    let declaration = r#"defineTable(name="events", query={ user=alice }, include=[user]) | match(table="events", field=user, column=user)"#;
    assert_eq!(check(declaration).outcome, Outcome::Ready);
    let correlation = "correlate(A: { user=alice }, B: { user=bob | aid <=> A.aid }, within=5m)";
    assert_eq!(check(correlation).outcome, Outcome::Ready);
    for query in [
        "join(A: { user=alice }, field=user)",
        r#"defineTable(name="events", query=A: { user=alice })"#,
        "correlate(A: { user=alice }, A: { user=bob })",
        "correlate(A: { user=alice }, B: { user=bob | aid <=> Unknown.aid })",
    ] {
        assert_eq!(check(query).outcome, Outcome::Invalid, "{query}");
    }
    let plan = Plan {
        filters: vec![Predicate::Eq {
            field: "user".into(),
            value: Scalar::Text("alice".into()),
        }],
        ..Plan::default()
    };
    let result = generate(vec![Stage::Correlate {
        queries: vec![
            NamedPlan {
                name: "A".into(),
                plan: plan.clone(),
            },
            NamedPlan {
                name: "B".into(),
                plan,
            },
        ],
        links: vec![Link {
            from: "B".into(),
            field: "aid".into(),
            to: "A".into(),
            key: "aid".into(),
        }],
        sequence: false,
        within: "5m".into(),
    }]);
    assert_eq!(result.outcome, Outcome::Ready);
    assert!(result.query.is_some());
}

#[test]
fn raw_boolean_composition_cannot_embed_filter_function_calls() {
    for query in [
        r#"user=alice AND in(user, values=["alice"])"#,
        r#"user=bob OR in(user, values=["alice"])"#,
        r#"user=alice in(user, values=["alice"])"#,
        r#"user=alice AND not in(user, values=["bob"])"#,
        r#"selfJoinFilter(user, where=[{ user=alice OR in(user, values=["alice"]) }])"#,
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Invalid, "{query}");
        assert_eq!(result.validation.status, Acceptance::Rejected);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_FUNCTION_FILTER_COMPOSITION"),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for query in [
        r#"user=alice AND (kind=login OR kind=logout)"#,
        r#"user=alice | in(user, values=["alice"]) | !in(user, values=["bob"])"#,
        r#"in(user, values=["alice"])"#,
        r#"not in(user, values=["bob"])"#,
        r#"! (user=alice OR user=bob)"#,
    ] {
        assert_eq!(check(query).outcome, Outcome::Ready, "{query}");
    }
    assert_eq!(
        check(r#"in(user, values=["alice"]) OR user=bob"#).outcome,
        Outcome::Unsupported
    );
    assert_eq!(
        check(r#"! (user=alice OR in(user, values=["alice"]))"#).outcome,
        Outcome::Invalid
    );
}

#[test]
fn regex_capture_markers_under_or_are_explicitly_outside_the_profile() {
    for query in [
        r#"/(?<matched>alice)/ OR user=bob"#,
        r#"user=/(?<matched>alice)/ OR user=bob"#,
        r#"user=alice OR (/(?<matched>alice)/ AND kind=login)"#,
        r#"not /(?<matched>alice)/ OR user=bob"#,
        r#"user=/\(?<literal>/ OR user=bob"#,
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Unsupported, "{query}");
        assert_eq!(result.validation.status, Acceptance::Unknown);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_UNSUPPORTED_REGEX_FILTER_COMPOSITION")
        );
    }
    for query in [
        r#"/(?<matched>alice)/"#,
        r#"/(?<matched>alice)/ AND user=bob"#,
        r#"user=/alice/ OR user=bob"#,
        r#"user=/(?:alice|bob)/ OR user=carol"#,
    ] {
        assert_eq!(check(query).outcome, Outcome::Ready, "{query}");
    }
}
