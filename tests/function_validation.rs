use cqlmatic::{app, catalog, contracts::*};
use std::collections::{BTreeMap, BTreeSet};

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

fn invalid(query: &str, code: &str) {
    let result = check(query);
    assert_eq!(result.outcome, Outcome::Invalid, "{query}: {result:?}");
    assert!(
        result.diagnostics.iter().any(|d| d.code == code),
        "{query}: {:?}",
        result.diagnostics
    );
}

fn ready(query: &str) {
    let result = check(query);
    assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
    assert_eq!(result.validation.execution, "not-run");
}

fn unsupported(query: &str, code: &str) {
    let result = check(query);
    assert_eq!(result.outcome, Outcome::Unsupported, "{query}: {result:?}");
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == code),
        "{query}: {result:?}"
    );
}

#[test]
fn function_arguments_require_calls_including_inside_arrays() {
    for query in [
        "x=1 | stats(count)",
        "x=1 | stats([count])",
        "x=1 | groupBy(k, function=count)",
        "x=1 | groupBy(k, function=[count, max(y)])",
        r#"x=1 | stats("count")"#,
        r#"x=1 | stats(["count"])"#,
        r#"x=1 | groupBy(k, function="count")"#,
        r#"x=1 | groupBy(k, function=[count(), "max"])"#,
    ] {
        invalid(query, "CQL_ARGUMENT_TYPE");
        assert_eq!(
            check(query).validation.status,
            Acceptance::Rejected,
            "{query}"
        );
    }
    for query in [
        "x=1 | stats(count())",
        "x=1 | stats([count(), avg(y)])",
        "x=1 | groupBy(k, function=count())",
        "x=1 | [count(), max(y)]",
        "x=1 | stats()",
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
fn direct_function_assignment_requires_one_output_alias() {
    for query in [
        "x=1 | v := collect(user)",
        "x=1 | v := table([a])",
        r#"x=1 | v := cidr(ip, subnet="10.0.0.0/8")"#,
        r#"x=1 | v := wildcard("a*", field=message)"#,
        "x=1 | v := stats(count())",
        r#"x=1 | v := now("current")"#,
        "x=1 | v := lower(y, as=w)",
        "x=1 | v := count(as=w)",
        r#"x=1 | v := start("current")"#,
    ] {
        invalid(query, "CQL_ASSIGN_ALIAS");
        assert_eq!(
            check(query).validation.status,
            Acceptance::Rejected,
            "{query}"
        );
    }
    for query in [
        "x=1 | v := count()",
        "x=1 | v := min(y)",
        "x=1 | v := lower(y)",
        "x=1 | v := 1",
        "x=1 | v := y + 1",
        "x=1 | v := now()",
        "x=1 | lower(y, as=v)",
        "x=1 | v := lower(y, as=w) + 1",
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{query}: {result:?}"
        );
    }
    assert_eq!(
        check("v := unclassified(y)").validation.status,
        Acceptance::Unknown
    );
    invalid("v := unclassified(as=w)", "CQL_ASSIGN_ALIAS");
    let eval = check("x=1 | v := eval(result=(value + 1))");
    assert_eq!(eval.validation.status, Acceptance::Unknown, "{eval:?}");
    assert!(
        eval.diagnostics
            .iter()
            .all(|d| d.code != "CQL_ASSIGN_ALIAS"),
        "{eval:?}"
    );
}

#[test]
fn timestamp_parsing_requires_a_named_field_and_binds_unnamed_format() {
    for query in [
        "parseTimestamp(timestamp)",
        r#"parseTimestamp("timestamp")"#,
        r#"parseTimestamp("time_text", format="yyyy-MM-dd HH:mm:ss", timezone="UTC")"#,
    ] {
        invalid(query, "CQL_ARGUMENT_REQUIRED");
        assert_eq!(
            check(query).validation.status,
            Acceptance::Rejected,
            "{query}"
        );
    }
    for query in [
        "parseTimestamp(field=expiryTime)",
        r#"parseTimestamp("millis", field=timestamp)"#,
        r#"parseTimestamp("yyyy-MM-dd'T'HH:mm:ss", field=timestamp, timezone="America/New_York")"#,
        "parseTimestamp(field=_bucket, format=millis)",
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
fn zero_relative_time_literals_remain_outside_the_profile() {
    unsupported("setTimeInterval(start=0d)", "CQL_UNSUPPORTED_DURATION");
    ready("setTimeInterval(start=0,end=1)");
}

#[test]
fn double_precision_accuracy_readings_stay_inside_exclusive_bounds() {
    let queries = [
        "percentile(x,accuracy=0.99999999999999999)",
        "percentile(x,accuracy=1e-400)",
    ];
    let accepted = queries
        .iter()
        .filter(|query| check(query).outcome == Outcome::Ready)
        .collect::<Vec<_>>();
    assert!(
        accepted.is_empty(),
        "Out-of-range double readings accepted: {accepted:?}"
    );
}

#[test]
fn assigned_aggregate_cannot_drop_a_mandatory_query_include() {
    let query = "correlate(a: { x=1 | n := count() } include: [x], b: { y=1 | y <=> a.x } include: [y], within=1h)";
    invalid(query, "CQL_CORRELATION_BRANCH");
    invalid(
        &query.replace("n := count()", "count()"),
        "CQL_CORRELATION_BRANCH",
    );
}

#[test]
fn assigned_aggregate_cannot_replace_an_array_filter_stream() {
    let query = r#"array:filter("a[]", var=e, function={n:=count()}, asArray="b[]")"#;
    invalid(query, "CQL_INLINE_EFFECT");
    invalid(&query.replace("n:=count()", "count()"), "CQL_INLINE_EFFECT");
}

#[test]
fn assigned_aggregates_obey_correlation_branch_placement() {
    for body in [
        "n:=count()",
        "n:=sum(value)",
        "case {true | n:=count(); * | n:=count()}",
        "user match {a => n:=count(); * => n:=count()}",
        "count()",
    ] {
        invalid(
            &format!("correlate(A:{{{body}}})"),
            "CQL_CORRELATION_BRANCH",
        );
    }
}

#[test]
fn assigned_aggregates_obey_inline_element_placement() {
    for (function, parameter) in [
        ("array:filter", "function"),
        ("array:eval", "function"),
        ("objectArray:eval", "function"),
        ("array:exists", "condition"),
        ("objectArray:exists", "condition"),
    ] {
        for body in [
            "n:=count()",
            "n:=sum(x)",
            "case {true | n:=count(); * | n:=count()}",
            "x match {a => n:=count(); * => n:=count()}",
            "count()",
        ] {
            invalid(
                &format!(r#"{function}("items[]",var=x,{parameter}={{{body}}})"#),
                "CQL_INLINE_EFFECT",
            );
        }
    }
}

#[test]
fn assigned_aggregate_and_element_controls_remain_accepted() {
    for query in [
        "n:=count()",
        "n:=sum(value)",
        "correlate(A:{user=*},aggregation=[B:{n:=count()}])",
        "stats({n:=count()})",
        r#"array:filter("items[]",var=x,function={test(x>1)})"#,
        r#"array:eval("items[]",asArray="out[]",var=x,function={out:=lower(x)})"#,
        r#"objectArray:eval("items[]",asArray="out[]",var=x,function={out:=lower(x.name)})"#,
    ] {
        ready(query);
    }
}

#[test]
fn assigned_renames_cannot_preserve_removed_inline_outputs() {
    for function in ["array:eval", "objectArray:eval"] {
        let query = format!(
            r#"{function}("items[]",asArray="out[]",var=x,function={{out:=1|renamed:=rename(out)}})"#
        );
        let result = check(&query);
        assert_eq!(result.outcome, Outcome::Unsupported, "{query}: {result:?}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_UNSUPPORTED_INLINE_OUTPUT")
        );
        ready(&format!(
            r#"{function}("items[]",asArray="out[]",var=x,function={{out:=1|renamed:=rename(out)|out:=renamed}})"#
        ));
    }
}

#[test]
fn sample_rejects_overflowing_numeric_literals() {
    invalid("sample(percentage=1e400)", "CQL_ARGUMENT_TYPE");
    invalid("percentile(x, accuracy=1e400)", "CQL_ARGUMENT_TYPE");
}

#[test]
fn numeric_bounds_compare_the_unrounded_decimal_literal() {
    for value in [
        "100.0000000000000001",
        "1.000000000000000001e2",
        "1000000000000000001e-16",
    ] {
        invalid(&format!("sample(percentage={value})"), "CQL_ARGUMENT_RANGE");
    }
    for value in ["100", "100.0000000000000000", "1e2", "99.9999999999999999"] {
        ready(&format!("sample(percentage={value})"));
    }
    invalid(
        "percentile(x,accuracy=0.99999999999999999)",
        "CQL_ARGUMENT_RANGE",
    );
    invalid("percentile(x,accuracy=1e-400)", "CQL_ARGUMENT_RANGE");
    ready("percentile(x,accuracy=0.999999999999999)");
    invalid(
        "percentile(x,accuracy=1.00000000000000001)",
        "CQL_ARGUMENT_RANGE",
    );
}

#[test]
fn catalog_numeric_bounds_have_literal_boundary_controls() {
    let templates = BTreeMap::from([
        (("groupBy", "limit"), "groupBy(a,limit=BOUND)"),
        (("sort", "limit"), "sort(a,limit=BOUND)"),
        (("table", "limit"), "table([a],limit=BOUND)"),
        (("bucket", "limit"), "bucket(span=1m,limit=BOUND)"),
        (("collect", "limit"), "collect(user,limit=BOUND)"),
        (
            ("communityId", "seed"),
            "communityId(sourceip=src,destinationip=dst,proto=proto,seed=BOUND)",
        ),
        (("fieldstats", "limit"), "fieldstats(limit=BOUND)"),
        (("hash", "limit"), "hash(user,limit=BOUND)"),
        (
            ("hashMatch", "bits"),
            "hashMatch(\"x\",salt=\"s\",bits=BOUND)",
        ),
        (
            ("hashRewrite", "bits"),
            "hashRewrite(user,salt=\"s\",bits=BOUND)",
        ),
        (("head", "limit"), "head(BOUND)"),
        (
            ("join", "limit"),
            "join(query={user=*},field=user,limit=BOUND)",
        ),
        (
            ("match", "nrows"),
            "match(\"lookup.csv\",field=user,nrows=BOUND)",
        ),
        (
            ("matchAsArray", "nrows"),
            "matchAsArray(\"lookup.csv\",field=user,nrows=BOUND)",
        ),
        (
            ("neighbor", "distance"),
            "sort(@timestamp)|neighbor(user,distance=BOUND)",
        ),
        (
            ("readFile", "limit"),
            "readFile(\"lookup.csv\",limit=BOUND)",
        ),
        (("sample", "percentage"), "sample(percentage=BOUND)"),
        (
            ("selfJoin", "limit"),
            "selfJoin(user,where=[{user=*}],limit=BOUND)",
        ),
        (
            ("slidingWindow", "events"),
            "sort(@timestamp)|slidingWindow(count(),events=BOUND)",
        ),
        (("tail", "limit"), "tail(BOUND)"),
        (
            ("text:editDistance", "maxDistance"),
            "text:editDistance(reference=\"x\",target=\"y\",maxDistance=BOUND)",
        ),
        (
            ("text:editDistanceAsArray", "maxDistance"),
            "text:editDistanceAsArray(references=[\"x\"],target=\"y\",maxDistance=BOUND)",
        ),
        (("timeChart", "buckets"), "timeChart(buckets=BOUND)"),
        (("timeChart", "limit"), "timeChart(limit=BOUND)"),
        (("top", "limit"), "top(user,limit=BOUND)"),
        (("transpose", "limit"), "transpose(limit=BOUND)"),
    ]);
    let mut covered = BTreeSet::new();
    let mut runtime_fields = BTreeSet::new();
    for function in catalog::FUNCTIONS {
        for rule in catalog::parameter_rules(function.name) {
            if rule.minimum.is_none() && rule.maximum.is_none() {
                continue;
            }
            let key = (function.name, rule.name.as_str());
            if function.argument_kind(&rule.name) == Some("f") {
                runtime_fields.insert(key);
                continue;
            }
            let template = templates
                .get(&key)
                .unwrap_or_else(|| panic!("missing boundary controls: {key:?}"));
            covered.insert(key);
            for (bound, direction) in [(rule.minimum, -1.0), (rule.maximum, 1.0)] {
                let Some(bound) = bound else { continue };
                assert_eq!(bound.fract(), 0.0, "{key:?}");
                ready(&template.replace("BOUND", &bound.to_string()));
                let outside = template.replace("BOUND", &(bound + direction).to_string());
                assert_ne!(check(&outside).outcome, Outcome::Ready, "{outside}");
                if direction > 0.0 {
                    let rounded = template.replace("BOUND", &format!("{bound}.0000000000000001"));
                    assert_ne!(check(&rounded).outcome, Outcome::Ready, "{rounded}");
                }
            }
            let overflow = template.replace("BOUND", "1e400");
            assert_ne!(check(&overflow).outcome, Outcome::Ready, "{overflow}");
        }
    }
    assert_eq!(covered, templates.keys().copied().collect());
    assert_eq!(covered.len(), 26);
    assert_eq!(
        runtime_fields,
        BTreeSet::from([
            ("communityId", "destinationport"),
            ("communityId", "icmpcode"),
            ("communityId", "icmptype"),
            ("communityId", "sourceport"),
        ])
    );
}

#[test]
fn numeric_parameters_keep_finite_types_and_documented_bounds() {
    for query in [
        "sample(percentage=100.01)",
        "percentile(value,accuracy=0)",
        "percentile(value,accuracy=1)",
    ] {
        invalid(query, "CQL_ARGUMENT_RANGE");
    }
    for query in [
        "sample(percentage=0)",
        "sample(percentage=100)",
        "sample(percentage=1e1)",
        "top(user,error=2.5)",
        "percentile(value,accuracy=0.01,percentiles=[0,50,100])",
    ] {
        ready(query);
    }
    for query in ["sample(percentage=true)", r#"sample(percentage="1")"#] {
        invalid(query, "CQL_ARGUMENT_TYPE");
    }
}

#[test]
fn numeric_siblings_reject_nonfinite_literals() {
    for query in [
        "top(user,error=1e400)",
        "percentile(value,accuracy=1e400)",
        "percentile(value,percentiles=[1e400])",
        "math:mod(value,divisor=1e400)",
        "math:pow(value,exponent=1e400)",
        "math:pow(value,exponent=1e400+1)",
    ] {
        invalid(query, "CQL_ARGUMENT_TYPE");
    }
    for query in [
        "math:mod(value,divisor=2.5)",
        "math:pow(value,exponent=2+1)",
        "math:pow(value,exponent=power)",
    ] {
        ready(query);
    }
}

#[test]
fn numeric_operands_reject_nonfinite_literals_inside_scalar_calls() {
    let queries = [
        "math:pow(value,exponent=if(condition=true,then=1e400,else=1))",
        "math:mod(value,divisor=if(condition=true,then=1e400,else=1))",
        "math:pow(value,exponent=coalesce([1e400,1]))",
        "math:pow(value,exponent=if(condition=true,then=1e400+1,else=1))",
    ];
    let accepted = queries
        .iter()
        .filter(|query| check(query).outcome == Outcome::Ready)
        .collect::<Vec<_>>();
    assert!(
        accepted.is_empty(),
        "Accepted nonfinite literals: {accepted:?}"
    );
    for query in queries {
        invalid(query, "CQL_ARGUMENT_TYPE");
        ready(&query.replace("1e400", "2"));
    }
}

#[test]
fn interval_siblings_reject_malformed_bounds() {
    for query in [
        r#"defineTable(name=t,query={count()},include=[_count],start="garbage")|readFile(t)"#,
        r#"defineTable(name=t,query={count()},include=[_count],end="garbage")|readFile(t)"#,
        r#"remoteTable(connection=c,name=t,query={count()},include=[_count],start="garbage")|readFile(t)"#,
        r#"remoteTable(connection=c,name=t,query={count()},include=[_count],end="garbage")|readFile(t)"#,
        r#"join(query={user=*},field=user,start="garbage")"#,
        r#"join(query={user=*},field=user,end="garbage")"#,
        r#"readPersistedAggregation("hourly",start="garbage")"#,
        r#"readPersistedAggregation("hourly",end="garbage")"#,
        r#"head(10)|slidingTimeWindow(count(),span="garbage")"#,
    ] {
        unsupported(query, "CQL_UNSUPPORTED_DURATION");
        ready(&query.replace("garbage", "7d"));
    }
}

#[test]
fn correlation_aggregation_constraints_read_inputs_before_assigned_reducers() {
    let pack = serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    for body in [
        "n:=count()",
        "count(as=n)",
        "case {true | v:=1; * | v:=2}|n:=count()|head(1)",
        "user match {a => v:=1; * => v:=2}|n:=count()|head(1)",
    ] {
        let result = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: format!(
                    "#repo=example|correlate(A:{{user=*}},aggregation=[B:{{{body}}}],globalConstraints=[user])"
                ),
                context: Context {
                    require_observed: true,
                    ..Context::default()
                },
            },
            Some(&pack),
        );
        assert_eq!(result.outcome, Outcome::Ready, "{body}: {result:?}");
    }
}

#[test]
fn correlation_constraints_preserve_preaggregate_branch_writes() {
    let pack = serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    for body in [
        "constrained:=1|n:=count()",
        "case {true | constrained:=1; * | constrained:=1}|n:=count()|head(1)",
        "user match {a => constrained:=1; * => constrained:=1}|n:=count()|head(1)",
        "case {true | constrained:=1; * | drop(constrained)}|constrained:=1|n:=count()",
    ] {
        let result = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: format!(
                    "#repo=example|correlate(A:{{constrained:=1}},aggregation=[B:{{{body}}}],globalConstraints=[constrained])"
                ),
                context: Context {
                    require_observed: true,
                    ..Context::default()
                },
            },
            Some(&pack),
        );
        assert_eq!(result.outcome, Outcome::Ready, "{body}: {result:?}");
    }
}

