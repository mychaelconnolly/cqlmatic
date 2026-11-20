use cqlmatic::{app, contracts::*};
use serde_json::json;

const NOTE: &str = "CQL_ABSOLUTE_TIME_FILTER";

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

fn notes(result: &Analysis) -> Vec<(&str, Severity)> {
    result
        .diagnostics
        .iter()
        .filter(|d| d.code == NOTE)
        .map(|d| (d.message.as_str(), d.severity.clone()))
        .collect()
}

const MESSAGE: &str = "@timestamp is compared with a fixed time, which ties the query to one window. Set the time window with the search request's start and end instead.";

#[test]
fn a_fixed_time_comparison_of_timestamp_gets_one_informational_note() {
    for query in [
        "@timestamp >= 1700000000000 @timestamp < 1700003600000 | count()",
        "@timestamp >= \"1700000000000\" | count()",
        "@timestamp >= 1700000000000 AND @timestamp < 1700003600000 | count()",
        "kind=a | test(@timestamp >= 1700000000000) | test(@timestamp < 1700003600000)",
        "kind=a | test((@timestamp <= 1700003600000))",
        "kind=a | test(1700000000000 < @timestamp)",
        "kind=a | join({kind=b | @timestamp > 1700000000000}, field=[user], include=[host], max=10)",
    ] {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(notes(&result), [(MESSAGE, Severity::Info)], "{query}");
    }
}

#[test]
fn relative_windows_other_fields_and_equality_get_no_note() {
    for query in [
        "count()",
        "kind=a | test(@timestamp > now() - 3600000)",
        "kind=a | test(@timestamp > login_at)",
        "event_time >= 1700000000000 | count()",
        "@timestamp = 1700000000000 | count()",
    ] {
        let result = check(query);
        assert!(
            notes(&result).is_empty(),
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn generate_keeps_the_plan_comparison_and_adds_the_note() {
    let result = app::generate(
        serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{
            "source":{"repo":"example_repo"},
            "filters":[
                {"op":"compare","field":"@timestamp","operator":"ge","value":1700000000000_i64},
                {"op":"compare","field":"@timestamp","operator":"lt","value":1700003600000_i64}
            ],
            "steps":[{"op":"count"}]
        }}}))
        .unwrap(),
        None,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    let query = result.query.as_deref().unwrap();
    assert!(
        query.contains("1700000000000") && query.contains("1700003600000"),
        "{query}"
    );
    assert_eq!(notes(&result), [(MESSAGE, Severity::Info)]);
}
