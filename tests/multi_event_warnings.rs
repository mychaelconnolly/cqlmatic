use cqlmatic::{app, contracts::*};

fn check_with(query: &str, require_observed: bool) -> Analysis {
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context {
                require_observed,
                ..Context::default()
            },
        },
        None,
    )
}

fn check(query: &str) -> Analysis {
    check_with(query, false)
}

/// The check result of a query that must be ready, so that a missing warning is not an invalid query.
fn ready(query: &str) -> Analysis {
    let result = check(query);
    assert_eq!(
        result.outcome,
        Outcome::Ready,
        "{query}: {:?}",
        result.diagnostics
    );
    result
}

fn messages<'a>(result: &'a Analysis, code: &str) -> Vec<&'a str> {
    result
        .diagnostics
        .iter()
        .filter(|d| d.code == code)
        .inspect(|d| assert_eq!(d.severity, Severity::Warning, "{d:?}"))
        .map(|d| d.message.as_str())
        .collect()
}

fn join_warning(compared: &str) -> String {
    format!(
        "join keeps one matching subquery row per key unless max is set, so the later comparison of {compared} tests only that row. Set max to the largest number of matches one key can have."
    )
}

const JOIN: &str = "kind=login | login_at := @timestamp | join({kind=alert | alert_at := @timestamp}, field=[user], include=[alert_at]";

#[test]
fn join_without_max_warns_before_a_comparison_of_a_joined_value() {
    for (rest, compared) in [
        (") | test(alert_at >= login_at)", "alert_at"),
        (
            ", mode=inner, limit=5000) | test(alert_at > login_at)",
            "alert_at",
        ),
        (") | gap := alert_at - login_at | test(gap <= 60000)", "gap"),
        (") | gap := alert_at - login_at | gap >= \"0\"", "gap"),
        (
            ") | gap := alert_at - login_at | gap > 0 gap < 60000",
            "gap",
        ),
        (") | eval(gap = alert_at - login_at) | gap < 60000", "gap"),
        (
            ") | eval(seen = alert_at, gap = seen - login_at) | test(gap > 0)",
            "gap",
        ),
        (
            ") | rename(alert_at, as=seen_at) | test(seen_at > login_at)",
            "seen_at",
        ),
        (
            ") | case { kind=vpn | alert_at := 1; * | note := 1 } | alert_at > 1",
            "alert_at",
        ),
        (
            ") | case { kind=vpn | seen_at := alert_at; * } | test(seen_at > login_at)",
            "seen_at",
        ),
    ] {
        let query = format!("{JOIN}{rest}");
        assert_eq!(
            messages(&ready(&query), "CQL_JOIN_MAX"),
            [join_warning(compared)],
            "{query}"
        );
    }
}

#[test]
fn join_with_max_or_without_a_comparison_of_a_joined_value_is_quiet() {
    for rest in [
        ", max=500) | test(alert_at >= login_at)",
        ", max=1) | gap := alert_at - login_at | test(gap <= 60000)",
        ") | groupBy([user], function=count())",
        ") | alert_at = \"x\"",
        ") | test(login_at > 0)",
        ") | alert_at := login_at | test(alert_at > 0)",
        ") | eval(alert_at = login_at) | test(alert_at > 0)",
        ") | case { kind=vpn | alert_at := 1; * | alert_at := 2 } | alert_at > 1",
    ] {
        let query = format!("{JOIN}{rest}");
        assert!(
            messages(&ready(&query), "CQL_JOIN_MAX").is_empty(),
            "{query}"
        );
    }
    let result = ready(
        "kind=login | test(login_at > 0) | join({kind=alert}, field=[user], include=[alert_at])",
    );
    assert!(messages(&result, "CQL_JOIN_MAX").is_empty());
}