#[test]
fn correlation_removed_branch_inputs_require_context_control() {
    let pack = serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    for body in [
        "case {true | drop(user); * | drop(user)}|n:=count()|head(1)",
        "user match {a => drop(user); * => drop(user)}|n:=count()|head(1)",
        "drop(user)|n:=count()",
    ] {
        let result = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: format!(
                    "#repo=example|correlate(A:{{user=*}},aggregation=[B:{{{body}}}],globalConstraints=[user])"
                ),
                context: Context {
                    require_observed: true,
                    ..Context::default()
                },
            },
            Some(&pack),
        );
        assert_eq!(result.outcome, Outcome::NeedsContext, "{body}: {result:?}");
        assert!(
            result
                .grounding
                .missing
                .iter()
                .any(|field| field.contains("user"))
        );
    }
}

#[test]
fn interval_start_rejects_malformed_values() {
    unsupported(
        r#"setTimeInterval(start="garbage")"#,
        "CQL_UNSUPPORTED_DURATION",
    );
}

#[test]
fn sliding_time_spans_reject_malformed_and_negative_values() {
    let time_arguments = catalog::FUNCTIONS
        .iter()
        .flat_map(|function| {
            function
                .arguments
                .iter()
                .filter_map(move |(name, kind)| (*kind == "t").then_some((function.name, *name)))
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        time_arguments,
        BTreeSet::from([
            ("setTimeInterval", "start"),
            ("setTimeInterval", "end"),
            ("slidingTimeWindow", "span"),
        ])
    );
    for span in ["garbage", "-5m", "now", "0", "0s"] {
        unsupported(
            &format!("sort(@timestamp) | slidingTimeWindow(function=count(), span={span})"),
            "CQL_UNSUPPORTED_DURATION",
        );
    }
    for span in ["5m", "5000"] {
        ready(&format!(
            "sort(@timestamp) | slidingTimeWindow(function=count(), span={span})"
        ));
    }
}

#[test]
fn time_intervals_reject_provably_reversed_endpoints() {
    for query in [
        "setTimeInterval(start=1h, end=2h)",
        r#"setTimeInterval(start="-1h", end="-2h")"#,
        "setTimeInterval(start=+1h)",
        "setTimeInterval(start=2000,end=1000)",
        r#"setTimeInterval(start="2026-01-02T00:00:00Z",end="2026-01-01T00:00:00Z")"#,
        "setTimeInterval(start=+2h,end=+1h)",
        "setTimeInterval(start=now,end=1h)",
        r#"setTimeInterval(start="2026-01-01T00:00:00-05:00",end="2026-01-01T04:00:00Z")"#,
        r#"setTimeInterval(start=1767225600001,end="2026-01-01T00:00:00Z")"#,
        r#"setTimeInterval(start="2026-01-01T00:00:00.000000001Z",end=1767225600000)"#,
    ] {
        invalid(query, "CQL_TIME_INTERVAL_ORDER");
    }
    for query in [
        "setTimeInterval(start=2h,end=1h)",
        "setTimeInterval(start=1h,end=now)",
        "setTimeInterval(start=1h,end=1h)",
        "setTimeInterval(start=1000,end=2000)",
        r#"setTimeInterval(start="2026-01-01T00:00:00Z",end=now)"#,
        r#"setTimeInterval(start="2026-01-01T00:00:00-05:00",end="2026-01-01T05:00:00Z")"#,
        r#"setTimeInterval(start=1767225600000,end="2026-01-01T00:00:00Z")"#,
        "setTimeInterval(start=-5m,end=-1m)",
    ] {
        ready(query);
    }
    let unresolved = check("setTimeInterval(start=1h,end=?end)");
    assert_eq!(unresolved.outcome, Outcome::Unsupported);
    assert!(
        !unresolved
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_TIME_INTERVAL_ORDER")
    );
}

#[test]
fn top_rest_rejects_arbitrary_labels_in_raw_and_typed_calls() {
    invalid(r#"top(user,rest="others")"#, "CQL_ARGUMENT_VALUE");
    let request = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "input": {"mode": "plan", "plan": {"source": {}, "steps": [{
            "op": "call", "function": "top", "args": [
                {"name": "field", "value": {"kind": "field", "name": "user"}},
                {"name": "rest", "value": {"kind": "literal", "value": "others"}}
            ]
        }]}}
    }))
    .unwrap();
    let result = app::generate(request, None);
    assert_eq!(result.outcome, Outcome::Invalid, "{result:?}");
    assert!(result.query.is_none());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_ARGUMENT_VALUE")
    );
}

