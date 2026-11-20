use cqlmatic::{app, contracts::*};
use serde_json::json;

const NOTE: &str = "CQL_ABSOLUTE_TIME_FILTER";

/// The check result of a query that must be ready, so that a missing note is not an invalid query.
fn ready(query: &str) -> Analysis {
    let result = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context::default(),
        },
        None,
    );
    assert_eq!(
        result.outcome,
        Outcome::Ready,
        "{query}: {:?}",
        result.diagnostics
    );
    result
}

fn notes(result: &Analysis) -> Vec<(&str, Severity)> {
    result
        .diagnostics
        .iter()
        .filter(|d| d.code == NOTE)
        .map(|d| (d.message.as_str(), d.severity.clone()))
        .collect()
}

const MESSAGE: &str = "A top-level filter compares @timestamp with a fixed time, which ties the query to one window. Set the time window with the search request's start and end instead.";

#[test]
fn a_fixed_time_comparison_in_the_leading_filter_gets_one_informational_note() {
    for query in [
        "@timestamp >= 1700000000000 @timestamp < 1700003600000 | count()",
        "@timestamp >= \"1700000000000\" | count()",
        "@timestamp >= 1700000000000 AND @timestamp < 1700003600000 | count()",
        "kind=a AND @timestamp >= 1700000000000 | count()",
        "#repo=example_repo kind=a | @timestamp > 1700000000000 | @timestamp <= 1700003600000 | count()",
        "kind=a | @timestamp >= -86400000",
        "kind=a | @timestamp >= 1700000000000 | case { level=high | points := 3; * | points := 0 } | groupBy([user], function=sum(points, as=score), limit=max) | sort(score, limit=3)",
    ] {
        assert_eq!(notes(&ready(query)), [(MESSAGE, Severity::Info)], "{query}");
    }
}

#[test]
fn a_comparison_outside_the_leading_filter_or_in_a_query_that_uses_the_window_gets_no_note() {
    for query in [
        "count()",
        "kind=a | test(@timestamp > now() - 3600000)",
        "kind=a | test(@timestamp > login_at)",
        "event_time >= 1700000000000 | count()",
        "@timestamp = 1700000000000 | count()",
        "kind=a OR @timestamp >= 1700000000000 | count()",
        "kind=a !(@timestamp >= 1700000000000) | count()",
        "kind=a | !test(@timestamp >= 1700000000000)",
        "kind=a | test(@timestamp >= 1700000000000) | count()",
        "sort(@timestamp, order=desc, limit=1) | @timestamp < 1700000000000",
        "parseTimestamp(field=logged_at, format=millis) | @timestamp >= 1700000000000",
        "kind=a | seen := 1 | @timestamp >= 1700000000000 | count()",
        "kind=a | !cidr(ip, subnet=\"10.0.0.0/8\") | @timestamp >= 1700000000000 | count()",
        "kind=a | case { @timestamp >= 1700000000000 | recent := 1; * | recent := 0 }",
        "kind=a | join({kind=b | @timestamp > 1700000000000}, field=[user], include=[host], max=10)",
        "kind=a @timestamp >= 1700000000000 | join({kind=b}, field=[user], include=[host], max=10)",
        "@timestamp >= 1700000000000 | correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, within=5m)",
        "defineTable(query={kind=b}, name=\"t\", include=[user]) | @timestamp >= 1700000000000 | match(table=\"t\", field=user)",
        "setTimeInterval(start=\"7d\") | @timestamp >= 1700000000000 | count()",
        "kind=a @timestamp >= 1700000000000 | timeChart(span=1h)",
        "kind=a @timestamp >= 1700000000000 | bucket(buckets=10)",
        "kind=a @timestamp >= 1700000000000 | start() | count()",
        "kind=a @timestamp >= 1700000000000 | case { level=high | end(as=window_end); * } | count()",
    ] {
        let result = ready(query);
        assert!(
            notes(&result).is_empty(),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

fn generate(filters: serde_json::Value) -> Analysis {
    let result = app::generate(
        serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{
            "source":{"repo":"example_repo"},
            "filters":filters,
            "steps":[{"op":"count"}]
        }}}))
        .unwrap(),
        None,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    result
}

#[test]
fn generate_keeps_the_plan_comparison_and_adds_the_note() {
    let result = generate(json!([
        {"op":"compare","field":"@timestamp","operator":"ge","value":1700000000000_i64},
        {"op":"compare","field":"@timestamp","operator":"lt","value":1700003600000_i64}
    ]));
    assert_eq!(
        result.query.as_deref(),
        Some(
            "#repo = \"example_repo\" | @timestamp >= \"1700000000000\" | @timestamp < \"1700003600000\" | count(as=\"_count\")"
        )
    );
    assert_eq!(notes(&result), [(MESSAGE, Severity::Info)]);
    for filters in [
        json!([{"op":"call","function":"test","args":[{"value":{"kind":"compare",
        "left":{"kind":"field","name":"@timestamp"},"operator":"ge",
        "right":{"kind":"literal","value":1700000000000_i64}}}]}]),
        json!([{"op":"any","items":[
            {"op":"eq","field":"kind","value":"alert"},
            {"op":"compare","field":"@timestamp","operator":"ge","value":1700000000000_i64}
        ]}]),
    ] {
        let result = generate(filters);
        assert!(notes(&result).is_empty(), "{:?}", result.diagnostics);
    }
}
