use cqlmatic::{
    app,
    contracts::{CheckRequest, Context, Outcome, Severity},
    language::{parser, validate},
};
use serde_json::json;
use std::process::Command;

fn raw_scan_warnings(query: &str) -> usize {
    let pipeline = parser::parse(query).unwrap();
    let report = validate::validate(&pipeline, query);
    assert_eq!(
        report.status,
        cqlmatic::contracts::Acceptance::Accepted,
        "{query}: {:?}",
        report.diagnostics
    );
    report
        .diagnostics
        .iter()
        .filter(|d| d.code == "CQL_RAW_SCAN_COST")
        .count()
}

#[test]
fn raw_regex_and_wildcard_forms_warn_without_changing_acceptance() {
    for query in [
        "/needle/",
        "@rawstring=/needle/",
        "@rawstring=\"needle*\"",
        "needle*",
        "regex(\"needle\")",
        "regex(\"needle\",field=@rawstring)",
        "wildcard(\"needle*\")",
        "wildcard(\"needle*\",field=@rawstring)",
        "wildcard(\"needle*\",field=[user,@rawstring])",
        "wildcard(\"needle*\",field=[])",
        "x:=regex(\"needle\")",
        "@rawstring match { /needle/ => x:=1; * => x:=2 }",
    ] {
        assert_eq!(raw_scan_warnings(query), 1, "{query}");
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
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_RAW_SCAN_COST" && d.severity == Severity::Warning)
        );
    }
}

#[test]
fn only_guaranteed_prior_narrowing_suppresses_the_advisory() {
    for query in [
        "#repo=example | regex(\"needle\")",
        "#type=example | wildcard(\"needle*\")",
        "host=endpoint | /needle/",
        "#repo=one OR #repo=two | regex(\"needle\")",
        "#repo=example AND * | regex(\"needle\")",
        "regex(\"endpoint\",field=host) | regex(\"needle\")",
        "wildcard(\"end*\",field=host) | regex(\"needle\")",
        "in(host,values=[\"endpoint\"]) | regex(\"needle\")",
        "case { #repo=one | x:=1; #repo=two | x:=2 } | regex(\"needle\")",
        "#repo=example | case { host=one | x:=1; * | x:=2 } | regex(\"needle\")",
        "#repo=example | objectArray:exists(\"items[]\",var=x,condition={regex(\"needle\")})",
        "#repo=example | array:eval(\"items[]\",asArray=\"out[]\",var=x,function={out:=regex(\"needle\")})",
    ] {
        assert_eq!(raw_scan_warnings(query), 0, "{query}");
    }
    for query in [
        "regex(\"needle\") | #repo=example",
        "#repo=example OR * | regex(\"needle\")",
        "not (#repo=example) | regex(\"needle\")",
        "#repo=* | regex(\"needle\")",
        "wildcard(\"*\",field=host) | regex(\"needle\")",
        "regex(\".*\",field=host) | regex(\"needle\")",
        "regex(\"endpoint\",field=host,strict=false) | regex(\"needle\")",
        "case { #repo=one | x:=1; * | x:=2 } | regex(\"needle\")",
        "#repo=example | readFile(\"events.csv\") | regex(\"needle\")",
        "#repo=example | join(query={regex(\"needle\")},field=id,include=[value],max=1)",
        "join(query={#repo=example},field=id,include=[value],max=1) | regex(\"needle\")",
    ] {
        assert_eq!(raw_scan_warnings(query), 1, "{query}");
    }
}

#[test]
fn typed_generation_uses_the_same_advisory_and_keeps_the_query() {
    for (source, expected) in [(json!({}), true), (json!({"repo":"example"}), false)] {
        let request = serde_json::from_value(json!({
            "schema_version":1,
            "input":{"mode":"plan","plan":{
                "source":source,
                "filters":[{"op":"regex","field":"@rawstring","pattern":"needle","flags":""}]
            }}
        }))
        .unwrap();
        let result = app::generate(request, None);
        assert_eq!(result.outcome, Outcome::Ready);
        assert!(result.query.is_some());
        assert_eq!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_RAW_SCAN_COST"),
            expected
        );
    }
}

#[test]
fn fail_on_warning_changes_exit_status_without_rewriting_or_rejecting_query() {
    for fail_on_warning in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
        command.args(["check", "regex(\"needle\")", "--json"]);
        if fail_on_warning {
            command.arg("--fail-on-warning");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(i32::from(fail_on_warning)));
        let result: cqlmatic::contracts::Analysis = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.outcome, Outcome::Ready);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_RAW_SCAN_COST")
        );
    }
}