#[test]
fn interval_end_rejects_malformed_values() {
    unsupported(
        r#"setTimeInterval(start="7d",end="garbage")"#,
        "CQL_UNSUPPORTED_DURATION",
    );
}

#[test]
fn interval_endpoints_keep_relative_absolute_and_epoch_forms() {
    for query in [
        r#"setTimeInterval(start="7d")"#,
        r#"setTimeInterval(start="-7d",end=now)"#,
        r#"setTimeInterval(start="2026-01-01T00:00:00Z",end="2026-01-02T00:00:00+00:00")"#,
        "setTimeInterval(start=0,end=1767312000000)",
        r#"setTimeInterval(start="1767225600000",end="1767312000000")"#,
    ] {
        ready(query);
    }
}

#[test]
fn top_rest_accepts_boolean_spellings_of_its_string_choices() {
    for value in ["true", "false", r#""true""#, r#""false""#] {
        ready(&format!("top(user,rest={value})"));
    }
    for query in [
        r#"top(user,rest="yes")"#,
        "top(user,rest=yes)",
        r#"top(user,rest="others")"#,
    ] {
        invalid(query, "CQL_ARGUMENT_VALUE");
    }
    invalid("top(user,rest=1)", "CQL_ARGUMENT_VALUE");
    invalid("lower(user,as=true)", "CQL_ARGUMENT_TYPE");
    invalid("sort(user,order=true)", "CQL_ARGUMENT_TYPE");

    let boolean_strings = catalog::FUNCTIONS
        .iter()
        .flat_map(|function| {
            catalog::parameter_rules(function.name)
                .iter()
                .filter(|rule| {
                    function.argument_kind(&rule.name) == Some("s")
                        && rule.choices == ["false", "true"]
                })
                .map(move |rule| (function.name, rule.name.as_str()))
        })
        .collect::<Vec<_>>();
    assert_eq!(boolean_strings, [("top", "rest")]);
}

