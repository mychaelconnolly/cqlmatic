use cqlmatic::{
    app,
    contracts::{Analysis, CheckRequest, Context, Outcome, Severity},
};
use std::process::Command;

const NOTES: [(&str, &[&str]); 10] = [
    ("kind=conn | groupBy([src])", &["CQL_GROUPBY_LIMIT"]),
    (
        "kind=a | join({kind=b}, field=k, key=k)",
        &["CQL_JOIN_COST"],
    ),
    (
        "kind=a | groupBy([k], limit=max) | join({kind=b}, field=k, key=k)",
        &["CQL_JOIN_COST", "CQL_JOIN_AFTER_AGGREGATE"],
    ),
    (
        "kind=a | count(field=user, distinct=true)",
        &["CQL_APPROXIMATE_DISTINCT"],
    ),
    (
        "correlate(first: { kind=a } include: [user], second: { kind=b | user <=> first.user }, sequence=true, within=5m)",
        &["CQL_CORRELATION_CAPABILITY", "CQL_TARGET_REQUIREMENT"],
    ),
    ("* | regex(\"needle\")", &["CQL_RAW_SCAN_COST"]),
    (
        "kind=a | match(file=\"hosts.csv\", field=host, column=host)",
        &["CQL_LOOKUP_COMPLETENESS", "CQL_TARGET_REQUIREMENT"],
    ),
    (
        "kind=conn | bucket(span=1h, function=count())",
        &["CQL_TIME_BUCKET_LIMIT"],
    ),
    (
        "kind=a | groupBy([k], function=collect([v]), limit=max)",
        &["CQL_COLLECT_LIMIT"],
    ),
    ("kind=a | split(items)", &["CQL_SPLIT_SHAPE"]),
];

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
fn notes_about_how_a_function_behaves_are_informational() {
    for (query, codes) in NOTES {
        let result = check(query);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        for code in codes {
            let severities: Vec<_> = result
                .diagnostics
                .iter()
                .filter(|d| d.code == *code)
                .map(|d| d.severity.clone())
                .collect();
            assert!(!severities.is_empty(), "{query}: no {code}");
            assert!(
                severities.iter().all(|s| *s == Severity::Info),
                "{query}: {code} is {severities:?}"
            );
        }
        assert_eq!(
            result.exit_code(true),
            0,
            "{query}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn fail_on_warning_ignores_notes_and_still_fails_on_a_warning() {
    // A saved inventory adds its own warnings about these invented fields.
    let data = std::env::temp_dir().join(format!(
        "cqlmatic-informational-notes-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&data).unwrap();
    for (query, flag, expected) in [
        ("kind=a | join({kind=b}, field=k, key=k)", true, 0),
        (
            "kind=a | groupBy([k], limit=max) | sort(_count) | head(3)",
            false,
            0,
        ),
        (
            "kind=a | groupBy([k], limit=max) | sort(_count) | head(3)",
            true,
            1,
        ),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
        command
            .env("LOCALAPPDATA", &data)
            .env("XDG_DATA_HOME", &data)
            .args(["check", query, "--json"]);
        if flag {
            command.arg("--fail-on-warning");
        }
        let output = command.output().unwrap();
        let result: Analysis = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.outcome, Outcome::Ready, "{query}");
        assert_eq!(
            output.status.code(),
            Some(expected),
            "{query} with flag {flag}: {:?}",
            result.diagnostics
        );
    }
    std::fs::remove_dir_all(&data).unwrap();
}
