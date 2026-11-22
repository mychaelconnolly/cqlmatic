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
        "join keeps one matching subquery row per key unless max is set, so the later comparison of {compared} may test only one of several matches."
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
        (") | kind=alert | test(alert_at > login_at)", "alert_at"),
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
        ") | message=/(?<alert_at>[0-9]+)/ | test(alert_at > login_at)",
        // The check stops at any stage other than a filter, test(), := or eval.
        ") | rename(alert_at, as=seen_at) | test(seen_at > login_at)",
        ") | rename(host, as=hostname) | test(alert_at > login_at)",
        ") | seen_at := rename(\"alert_at\") | test(seen_at > login_at)",
        ") | seen_at := lower(alert_at) | test(seen_at > login_at)",
        ") | rename(alert_at, as=seen) | groupBy([user], function=count(as=seen), limit=max) | seen > 1",
        ") | groupBy([user], function=max(alert_at, as=alert_at), limit=max) | alert_at > 0",
        ") | case { kind=vpn | note := 1; * } | test(alert_at > login_at)",
        ") | kind match { vpn => note := 1; * => * } | test(alert_at > login_at)",
        ") | formatTime(\"%H\", field=login_at, as=hour) | test(alert_at > login_at)",
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
        // One row per user that carries its host is one row per user and host.
        "alert_at := @timestamp | groupBy([user], function=selectFromMax(field=@timestamp, include=[host, alert_at]), limit=max)",
    ] {
        let query = joined(subquery);
        assert!(
            messages(&ready(&query), "CQL_JOIN_MAX").is_empty(),
            "{query}"
        );
    }
    for subquery in [
        "groupBy([user, host, process], function=max(@timestamp, as=alert_at), limit=max)",
        // Grouped by only some of the join keys, the subquery no longer has the others to join on.
        "groupBy([user], function=max(@timestamp, as=alert_at), limit=max)",
        "alert_at := @timestamp | groupBy([user, host], function=head(2), limit=max)",
        "alert_at := @timestamp | head(2)",
        // A groupBy limit is not taken into account.
        "groupBy([user, process], function=max(@timestamp, as=alert_at), limit=1)",
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
fn a_read_both_warnings_describe_gets_both() {
    for (query, unexported, dropped) in [
        (
            format!("{SEQUENCE} | groupBy([first.host], limit=max)"),
            "first.host is not exported by correlate query first.",
            "'first.host' is not among the known projected/aggregate outputs.",
        ),
        (
            // A removes user and reads it; B, separately, does not export it.
            "correlate(A: { select([other, @timestamp]) | user=* }, B: { select(@timestamp) | user <=> A.other })".into(),
            "user is not exported by correlate query B.",
            "'user' is not among the known projected/aggregate outputs.",
        ),
    ] {
        let result = ready(&query);
        assert_eq!(
            messages(&result, "CQL_UNEXPORTED_FIELD"),
            [unexported],
            "{query}"
        );
        assert_eq!(messages(&result, "CQL_DROPPED_FIELD"), [dropped], "{query}");
    }
}

#[test]
fn correlate_names_a_field_its_query_removed_before_correlate_reads_it() {
    for (query, message) in [
        (
            "correlate(first: { kind=a }, second: { kind=b | select([@timestamp]) | user <=> first.user }, within=5m)",
            "user is not exported by correlate query second.",
        ),
        (
            "correlate(query=first: { kind=a }, aggregation=[later: { kind=b | select([user]) | user <=> first.user | count() }], within=5m, root=\"first\")",
            "@timestamp is not exported by correlate query later.",
        ),
        (
            "correlate(first: { kind=a | drop([host]) } include: *, second: { kind=b | user <=> first.user }, within=5m) | first.host = *",
            "first.host is not exported by correlate query first.",
        ),
        (
            "correlate(first: { kind=a | rename(host, as=hostname) } include: *, second: { kind=b | user <=> first.user }, within=5m) | first.host = *",
            "first.host is not exported by correlate query first.",
        ),
    ] {
        assert_eq!(
            messages(&ready(query), "CQL_UNEXPORTED_FIELD"),
            [message],
            "{query}"
        );
    }
}

#[test]
fn a_field_removed_after_correlate_is_not_reported_against_it() {
    let query = "correlate(first: { kind=a | host := \"h1\" } include: *, second: { kind=b | user <=> first.user }, within=5m) | drop([first.host]) | first.host != *";
    assert!(messages(&ready(query), "CQL_UNEXPORTED_FIELD").is_empty());
}

fn generate(plan: serde_json::Value) -> Analysis {
    let result = app::generate(
        serde_json::from_value(
            serde_json::json!({"schema_version": 1, "input": {"mode": "plan", "plan": plan}}),
        )
        .unwrap(),
        None,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    result
}

#[test]
fn generate_reports_a_field_correlate_does_not_export() {
    let linked = generate(
        serde_json::json!({"source": {}, "steps": [{"op": "correlate", "within": "5m",
        "queries": [
            {"name": "first", "plan": {"source": {}, "filters": [{"op": "eq", "field": "kind", "value": "a"}]}},
            {"name": "second", "plan": {"source": {}, "filters": [{"op": "eq", "field": "kind", "value": "b"}],
                "steps": [{"op": "select", "fields": ["@timestamp"]}]}}
        ],
        "links": [{"from": "second", "field": "user", "to": "first", "key": "user"}]}]}),
    );
    assert_eq!(
        messages(&linked, "CQL_UNEXPORTED_FIELD"),
        ["user is not exported by correlate query second."]
    );
    let read = generate(serde_json::json!({"source": {}, "steps": [
        {"op": "call", "function": "correlate", "args": [{"value": {"kind": "named-query", "name": "first",
            "plan": {"source": {}, "filters": [{"op": "eq", "field": "kind", "value": "a"}]},
            "include": {"mode": "fields", "fields": ["user"]}}}]},
        {"op": "filter", "predicate": {"op": "exists", "field": "first.host"}}
    ]}));
    assert_eq!(
        messages(&read, "CQL_UNEXPORTED_FIELD"),
        ["first.host is not exported by correlate query first."]
    );
    assert_eq!(
        messages(&read, "CQL_DROPPED_FIELD"),
        ["'first.host' is not among the known projected/aggregate outputs."]
    );
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
    assert_eq!(messages(&result, "CQL_DROPPED_FIELD").len(), 1);
    assert!(
        result
            .grounding
            .missing
            .contains(&"*:*:first.host: not exported by preceding query.".to_string()),
        "{:?}",
        result.grounding.missing
    );
}

#[test]
fn correlate_sequence_by_equated_key_warns() {
    for (query, expected) in [
        (
            "correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, sequence=true, sequenceBy=user, within=5m)",
            "user",
        ),
        (
            "correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, sequence=true, sequenceBy=[user, kind], within=5m)",
            "user",
        ),
        (
            "correlate(first: { kind=a } include: [user], second: { kind=b }, globalConstraints=[user], sequence=true, sequenceBy=[user], within=5m)",
            "user",
        ),
        (
            "correlate(first: { kind=a | user <=> second.user } include: [user], second: { kind=b }, sequence=true, sequenceBy=user, within=5m)",
            "user",
        ),
    ] {
        let result = ready(query);
        let warns = messages(&result, "CQL_CORRELATION_SEQUENCE_BY");
        assert_eq!(warns.len(), 1, "{query}: {warns:?}");
        assert!(warns[0].contains(expected), "{query}: {}", warns[0]);
        assert!(warns[0].contains("@id"), "{query}: {}", warns[0]);
    }
    for quiet in [
        "correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, sequence=true, within=5m)",
        "correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, sequence=true, sequenceBy=@timestamp, within=5m)",
        "correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, sequence=true, sequenceBy=[@timestamp, @timestamp.nanos], within=5m)",
        "correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, sequence=true, sequenceBy=ordinal, within=5m)",
        "correlate(first: { kind=a } include: [src], second: { kind=b | dst <=> first.src }, sequence=true, sequenceBy=src, within=5m)",
        "correlate(first: { kind=a }, aggregation=[agg: { kind=b | count() }], globalConstraints=[user], sequence=true, sequenceBy=user, within=5m)",
    ] {
        assert!(
            messages(&ready(quiet), "CQL_CORRELATION_SEQUENCE_BY").is_empty(),
            "{quiet}"
        );
    }
}

#[test]
fn correlate_query_argument_forms() {
    for accepted in [
        "correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, sequence=true, within=5m)",
        "correlate(query=[first: { kind=a } include: [user], second: { kind=b | user <=> first.user }], sequence=true, within=5m)",
        "correlate([first: { kind=a } include: [user], second: { kind=b | user <=> first.user }], sequence=true, within=5m)",
        "correlate(query=first: { kind=a } include: [user], aggregation=[agg: { kind=b | user <=> first.user | count() }], within=5m)",
        "correlate(query=[first: { kind=a } include: [user]], aggregation=[agg: { kind=b | user <=> first.user | count() }], within=5m)",
    ] {
        let result = ready(accepted);
        assert!(
            result
                .diagnostics
                .iter()
                .all(|d| d.severity != Severity::Error),
            "{accepted}: {:?}",
            result.diagnostics
        );
    }
    for rejected in [
        "correlate(query=first: { kind=a }, second: { kind=b | user <=> first.user }, sequence=true, within=5m)",
        "correlate(first: { kind=a }, query=second: { kind=b | user <=> first.user }, sequence=true, within=5m)",
        "correlate(query=[first: { kind=a }], second: { kind=b | user <=> first.user }, sequence=true, within=5m)",
        "correlate([first: { kind=a }], second: { kind=b | user <=> first.user }, sequence=true, within=5m)",
    ] {
        let result = check(rejected);
        assert_eq!(result.outcome, Outcome::Invalid, "{rejected}: {result:?}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_DUPLICATE_ARGUMENT" && d.severity == Severity::Error),
            "{rejected}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn bucket_and_timechart_warn_when_series_limit_is_omitted() {
    for (query, fn_name) in [
        (
            "kind=conn | bucket(span=1h, field=[src], function=[count(as=n), sum(pts, as=score)]) | score >= 20",
            "bucket",
        ),
        (
            "kind=conn | bucket(span=1h, field=src, function=count())",
            "bucket",
        ),
        (
            "kind=conn | timeChart(series=src, span=1h, function=count())",
            "timeChart",
        ),
        (
            "kind=conn | timeChart(src, span=1h, function=count())",
            "timeChart",
        ),
    ] {
        let result = ready(query);
        let warns = messages(&result, "CQL_TIME_BUCKET_SERIES_LIMIT");
        assert_eq!(warns.len(), 1, "{query}: {warns:?}");
        assert!(warns[0].contains(fn_name), "{query}: {}", warns[0]);
        assert!(warns[0].contains("10"), "{query}: {}", warns[0]);
        assert!(warns[0].contains("500"), "{query}: {}", warns[0]);
    }
    for quiet in [
        "kind=conn | bucket(span=1h, function=count())",
        "kind=conn | bucket(span=1h, field=[], function=count())",
        "kind=conn | bucket(span=1h, field=[src], limit=500, function=count())",
        "kind=conn | bucket(span=1h, field=[src], limit=50, function=count())",
        "kind=conn | timeChart(span=1h, function=count())",
        "kind=conn | timeChart(series=src, span=1h, limit=500, function=count())",
    ] {
        assert!(
            messages(&ready(quiet), "CQL_TIME_BUCKET_SERIES_LIMIT").is_empty(),
            "{quiet}"
        );
    }
}

#[test]
fn match_against_unaggregated_define_table_warns_before_ordered_comparison() {
    let raw_table = "defineTable(name=\"events_b\", query={kind=b | seen_at := @timestamp | event_id := @id}, include=[key, seen_at, event_id]) | kind=a";
    for (rest, compared, cap_hint) in [
        (
            " | match(table=\"events_b\", field=key, column=key, include=[seen_at], nrows=max) | delta := seen_at - @timestamp | delta >= 0 | delta <= 60000",
            "delta",
            "500",
        ),
        (
            " | match(file=\"events_b\", field=key, column=key, include=[seen_at], nrows=max) | test(seen_at >= @timestamp)",
            "seen_at",
            "500",
        ),
        (
            " | match(table=\"events_b\", field=key, column=key, nrows=max) | delta := seen_at - @timestamp | test(delta >= 0)",
            "delta",
            "500",
        ),
        (
            " | match(table=\"events_b\", field=key, column=key, include=[seen_at]) | test(seen_at >= @timestamp)",
            "seen_at",
            "1",
        ),
        (
            " | match(table=\"events_b\", field=key, column=key, include=[seen_at], nrows=25) | seen_at > 100",
            "seen_at",
            "25",
        ),
    ] {
        let query = format!("{raw_table}{rest}");
        let result = ready(&query);
        let warns = messages(&result, "CQL_MATCH_NROWS");
        assert_eq!(warns.len(), 1, "{query}: {warns:?}");
        assert!(warns[0].contains(compared), "{query}: {}", warns[0]);
        assert!(warns[0].contains(cap_hint), "{query}: {}", warns[0]);
    }
    for non_exact_grouped in [
        "defineTable(name=\"events_b\", query={kind=b | groupBy([key], function=[max(@timestamp, as=last_ts)], limit=max)}, include=[key, last_ts]) | kind=a | match(table=\"events_b\", field=key, column=key, include=[last_ts], ignoreCase=true) | last_ts > 0",
        "defineTable(name=\"events_b\", query={kind=b | groupBy([key], function=[max(@timestamp, as=last_ts)], limit=max)}, include=[key, last_ts]) | kind=a | match(table=\"events_b\", field=key, column=key, include=[last_ts], mode=glob) | last_ts > 0",
        "defineTable(name=\"events_b\", query={kind=b | groupBy([key], function=[max(@timestamp, as=last_ts)], limit=max)}, include=[key, last_ts]) | kind=a | match(table=\"events_b\", field=key, column=key, include=[last_ts], mode=cidr) | last_ts > 0",
    ] {
        let result = ready(non_exact_grouped);
        assert_eq!(
            messages(&result, "CQL_MATCH_NROWS").len(),
            1,
            "{non_exact_grouped}"
        );
    }
    for quiet in [
        "defineTable(name=\"events_b\", query={kind=b | groupBy([key], function=[max(@timestamp, as=last_ts)], limit=max)}, include=[key, last_ts]) | kind=a | match(table=\"events_b\", field=key, column=key, include=[last_ts]) | test(last_ts >= @timestamp)",
        "defineTable(name=\"events_b\", query={kind=b | groupBy([k1, k2], function=[min(@timestamp, as=first_ts)], limit=max)}, include=[k1, k2, first_ts]) | kind=a | match(file=\"events_b\", field=[k1, k2], include=[first_ts], strict=false) | first_ts > 0",
        "defineTable(name=\"events_b\", query={kind=b | seen_at := @timestamp | head(1)}, include=[key, seen_at]) | kind=a | match(table=\"events_b\", field=key, column=key, include=[seen_at], ignoreCase=true) | seen_at > 0",
        "defineTable(name=\"events_b\", query={kind=b | seen_at := @timestamp | head(2)}, include=[key, seen_at]) | kind=a | match(table=\"events_b\", field=key, column=key, include=[seen_at], nrows=2) | seen_at > 0",
        "kind=a | match(file=\"users.csv\", field=user, column=user, include=[risk_score]) | risk_score >= 80",
        "defineTable(name=\"events_b\", query={kind=b | seen_at := @timestamp}, include=[key, seen_at]) | kind=a | !match(table=\"events_b\", field=key, column=key) | seen_at > 0",
        "defineTable(name=\"events_b\", query={kind=b | seen_at := @timestamp}, include=[key, seen_at]) | kind=a | match(table=\"events_b\", field=key, column=key, include=[key, seen_at], nrows=max) | key > 10",
        "defineTable(name=\"events_b\", query={kind=b | seen_at := @timestamp}, include=[key, seen_at]) | kind=a | match(table=\"events_b\", field=key, column=key, include=[seen_at], nrows=max) | seen_at = \"100\"",
    ] {
        assert!(
            messages(&ready(quiet), "CQL_MATCH_NROWS").is_empty(),
            "{quiet}"
        );
    }
}