#[test]
fn named_remote_tables_and_documented_positional_forms_keep_their_bindings() {
    for query in [
        r#"remoteTable(connection=c,name=t,query="select user from events")|readFile(t)"#,
        "math:cosh(value)",
        "math:cosh(field=value)",
        r#"getField("user")"#,
        r#"getField(source="user")"#,
        "math:mod(value,divisor=3)",
        r#"parseTimestamp("seconds",field=timestamp)"#,
        r#"parseTimestamp(field=timestamp,format="seconds")"#,
    ] {
        ready(query);
    }
    invalid(
        r#"remoteTable(t,connection=c,query="select user from events")"#,
        "CQL_ARGUMENT_COUNT",
    );
    for (query, argument) in [
        (r#"parseTimestamp("seconds",field=timestamp)"#, "format"),
        (
            r#"parseTimestamp(field=timestamp,format="seconds")"#,
            "field",
        ),
    ] {
        let pipeline = cqlmatic::language::parser::parse(query).unwrap();
        let cqlmatic::language::ast::Step::Call(call) = &pipeline.steps[0] else {
            panic!("expected a function call");
        };
        let resolved = catalog::resolve(call).unwrap();
        assert_eq!(resolved.ordered[0].0, argument);
    }
}

#[test]
fn typed_top_rest_preserves_boolean_and_string_choices() {
    for value in [
        serde_json::json!(true),
        serde_json::json!(false),
        serde_json::json!("true"),
        serde_json::json!("false"),
    ] {
        let request = serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "input": {"mode": "plan", "plan": {"source": {}, "steps": [{
                "op": "call", "function": "top", "args": [
                    {"name": "field", "value": {"kind": "field", "name": "user"}},
                    {"name": "rest", "value": {"kind": "literal", "value": value}}
                ]
            }]}}
        }))
        .unwrap();
        let result = app::generate(request, None);
        assert_eq!(result.outcome, Outcome::Ready, "{result:?}");
        ready(result.query.as_deref().unwrap());
    }
}

