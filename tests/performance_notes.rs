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

const NOTES: [&str; 6] = [
    "CQL_TAG_FILTER_ORDER",
    "CQL_FILTER_AFTER_TRANSFORM",
    "CQL_SORT_COST",
    "CQL_SPLIT_COST",
    "CQL_WILDCARD_IGNORE_CASE",
    "CQL_REGEX_ALL_FIELDS",
];

/// The performance notes a query receives, each of which must be informational.
fn notes(query: &str) -> Vec<String> {
    let result = check(query);
    assert_eq!(result.outcome, Outcome::Ready, "{query}: {result:?}");
    result
        .diagnostics
        .into_iter()
        .filter(|d| NOTES.contains(&d.code.as_str()))
        .map(|d| {
            assert_eq!(d.severity, Severity::Info, "{query}: {d:?}");
            d.code
        })
        .collect()
}

#[test]
fn tag_filters_belong_before_other_stages() {
    for query in [
        "ComputerName=a | #event_simpleName=ProcessRollup2",
        "x := 1 | #event_simpleName=ProcessRollup2",
        r#""error" | #repo=example"#,
        "ComputerName=a | #repo=example | #type=json",
        "#repo=example | join({y := 1 | #type=a}, field=k)",
    ] {
        assert_eq!(notes(query), ["CQL_TAG_FILTER_ORDER"], "{query}");
    }
    for query in [
        "#repo=example | #event_simpleName=ProcessRollup2 | ComputerName=a",
        "ComputerName=a #event_simpleName=ProcessRollup2",
        "#repo=example | groupBy(k) | #type=json",
        "#repo=example | groupBy(k, function={y := 1 | #type=a | count()})",
        "#repo=example | case { a=1 | #type=json; * }",
        // Moving the filter would change the result.
        "selfJoinFilter(field=k, where=[{#repo=alpha},{#repo=beta}]) | #repo=alpha",
        "x := 1 | NOT (#repo=alpha AND x=1)",
        "#repo=example | in(a, values=[1, 2]) | #type=json",
        "#repo=example | rename(#type, as=kind) | #type=json",
    ] {
        assert!(notes(query).is_empty(), "{query}");
    }
}

#[test]
fn filters_that_ignore_earlier_transformations_are_noted_once() {
    for query in [
        "#repo=example | y := lower(a) | ComputerName=b",
        "#repo=example | y := 1 | !ComputerName=a",
        r#"#repo=example | eval(start = @timestamp - 600) | formatTime("%F", as=day, field=@timestamp) | ComputerName=b"#,
        "#repo=example | y := 1 | a=1 | b=2",
    ] {
        assert_eq!(notes(query), ["CQL_FILTER_AFTER_TRANSFORM"], "{query}");
    }
}

#[test]
fn filters_on_written_or_unlisted_fields_are_not_noted() {
    for query in [
        // The filter reads what an earlier stage wrote.
        "#repo=example | y := lower(a) | y=b",
        "#repo=example | eval(start = @timestamp - 600) | start > 5",
        "#repo=example | y := 1 | a.b := 2 | a.b=2",
        "#repo=example | y := 1 | rename(a, as=b) | b=1",
        r#"#repo=example | y := 1 | splitString(a, by=",", as=parts) | parts[0]=q"#,
        r"#repo=example | a=/(?<user>\w+)/ | y := 1 | user=bob",
        "#repo=example | y := lower(a) | a=b",
        "#repo=example | y := 1 | rename(a, as=b) | a=1",
        r#"#repo=example | array:rename(array="a[]", asArray="b[]") | a[0]=*"#,
        r#"#repo=example | text:editDistanceAsArray(target=a, references=["foo"], maxDistance=2, asArray="edits[]") | edits[0].reference=foo"#,
        // The filter extracts a field an earlier stage reads.
        "#repo=example | x := user | a=/(?<user>bob)/",
        // An earlier stage writes fields that cannot be listed, or is not a per-event step.
        "#repo=example | y := 1 | parseJson(field=payload) | user=bob",
        r#"#repo=example | y := 1 | regex("(?<user>\w+)") | user=bob"#,
        r#"#repo=example | y := 1 | match(file="f.csv", field=k) | col=1"#,
        "#repo=example | y := 1 | join({z=1}, field=k) | k=1",
        "#repo=example | y := 1 | drop(a) | b=1",
        r#"#repo=example | lowercase(field=I, include=fields, locale=tr) | "ı"=*"#,
        "#repo=example | y := 1 | in(a, values=[1, 2]) | b=2",
        "#repo=example | findTimestamp(field=msg, addErrors=true) | @error_msg[0]=*",
        "#repo=example | y := 1 | ioc:lookup(field=ip, type=ip_address) | ioc[0].type=ip_address",
        "#repo=example | y := 1 | case { a=1 | z := 2; * } | q=2",
        // Not a plain field comparison, no transformation yet, or already aggregated.
        r#"#repo=example | y := 1 | "powershell""#,
        "#repo=example | a=1 | b=2 | y := 1",
        "#repo=example | y := 1 | groupBy(k) | _count > 5",
        "#repo=example | groupBy(k, function={y := 1 | b=2 | count()})",
    ] {
        assert!(
            !notes(query).contains(&"CQL_FILTER_AFTER_TRANSFORM".to_string()),
            "{query}"
        );
    }
}

