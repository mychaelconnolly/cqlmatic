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

fn generate(plan: Value) -> Analysis {
    let request = serde_json::from_value(json!({
        "schema_version": 1,
        "input": {"mode": "plan", "plan": plan}
    }))
    .unwrap();
    app::generate(request, None)
}

#[test]
fn string_array_values_do_not_become_input_fields() {
    for query in [
        "array:contains(\"tags[]\",value=a)",
        "array:contains(\"tags[]\",value=\"a\")",
    ] {
        let result = check(query, false);
        assert_eq!(result.outcome, Outcome::Ready, "{result:?}");
        assert!(
            result
                .grounding
                .basis
                .keys()
                .any(|key| key.ends_with(":tags[]"))
        );
        assert!(!result.grounding.basis.keys().any(|key| key.ends_with(":a")));
    }
    assert_eq!(
        check("array:contains(\"tags[]\",value=a+1)", false).outcome,
        Outcome::Unsupported
    );
}

#[test]
fn synthetic_sources_do_not_export_arbitrary_fields() {
    for query in [
        "createEvents([\"a\"])|select([n])",
        "count(as=n)|createEvents([\"a\"])|select([n])",
    ] {
        let result = check(query, true);
        assert_eq!(result.outcome, Outcome::NeedsContext, "{result:?}");
        assert_ne!(
            result.grounding.basis.get("*:*:n").map(String::as_str),
            Some("query-produced")
        );
    }
    let control = check(
        "createEvents([\"a\"])|select([@rawstring,@timestamp,@timestamp.nanos])",
        true,
    );
    assert_eq!(control.outcome, Outcome::Ready, "{control:?}");
    let extracted = check("createEvents([\"a=1\"])|kvParse()|select([unlisted])", true);
    assert_eq!(extracted.outcome, Outcome::NeedsContext, "{extracted:?}");
    assert_ne!(
        extracted
            .grounding
            .basis
            .get("*:*:unlisted")
            .map(String::as_str),
        Some("query-produced")
    );
}