#[test]
fn positional_contracts_match_omittable_parameter_facts() {
    let mut differences = BTreeMap::new();
    for function in catalog::FUNCTIONS {
        let positional = function.positional.iter().copied().collect::<BTreeSet<_>>();
        let omittable = catalog::parameter_rules(function.name)
            .iter()
            .filter(|rule| rule.omittable_name)
            .map(|rule| rule.name.as_str())
            .collect::<BTreeSet<_>>();
        if positional != omittable {
            differences.insert(function.name, (positional, omittable));
        }
    }
    // Finite example-only positional forms do not change parameter-table facts.
    // setTimeInterval requires named start despite its omittable-name table annotation.
    assert_eq!(
        differences,
        BTreeMap::from([
            ("getField", (BTreeSet::from(["source"]), BTreeSet::new())),
            ("math:cosh", (BTreeSet::from(["field"]), BTreeSet::new())),
            (
                "setTimeInterval",
                (BTreeSet::new(), BTreeSet::from(["start"]))
            ),
        ])
    );
}

#[test]
fn unqualified_time_spellings_follow_duration_coverage_rules() {
    let templates = [
        "setTimeInterval(start=TIME)",
        "join(query={user=*},field=user,start=TIME)",
        "defineTable(name=t,query={count()},include=[_count],start=TIME)|readFile(t)",
        "remoteTable(connection=c,name=t,query={count()},include=[_count],start=TIME)|readFile(t)",
        "readPersistedAggregation(\"hourly\",start=TIME)",
        "sort(@timestamp)|slidingTimeWindow(function=count(),span=TIME)",
    ];
    for value in [
        "1y",
        "1mon",
        "1q",
        "1year",
        "24hrs",
        "30mins",
        "10secs",
        "7D",
        "NOW",
        r#""2026-01-01T00:00:00""#,
        r#""garbage""#,
        "0d",
        r#""-0d""#,
    ] {
        for template in templates {
            unsupported(&template.replace("TIME", value), "CQL_UNSUPPORTED_DURATION");
        }
    }
    for query in [
        "bucket(span=garbage)",
        "timeChart(span=garbage)",
        "correlate(A:{user=*},B:{user=*},within=garbage)",
    ] {
        assert_eq!(check(query).outcome, Outcome::Unsupported, "{query}");
    }
    ready("setTimeInterval(start=0,end=1)");
}

