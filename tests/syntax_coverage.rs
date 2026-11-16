use cqlmatic::{app, contracts::*};

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

#[test]
fn free_text_must_precede_the_first_aggregate() {
    for query in [
        r#""Lorem ipsum dolor" | tail(200) | "sit amet, consectetur""#,
        "x=1 | groupBy(y) | head 10",
        "x=1 | groupBy(y) | sort -_count | head 10",
        "x=1 | count() | foo",
        "x=1 | sort(y) | foo",
        "x=1 | table([a]) | foo",
        "x=1 | groupBy(y) | /foo/",
        "x=1 | groupBy(y) | NOT foo",
        "x=1 | groupBy(y) | a=1 OR foo",
        r#"x=1 | count() | wildcard("a*")"#,
        "groupBy(k) | case { foo; * }",
        "head(1) | series(user, startmatch={foo})",
        r#"createEvents(["a=1"]) | foo"#,
        "count() | a=1 AND (b=2 OR NOT /foo/)",
        "count() | k match { a => foo; * => * }",
        r#"readFile("lookup.csv") | foo"#,
        r#"readPersistedAggregation("hourly") | foo"#,
        "correlate(A: { x=1 }) | foo",
        "n := count() | foo",
        "count() | wildcard(\"a*\", field=y) | foo",
        "count() | selfJoinFilter(field=k, where=[{foo}])",
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Rejected,
            "{query}: {result:?}"
        );
        assert!(
            result.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "CQL_FREE_TEXT_AFTER_AGGREGATE"
                    && diagnostic.severity == Severity::Error
                    && diagnostic.message.contains("Free-text")
            }),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for query in [
        r#""Lorem ipsum dolor" | "sit amet" | tail(200)"#,
        "x=1 | eval(z=1) | foo",
        "x=1 | tail(200) | y=2",
        "count() | *",
        "count() | _count=*",
        "count() | case { _count>1; * }",
        "x=1 | groupBy(y) | @rawstring=/foo/",
        "groupBy(k) | join({foo}, field=k)",
        r#"x=1 | count() | wildcard("a*", field=y)"#,
        r#"count() | wildcard("a*", field=[])"#,
        "foo | groupBy(k)",
        r#"wildcard("a*") | count()"#,
        r#"createEvents(["a=1"]) | a=1"#,
        "case { a=1 | v:=1; foo } | count()",
        "defineTable(name=t, query={foo | count()}, include=[_count]) | foo",
        "groupBy(k) | join({foo}, field=k) | _count=*",
        "head(1) | series(user, startmatch={user=*})",
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{query}: {result:?}"
        );
    }
}

