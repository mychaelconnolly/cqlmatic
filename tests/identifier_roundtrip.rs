use cqlmatic::{
    app,
    contracts::*,
    language::{
        ast::{Filter, Pattern, Pipeline, Step, Value, field, quote},
        parser, plan,
    },
};

fn equality(field: &str) -> Filter {
    Filter::Compare {
        field: field.into(),
        op: "=".into(),
        value: Pattern::Text("alice".into()),
    }
}

#[test]
fn boolean_keyword_field_prefixes_are_quoted_in_assignments_and_filters() {
    for keyword in ["not", "NOT", "NoT", "and", "AND", "AnD", "or", "OR", "Or"] {
        for suffix in [".user", "[0].user", "[12]", ".用户"] {
            let name = format!("{keyword}{suffix}");
            let expected = Pipeline {
                steps: vec![
                    Step::Assign {
                        field: name.clone(),
                        value: Value::String("alice".into()),
                    },
                    Step::Filter(equality(&name)),
                ],
            };
            let query = expected.render();
            assert_eq!(
                query,
                format!(
                    "{} := \"alice\" | {} = \"alice\"",
                    quote(&name),
                    quote(&name)
                ),
                "{name}"
            );
            assert_eq!(parser::parse(&query).unwrap(), expected, "{name}");
        }
    }
    for name in [
        "notice.user",
        "notable.user",
        "not_user",
        "andrew.user",
        "order.user",
        "not9.user",
    ] {
        assert_eq!(field(name), name);
    }
}

#[test]
fn raw_boolean_keyword_assignments_require_quoted_field_names() {
    let check = |query: String| {
        app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query,
                context: Context::default(),
            },
            None,
        )
    };
    for keyword in ["not", "NOT", "NoT", "and", "AND", "AnD", "or", "OR", "Or"] {
        for suffix in [".user", "[0].user", "", "[12]"] {
            let name = format!("{keyword}{suffix}");
            for prefix in ["", "head(1) | "] {
                let query = format!("{prefix}{name} := \"alice\"");
                let result = check(query.clone());
                assert_eq!(result.outcome, Outcome::Unsupported, "{query}");
                assert!(result.diagnostics.iter().any(|diagnostic| {
                    diagnostic.code == "CQL_UNSUPPORTED_SYNTAX"
                        && diagnostic.message.contains("Quote")
                        && diagnostic
                            .span
                            .as_ref()
                            .is_some_and(|span| span.start == prefix.len())
                }));
                let query = format!("{prefix}{} := \"alice\"", quote(&name));
                let result = check(query.clone());
                assert_eq!(
                    result.outcome,
                    Outcome::Ready,
                    "{query}: {:?}",
                    result.diagnostics
                );
            }
        }
    }
    for query in [
        "notice.user := \"alice\"",
        "not_user := \"alice\"",
        "not9.user := \"alice\"",
        "andrew.user := \"alice\"",
        "order.user := \"alice\"",
        "not.user = alice",
        "NOT user = alice",
        "NOT(user = alice)",
    ] {
        let result = check(query.into());
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn keyword_field_names_keep_their_identity_in_generation_and_grounding() {
    for field in [
        "not",
        "NOT",
        "and",
        "AND",
        "or",
        "OR",
        "not.user",
        "not[0].user",
        "NOT.user",
        "NoT[12].user",
    ] {
        let requested = Plan {
            filters: vec![Predicate::Eq {
                field: field.into(),
                value: Scalar::Text("alice".into()),
            }],
            ..Plan::default()
        };
        let result = app::generate(
            GenerateRequest {
                schema_version: 1,
                id: None,
                input: GenerateInput::Plan {
                    plan: requested.clone(),
                },
                context: Context::default(),
            },
            None,
        );
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{field}: {:?}",
            result.diagnostics
        );
        assert_eq!(result.interpretation.plan, Some(requested.clone()));
        assert_eq!(
            result.grounding.basis.keys().collect::<Vec<_>>(),
            vec![&format!("*:*:{field}")],
            "{field}"
        );
        assert_eq!(
            parser::parse(result.query.as_ref().unwrap()).unwrap(),
            plan::lower(&requested).unwrap(),
            "{field}"
        );
    }
}

#[test]
fn keyword_field_names_roundtrip_in_standalone_and_boolean_contexts() {
    for keyword in [
        "not", "NOT", "NoT", "and", "AND", "AnD", "or", "OR", "Or", "case", "CASE", "match",
        "like", "true", "false",
    ] {
        for suffix in [
            "",
            ".user",
            "[0]",
            "[12].user",
            "_user",
            "user",
            "9",
            "é",
            ".用户[2]",
        ] {
            let filter = equality(&format!("{keyword}{suffix}"));
            for expected in [
                filter.clone(),
                Filter::And(vec![equality("marker"), filter.clone()]),
                Filter::Or(vec![equality("marker"), filter.clone()]),
                Filter::Not(Box::new(filter.clone())),
            ] {
                let query = expected.render();
                assert_eq!(
                    parser::parse(&query).unwrap_or_else(|error| panic!("{query}: {error:?}")),
                    Pipeline {
                        steps: vec![Step::Filter(expected)],
                    },
                    "{query}"
                );
            }
            let query = format!("marker=alice {}", filter.render());
            assert_eq!(
                parser::parse(&query).unwrap_or_else(|error| panic!("{query}: {error:?}")),
                Pipeline {
                    steps: vec![Step::Filter(Filter::And(vec![equality("marker"), filter]))],
                },
                "{query}"
            );
        }
    }
}

#[test]
fn implicit_conjunction_keeps_operator_prefixed_field_names() {
    for field in ["OR.user", "AND.user", "OR[0].user", "AND[0].user"] {
        let query = format!("marker=alice {field}=alice");
        assert_eq!(
            parser::parse(&query).unwrap(),
            Pipeline {
                steps: vec![Step::Filter(Filter::And(vec![
                    equality("marker"),
                    equality(field),
                ]))],
            },
            "{query}"
        );
    }
}

#[test]
fn unary_not_keeps_its_boolean_meaning() {
    let negated = Filter::Not(Box::new(equality("user")));
    for query in [
        "NOT user=alice",
        "NOT(user=alice)",
        "not/* gap */(user=alice)",
        "!(user=alice)",
    ] {
        assert_eq!(
            parser::parse(query).unwrap(),
            Pipeline {
                steps: vec![Step::Filter(negated.clone())],
            },
            "{query}"
        );
    }
    assert_eq!(
        parser::parse("marker=alice NOT user=alice").unwrap(),
        Pipeline {
            steps: vec![Step::Filter(Filter::And(vec![equality("marker"), negated]))],
        }
    );
}