#[test]
fn join_to_a_subquery_with_one_row_per_key_is_quiet() {
    let joined = |subquery: &str| {
        format!(
            "kind=login | join({{kind=alert | {subquery}}}, field=[user, host], include=[alert_at]) | test(alert_at > @timestamp)"
        )
    };
    for subquery in [
        "groupBy([user, host], function=max(@timestamp, as=alert_at), limit=max)",
        "groupBy([user, host], function=[count(as=alerts), max(@timestamp, as=alert_at)], limit=max)",
        "groupBy([user, host], function=max(@timestamp, as=alert_at), limit=max) | select([user, host, alert_at])",
        "alert_at := @timestamp | head(1)",
    ] {
        let query = joined(subquery);
        assert!(
            messages(&ready(&query), "CQL_JOIN_MAX").is_empty(),
            "{query}"
        );
    }
    for subquery in [
        "groupBy([user, host, process], function=max(@timestamp, as=alert_at), limit=max)",
        "alert_at := @timestamp | groupBy([user, host], function=head(2), limit=max)",
        "alert_at := @timestamp | head(2)",
    ] {
        let query = joined(subquery);
        assert_eq!(
            messages(&ready(&query), "CQL_JOIN_MAX"),
            [join_warning("alert_at")],
            "{query}"
        );
    }
    let result = ready(
        "kind=login | join({kind=alert | groupBy([account], function=count(as=alerts), limit=max)}, field=[user], key=[account], include=[alerts]) | alerts > 0",
    );
    assert!(messages(&result, "CQL_JOIN_MAX").is_empty());
}

#[test]
fn join_compared_on_its_own_key_is_quiet() {
    for query in [
        "kind=conn | join({flagged=true}, field=local_port, key=port, include=port) | port > 1023 | count()",
        "kind=conn | join({flagged=true}, field=[user], include=[user]) | test(user > \"m\")",
    ] {
        assert!(
            messages(&ready(query), "CQL_JOIN_MAX").is_empty(),
            "{query}"
        );
    }
    let query = "kind=conn | join({flagged=true}, field=local_port, key=port, include=[port, seen]) | seen > 1023 | count()";
    assert_eq!(
        messages(&ready(query), "CQL_JOIN_MAX"),
        [join_warning("seen")]
    );
}

const SEQUENCE: &str = "correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, sequence=true, within=5m)";

#[test]
fn correlate_names_a_field_it_does_not_export_and_its_query() {
    let cases = [
        (
            format!("{SEQUENCE} | groupBy([first.host], limit=max)"),
            "first.host is not exported by correlate query first.",
        ),
        (
            format!("{SEQUENCE} | groupBy([host, user], limit=max)"),
            "host, user are not exported by correlate, which names the fields of each query QUERY.FIELD.",
        ),
        (
            // The aggregation query's columns are unknown, but an unprefixed name is still not one.
            "correlate(query=first: { kind=a }, aggregation=[later: { kind=b | user <=> first.user | head(1) }], within=5m, root=\"first\") | groupBy([host], limit=max)".into(),
            "host is not exported by correlate, which names the fields of each query QUERY.FIELD.",
        ),
        (
            // Sequence order reads @timestamp from each query.
            "correlate(first: { kind=a | select([user]) } include: [user], second: { kind=b | user <=> first.user }, sequence=true, within=5m) | groupBy([first.user], limit=max)".into(),
            "@timestamp is not exported by correlate query first.",
        ),
        (
            "correlate(first: { kind=a | select([user, @timestamp]) } include: [user], second: { kind=b | user <=> first.account }, sequence=true, within=5m) | groupBy([first.user], limit=max)".into(),
            "account is not exported by correlate query first.",
        ),
    ];
    for (query, message) in cases {
        assert_eq!(
            messages(&ready(&query), "CQL_UNEXPORTED_FIELD"),
            [message],
            "{query}"
        );
    }
}

#[test]
fn exported_fields_other_drops_and_strict_mode_add_no_unexported_warning() {
    for query in [
        format!("{SEQUENCE} | groupBy([first.user, second.user], limit=max)"),
        format!("{SEQUENCE} | groupBy([first.user], function=count(@id), limit=max)"),
        "kind=a | groupBy([user], function=count(), limit=max) | test(host > 0)".into(),
    ] {
        let result = ready(&query);
        assert!(
            messages(&result, "CQL_UNEXPORTED_FIELD").is_empty(),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    let result = check_with(
        &format!("{SEQUENCE} | groupBy([first.host], limit=max)"),
        true,
    );
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert!(messages(&result, "CQL_UNEXPORTED_FIELD").is_empty());
    assert!(
        result
            .grounding
            .missing
            .contains(&"*:*:first.host: not exported by preceding query.".to_string()),
        "{:?}",
        result.grounding.missing
    );
}