#[test]
fn saved_query_calls_require_resource_resolution() {
    for query in [
        "$helper()",
        "$helper(field=ProductType)",
        "$falcon/helper:notAFunction(field=ProductType)",
        "$falcon/helper:count(field=ProductType)",
        "$\"Saved Query\"()",
        "$query://helper()",
        "$query://other/falcon/helper:saved()",
        "$parser://other/falcon/helper:parser()",
        "$\"query://other/falcon/helper:Saved Query\"()",
        "$helper /* a comment */ ()",
        "#repo=events | $falcon/helper:notAFunction(field=ProductType)",
        "#repo=events $falcon/helper:notAFunction(field=ProductType)",
        "NOT $helper()",
        "message=$helper()",
        "result := $helper()",
        "result := 1 + $\"Saved Query\"()",
        "test($helper())",
        "groupBy(host, function=[count(), {id=42 | $\"Saved Query\"()}])",
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Unsupported,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(result.validation.status, Acceptance::Unknown, "{query}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_UNSUPPORTED_SYNTAX"),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn dollar_text_in_patterns_strings_regex_and_comments_remains_supported() {
    for query in [
        "$helper",
        "$100",
        "message=$helper",
        "message=$falcon/helper:saved",
        "\"$helper()\"",
        "message=\"$falcon/helper:notAFunction(field=ProductType)\"",
        "result := \"$helper()\"",
        r"message=/\$helper\(\)/",
        "/* $falcon/helper:notAFunction(field=ProductType) */ count()",
        "// $\"Saved Query\"()\ncount()",
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn unresolved_parameters_are_coverage_gaps_in_typed_and_nested_arguments() {
    for query in [
        "host=?host",
        "size > ?threshold",
        "message=?\"Search Text\"",
        "result := ?value + 1",
        "count(field=?field)",
        "count(distinct=?distinct)",
        "in(field=host, values=?values)",
        "in(field=host, values=[?value])",
        "in(field=host, values=[?value + 1])",
        "sort(field=[host, ?field], order=?order, limit=?limit)",
        "test(size > ?threshold)",
        "test(size > (?threshold + 1))",
        "groupBy([host], function=[sum(?field)])",
        "rename(field=?fields)",
        "rename(field=[[host, ?\"#output\"]])",
        "match(file=\"lookup.csv\", field=[host], column=[host], mode=?glob)",
        "join(query={type=login}, field=?field, key=host)",
        "series(collect=host, memlimit=?memory)",
        "kvParse(separator=?separator)",
        "array:eval(array=?array, var=?binding, function={x := 1})",
        "findTimestamp(as=?\"#output\")",
        "sort(order=?\"not an order\")",
        "head(?0)",
        "percentile(value,percentiles=[?percentile,99])",
        "bitfield:extractFlags(field=flags,output=[[?bit,\"flag\"]])",
        "bitfield:extractFlags(field=flags,output=[[0,?\"#output\"]])",
        "bitfield:extractFlags(field=flags,output=[?pair])",
        "rename([?pair])",
        "correlate(query=?query)",
        "correlate(A: {type=login}, root=?root)",
        "correlate(A: {type=login}, sequence=?sequence, sequenceBy=[@timestamp])",
        "correlate(A: {type=login} include:?fields)",
        "correlate(A: {type=login}, aggregation=[B: {count()} after:?anchor])",
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Unsupported,
            "{query}: {:?}",
            result.diagnostics
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_UNSUPPORTED_PARAMETER"),
            "{query}: {:?}",
            result.diagnostics
        );
        assert!(
            result
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error || d.is_unsupported()),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn parameter_defaults_remain_outside_the_profile() {
    for query in [
        "?{text=\"*\"}",
        "host=?{host=\"alpha\"}",
        "groupBy(field=?{field=\"host\"})",
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Unsupported,
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn parameters_preserve_independent_errors() {
    for (query, code) in [
        ("count(field=?field, field=host)", "CQL_DUPLICATE_ARGUMENT"),
        ("count(field=?field, unknown=true)", "CQL_ARGUMENT_UNKNOWN"),
        ("in(values=?values)", "CQL_ARGUMENT_REQUIRED"),
        ("sort(field=?field, limit=false)", "CQL_ARGUMENT_TYPE"),
        ("in(field=host, values=[?value, true])", "CQL_ARGUMENT_TYPE"),
        ("sort(order=[?order, invalid])", "CQL_ARGUMENT_VALUE"),
        (
            "bucket(span=\"1m\",function=window(count(),span=?s,buckets=2))",
            "CQL_WINDOW_WIDTH",
        ),
        ("rename([[?old,\"#tag\"]])", "CQL_OUTPUT_FIELD"),
        (
            "bitfield:extractFlags(field=flags,output=[[?bit,\"#tag\"]])",
            "CQL_OUTPUT_FIELD",
        ),
        (
            "groupBy(host, function=[lower(?field)])",
            "CQL_AGGREGATE_REQUIRED",
        ),
        ("count(field=?field,", "CQL_SYNTAX"),
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Invalid,
            "{query}: {:?}",
            result.diagnostics
        );
        assert!(
            result.diagnostics.iter().any(|d| d.code == code),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn bound_values_and_parameter_text_remain_supported() {
    for query in [
        "count(field=host)",
        "in(field=host, values=[\"alpha\", \"beta\"])",
        "test(size > (10 + 1))",
        "message=\"?value\"",
        "result := \"?value\"",
        "/* ?value */ count()",
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn documented_bare_wildcard_patterns_remain_literal_function_arguments() {
    use cqlmatic::language::{
        ast::{Step, Value},
        parser,
    };
    for (query, expected) in [
        ("wildcard(field=UserName,pattern=*foo*)", "*foo*"),
        ("wildcard(field=UserName,pattern=foo*)", "foo*"),
        (
            "wildcard(field=animal,pattern=*h*rse*,ignoreCase=true)",
            "*h*rse*",
        ),
        (
            "wildcard(field=animal,pattern=*,includeEverythingOnAsterisk=true)",
            "*",
        ),
        ("wildcard(*foo*,field=UserName)", "*foo*"),
        ("wildcard(field=UserName,*foo*)", "*foo*"),
        ("UserName =~ wildcard(*foo*)", "*foo*"),
        (
            "wildcard(pattern=*foo* /* ignored ) */ ,field=UserName)",
            "*foo*",
        ),
        (
            "wildcard(pattern=*foo*// ignored )\n,field=UserName)",
            "*foo*",
        ),
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        let parsed = parser::parse(query).unwrap();
        let Step::Call(call) = &parsed.steps[0] else {
            panic!("expected function call")
        };
        let resolved = cqlmatic::catalog::resolve(call).unwrap();
        assert!(
            matches!(resolved.arguments.get("pattern"), Some(Value::String(value)) if value==expected)
        );
        let rendered = parsed.render();
        assert_eq!(parser::parse(&rendered).unwrap().render(), rendered);
    }
}

#[test]
fn standalone_and_field_shorthand_negation_use_the_existing_filter_ast() {
    use cqlmatic::language::{
        ast::{Filter, Step},
        parser,
    };
    for query in [
        "!wildcard(field=UserName,pattern=*foo*)",
        "not wildcard(field=UserName,pattern=*foo*)",
        "NOT wildcard(field=UserName,pattern=\"*foo*\")",
        "!in(field=UserName,values=[\"ERROR\",\"WARN\"])",
        "not in(UserName,values=[\"ERROR\",\"WARN\"])",
        "NOT in(UserName,values=[\"ERROR\",\"WARN\"])",
        "UserName =~ !wildcard(*foo*)",
        "UserName =~ !in(values=[\"ERROR\",\"WARN\"])",
        "\"User Name\" =~ !in(values=[\"ERROR\"])",
        "UserName =~ /* ignored */ ! /* ignored */ wildcard(*foo*)",
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        let parsed = parser::parse(query).unwrap();
        let Step::Filter(Filter::Not(inner)) = &parsed.steps[0] else {
            panic!("expected negated filter: {query}")
        };
        assert!(matches!(inner.as_ref(), Filter::Call(_)));
        let rendered = parsed.render();
        assert!(rendered.starts_with('!'));
        assert_eq!(check(&rendered).outcome, Outcome::Ready, "{rendered}");
    }
    assert_eq!(
        parser::parse("UserName =~ !wildcard(*foo*)")
            .unwrap()
            .render(),
        parser::parse("!wildcard(\"*foo*\",field=\"UserName\")")
            .unwrap()
            .render()
    );
}

#[test]
fn wildcard_shorthand_does_not_change_other_argument_or_literal_boundaries() {
    for query in [
        "value := 2 * 3",
        "*foo*",
        "message=*foo*",
        "\"!wildcard(pattern=*foo*)\"",
        "wildcard(pattern=\"*foo/bar*\",field=UserName)",
        "wildcard(pattern=\"*foo* /* text */\",field=UserName)",
        r#"regex("foo.*",field=UserName)"#,
        r"message=/foo.*/",
        "wildcard(pattern=*foo*,field=UserName) // !in(unknown=true)",
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for query in [
        "in(field=UserName,values=[*foo*])",
        "sum(field=foo*bar)",
        "wildcard(field=*foo*,pattern=\"foo\")",
        "wildcard(pattern=*foo* | count())",
        "wildcard(pattern=*foo*,field=UserName) OR !in(UserName,values=[\"ERROR\"])",
        "wildcard(pattern=?pattern)",
        "wildcard(pattern=$helper())",
    ] {
        assert_ne!(check(query).outcome, Outcome::Ready, "{query}");
    }
    for (query, code) in [
        (
            "wildcard(pattern=*foo*,pattern=\"bar\")",
            "CQL_DUPLICATE_ARGUMENT",
        ),
        (
            "wildcard(pattern=*foo*,unknown=true)",
            "CQL_ARGUMENT_UNKNOWN",
        ),
        (
            "UserName =~ !in(field=other,values=[\"ERROR\"])",
            "CQL_DUPLICATE_ARGUMENT",
        ),
        ("UserName =~ !count()", "CQL_FILTER_FUNCTION"),
    ] {
        let result = check(query);
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
                .any(|diagnostic| diagnostic.code == code),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn typed_negated_function_predicates_keep_literals_quoted_and_recheck_raw_forms() {
    use cqlmatic::language::parser;
    use serde_json::json;
    for (function, argument) in [
        (
            "wildcard",
            json!({"name":"pattern","value":{"kind":"literal","value":"*foo*"}}),
        ),
        (
            "wildcard",
            json!({"name":"pattern","value":{"kind":"literal","value":"*foo*) | count()"}}),
        ),
        (
            "in",
            json!({"name":"values","value":{"kind":"array","items":[{"kind":"literal","value":"ERROR"},{"kind":"literal","value":"WARN"}]}}),
        ),
    ] {
        let request: GenerateRequest =
            serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{
            "source":{},"filters":[{"op":"call","function":function,"negate":true,"args":[
                {"name":"field","value":{"kind":"literal","value":"UserName"}},argument]}]}}}))
            .unwrap();
        let generated = app::generate(request, None);
        assert_eq!(
            generated.outcome,
            Outcome::Ready,
            "{function}: {:?}",
            generated.diagnostics
        );
        let query = generated.query.unwrap();
        assert!(query.starts_with(&format!("!{function}(")), "{query}");
        assert_eq!(parser::parse(&query).unwrap().steps.len(), 1, "{query}");
        assert_eq!(check(&query).outcome, Outcome::Ready, "{query}");
    }
}

#[test]
fn parameterized_aggregate_children_are_checked_once_in_their_parent_scope() {
    for query in [
        "bucket(span=\"1m\",function=window(count(),span=?s))",
        "timeChart(span=\"1m\",function=window(sum(value),buckets=?n))",
        "head(10)|accumulate(sum(?field))",
        "array:reduceRow(\"a[]\",var=x,function=sum(?field))",
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Unsupported, "{query}: {result:?}");
        assert_eq!(
            result
                .diagnostics
                .iter()
                .filter(|d| d.code == "CQL_UNSUPPORTED_PARAMETER")
                .count(),
            1,
            "{query}: {result:?}"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error || d.is_unsupported()),
            "{query}: {result:?}"
        );
    }
    for (query, code) in [
        (
            "bucket(span=\"1m\",function=window(count(unknown=true),span=?s))",
            "CQL_ARGUMENT_UNKNOWN",
        ),
        (
            "bucket(span=\"1m\",function=[window(count(),span=?s),lower(user)])",
            "CQL_AGGREGATE_REQUIRED",
        ),
        ("window(count(),span=?s)", "CQL_WINDOW_PLACEMENT"),
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Invalid, "{query}: {result:?}");
        assert!(
            result.diagnostics.iter().any(|d| d.code == code),
            "{query}: {result:?}"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_UNSUPPORTED_PARAMETER"),
            "{query}: {result:?}"
        );
    }
}

#[test]
fn time_declarations_are_metadata_only_and_require_a_top_level_preamble() {
    use serde_json::json;
    for query in [
        "setTimeInterval(start=\"7d\")|defineTable(name=t,query={count(as=n)},include=[n])|readFile(t)",
        "SETTIMEINTERVAL(start=\"7d\")|defineTable(name=t,query={count(as=n)},include=[n])|readFile(t)",
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_TABLE_PLACEMENT" || d.code == "CQL_SOURCE_DISCARDS_INPUT")
        );
    }
    for query in [
        "!setTimeInterval(start=\"7d\")",
        "(setTimeInterval(start=\"7d\"))",
        "case {setTimeInterval(start=\"7d\")}",
        "n:=setTimeInterval(start=\"7d\")",
        "eval(n=setTimeInterval(start=\"7d\"))",
        "defineTable(name=t,query={setTimeInterval(start=\"7d\")|count()},include=[_count])",
        "host=x|setTimeInterval(start=\"7d\")",
        "count()|setTimeInterval(start=\"7d\")",
        "setTimeInterval(start=\"7d\")|setTimeInterval(start=\"1d\")",
        "defineTable(name=t,query={count()},include=[_count])|setTimeInterval(start=\"7d\")",
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Invalid, "{query}: {result:?}");
        let code = if query.starts_with('(') {
            "CQL_SYNTAX"
        } else {
            "CQL_TIME_INTERVAL_PLACEMENT"
        };
        assert!(
            result.diagnostics.iter().any(|d| d.code == code),
            "{query}: {result:?}"
        );
    }
    let interval = json!({"op":"call","function":"setTimeInterval","args":[
        {"name":"start","value":{"kind":"literal","value":"7d"}}
    ]});
    let table = json!({"op":"call","function":"defineTable","args":[
        {"name":"name","value":{"kind":"literal","value":"t"}},
        {"name":"query","value":{"kind":"query","plan":{"source":{},"steps":[
            {"op":"count","as":"n"}
        ]}}},
        {"name":"include","value":{"kind":"array","items":[{"kind":"literal","value":"n"}]}}
    ]});
    let read = json!({"op":"call","function":"readFile","args":[{"value":{"kind":"literal","value":"t"}}]});
    let generate = |plan| {
        let request =
            serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":plan}}))
                .unwrap();
        app::generate(request, None)
    };
    let good = generate(json!({"source":{},"steps":[interval.clone(),table,read]}));
    assert_eq!(good.outcome, Outcome::Ready, "{good:?}");
    for negate in [false, true] {
        let result = generate(
            json!({"source":{},"filters":[{"op":"call","function":"setTimeInterval","negate":negate,"args":[
                {"name":"start","value":{"kind":"literal","value":"7d"}}
            ]}]}),
        );
        assert_eq!(result.outcome, Outcome::Invalid, "{result:?}");
    }
    for steps in [
        json!([{"op":"case","branches":[{"source":{},"steps":[interval.clone()]}]}]),
        json!([{"op":"assign","field":"n","expression":{"kind":"call","function":"setTimeInterval","args":[
            {"name":"start","value":{"kind":"literal","value":"7d"}}
        ]}}]),
        json!([interval.clone(), interval]),
    ] {
        let result = generate(json!({"source":{},"steps":steps}));
        assert_eq!(result.outcome, Outcome::Invalid, "{result:?}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_TIME_INTERVAL_PLACEMENT"),
            "{result:?}"
        );
    }
}