#[test]
fn default_and_neighbor_use_documented_binding_and_prefix() {
    let result = check("default(\"unknown\",field=user)", false);
    assert_eq!(result.outcome, Outcome::Ready, "{result:?}");
    let result = check(
        "#repo=example|select([user])|head(1)|neighbor([user])|select([_neighbor.user])",
        true,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{result:?}");
    assert_eq!(
        result
            .grounding
            .basis
            .get("example:*:_neighbor.user")
            .map(String::as_str),
        Some("query-produced")
    );
}

#[test]
fn composite_sequences_inherit_order_and_enforce_child_cardinality() {
    for query in [
        "head(10)|groupBy(host,function={accumulate(sum(value,as=running))})",
        "head(10)|accumulate(count)",
        "head(10)|stats([accumulate(sum(value)),count()])",
    ] {
        let result = check(query, false);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
    }
    for query in [
        "head(10)|groupBy(host,function={select([host])})",
        "head(10)|accumulate(sort(value))",
        "head(10)|stats([accumulate(sum(value)),sort(host)])",
        "head(10)|stats([neighbor(host),accumulate(sum(value))])",
    ] {
        let result = check(query, false);
        assert_eq!(result.outcome, Outcome::Invalid, "{query}: {result:?}");
    }
    let result = check(
        "array:reduceRow(\"a[]\",var=x,function=[sum(x),max(x)])",
        false,
    );
    assert_eq!(result.outcome, Outcome::Unsupported, "{result:?}");
}

#[test]
fn foreign_tables_do_not_borrow_observations_and_prefixes_preserve_tag_immutability() {
    for query in [
        "remoteTable(connection=\"demo\",name=t,query=\"select user from events\")|readFile(t,include=[user])|select([user])",
        "remoteTable(connection=\"demo\",name=t,query={#repo=example|select([user])},include=[user])|readFile(t)|select([user])",
    ] {
        let result = check(query, true);
        assert_eq!(result.outcome, Outcome::NeedsContext, "{query}: {result:?}");
        assert!(
            !result
                .grounding
                .basis
                .values()
                .any(|basis| basis.contains("observed")),
            "{result:?}"
        );
    }
    for query in [
        "parseXml(prefix=\"#tag\")",
        "parseCEF(headerprefix=\"#tag\")",
        "parseJson(prefix=\"#tag\")",
        "eventInternals(prefix=\"#tag\")",
    ] {
        assert_eq!(check(query, false).outcome, Outcome::Invalid, "{query}");
    }
    assert_eq!(
        check("x:=eventSize()+1", false).outcome,
        Outcome::Unsupported
    );
    assert_eq!(
        check("x:=eventInternals()", false).outcome,
        Outcome::Unsupported
    );
}

#[test]
fn lowercase_checks_mutations_in_every_include_mode() {
    for mode in [None, Some("values"), Some("fields"), Some("both")] {
        for (field, outcome) in [
            ("Message", Outcome::Ready),
            ("#repo", Outcome::Invalid),
            ("*", Outcome::Unsupported),
        ] {
            let suffix = mode
                .map(|mode| format!(",include={mode}"))
                .unwrap_or_default();
            let raw = check(&format!("lowercase(\"{field}\"{suffix})"), false);
            let mut args = vec![json!({"value":{"kind":"literal","value":field}})];
            if let Some(mode) = mode {
                args.push(json!({"name":"include","value":{"kind":"literal","value":mode}}));
            }
            let typed = generate(json!({"source":{},"steps":[
                {"op":"call","function":"lowercase","args":args}
            ]}));
            for result in [raw, typed] {
                assert_eq!(result.outcome, outcome, "{field} {mode:?}: {result:?}");
                assert_eq!(
                    result
                        .diagnostics
                        .iter()
                        .any(|d| d.code == "CQL_LOWERCASE_RETENTION"
                            && d.severity == Severity::Warning),
                    matches!(mode, Some("fields" | "both")),
                    "{field} {mode:?}: {result:?}"
                );
                if outcome == Outcome::Invalid {
                    assert!(
                        result
                            .diagnostics
                            .iter()
                            .any(|d| d.code == "CQL_OUTPUT_FIELD")
                    );
                }
            }
        }
    }
    let unresolved = check("lowercase([\"#repo\",?field],include=values)", false);
    assert_eq!(unresolved.outcome, Outcome::Invalid, "{unresolved:?}");
    assert!(
        unresolved
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_UNSUPPORTED_PARAMETER")
    );
}

#[test]
fn direct_assignment_rejects_a_second_output_alias() {
    for (function, mut args) in [
        ("count", vec![]),
        (
            "lower",
            vec![json!({"value":{"kind":"field","name":"user"}})],
        ),
        (
            "math:abs",
            vec![json!({"value":{"kind":"field","name":"value"}})],
        ),
    ] {
        let raw_args = if args.is_empty() {
            ""
        } else if function == "lower" {
            "user,"
        } else {
            "value,"
        };
        let raw = check(&format!("n:={function}({raw_args}as=m)"), false);
        args.push(json!({"name":"as","value":{"kind":"literal","value":"m"}}));
        let typed = generate(json!({"source":{},"steps":[{"op":"assign","field":"n",
            "expression":{"kind":"call","function":function,"args":args}}]}));
        for result in [raw, typed] {
            assert_eq!(result.outcome, Outcome::Invalid, "{function}: {result:?}");
            assert!(
                result
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "CQL_ASSIGN_ALIAS")
            );
        }
    }
    for query in [
        "n:=count()",
        "n:=lower(user)",
        "n:=math:abs(value)",
        "count(as=n)",
    ] {
        let result = check(query, false);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
    }
}

#[test]
fn assigned_aggregates_establish_single_row_bounds_in_sequence_and_reduce_children() {
    for (function, mut args, raw) in [
        ("accumulate", vec![], "head(10)|accumulate({n:=count()})"),
        (
            "slidingWindow",
            vec![json!({"name":"events","value":{"kind":"literal","value":2}})],
            "head(10)|slidingWindow({n:=count()},events=2)",
        ),
        (
            "slidingTimeWindow",
            vec![json!({"name":"span","value":{"kind":"literal","value":"5s"}})],
            "head(10)|slidingTimeWindow({n:=count()},span=\"5s\")",
        ),
        (
            "array:reduceRow",
            vec![
                json!({"name":"array","value":{"kind":"literal","value":"a[]"}}),
                json!({"name":"var","value":{"kind":"literal","value":"x"}}),
            ],
            "head(10)|array:reduceRow(\"a[]\",var=x,function={n:=count()})",
        ),
        (
            "array:reduceColumn",
            vec![
                json!({"name":"array","value":{"kind":"literal","value":"a[]"}}),
                json!({"name":"var","value":{"kind":"literal","value":"x"}}),
            ],
            "head(10)|array:reduceColumn(\"a[]\",var=x,function={n:=count()})",
        ),
    ] {
        args.push(
            json!({"name":"function","value":{"kind":"query","plan":{"source":{},"steps":[
                {"op":"assign","field":"n","expression":{"kind":"call","function":"count"}}
            ]}}}),
        );
        let typed = generate(json!({"source":{},"steps":[{"op":"head","limit":10},
            {"op":"call","function":function,"args":args}]}));
        for result in [check(raw, false), typed] {
            assert_eq!(result.outcome, Outcome::Ready, "{function}: {result:?}");
        }
    }
    for query in [
        "head(10)|accumulate({n:=count()|n:=1})",
        "head(10)|accumulate({n:=count()|collect(n,multival=false)|head(1)})",
    ] {
        let result = check(query, false);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
    }
    for query in [
        "head(10)|accumulate({n:=count()|split(n)})",
        "head(10)|slidingWindow({n:=collect(user,multival=false)},events=2)",
    ] {
        let result = check(query, false);
        assert_eq!(result.outcome, Outcome::Invalid, "{query}: {result:?}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_SEQUENCE_AGGREGATE_ROWS")
        );
    }
}