#[test]
fn explicit_subquery_time_bounds_reject_reversed_endpoints() {
    for template in [
        "join(query={user=*},field=user,start=START,end=END)",
        "defineTable(name=t,query={count()},include=[_count],start=START,end=END)|readFile(t)",
        "remoteTable(connection=c,name=t,query={count()},include=[_count],start=START,end=END)|readFile(t)",
        "readPersistedAggregation(\"hourly\",start=START,end=END)",
    ] {
        for (start, end) in [
            (r#""1h""#, r#""2h""#),
            (r#""2000""#, r#""1000""#),
            (r#""2026-01-02T00:00:00Z""#, r#""2026-01-01T00:00:00Z""#),
        ] {
            invalid(
                &template.replace("START", start).replace("END", end),
                "CQL_TIME_INTERVAL_ORDER",
            );
            ready(&template.replace("START", end).replace("END", start));
        }
        ready(
            &template
                .replace("START", r#""2026-01-01T00:00:00Z""#)
                .replace("END", "now"),
        );
    }
}

#[test]
fn assigned_calls_warn_for_reads_dropped_by_prior_projection() {
    let queries = [
        "groupBy(a)|n:=max(b)",
        "select(a)|x:=rename(b)",
        "select(a)|x:=lower(b)",
        "select(a)|x:=b+1",
        "groupBy(a)|max(b)",
        "select(a)|x:=x+1",
    ];
    let missing = queries
        .iter()
        .filter(|query| {
            !check(query)
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD")
        })
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "Missing projection warnings: {missing:?}"
    );
    for query in queries {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
        let warnings = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD")
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1, "{query}: {result:?}");
        let input = if query.ends_with("x+1") { "'x'" } else { "'b'" };
        assert!(warnings[0].message.contains(input), "{query}: {result:?}");
    }
    for query in [
        "groupBy(a)|n:=max(a)",
        "select(a)|x:=rename(a)",
        "select(a)|x:=lower(a)",
        "select(a)|x:=a+1",
        "select(a)|x:=1",
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD"),
            "{query}: {result:?}"
        );
    }
}

#[test]
fn composite_aggregate_reads_keep_local_fields_scoped() {
    for query in [
        "select(@timestamp)|stats({x:=1|n:=count()})",
        "select(@timestamp)|stats({x:=@timestamp|n:=sum(x)})",
        "select(@timestamp)|stats({x:=1|count()})",
        "select(@timestamp)|stats({x:=@timestamp|sum(x)})",
        "select(@timestamp)|stats({case {true|x:=@timestamp;*|x:=@timestamp}|n:=sum(x)})",
        "select(a)|stats({b:=a|b match {yes => true; * => true}|n:=count()})",
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD"),
            "{query}: {result:?}"
        );
    }
    for (query, input) in [
        ("select(a)|stats({x:=b|n:=sum(x)})", "b"),
        ("select(a)|stats({x:=b|sum(x)})", "b"),
        ("select(a)|stats({x:=x+1|n:=sum(x)})", "x"),
        (
            "select(a)|stats({b match {yes => true; * => true}|n:=count()})",
            "b",
        ),
        (
            "select(a)|stats([sum(b,as=outside),{x:=1|sum(x,as=inside)}])",
            "b",
        ),
        (
            "select(a)|stats([{x:=1|sum(x,as=inside)},sum(b,as=outside)])",
            "b",
        ),
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
        let warnings = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD")
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1, "{query}: {result:?}");
        assert!(
            warnings[0].message.contains(&format!("'{input}'")),
            "{query}: {result:?}"
        );
    }
}