#[test]
fn sort_and_split_on_unnarrowed_events_are_noted() {
    assert_eq!(notes("sort(@timestamp)"), ["CQL_SORT_COST"]);
    assert_eq!(notes("y := 1 | sort(y)"), ["CQL_SORT_COST"]);
    assert_eq!(notes("split(items)"), ["CQL_SPLIT_COST"]);
    assert_eq!(
        notes(
            r#"defineTable({sort(@timestamp)}, name="t", include=user) | match(table=t, field=user)"#
        ),
        ["CQL_SORT_COST"]
    );
    for query in [
        "ComputerName=a | sort(@timestamp)",
        "@rawstring=/error/ | sort(@timestamp)",
        "groupBy(k) | sort(_count)",
        "count(k) | sort(_count)",
        "#repo=example a=b | split(items)",
        "a!=* | split(items)",
        // A function may drop the events it cannot process.
        "parseXml(field=payload, strict=true) | sort(@timestamp)",
        "lower(a) | split(items)",
    ] {
        assert!(notes(query).is_empty(), "{query}");
    }
}

#[test]
fn case_insensitive_wildcards_and_unfielded_regexes_are_noted() {
    assert_eq!(
        notes(r#"#repo=example | wildcard(field=a, pattern="*x*", ignoreCase=true)"#),
        ["CQL_WILDCARD_IGNORE_CASE"]
    );
    assert!(notes(r#"#repo=example | wildcard(field=a, pattern="*x*")"#).is_empty());
    assert!(notes(r#"#repo=example | wildcard(field=a, pattern="*", ignoreCase=true)"#).is_empty());
    assert!(
        notes(r#"#repo=example | wildcard(field=a, pattern="*x*", ignoreCase=false)"#).is_empty()
    );
    assert_eq!(
        notes("#repo=example | /powershell/i"),
        ["CQL_REGEX_ALL_FIELDS"]
    );
    assert!(notes("#repo=example | CommandLine=/powershell/i").is_empty());
    // After an assignment a named field may differ from the event's own value.
    assert!(notes(r#"#repo=example | a := "other" | /needle/"#).is_empty());
    // Function arguments and conditional branches carry no notes.
    for query in [
        "groupBy(k, function={x=1 | /foo/ | count()})",
        "#repo=example | case { x=1 | /foo/; * }",
        r#"case { wildcard(field=a, pattern="*x*", ignoreCase=true); * }"#,
        r#"x := if(wildcard(field=a, pattern="*x*", ignoreCase=true), then="a", else="b")"#,
    ] {
        assert!(notes(query).is_empty(), "{query}");
    }
    assert!(notes(r#"#repo=example | "powershell""#).is_empty());
    // Without an earlier narrowing filter the scan warning speaks instead.
    let unnarrowed = check("/powershell/i");
    assert!(
        unnarrowed
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_RAW_SCAN_COST" && d.severity == Severity::Warning)
    );
    assert!(
        unnarrowed
            .diagnostics
            .iter()
            .all(|d| d.code != "CQL_REGEX_ALL_FIELDS")
    );
}

#[test]
fn notes_do_not_change_the_warning_exit_code() {
    let noted = check("y := 1 | sort(y)");
    assert!(noted.diagnostics.iter().any(|d| d.code == "CQL_SORT_COST"));
    assert_eq!(noted.exit_code(false), 0);
    assert_eq!(noted.exit_code(true), 0);
}