#[test]
fn sequence_stats_checks_descend_into_assigned_child_functions() {
    for (sequence, sibling, invalid) in [
        (true, "sort", true),
        (true, "count", false),
        (false, "sort", false),
    ] {
        let sum = json!({"kind":"call","function":"sum","args":[
            {"value":{"kind":"field","name":"value"}}
        ]});
        let (raw_function, expression) = if sequence {
            (
                "partition(function=sum(value),condition=test(value>0))",
                json!({
                    "kind":"call","function":"partition","args":[
                        {"name":"function","value":sum},
                        {"name":"condition","value":{"kind":"call","function":"test","args":[
                            {"value":{"kind":"compare","operator":"gt",
                                "left":{"kind":"field","name":"value"},
                                "right":{"kind":"literal","value":0}}}
                        ]}}
                    ]
                }),
            )
        } else {
            ("sum(value)", sum)
        };
        for assigned in [false, true] {
            let raw_sibling = if sibling == "sort" {
                "sort(value)"
            } else {
                "count()"
            };
            let raw = format!(
                "head(10)|stats([{{{}{raw_function}}},{raw_sibling}])",
                if assigned { "n:=" } else { "" }
            );
            let step = if assigned {
                json!({"op":"assign","field":"n","expression":expression})
            } else {
                json!({"op":"call","function":expression["function"],"args":expression["args"]})
            };
            let typed = generate(json!({"source":{},"steps":[{"op":"head","limit":10},
                {"op":"call","function":"stats","args":[{"value":{"kind":"array","items":[
                    {"kind":"query","plan":{"source":{},"steps":[step]}},
                    {"kind":"call","function":sibling,"args":if sibling == "sort" {
                        vec![json!({"value":{"kind":"field","name":"value"}})]
                    } else { vec![] }}
                ]}}]}
            ]}));
            for result in [check(&raw, false), typed] {
                assert_eq!(
                    result.outcome,
                    if invalid {
                        Outcome::Invalid
                    } else {
                        Outcome::Ready
                    },
                    "{raw}: {result:?}"
                );
                assert_eq!(
                    result
                        .diagnostics
                        .iter()
                        .any(|d| d.code == "CQL_SEQUENCE_STATS"),
                    invalid,
                    "{raw}: {result:?}"
                );
            }
        }
    }
}

#[test]
fn sequence_stats_checks_use_the_canonical_function_name() {
    for sequence in [
        "ACCUMULATE(sum(value))",
        "aCcUmUlAtE(sum(value))",
        "SLIDINGWINDOW(count(),events=2)",
        "SLIDINGTIMEWINDOW(count(),span=\"5s\")",
    ] {
        let bad = check(&format!("head(10)|stats([{sequence},sort(host)])"), false);
        assert_eq!(bad.outcome, Outcome::Invalid, "{sequence}: {bad:?}");
        assert!(
            bad.diagnostics
                .iter()
                .any(|d| d.code == "CQL_SEQUENCE_STATS")
        );
        let good = check(&format!("head(10)|stats([{sequence},count()])"), false);
        assert_eq!(good.outcome, Outcome::Ready, "{sequence}: {good:?}");
    }
    let result = generate(json!({"source":{},"steps":[{"op":"head","limit":10},
    {"op":"call","function":"stats","args":[{"value":{"kind":"array","items":[
        {"kind":"call","function":"ACCUMULATE","args":[{"value":{"kind":"call","function":"count"}}]},
        {"kind":"call","function":"sort","args":[{"value":{"kind":"field","name":"host"}}]}
    ]}}]}]}));
    assert_eq!(result.outcome, Outcome::Invalid, "{result:?}");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_SEQUENCE_STATS")
    );
}