#[test]
fn continuing_queries_and_element_aggregates_keep_read_scopes() {
    for query in [
        "select([a,@timestamp])|series(a,startmatch={b=*})",
        "select([a,@timestamp])|!selfJoinFilter(a,where=[{b=*},{a=*}])",
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
        let warnings = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD")
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1, "{query}: {result:?}");
        assert!(warnings[0].message.contains("'b'"), "{query}: {result:?}");
    }
    for query in [
        "select([a,@timestamp])|series(a,startmatch={a=*})",
        r#"select(["values[]"])|array:reduceRow("values[]",function={sum(item)},var=item)"#,
        r#"select(["values[]"])|array:reduceRow("values[]",function=stats({sum(item)}),var=item)"#,
    ] {
        let result = check(query);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD"),
            "{query}: {result:?}"
        );
    }
}

#[test]
fn element_aggregates_shadow_only_the_bound_variable() {
    for function in ["array:reduceRow", "array:reduceColumn", "array:reduceAll"] {
        for reducer in ["sum(b)", "{sum(b)}", "{x:=item+b|sum(x)}", "{x:=b|sum(x)}"] {
            let query = format!(
                r#"select(["values[]"])|{function}("values[]",function={reducer},var=item)"#
            );
            let result = check(&query);
            assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
            let warnings = result
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD")
                .collect::<Vec<_>>();
            assert_eq!(warnings.len(), 1, "{query}: {result:?}");
            assert!(warnings[0].message.contains("'b'"), "{query}: {result:?}");
        }
        for reducer in [
            "sum(item)",
            "{sum(item)}",
            "stats({sum(item)})",
            "{x:=item|sum(x)}",
        ] {
            let query = format!(
                r#"select(["values[]"])|{function}("values[]",function={reducer},var=item)"#
            );
            let result = check(&query);
            assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
            assert!(
                !result
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD"),
                "{query}: {result:?}"
            );
        }
    }
    let result = check(
        r#"select([a,"item[]"])|array:reduceRow("item[]",function={x:=item+a|sum(x)},var=item)"#,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{result:?}");
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD"),
        "{result:?}"
    );
}

#[test]
fn flat_element_bindings_do_not_shadow_event_members() {
    for function in ["array:reduceRow", "array:reduceColumn", "array:reduceAll"] {
        for field in ["item.other", "item[0]"] {
            for reducer in [
                format!("sum({field})"),
                format!("{{sum({field})}}"),
                format!("{{x:=item+{field}|sum(x)}}"),
            ] {
                let query =
                    format!(r#"select(a)|{function}("values[]",function={reducer},var=item)"#);
                let result = check(&query);
                assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
                let warnings = result
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD")
                    .collect::<Vec<_>>();
                assert_eq!(warnings.len(), 1, "{query}: {result:?}");
                assert!(
                    warnings[0].message.contains(&format!("'{field}'")),
                    "{query}: {result:?}"
                );
                let control = check(&query.replace("select(a)", &format!("select({field})")));
                assert_eq!(control.outcome, Outcome::Ready, "{control:?}");
                assert!(
                    !control
                        .diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD"),
                    "{control:?}"
                );
            }
        }
    }
    for field in ["item.other", "item[0]"] {
        let query = format!(
            r#"objectArray:eval("objects[]",asArray="out[]",var=item,function={{select(a)|array:reduceRow("values[]",function=sum({field}),var=item)|out:=1}})"#
        );
        let result = check(&query);
        assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
        let warnings = result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD")
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1, "{query}: {result:?}");
        assert!(
            warnings[0].message.contains(&format!("'{field}'")),
            "{query}: {result:?}"
        );
        let control = check(&format!(
            r#"objectArray:eval("objects[]",asArray="out[]",var=item,function={{select(a)|out:={field}}})"#
        ));
        assert_eq!(control.outcome, Outcome::Ready, "{control:?}");
        assert!(
            !control
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD"),
            "{control:?}"
        );
    }
}

#[test]
fn implicit_structured_bindings_keep_longer_member_paths_local() {
    let query = r#"objectArray:eval("items.nested[]",asArray="out[]",function={select(a)|array:reduceRow("values[]",function=sum(items.nested.foo),var=items)|out:=1})"#;
    let result = check(query);
    assert_eq!(result.outcome, Outcome::Ready, "{result:?}");
    assert!(
        !result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_DROPPED_FIELD"),
        "{result:?}"
    );
}

#[test]
fn implicit_structured_bindings_keep_longer_member_paths_read_only() {
    unsupported(
        r#"objectArray:eval("items.nested[]",asArray="out[]",function={select(a)|array:reduceRow("values[]",function={items.nested.foo:=1|sum(items)},var=items)|out:=1})"#,
        "CQL_UNSUPPORTED_PSEUDO_WRITE",
    );
}

#[test]
fn set_time_interval_requires_named_start() {
    for query in [
        "setTimeInterval(1d) | count()",
        "setTimeInterval(1m)",
        r#"setTimeInterval("1h", end="30min")"#,
    ] {
        invalid(query, "CQL_ARGUMENT_COUNT");
    }
    for query in [
        "setTimeInterval(start=1d) | count()",
        "setTimeInterval(start=7d, end=1d) | count()",
        r#"setTimeInterval(start="1h", end="30min") | count()"#,
        "setTimeInterval(start=1746054000000, end=1746780124517)",
    ] {
        ready(query);
        assert!(
            check(query)
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_QUERY_TIME_OVERRIDE")
        );
    }
}
