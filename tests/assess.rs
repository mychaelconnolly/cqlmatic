use cqlmatic::{
    app,
    assess::{self, AssessOutcome, AssessRequest, AssessResult, ColumnStats},
    contracts::{Acceptance, CheckRequest, Cloud, Context, EnvironmentId, Severity},
    environment::{Pack, PackData},
};
use serde_json::{Value, json};

fn assess(query: &str, result: Value) -> AssessResult {
    assess::assess(
        AssessRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            result,
            context: Context::default(),
        },
        None,
    )
}

fn finding(result: &AssessResult, code: &str, severity: Severity) {
    assert!(
        result
            .findings
            .iter()
            .any(|finding| finding.code == code && finding.severity == severity),
        "missing {code}: {:?}",
        result.findings
    );
}

fn rows(n: usize) -> Value {
    json!({
        "result": (0..n).map(|i| json!({"user": format!("user_{i}"), "_count": 1})).collect::<Vec<_>>()
    })
}

fn grouped_x_rows(n: usize) -> Value {
    json!({
        "result": (0..n).map(|i| json!({"x": format!("x_{i}"), "_count": 1})).collect::<Vec<_>>()
    })
}

#[test]
fn nonzero_count_is_no_findings_but_not_attested() {
    let request: AssessRequest =
        serde_json::from_str(include_str!("../examples/assess.request.json")).unwrap();
    let result = assess::assess(request, None);
    assert_eq!(result.id.as_deref(), Some("synthetic-count-1"));
    assert_eq!(
        result.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        result.findings
    );
    assert_eq!(result.rows, 1);
    assert_eq!(result.query_sha256, cqlmatic::sha256_hex(b"count()"));
    assert_eq!(
        result.columns["_count"],
        ColumnStats {
            present: 1,
            null: 0,
            empty: 0
        }
    );
    assert_eq!(result.findings.len(), 1);
    finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
}

#[test]
fn empty_result_is_inconclusive_and_zero_count_is_advisory() {
    let result = assess("count()", json!({"result":[]}));
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    finding(&result, "RESULT_EMPTY", Severity::Warning);
    finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
    for count in [json!("0"), json!(0)] {
        let result = assess("count()", json!({"result":[{"_count":count}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Advisory,
            "{:?}",
            result.findings
        );
        finding(&result, "RESULT_ZERO_COUNT", Severity::Warning);
    }
}

#[test]
fn zero_count_follows_aggregate_aliases_without_treating_stale_columns_as_counts() {
    let result = assess(
        "count(as=first) | count(as=second)",
        json!({"result":[{"first":0,"second":24}]}),
    );
    assert_eq!(result.outcome, AssessOutcome::NoFindings);
    finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
    assert!(
        !result
            .findings
            .iter()
            .any(|f| f.code == "RESULT_ZERO_COUNT")
    );

    let grouped = assess(
        "groupBy([], function=count(as=n))",
        json!({"result":[{"n":0}]}),
    );
    assert_eq!(grouped.outcome, AssessOutcome::Advisory);
    finding(&grouped, "RESULT_ZERO_COUNT", Severity::Warning);
    let stats = assess(
        "stats(function=[count(as=n), sum(score)])",
        json!({"result":[{"n":0,"_sum":1}]}),
    );
    assert_eq!(stats.outcome, AssessOutcome::Advisory);
    finding(&stats, "RESULT_ZERO_COUNT", Severity::Warning);
}

#[test]
fn zero_grouped_count_in_any_row_is_advisory() {
    for zero in [json!(0), json!("0")] {
        let result = assess(
            "groupBy([user])",
            json!({"result":[
                {"user":"a","_count":3},
                {"user":"b","_count":zero}
            ]}),
        );
        assert_eq!(
            result.outcome,
            AssessOutcome::Advisory,
            "{:?}",
            result.findings
        );
        finding(&result, "RESULT_ZERO_COUNT", Severity::Warning);
    }
}

#[test]
fn default_groupby_cap_is_reported_at_20000_not_19999() {
    let result = assess("groupBy([user])", rows(20_000));
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    assert_eq!(result.caps.len(), 1);
    assert_eq!(result.caps[0].stage, "groupBy");
    assert_eq!(result.caps[0].cap, 20_000);
    assert!(result.caps[0].defaulted);
    finding(&result, "RESULT_CAP_REACHED", Severity::Warning);
    finding(&result, "RESULT_ROW_LIMIT", Severity::Warning);
    assert!(
        result
            .findings
            .iter()
            .find(|f| f.code == "RESULT_CAP_REACHED")
            .unwrap()
            .message
            .contains("limit=max")
    );

    let below = assess("groupBy([user])", rows(19_999));
    assert_eq!(below.outcome, AssessOutcome::Inconclusive);
    finding(&below, "RESULT_ROW_LIMIT", Severity::Warning);
    assert!(
        !below
            .findings
            .iter()
            .any(|f| f.code == "RESULT_CAP_REACHED")
    );
}

#[test]
fn max_groupby_upstream_is_unverified_before_requested_head_size() {
    let result = assess("groupBy([user], limit=max) | head(10)", rows(10));
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    assert_eq!(
        result
            .caps
            .iter()
            .map(|cap| (cap.stage.as_str(), cap.cap))
            .collect::<Vec<_>>(),
        [("head", 10)]
    );
    finding(&result, "RESULT_CAP_REACHED", Severity::Info);
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    let message = &result
        .findings
        .iter()
        .find(|f| f.code == "RESULT_CAP_REACHED")
        .unwrap()
        .message;
    assert!(message.contains("head") && message.contains("10"));
}

#[test]
fn requested_table_and_default_top_caps_have_distinct_severity() {
    let table = assess("table([user], limit=5)", rows(5));
    assert_eq!(table.outcome, AssessOutcome::NoFindings);
    assert_eq!(table.caps[0].cap, 5);
    assert!(!table.caps[0].defaulted);
    finding(&table, "RESULT_CAP_REACHED", Severity::Info);

    let top = assess("top(user)", rows(10));
    assert_eq!(top.outcome, AssessOutcome::Inconclusive);
    assert_eq!(top.caps[0].cap, 10);
    assert!(top.caps[0].defaulted);
    finding(&top, "RESULT_CAP_REACHED", Severity::Warning);
}

#[test]
fn defaults_and_maxima_for_row_limited_stages_are_recorded() {
    for query in ["head()", "tail()", "sort(user)", "table([user])"] {
        let result = assess(query, rows(1));
        assert_eq!(result.caps.len(), 1, "{query}");
        assert_eq!(result.caps[0].cap, 200, "{query}");
        assert!(result.caps[0].defaulted, "{query}");
        assert_eq!(result.outcome, AssessOutcome::NoFindings, "{query}");
    }
    let table_max = assess("table([user], limit=max)", rows(1));
    assert!(table_max.caps.is_empty());
    assert_eq!(table_max.outcome, AssessOutcome::Inconclusive);
    finding(
        &table_max,
        "RESULT_UPSTREAM_CAP_UNVERIFIED",
        Severity::Warning,
    );
    let group_max = assess("groupBy([user], limit=max)", rows(1));
    assert!(group_max.caps.is_empty());
    assert_eq!(group_max.outcome, AssessOutcome::Inconclusive);
    finding(
        &group_max,
        "RESULT_UPSTREAM_CAP_UNVERIFIED",
        Severity::Warning,
    );
    let explicit_top = assess("top(user, limit=3)", rows(3));
    assert_eq!(explicit_top.caps[0].cap, 3);
    assert_eq!(explicit_top.outcome, AssessOutcome::NoFindings);
    finding(&explicit_top, "RESULT_CAP_REACHED", Severity::Info);
}

fn assess_accepted(query: &str, payload: Value) -> AssessResult {
    let validation = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context::default(),
        },
        None,
    );
    assert_eq!(
        validation.validation.status,
        Acceptance::Accepted,
        "{query}: {:?}",
        validation.diagnostics
    );
    assess(query, payload)
}

#[test]
fn upstream_numeric_groupby_cap_is_unverifiable_before_larger_head() {
    let result = assess_accepted("groupBy([user], limit=2) | head(100)", rows(2));
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    assert_eq!(
        result.caps.iter().map(|cap| cap.cap).collect::<Vec<_>>(),
        [2, 100]
    );
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    assert!(
        !result
            .findings
            .iter()
            .any(|f| f.code == "RESULT_CAP_REACHED")
    );
}

#[test]
fn upstream_numeric_groupby_cap_is_unverifiable_after_count() {
    let result = assess_accepted(
        "groupBy([user], limit=2) | count()",
        json!({"result":[{"_count":2}]}),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    assert_eq!(result.caps[0].cap, 2);
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    assert!(
        !result
            .findings
            .iter()
            .any(|f| f.code == "RESULT_CAP_REACHED")
    );
}

#[test]
fn nested_define_table_groupby_cap_is_unverifiable() {
    let result = assess_accepted(
        "defineTable(name=t, query={ #repo=fw_repo | groupBy([user], limit=1) }, include=[user]) | readFile(t)",
        json!({"result":[{"user":"a"}]}),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    assert_eq!(result.caps[0].stage, "groupBy");
    assert_eq!(result.caps[0].cap, 1);
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    assert!(
        !result
            .findings
            .iter()
            .any(|f| f.code == "RESULT_CAP_REACHED")
    );
}

#[test]
fn nested_join_groupby_cap_is_unverifiable() {
    let result = assess_accepted(
        "join({ #repo=fw_repo | groupBy([user], limit=1) }, field=user, key=user, include=[user])",
        json!({"result":[{"user":"a"}]}),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    assert_eq!(
        result
            .caps
            .iter()
            .map(|cap| cap.stage.as_str())
            .collect::<Vec<_>>(),
        ["join", "groupBy"]
    );
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
}

#[test]
fn default_join_subquery_limit_is_unverifiable() {
    let result = assess_accepted(
        "join({ #repo=fw_repo }, field=user, key=user, include=[note]) | count(as=matches)",
        json!({"result":[{"matches":1}]}),
    );
    assert_eq!(
        result.outcome,
        AssessOutcome::Inconclusive,
        "{:?}",
        result.findings
    );
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    assert_eq!(result.caps[0].stage, "join");
    assert_eq!(result.caps[0].cap, 100_000);
    assert!(result.caps[0].defaulted);
    assert!(
        result
            .findings
            .iter()
            .any(|f| f.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                && f.message.contains("join() subquery row cap of 100000")
                && f.message.contains("not visible"))
    );
}

#[test]
fn numeric_and_nested_join_subquery_caps_are_unverifiable() {
    for query in [
        "join({ #repo=fw_repo }, field=user, limit=2) | count(as=matches)",
        "join({ #repo=fw_repo | join({ #repo=fw_repo }, field=user, limit=2) }, field=user) | count(as=matches)",
    ] {
        let result = assess_accepted(query, json!({"result":[{"matches":1}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            result
                .caps
                .iter()
                .any(|cap| cap.stage == "join" && cap.cap == 2 && !cap.defaulted)
        );
        assert!(
            result
                .findings
                .iter()
                .any(|f| f.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                    && f.severity == Severity::Warning
                    && f.message.contains("explicit join() subquery row cap of 2")
                    && f.message.contains("not visible")),
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn groupby_max_upstream_is_unverified_before_requested_sort_size() {
    let result = assess_accepted(
        "groupBy([x], limit=max) | sort(_count, limit=10)",
        grouped_x_rows(10),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    finding(&result, "RESULT_CAP_REACHED", Severity::Info);
    let message = &result
        .findings
        .iter()
        .find(|f| f.code == "RESULT_CAP_REACHED")
        .unwrap()
        .message;
    assert!(message.contains("requested sort size"), "{message}");
}

#[test]
fn default_groupby_upstream_remains_inconclusive_before_requested_sort() {
    let result = assess_accepted("groupBy([x]) | sort(_count, limit=10)", grouped_x_rows(10));
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    finding(&result, "RESULT_CAP_REACHED", Severity::Info);
}

#[test]
fn requested_head_size_is_informational_at_ten_rows() {
    let result = assess_accepted("head(10)", rows(10));
    assert_eq!(result.outcome, AssessOutcome::NoFindings);
    finding(&result, "RESULT_CAP_REACHED", Severity::Info);
}

#[test]
fn default_table_cap_is_inconclusive_at_two_hundred_rows() {
    let result = assess_accepted("table([user])", rows(200));
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    assert!(result.caps[0].defaulted);
    finding(&result, "RESULT_CAP_REACHED", Severity::Warning);
}

#[test]
fn state_row_limit_default_cap_messages_preserve_unknown_target_maximum() {
    for query in ["sort(user)", "head()", "tail()", "table([user])"] {
        let result = assess_accepted(query, rows(200));
        assert_eq!(result.outcome, AssessOutcome::Inconclusive, "{query}");
        finding(&result, "RESULT_CAP_REACHED", Severity::Warning);
        let message = &result
            .findings
            .iter()
            .find(|f| f.code == "RESULT_CAP_REACHED")
            .unwrap()
            .message;
        assert_eq!(result.caps[0].cap, 200);
        assert!(result.caps[0].defaulted);
        assert!(message.contains("StateRowLimit"), "{message}");
        assert!(
            message.contains("effective numeric cap is unknown"),
            "{message}"
        );
        assert!(!message.contains("has no limit=max"), "{message}");
        assert!(!message.contains("limit=max is"), "{message}");
        finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
    }
}

#[test]
fn state_row_limit_max_reports_unknown_target_cap_in_final_upstream_and_nested_queries() {
    for (stage, call) in [
        ("sort", "sort(user, limit=max)"),
        ("head", "head(limit=max)"),
        ("tail", "tail(limit=max)"),
        ("table", "table([user], limit=max)"),
    ] {
        for (query, returned) in [
            (call.to_string(), 1),
            (call.to_string(), 200),
            (format!("{call} | count()"), 1),
            (format!("groupBy([user], function={call}, limit=max)"), 1),
            (
                format!("defineTable(name=t, query={{ {call} }}, include=[user]) | readFile(t)"),
                1,
            ),
            (format!("join({{ {call} }}, field=user)"), 1),
        ] {
            let result = assess_accepted(&query, rows(returned));
            assert_eq!(result.outcome, AssessOutcome::Inconclusive, "{query}");
            assert!(result.caps.iter().all(|cap| cap.stage != stage), "{query}");
            finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
            finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
            assert!(
                result.findings.iter().any(|finding| {
                    finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                        && finding.severity == Severity::Warning
                        && finding.message.contains(&format!("{stage}(limit=max)"))
                        && finding.message.contains("StateRowLimit")
                        && finding.message.contains("effective numeric cap is unknown")
                }),
                "{query}: {:?}",
                result.findings
            );
            assert!(
                !result.findings.iter().any(|finding| {
                    finding.code == "RESULT_CAP_REACHED" && finding.message.contains(stage)
                }),
                "{query}"
            );
        }
    }
}

#[test]
fn explicit_state_row_limit_integer_caps_remain_requested_sizes() {
    for (stage, query) in [
        ("sort", "sort(user, limit=2)"),
        ("head", "head(limit=2)"),
        ("tail", "tail(limit=2)"),
        ("table", "table([user], limit=2)"),
    ] {
        let result = assess_accepted(query, rows(2));
        assert_eq!(result.outcome, AssessOutcome::NoFindings, "{query}");
        assert_eq!(result.caps.len(), 1);
        assert_eq!(result.caps[0].stage, stage);
        assert_eq!(result.caps[0].cap, 2);
        assert!(!result.caps[0].defaulted);
        finding(&result, "RESULT_CAP_REACHED", Severity::Info);
        finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
        assert!(
            !result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED")
        );
    }
}

#[test]
fn row_limit_at_10000_is_inconclusive() {
    let result = assess("select([user])", rows(10_000));
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    finding(&result, "RESULT_ROW_LIMIT", Severity::Warning);
    assert!(
        !result
            .findings
            .iter()
            .any(|f| f.code == "RESULT_CAP_REACHED")
    );
}

#[test]
fn missing_null_and_empty_columns_report_rates_without_row_values() {
    let result = assess(
        "groupBy([user])",
        json!({"result":[
            {"user":"private-row-token", "_count":1},
            {"user":null, "_count":1},
            {"user":"", "_count":1},
            {"_count":1}
        ]}),
    );
    assert_eq!(
        result.outcome,
        AssessOutcome::Advisory,
        "{:?}",
        result.findings
    );
    assert_eq!(
        result.columns["user"],
        ColumnStats {
            present: 3,
            null: 1,
            empty: 1
        }
    );
    assert_eq!(result.columns["_count"].present, 4);
    let diagnostics = result
        .findings
        .iter()
        .filter(|f| f.code == "RESULT_NULL_FIELDS")
        .collect::<Vec<_>>();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, Severity::Warning);
    assert!(
        diagnostics[0]
            .message
            .contains("missing 1/4, null 1/4, empty 1/4")
    );
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("private-row-token")
    );
}

fn coverage_pack() -> Pack {
    let mut fields = vec![json!({
        "id":"env:field:example:user",
        "source_id":"env:repo:example",
        "path":"a.b.user",
        "data_type":"string",
        "evidence_kind":"observed",
        "is_tag":false,
        "provenance_ids":["prov:synthetic"],
        "coverage":{"rate":0.6,"observed":true}
    })];
    for (name, kind) in [
        ("a.b.actor", "observed"),
        ("a.b.login", "observed"),
        ("a.c.unrelated", "observed"),
        ("a.b.declared", "declared"),
        ("a.b.sibling1", "observed"),
        ("a.b.sibling2", "observed"),
        ("a.b.sibling3", "observed"),
        ("a.b.sibling4", "observed"),
    ] {
        fields.push(json!({
            "id":format!("env:field:example:{name}"),
            "source_id":"env:repo:example",
            "path":name,
            "data_type":"string",
            "evidence_kind":kind,
            "is_tag":false,
            "provenance_ids":["prov:synthetic"]
        }));
    }
    Pack::new(
        EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "search-all".into(),
        },
        "2026-01-01T00:00:00Z".into(),
        PackData {
            sources: vec![json!({
                "id":"env:repo:example",
                "kind":"repo",
                "name":"example",
                "inventory_status":"observed-only",
                "selectors":{"repo":"example"},
                "provenance_ids":["prov:synthetic"]
            })],
            fields,
            provenance: vec![json!({
                "id":"prov:synthetic",
                "source_type":"ngsiem-api",
                "canonical_source":"synthetic example",
                "retrieved_at":"2026-01-01T00:00:00Z",
                "redaction_status":"none",
                "coverage_status":"complete"
            })],
            ..PackData::default()
        },
    )
    .unwrap()
}

#[test]
fn incomplete_key_coverage_lists_observed_sibling_paths_only() {
    let pack = coverage_pack();
    let request = AssessRequest {
        schema_version: 1,
        id: None,
        query: "#repo=example | groupBy([a.b.user])".into(),
        result: json!({"result":[{"a.b.user":"actor_1","_count":1}]}),
        context: Context::default(),
    };
    let result = assess::assess(request, Some(&pack));
    assert_eq!(
        result.outcome,
        AssessOutcome::Advisory,
        "{:?}",
        result.findings
    );
    let coverage = result
        .findings
        .iter()
        .find(|f| f.code == "RESULT_KEY_COVERAGE")
        .unwrap();
    assert_eq!(coverage.severity, Severity::Warning);
    assert!(coverage.message.contains("0.6"));
    assert!(coverage.message.contains("a.b.actor"));
    assert!(coverage.message.contains("a.b.login"));
    assert!(!coverage.message.contains("a.c.unrelated"));
    assert!(!coverage.message.contains("a.b.declared"));
    assert!(coverage.message.contains("a.b.sibling3"));
    assert!(!coverage.message.contains("a.b.sibling4"));
    assert!(!serde_json::to_string(&result).unwrap().contains("actor_1"));
}

#[test]
fn filter_fields_also_check_pack_coverage() {
    let result = assess::assess(
        AssessRequest {
            schema_version: 1,
            id: None,
            query: "#repo=example a.b.user=\"synthetic\" | count()".into(),
            result: json!({"result":[{"_count":"24"}]}),
            context: Context::default(),
        },
        Some(&coverage_pack()),
    );
    assert_eq!(result.outcome, AssessOutcome::Advisory);
    finding(&result, "RESULT_KEY_COVERAGE", Severity::Warning);
}

#[test]
fn empty_results_still_report_key_coverage_from_pack() {
    let result = assess::assess(
        AssessRequest {
            schema_version: 1,
            id: None,
            query: "#repo=example | groupBy([a.b.user])".into(),
            result: json!({"result":[]}),
            context: Context::default(),
        },
        Some(&coverage_pack()),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    finding(&result, "RESULT_EMPTY", Severity::Warning);
    finding(&result, "RESULT_KEY_COVERAGE", Severity::Warning);
}

#[test]
fn legacy_untagged_field_still_reports_key_coverage() {
    let mut pack = coverage_pack();
    pack.data.fields[0]
        .as_object_mut()
        .unwrap()
        .remove("is_tag");
    pack.content_digest = pack.digest().unwrap();
    pack.validate().unwrap();
    let result = assess::assess(
        AssessRequest {
            schema_version: 1,
            id: None,
            query: "#repo=example | groupBy([a.b.user])".into(),
            result: json!({"result":[{"a.b.user":"a","_count":1}]}),
            context: Context::default(),
        },
        Some(&pack),
    );
    assert_eq!(result.outcome, AssessOutcome::Advisory);
    finding(&result, "RESULT_KEY_COVERAGE", Severity::Warning);
}

#[test]
fn coverage_keeps_tagged_and_ordinary_same_name_fields_separate() {
    for (tag_rate, ordinary_rate) in [(0.6, 1.0), (1.0, 0.4)] {
        for ordinary_namespace in [Some(false), None] {
            let mut pack = coverage_pack();
            let mut tagged = pack.data.fields[0].clone();
            tagged["id"] = json!("env:field:example:tagged-kind");
            tagged["path"] = json!("event.kind");
            tagged["is_tag"] = json!(true);
            tagged["coverage"]["rate"] = json!(tag_rate);
            let mut ordinary = tagged.clone();
            ordinary["id"] = json!("env:field:example:ordinary-kind");
            ordinary["coverage"]["rate"] = json!(ordinary_rate);
            if let Some(is_tag) = ordinary_namespace {
                ordinary["is_tag"] = json!(is_tag);
            } else {
                ordinary.as_object_mut().unwrap().remove("is_tag");
            }
            pack.data.fields = vec![ordinary, tagged];
            pack.content_digest = pack.digest().unwrap();
            pack.validate().unwrap();

            for (key, rate) in [("#event.kind", tag_rate), ("event.kind", ordinary_rate)] {
                let result = assess::assess(
                    AssessRequest {
                        schema_version: 1,
                        id: None,
                        query: format!("#repo=example | groupBy([{key}])"),
                        result: json!({"result":[{key:"synthetic","_count":1}]}),
                        context: Context::default(),
                    },
                    Some(&pack),
                );
                let expected = if rate < 1.0 {
                    AssessOutcome::Advisory
                } else {
                    AssessOutcome::NoFindings
                };
                assert_eq!(result.outcome, expected, "{key}: {:?}", result.findings);
                let coverage = result
                    .findings
                    .iter()
                    .filter(|finding| finding.code == "RESULT_KEY_COVERAGE")
                    .collect::<Vec<_>>();
                assert_eq!(coverage.len(), usize::from(rate < 1.0));
                if let Some(finding) = coverage.first() {
                    assert!(finding.message.contains(&format!("Key '{key}'")));
                    assert!(finding.message.contains(&format!("rate {rate}")));
                }
            }
        }
    }
}

#[test]
fn tagged_coverage_normalizes_one_prefix_and_lists_same_namespace_siblings() {
    let mut pack = coverage_pack();
    pack.data.fields[0]["path"] = json!("event.kind");
    pack.data.fields[0]["is_tag"] = json!(true);
    pack.data.fields[1]["path"] = json!("event.category");
    pack.data.fields[1]["is_tag"] = json!(true);
    pack.data.fields[2]["path"] = json!("event.action");
    pack.content_digest = pack.digest().unwrap();
    pack.validate().unwrap();

    for (key, expected) in [
        ("#event.kind", AssessOutcome::Advisory),
        ("##event.kind", AssessOutcome::NoFindings),
    ] {
        let result = assess::assess(
            AssessRequest {
                schema_version: 1,
                id: None,
                query: format!("#repo=example | groupBy([{key}])"),
                result: json!({"result":[{key:"synthetic","_count":1}]}),
                context: Context::default(),
            },
            Some(&pack),
        );
        assert_eq!(result.outcome, expected, "{key}: {:?}", result.findings);
        if expected == AssessOutcome::Advisory {
            let finding = result
                .findings
                .iter()
                .find(|finding| finding.code == "RESULT_KEY_COVERAGE")
                .unwrap();
            assert!(finding.message.contains("#event.category"));
            assert!(!finding.message.contains("event.action"));
        }
    }
}

#[test]
fn uncertain_or_missing_positive_source_does_not_apply_source_coverage() {
    let pack = coverage_pack();
    for query in [
        "groupBy([a.b.user])",
        "#repo!=other | groupBy([a.b.user])",
        "#repo=exam* | groupBy([a.b.user])",
        "(#repo=example OR #repo=other) | groupBy([a.b.user])",
        "#repo=example | #repo!=other | groupBy([a.b.user])",
    ] {
        let result = assess::assess(
            AssessRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                result: json!({"result":[{"a.b.user":"a","_count":1}]}),
                context: Context::default(),
            },
            Some(&pack),
        );
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            !result
                .findings
                .iter()
                .any(|f| f.code == "RESULT_KEY_COVERAGE"),
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn fields_in_test_expression_are_checked_for_coverage() {
    let result = assess::assess(
        AssessRequest {
            schema_version: 1,
            id: None,
            query: r#"#repo=example | test(a.b.user == "synthetic") | count()"#.into(),
            result: json!({"result":[{"_count":24}]}),
            context: Context::default(),
        },
        Some(&coverage_pack()),
    );
    assert_eq!(
        result.outcome,
        AssessOutcome::Advisory,
        "{:?}",
        result.findings
    );
    finding(&result, "RESULT_KEY_COVERAGE", Severity::Warning);
}

#[test]
fn stale_only_and_nonobservational_siblings_are_not_alternatives() {
    let mut pack = coverage_pack();
    pack.data.provenance.push(json!({
        "id":"prov:stale",
        "source_type":"ngsiem-api",
        "canonical_source":"synthetic old measurement",
        "retrieved_at":"2000-01-01T00:00:00Z",
        "redaction_status":"none",
        "coverage_status":"stale"
    }));
    pack.data.provenance.push(json!({
        "id":"prov:manual",
        "source_type":"manual-curation",
        "canonical_source":"synthetic declaration",
        "retrieved_at":"2026-01-01T00:00:00Z",
        "redaction_status":"none",
        "coverage_status":"complete"
    }));
    pack.data.fields[1]["provenance_ids"] = json!(["prov:stale"]);
    pack.data.fields[5]["provenance_ids"] = json!(["prov:manual"]);
    pack.content_digest = pack.digest().unwrap();
    pack.validate().unwrap();
    let result = assess::assess(
        AssessRequest {
            schema_version: 1,
            id: None,
            query: "#repo=example | groupBy([a.b.user])".into(),
            result: json!({"result":[{"a.b.user":"a","_count":1}]}),
            context: Context::default(),
        },
        Some(&pack),
    );
    assert_eq!(result.outcome, AssessOutcome::Advisory);
    finding(&result, "RESULT_KEY_COVERAGE", Severity::Warning);
    let message = &result
        .findings
        .iter()
        .find(|f| f.code == "RESULT_KEY_COVERAGE")
        .unwrap()
        .message;
    assert!(!message.contains("a.b.actor"), "{message}");
    assert!(!message.contains("a.b.sibling1"), "{message}");
    assert!(message.contains("a.b.login"), "{message}");
}

#[test]
fn envelope_errors_timeouts_and_incomplete_flags_are_inconclusive() {
    for (payload, code) in [
        (
            json!({"isError":true,"content":[{"type":"text","text":"search failed private-row-token"}]}),
            "RESULT_ERROR",
        ),
        (
            json!({"isError":true,"content":[{"type":"text","text":"deadline exceeded"}]}),
            "RESULT_TIMEOUT",
        ),
        (json!({"result":[],"truncated":true}), "RESULT_INCOMPLETE"),
        (json!({"result":[],"unknown":true}), "RESULT_SHAPE"),
    ] {
        let result = assess("count()", payload);
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{:?}",
            result.findings
        );
        finding(&result, code, Severity::Warning);
        finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
        assert!(!result.findings.iter().any(|f| f.code == "RESULT_EMPTY"));
        assert!(
            !serde_json::to_string(&result)
                .unwrap()
                .contains("private-row-token")
        );
    }
}

#[test]
fn floating_completeness_flags_cannot_underflow_into_no_findings() {
    for number in ["1e-10000", "-1e-10000", "0.0", "-0.0", "0e0"] {
        let raw = format!(r#"{{"result":[{{"_count":"1"}}],"partial":{number}}}"#);
        let result = assess("count()", serde_json::from_str(&raw).unwrap());
        assert_eq!(result.outcome, AssessOutcome::Inconclusive, "{number}");
        finding(&result, "RESULT_INCOMPLETE", Severity::Warning);
    }
}

#[test]
fn successful_mcp_text_envelope_is_normalized() {
    let body = json!({"result":[{"_count":"24"}]}).to_string();
    let result = assess(
        "count()",
        json!({"isError":false,"content":[{"type":"text","text":body}]}),
    );
    assert_eq!(result.outcome, AssessOutcome::NoFindings);
    assert_eq!(result.rows, 1);
    finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
}

#[test]
fn overlarge_saved_result_is_a_shape_finding() {
    let result = assess(
        "count()",
        json!({"result":[],"error":"x".repeat(8 * 1024 * 1024)}),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    finding(&result, "RESULT_SHAPE", Severity::Warning);
}

#[test]
fn query_parse_failure_is_an_error_and_request_is_strict() {
    let result = assess("count() | ?", json!({"result":[]}));
    assert_eq!(result.outcome, AssessOutcome::Error);
    finding(&result, "RESULT_QUERY_PARSE", Severity::Error);
    finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
    assert!(
        serde_json::from_value::<AssessRequest>(
            json!({"schema_version":1,"query":"count()","result":{"result":[]},"extra":true})
        )
        .is_err()
    );
    let result = assess::assess(
        AssessRequest {
            schema_version: 2,
            id: None,
            query: "count()".into(),
            result: json!({"result":[]}),
            context: Context::default(),
        },
        None,
    );
    assert_eq!(result.outcome, AssessOutcome::Error);
    finding(&result, "REQUEST_VERSION", Severity::Error);
}

#[test]
fn assessment_core_does_not_access_io_environment_or_clock() {
    let source = include_str!("../src/assess.rs");
    for forbidden in ["std::fs", "std::net", "std::env", "std::time::SystemTime"] {
        assert!(!source.contains(forbidden), "{forbidden}");
    }
}

#[test]
fn default_top_with_rest_row_still_reports_its_ten_group_cap() {
    let result = assess_accepted(
        r#"top(user, rest="true")"#,
        json!({"result":(0..10)
            .map(|n| json!({"user":format!("u{n}"),"_count":1}))
            .chain([json!({"user":"others","_count":5})])
            .collect::<Vec<_>>()}),
    );
    assert_eq!(result.rows, 11);
    assert_eq!(result.caps[0].cap, 10);
    assert_eq!(
        result.outcome,
        AssessOutcome::Inconclusive,
        "{:?}",
        result.findings
    );
    finding(&result, "RESULT_CAP_REACHED", Severity::Warning);
    let explicit = assess_accepted(
        r#"top(user, limit=1, rest="true")"#,
        json!({"result":[{"user":"a","_count":1},{"user":"others","_count":1}]}),
    );
    assert_eq!(
        explicit.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        explicit.findings
    );
    finding(&explicit, "RESULT_CAP_REACHED", Severity::Info);
}

#[test]
fn capped_timechart_series_before_count_cannot_be_no_findings() {
    let result = assess_accepted(
        "timeChart(series=user, span=1h, limit=1) | count(as=series_rows)",
        json!({"result":[{"series_rows":24}]}),
    );
    assert_eq!(result.caps[0].stage, "timeChart");
    assert_eq!(result.caps[0].cap, 1);
    assert_eq!(
        result.outcome,
        AssessOutcome::Inconclusive,
        "{:?}",
        result.findings
    );
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
}

#[test]
fn capped_bucket_series_before_count_cannot_be_no_findings() {
    let result = assess_accepted(
        "bucket(field=[user], span=1h, limit=1) | count(as=series_rows)",
        json!({"result":[{"series_rows":24}]}),
    );
    assert_eq!(result.caps[0].stage, "bucket");
    assert_eq!(result.caps[0].cap, 1);
    assert_eq!(
        result.outcome,
        AssessOutcome::Inconclusive,
        "{:?}",
        result.findings
    );
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
}

#[test]
fn default_and_final_series_caps_are_unverified_even_below_row_count() {
    for query in [
        "timeChart(series=user, span=1h)",
        "bucket(field=[user], span=1h)",
    ] {
        let result = assess_accepted(query, json!({"result":[{"_bucket":1,"_count":2}]}));
        assert_eq!(result.caps[0].cap, 10, "{query}");
        assert!(result.caps[0].defaulted, "{query}");
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    }
}

#[test]
fn default_collect_value_cap_cannot_pass_as_no_findings() {
    let result = assess_accepted(
        "groupBy([user], limit=max, function=collect([ip]))",
        json!({"result":[{"user":"example_user","ip":"sample value"}]}),
    );
    assert_eq!(
        result.outcome,
        AssessOutcome::Inconclusive,
        "{:?}",
        result.findings
    );
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    assert!(
        result
            .caps
            .iter()
            .any(|cap| cap.stage == "collect" && cap.cap == 2_000 && cap.defaulted)
    );
    assert!(
        result
            .findings
            .iter()
            .any(|f| f.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                && f.message.contains("collect() per-group cap of 2000")
                && f.message.contains("observed values below"))
    );
}

#[test]
fn collected_values_at_default_cap_are_detected_per_group() {
    for collected in [
        json!((0..2_000).map(|i| format!("ip_{i}")).collect::<Vec<_>>()),
        json!(
            (0..2_000)
                .map(|i| format!("ip_{i}"))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    ] {
        let result = assess_accepted(
            "groupBy([user], limit=max, function=collect([ip]))",
            json!({"result":[{"user":"a","ip":"one"},{"user":"b","ip":collected}]}),
        );
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{:?}",
            result.findings
        );
        finding(&result, "RESULT_CAP_REACHED", Severity::Warning);
        assert!(result.findings.iter().any(|f| {
            f.code == "RESULT_CAP_REACHED"
                && f.message
                    .contains("default collect() per-group cap of 2000")
        }));
    }
}

#[test]
fn requested_collect_size_reports_info_at_cap_not_below() {
    for value in [json!(["one", "two"]), json!("one\ntwo")] {
        let result = assess_accepted("collect([ip], limit=2)", json!({"result":[{"ip":value}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{:?}",
            result.findings
        );
        finding(&result, "RESULT_CAP_REACHED", Severity::Info);
        assert!(
            result
                .findings
                .iter()
                .any(|f| f.code == "RESULT_CAP_REACHED"
                    && f.message.contains("requested collect() per-group cap of 2"))
        );
    }
    let below = assess_accepted("collect([ip], limit=2)", json!({"result":[{"ip":"one"}]}));
    assert_eq!(below.outcome, AssessOutcome::NoFindings);
    assert!(
        !below
            .findings
            .iter()
            .any(|f| f.code == "RESULT_CAP_REACHED")
    );
}

#[test]
fn noncountable_or_nested_collect_values_are_unverifiable() {
    let other_type = assess_accepted("collect([ip], limit=2)", json!({"result":[{"ip":7}]}));
    assert_eq!(other_type.outcome, AssessOutcome::Inconclusive);
    finding(
        &other_type,
        "RESULT_UPSTREAM_CAP_UNVERIFIED",
        Severity::Warning,
    );
    assert!(
        other_type
            .findings
            .iter()
            .any(|f| f.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                && f.message.contains("collect() per-group cap of 2")
                && f.message.contains("non-array/non-string"))
    );

    let nested = assess_accepted(
        "defineTable(name=t, query={ #repo=fw_repo | collect([ip], limit=2) }, include=[ip]) | readFile(t)",
        json!({"result":[{"ip":["one","two"]}]}),
    );
    assert_eq!(nested.outcome, AssessOutcome::Inconclusive);
    assert!(
        nested
            .findings
            .iter()
            .any(|f| f.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                && f.severity == Severity::Warning
                && f.message.contains("collect() per-group value cap of 2")
                && f.message.contains("cannot be counted in the outer result"))
    );
}

#[test]
fn collect_numeric_limit_is_counted_even_below_cap() {
    let result = assess_accepted("collect([user], limit=2)", json!({"result":[{"user":"a"}]}));
    assert_eq!(
        result.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        result.findings
    );
    assert_eq!(result.caps.len(), 1);
    assert_eq!(result.caps[0].stage, "collect");
    assert_eq!(result.caps[0].cap, 2);
    assert!(!result.caps[0].defaulted);
    assert!(
        !result
            .findings
            .iter()
            .any(|f| f.code == "RESULT_UPSTREAM_CAP_UNVERIFIED")
    );
}

#[test]
fn scalar_and_mandatory_capture_outputs_are_checked_when_missing() {
    for (query, field) in [
        ("lower(@rawstring, as=normalized)", "normalized"),
        (r#"regex("(?<actor>[a-z]+)")"#, "actor"),
        (r#"@rawstring=/(?<actor>[a-z]+)/"#, "actor"),
    ] {
        let result = assess_accepted(query, json!({"result":[{"@rawstring":"synthetic"}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Advisory,
            "{query}: {:?}",
            result.findings
        );
        assert_eq!(result.columns[field].present, 0, "{query}");
        finding(&result, "RESULT_NULL_FIELDS", Severity::Warning);
    }
}

#[test]
fn optional_captures_and_conditional_enrichments_do_not_require_columns() {
    for query in [
        r#"regex("(?<actor>[a-z]+)?")"#,
        r#"regex("(?<actor>[a-z]+)", strict=false)"#,
        r#"regex("(?<actor>[a-z]+)", repeat=true)"#,
        "asn(ip)",
        "ipLocation(ip)",
        "head(1) | neighbor([user])",
    ] {
        let result = assess_accepted(query, json!({"result":[{"@rawstring":"synthetic"}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
    }
    let partial = assess_accepted(
        r#"regex("(?<actor>[a-z]+)?")"#,
        json!({"result":[{"@rawstring":"synthetic","actor":"a"},{"@rawstring":"synthetic"}]}),
    );
    assert_eq!(partial.outcome, AssessOutcome::NoFindings);
    assert_eq!(partial.columns["actor"].present, 1);
}

#[test]
fn outputs_are_required_only_across_all_surviving_branches() {
    for query in [
        r#"case { kind=a | label := "a"; * | label := "b" }"#,
        r#"kind match { a => label := "a"; * => label := "b" }"#,
        r#"case { kind=a | label := "a"; * | false }"#,
    ] {
        let result = assess_accepted(query, json!({"result":[{"kind":"a"}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Advisory,
            "{query}: {:?}",
            result.findings
        );
        assert_eq!(result.columns["label"].present, 0);
    }
    for query in [
        r#"case { kind=a | label := "a"; * }"#,
        r#"kind match { a => label := "a"; * => * }"#,
    ] {
        let result = assess_accepted(
            query,
            json!({"result":[{"kind":"a","label":"a"},{"kind":"b"}]}),
        );
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        assert_eq!(result.columns["label"].present, 1);
    }
}

#[test]
fn all_named_writes_invalidate_previous_count_lineage() {
    for query in [
        "count(as=n) | eval(n=0)",
        "count(as=n) | default(field=n, value=0)",
        "count(as=n) | lower(n, as=n)",
        "count(as=n) | findTimestamp(as=n)",
        "count(as=n) | case { n=1 | n := 0; * }",
        "count(as=n) | n match { 1 => n := 0; * => * }",
    ] {
        let result = assess(query, json!({"result":[{"n":0}]}));
        assert!(
            !result
                .findings
                .iter()
                .any(|f| f.code == "RESULT_ZERO_COUNT"),
            "{query}: {:?}",
            result.findings
        );
    }
    let named = assess(
        "count(as=ip.asn) | asn(ip)",
        json!({"result":[{"ip.asn":0}]}),
    );
    assert!(!named.findings.iter().any(|f| f.code == "RESULT_ZERO_COUNT"));
}

#[test]
fn source_replacement_discards_obsolete_outputs_counts_and_caps() {
    for query in [
        r#"count(as=n) | createEvents(["a"])"#,
        r#"head() | createEvents(["a"])"#,
        r#"defineTable(name=t, query={ groupBy([user], limit=1) }, include=[user]) | createEvents(["a"])"#,
    ] {
        let result = assess_accepted(query, json!({"result":[{"@rawstring":"a","n":0}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        assert!(result.caps.is_empty(), "{query}");
        assert!(!result.columns.contains_key("n"), "{query}");
    }
    let retained = assess_accepted(
        "defineTable(name=t, query={ groupBy([user], limit=1) }, include=[user]) | head() | readFile(t)",
        json!({"result":[{"user":"a"}]}),
    );
    assert_eq!(retained.caps.len(), 1);
    assert_eq!(retained.caps[0].stage, "groupBy");
    finding(
        &retained,
        "RESULT_UPSTREAM_CAP_UNVERIFIED",
        Severity::Warning,
    );
}

#[test]
fn only_referenced_tables_contribute_caps() {
    let result = assess_accepted(
        "defineTable(name=a, query={ groupBy([user], limit=1) }, include=[user]) | defineTable(name=b, query={ groupBy([user], limit=2) }, include=[user]) | readFile(a)",
        json!({"result":[{"user":"a"}]}),
    );
    assert_eq!(result.caps.len(), 1);
    assert_eq!(result.caps[0].cap, 1);
}

#[test]
fn source_replacement_preserves_only_used_source_coverage() {
    for (query, covered) in [
        (r#"#repo=example a.b.user=* | createEvents(["a"])"#, false),
        (
            r#"defineTable(name=t, query={ #repo=example a.b.user=* }, include=[a.b.user]) | createEvents(["a"])"#,
            false,
        ),
        (
            "defineTable(name=t, query={ #repo=example a.b.user=* }, include=[a.b.user]) | head() | readFile(t)",
            true,
        ),
    ] {
        let result = assess::assess(
            AssessRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                result: json!({"result":[{"@rawstring":"a","a.b.user":"a"}]}),
                context: Context::default(),
            },
            Some(&coverage_pack()),
        );
        assert_eq!(
            result
                .findings
                .iter()
                .any(|f| f.code == "RESULT_KEY_COVERAGE"),
            covered,
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn dynamic_writes_invalidate_only_counts_they_can_overwrite() {
    for (query, count_survives) in [
        ("count(as=n) | parseJson(field=payload)", false),
        (
            "count(as=n) | parseJson(field=payload, include=[other])",
            false,
        ),
        (
            "count(as=n) | parseJson(field=payload, prefix=\"decoded.\")",
            true,
        ),
        ("count(as=n) | kvParse(field=payload)", true),
        ("count(as=n) | kvParse(field=payload, override=true)", false),
        (
            "count(as=n) | kvParse(field=payload, override=true, as=decoded)",
            true,
        ),
    ] {
        let result = assess(query, json!({"result":[{"n":0}]}));
        assert_eq!(
            result
                .findings
                .iter()
                .any(|f| f.code == "RESULT_ZERO_COUNT"),
            count_survives,
            "{query}: {:?}",
            result.findings
        );
        finding(&result, "RESULT_OUTPUT_COVERAGE", Severity::Info);
        assert!(
            !result
                .findings
                .iter()
                .any(|f| f.code == "RESULT_QUERY_COVERAGE"),
            "{query}: {:?}",
            result.findings
        );
    }
    let dynamic = assess_accepted("parseJson()", json!({"result":[{"a":1},{"b":2}]}));
    assert_eq!(
        dynamic.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        dynamic.findings
    );
    finding(&dynamic, "RESULT_OUTPUT_COVERAGE", Severity::Info);
}

#[test]
fn unsupported_functions_and_unresolved_parameters_are_scoped_inconclusive() {
    for query in [
        "futureResult()",
        "lower(@rawstring, as=?output)",
        "user=?name | count()",
        "head(?size)",
        "head(\"invalid\")",
        "futureResult() | createEvents([\"a\"])",
        "defineTable(name=?table, query={ count() }, include=[_count]) | readFile(t)",
        "defineTable(name=t, query={ lower(@rawstring, as=?output) }, include=[user]) | createEvents([\"a\"])",
    ] {
        let result = assess(query, json!({"result":[{"value":"synthetic"}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        finding(&result, "RESULT_QUERY_COVERAGE", Severity::Warning);
        finding(&result, "RESULT_COMPLETENESS_UNATTESTED", Severity::Info);
    }
}

#[test]
fn composite_aggregate_pipelines_keep_final_count_lineage() {
    let overwritten = assess_accepted(
        "groupBy([user], function={ count(as=n) | eval(n=0) })",
        json!({"result":[{"user":"a","n":0}]}),
    );
    assert!(
        !overwritten
            .findings
            .iter()
            .any(|f| f.code == "RESULT_ZERO_COUNT")
    );
    let renamed = assess_accepted(
        "stats(function={ count(as=n) | rename(n, as=events) })",
        json!({"result":[{"events":0}]}),
    );
    finding(&renamed, "RESULT_ZERO_COUNT", Severity::Warning);
    for (query, required) in [
        (
            r#"groupBy([user], function={ count() | case { _count>1 | label := "many"; * } })"#,
            false,
        ),
        (
            r#"groupBy([user], function={ count() | case { _count>1 | label := "many"; * | label := "one" } })"#,
            true,
        ),
    ] {
        let result = assess_accepted(query, json!({"result":[{"user":"a","_count":1}]}));
        assert_eq!(
            result
                .findings
                .iter()
                .any(|f| f.code == "RESULT_NULL_FIELDS"),
            required,
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn assigned_aggregates_use_the_assignment_name_for_outputs_and_counts() {
    let result = assess_accepted("events := count()", json!({"result":[{"events":0}]}));
    finding(&result, "RESULT_ZERO_COUNT", Severity::Warning);
    assert!(!result.columns.contains_key("_count"));
    let overwritten = assess_accepted(
        "events := count() | eval(events=0)",
        json!({"result":[{"events":0}]}),
    );
    assert!(
        !overwritten
            .findings
            .iter()
            .any(|f| f.code == "RESULT_ZERO_COUNT")
    );
    let replaced = assess_accepted(
        "count(as=old) | total := sum(old)",
        json!({"result":[{"old":0,"total":1}]}),
    );
    assert_eq!(replaced.outcome, AssessOutcome::NoFindings);
    assert!(!replaced.columns.contains_key("old"));
    let shorthand = assess_accepted("stats(count)", json!({"result":[{"_count":0}]}));
    finding(&shorthand, "RESULT_ZERO_COUNT", Severity::Warning);
}

#[test]
fn unsupported_top_rest_values_keep_query_coverage_inconclusive() {
    let result = assess(
        r#"top(user, limit=1, rest="others")"#,
        json!({"result":[{"user":"a","_count":1},{"user":"others","_count":1}]}),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    finding(&result, "RESULT_QUERY_COVERAGE", Severity::Warning);
}

#[test]
fn referenced_remote_tables_keep_the_size_cap_and_declared_columns() {
    let result = assess_accepted(
        r#"remoteTable(connection="synthetic", name="t", query={ count(as=n) }, include=[n]) | head() | readFile(t)"#,
        json!({"result":[{"n":0}]}),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    assert!(result.caps.is_empty());
    assert_eq!(result.columns["n"].present, 1);
    finding(&result, "RESULT_ZERO_COUNT", Severity::Warning);
    assert!(
        result
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                && finding.message.contains("200 MB"))
    );
    assert!(
        !result
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_OUTPUT_COVERAGE")
    );

    let missing = assess_accepted(
        r#"remoteTable(connection="synthetic", name="t", query="select user from events", include=[user]) | readFile(t)"#,
        json!({"result":[{"other":1}]}),
    );
    assert_eq!(missing.columns["user"].present, 0);
    finding(&missing, "RESULT_NULL_FIELDS", Severity::Warning);
    assert!(
        !missing
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_ZERO_COUNT")
    );
}

#[test]
fn opaque_remote_query_outputs_are_informational_but_size_completeness_is_unverified() {
    let result = assess_accepted(
        r#"remoteTable(connection="synthetic", name="t", query="select count(*) as n from events") | readFile(t)"#,
        json!({"result":[{"n":0},{"other":1}]}),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    finding(&result, "RESULT_OUTPUT_COVERAGE", Severity::Info);
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    assert!(!result.findings.iter().any(|finding| matches!(
        finding.code.as_str(),
        "RESULT_ZERO_COUNT" | "RESULT_NULL_FIELDS" | "RESULT_QUERY_COVERAGE"
    )));
}

#[test]
fn unused_or_replaced_remote_tables_do_not_contribute_caps_or_columns() {
    for query in [
        r#"remoteTable(connection="synthetic", name="t", query="select user from events") | createEvents(["a"])"#,
        r#"remoteTable(connection="synthetic", name="t", query={ groupBy([user], limit=1) }, include=[user]) | readFile(t) | createEvents(["a"])"#,
    ] {
        let result = assess_accepted(query, json!({"result":[{"@rawstring":"a"}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        assert!(result.caps.is_empty());
        assert!(!result.columns.contains_key("user"));
        assert!(!result.findings.iter().any(|finding| matches!(
            finding.code.as_str(),
            "RESULT_OUTPUT_COVERAGE" | "RESULT_UPSTREAM_CAP_UNVERIFIED"
        )));
    }
}

#[test]
fn remote_query_sources_do_not_borrow_local_pack_coverage_or_named_tables() {
    let result = assess::assess(
        AssessRequest {
            schema_version: 1,
            id: None,
            query: r#"remoteTable(connection="synthetic", name="t", query={ #repo=example | groupBy([a.b.user], limit=1) }, include=[a.b.user]) | readFile(t)"#.into(),
            result: json!({"result":[{"a.b.user":"a"}]}),
            context: Context::default(),
        },
        Some(&coverage_pack()),
    );
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    assert_eq!(result.caps.len(), 1);
    assert_eq!(result.caps[0].stage, "groupBy");
    assert!(
        !result
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_KEY_COVERAGE")
    );

    let names = assess_accepted(
        r#"defineTable(name=local, query={ count(as=n) }, include=[n]) | remoteTable(connection="synthetic", name="t", query={ readFile(local) }, include=[n]) | readFile(t)"#,
        json!({"result":[{"n":0}]}),
    );
    assert!(
        !names
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_ZERO_COUNT")
    );
}

#[test]
fn remote_table_lookup_dependencies_survive_only_when_used() {
    for query in [
        r#"remoteTable(connection="synthetic", name="t", query="select user,note from events", include=[user,note]) | match(t, field=user, include=[note])"#,
        r#"remoteTable(connection="synthetic", name="t", query="select user,note from events", include=[user,note]) | matchAsArray(t, field=user, include=[note], nrows=1, asArray="matches[]")"#,
    ] {
        let result = assess_accepted(query, json!({"result":[{"user":"a"}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                    && finding.message.contains("200 MB"))
        );
    }
    let unknown_columns = assess_accepted(
        r#"remoteTable(connection="synthetic", name="t", query="select user,n from events") | count(as=n) | match(t, field=n)"#,
        json!({"result":[{"n":0}]}),
    );
    assert!(
        !unknown_columns
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_ZERO_COUNT")
    );
}

#[test]
fn logical_array_aggregate_outputs_use_observed_indexed_members() {
    for query in [
        r#"array:union("values[]", as=out)"#,
        r#"array:intersection("values[]", as=out)"#,
        r#"array:reduceColumn("values[]", var=item, function=sum(item), as=out)"#,
    ] {
        for payload in [json!({"result":[{"out[0]":"a"}]}), json!({"result":[{}]})] {
            let result = assess_accepted(query, payload);
            assert_eq!(
                result.outcome,
                AssessOutcome::NoFindings,
                "{query}: {:?}",
                result.findings
            );
            assert!(!result.columns.contains_key("out[]"));
            assert!(
                !result
                    .findings
                    .iter()
                    .any(|finding| finding.code == "RESULT_NULL_FIELDS")
            );
        }
    }
    let grouped = assess_accepted(
        r#"groupBy([user], function=array:union("values[]", as=out), limit=max)"#,
        json!({"result":[{"user":"a","out[0]":"x"},{"user":"b"}]}),
    );
    // The only adverse finding is the unknown GroupMaxLimit, not a missing indexed member.
    assert_eq!(grouped.outcome, AssessOutcome::Inconclusive);
    assert!(
        grouped
            .findings
            .iter()
            .filter(|finding| finding.severity != Severity::Info)
            .all(|finding| finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                && finding.message.contains("GroupMaxLimit")),
        "{:?}",
        grouped.findings
    );
    assert_eq!(grouped.columns["out[0]"].present, 1);
    assert!(!grouped.columns.contains_key("out[]"));

    let explicit = assess_accepted(
        r#"array:union("values[]", as=out) | select(["out[0]"])"#,
        json!({"result":[{}]}),
    );
    assert_eq!(explicit.outcome, AssessOutcome::Advisory);
    finding(&explicit, "RESULT_NULL_FIELDS", Severity::Warning);
}

#[test]
fn appended_aggregate_rows_have_conditional_detail_and_summary_columns() {
    for query in [
        "head(10) | appendAggregation({ count(as=total) })",
        "select([user, value]) | head(10) | appendAggregation({ sum(value, as=total) })",
    ] {
        let result = assess_accepted(
            query,
            json!({"result":[{"user":"a","value":1},{"total":1}]}),
        );
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        assert_eq!(result.columns["user"].present, 1);
        assert_eq!(result.columns["total"].present, 1);
        assert!(
            !result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_NULL_FIELDS")
        );
    }
    let common = assess_accepted(
        r#"select([user]) | head(10) | appendAggregation({ count(as=total) | user := "all" })"#,
        json!({"result":[{"user":"a"},{"total":1}]}),
    );
    assert_eq!(common.outcome, AssessOutcome::Advisory);
    finding(&common, "RESULT_NULL_FIELDS", Severity::Warning);
    assert_eq!(common.columns["user"].present, 1);

    let replaced_count = assess_accepted(
        "count(as=n) | appendAggregation({ sum(n, as=n) })",
        json!({"result":[{"n":2},{"n":0}]}),
    );
    assert_eq!(replaced_count.outcome, AssessOutcome::NoFindings);
    assert!(
        !replaced_count
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_ZERO_COUNT")
    );
    let common_count = assess_accepted(
        "count(as=n) | appendAggregation({ count(as=n) })",
        json!({"result":[{"n":0},{"n":1}]}),
    );
    finding(&common_count, "RESULT_ZERO_COUNT", Severity::Warning);
    let capped = assess_accepted(
        "head(10) | appendAggregation({ groupBy([user], limit=1) | count(as=total) })",
        json!({"result":[{"user":"a"},{"total":1}]}),
    );
    finding(&capped, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
}

#[test]
fn count_lineage_survives_sequence_window_and_element_aggregate_wrappers() {
    for query in [
        "head(2) | accumulate(count(as=n), current=exclude)",
        "head(2) | slidingWindow(count(as=n), events=2)",
        "head(2) | slidingTimeWindow(count(as=n), span=1m)",
        "head(2) | partition(count(as=n), condition=test(value > 0))",
        "timeChart(function=window(count(as=n), buckets=2), span=1m)",
        r#"array:reduceAll("values[]", function=count(as=n), var=item)"#,
    ] {
        let result = assess_accepted(query, json!({"result":[{"n":0,"_bucket":1}]}));
        finding(&result, "RESULT_ZERO_COUNT", Severity::Warning);
        let overwritten = assess_accepted(
            &format!("{query} | eval(n=0)"),
            json!({"result":[{"n":0,"_bucket":1}]}),
        );
        assert!(
            !overwritten
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_ZERO_COUNT"),
            "{query}: {:?}",
            overwritten.findings
        );
    }
    let nested_overwrite = assess_accepted(
        "head(2) | accumulate({ count(as=n) | eval(n=0) }, current=exclude)",
        json!({"result":[{"n":0}]}),
    );
    assert!(
        !nested_overwrite
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_ZERO_COUNT")
    );
    let previous_count = assess_accepted(
        "count(as=n) | accumulate(sum(n, as=n))",
        json!({"result":[{"n":0}]}),
    );
    assert!(
        !previous_count
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_ZERO_COUNT")
    );
}

#[test]
fn dynamic_writes_invalidate_matching_collect_cap_observability() {
    for query in [
        "collect([payload], limit=2) | parseJson(field=payload)",
        "collect([payload], limit=2) | kvParse(field=payload, override=true)",
    ] {
        let result = assess_accepted(query, json!({"result":[{"payload":["a","b"]}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
        assert!(
            !result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_CAP_REACHED")
        );
    }
    for query in [
        r#"collect([payload], limit=2) | parseJson(field=payload, prefix="decoded.")"#,
        "collect([payload], limit=2) | kvParse(field=payload)",
    ] {
        let result = assess_accepted(query, json!({"result":[{"payload":["a","b"]}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        finding(&result, "RESULT_CAP_REACHED", Severity::Info);
        assert!(
            !result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED")
        );
    }
}

#[test]
fn renames_preserve_surviving_collected_values_without_reusing_overwritten_lists() {
    for (values, reached) in [(json!(["a"]), false), (json!(["a", "b"]), true)] {
        let result = assess_accepted(
            "collect([payload], limit=2) | rename(payload, as=renamed)",
            json!({"result":[{"renamed":values}]}),
        );
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{:?}",
            result.findings
        );
        assert_eq!(result.caps[0].stage, "collect");
        assert_eq!(
            result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_CAP_REACHED"),
            reached
        );
        assert!(
            !result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED")
        );
    }
    let collision = assess_accepted(
        "collect([first, second], limit=2) | rename(first, as=second)",
        json!({"result":[{"second":["a","b"]}]}),
    );
    assert_eq!(collision.outcome, AssessOutcome::Inconclusive);
    finding(
        &collision,
        "RESULT_UPSTREAM_CAP_UNVERIFIED",
        Severity::Warning,
    );
    assert!(
        !collision
            .findings
            .iter()
            .any(|finding| finding.code == "RESULT_CAP_REACHED")
    );
}

#[test]
fn deprecated_finite_name_conversion_does_not_require_an_uncertain_old_column() {
    for mode in ["fields", "both"] {
        let query = format!(r#"N := "V" | lowercase(N,include={mode})"#);
        for rows in [json!([{"n":"v"}]), json!([{"N":"V","n":"v"}])] {
            let result = assess_accepted(&query, json!({"result":rows}));
            assert_eq!(
                result.outcome,
                AssessOutcome::NoFindings,
                "{query}: {:?}",
                result.findings
            );
            assert!(
                !result
                    .findings
                    .iter()
                    .any(|finding| finding.code == "RESULT_NULL_FIELDS")
            );
        }
    }
}

fn null_field_columns(result: &AssessResult) -> Vec<&str> {
    result
        .findings
        .iter()
        .filter(|finding| finding.code == "RESULT_NULL_FIELDS")
        .filter_map(|finding| {
            let (column, _) = finding
                .message
                .strip_prefix("Output column '")?
                .split_once("' has ")?;
            Some(column)
        })
        .collect()
}

#[test]
fn array_drop_and_rename_update_expected_element_columns_like_drop_and_rename() {
    for (query, row) in [
        (
            r#""a[0]":=1 | array:drop("a[]") | marker:=1"#,
            json!({"marker":1}),
        ),
        (
            r#""a[0]":=1 | array:rename(array="a[]", asArray="b[]") | marker:=1"#,
            json!({"b[0]":1,"marker":1}),
        ),
        (
            r#""a[0]":=1 | "a[1]":=2 | "a[2]":=3 | array:drop("a[]") | marker:=1"#,
            json!({"marker":1}),
        ),
        (
            r#""a[0]":=1 | "a[1]":=2 | "a[2]":=3 | array:rename("a[]", asArray="b[]") | marker:=1"#,
            json!({"b[0]":1,"b[1]":2,"b[2]":3,"marker":1}),
        ),
        // Unchanged controls: plain `drop` and `rename`.
        ("x:=1 | drop([x]) | marker:=1", json!({"marker":1})),
        (
            r#""a[0]":=1 | drop("a[0]") | marker:=1"#,
            json!({"marker":1}),
        ),
        (
            "x:=1 | rename(x, as=y) | marker:=1",
            json!({"y":1,"marker":1}),
        ),
    ] {
        let result = assess_accepted(query, json!({"result":[row]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            !result.columns.keys().any(|column| column.starts_with("a[")),
            "{query}: {:?}",
            result.columns
        );
    }
}

#[test]
fn array_rename_expects_moved_elements_under_the_new_name() {
    // A result that still holds the old name, or lacks a moved element, does not match the query.
    for (query, row, missing) in [
        (
            r#""a[0]":=1 | array:rename(array="a[]", asArray="b[]") | marker:=1"#,
            json!({"a[0]":1,"marker":1}),
            vec!["b[0]"],
        ),
        (
            r#""a[0]":=1 | "a[1]":=2 | "a[2]":=3 | array:rename("a[]", asArray="b[]") | marker:=1"#,
            json!({"b[0]":1,"b[1]":2,"marker":1}),
            vec!["b[2]"],
        ),
        // Control: the plain rename has the same expectation.
        (
            "x:=1 | rename(x, as=y) | marker:=1",
            json!({"x":1,"marker":1}),
            vec!["y"],
        ),
    ] {
        let result = assess_accepted(query, json!({"result":[row]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Advisory,
            "{query}: {:?}",
            result.findings
        );
        assert_eq!(null_field_columns(&result), missing, "{query}");
    }
    // A moved count keeps its count meaning.
    let query = r#"count(as=n) | rename(n, as="a[0]") | array:rename("a[]", asArray="b[]")"#;
    let zero = assess_accepted(query, json!({"result":[{"b[0]":0}]}));
    assert_eq!(zero.outcome, AssessOutcome::Advisory, "{:?}", zero.findings);
    finding(&zero, "RESULT_ZERO_COUNT", Severity::Warning);
    assert!(null_field_columns(&zero).is_empty(), "{:?}", zero.findings);
    let nonzero = assess_accepted(query, json!({"result":[{"b[0]":3}]}));
    assert_eq!(
        nonzero.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        nonzero.findings
    );
}

#[test]
fn dropped_array_elements_left_in_the_result_are_not_inspected_like_plain_drop() {
    // Assessment checks expected columns for missing values; it does not flag extra columns.
    // This result was no-findings before `array:drop` was followed too, with `a[0]` expected.
    for (query, row) in [
        (
            r#""a[0]":=1 | array:drop("a[]") | marker:=1"#,
            json!({"a[0]":1,"marker":1}),
        ),
        ("x:=1 | drop([x]) | marker:=1", json!({"x":1,"marker":1})),
    ] {
        let result = assess_accepted(query, json!({"result":[row]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        assert_eq!(
            result.columns.keys().collect::<Vec<_>>(),
            ["marker"],
            "{query}"
        );
    }
}

#[test]
fn array_drop_and_rename_keep_elements_they_may_not_remove() {
    for (query, row, missing) in [
        // An element after a gap is removed only if the event holds the missing index.
        (
            r#""a[0]":=1 | "a[2]":=3 | array:drop("a[]") | marker:=1"#,
            json!({"marker":1}),
            vec!["a[2]"],
        ),
        // Another array, or a root that only shares a prefix, is untouched.
        (
            r#""a[0]":=1 | "b[0]":=2 | array:drop("b[]") | marker:=1"#,
            json!({"marker":1}),
            vec!["a[0]"],
        ),
        (
            r#""a[0]":=1 | "b[0]":=2 | array:drop("b[]") | marker:=1"#,
            json!({"a[0]":1,"marker":1}),
            vec![],
        ),
        (
            r#""ab[0]":=1 | array:drop("a[]") | marker:=1"#,
            json!({"marker":1}),
            vec!["ab[0]"],
        ),
        // Structured members, nested arrays and an argument without `[]` are not followed.
        (
            r#""a[0].x":=1 | array:drop("a[]") | marker:=1"#,
            json!({"marker":1}),
            vec!["a[0].x"],
        ),
        (
            r#""a[0][0]":=1 | array:drop("a[0][]") | marker:=1"#,
            json!({"marker":1}),
            vec!["a[0][0]"],
        ),
        (
            r#""a[0]":=1 | array:rename("a[]", asArray="b") | marker:=1"#,
            json!({"b[0]":1,"marker":1}),
            vec!["a[0]"],
        ),
        // The destination's elements beyond the moved ones may or may not survive.
        (
            r#""a[0]":=1 | "b[0]":=5 | "b[1]":=6 | array:rename("a[]", asArray="b[]") | marker:=1"#,
            json!({"b[0]":1,"marker":1}),
            vec!["b[1]"],
        ),
    ] {
        let result = assess_accepted(query, json!({"result":[row]}));
        assert_eq!(null_field_columns(&result), missing, "{query}");
        assert_eq!(
            result.outcome,
            if missing.is_empty() {
                AssessOutcome::NoFindings
            } else {
                AssessOutcome::Advisory
            },
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn reducing_after_uncertain_composite_shapes_restores_count_checks() {
    for query in [
        "stats({parseJson()|count(as=n)})|select(n)",
        "stats({head(1)|drop(user)|count(as=n)})|select(n)",
    ] {
        let result = assess_accepted(query, json!({"result":[{"n":0}]}));
        finding(&result, "RESULT_ZERO_COUNT", Severity::Warning);
        assert!(
            result
                .findings
                .iter()
                .all(|finding| finding.code != "RESULT_QUERY_COVERAGE")
        );
    }
}

fn unverified_cap(result: &AssessResult, parts: &[&str]) -> bool {
    result.findings.iter().any(|finding| {
        finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
            && finding.severity == Severity::Warning
            && parts.iter().all(|part| finding.message.contains(part))
    })
}

#[test]
fn group_max_limit_is_an_unknown_target_ceiling_in_every_position() {
    for (query, payload) in [
        (
            "groupBy([x], limit=max) | sort(_count, limit=5)",
            grouped_x_rows(5),
        ),
        (
            "groupBy([x], limit=max) | sort(_count, limit=max) | head(5)",
            grouped_x_rows(5),
        ),
        ("groupBy([x], limit=max) | head(5)", grouped_x_rows(5)),
        ("groupBy([x], limit=max)", grouped_x_rows(5)),
        (
            "groupBy([x], limit=max) | count()",
            json!({"result":[{"_count":5}]}),
        ),
        (
            "defineTable(name=t, query={ #repo=fw_repo | groupBy([x], limit=max) }, include=[x]) | readFile(t)",
            json!({"result":[{"x":"a"}]}),
        ),
    ] {
        let result = assess_accepted(query, payload);
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            result.caps.iter().all(|cap| cap.stage != "groupBy"),
            "{query}: {:?}",
            result.caps
        );
        assert!(
            unverified_cap(
                &result,
                &[
                    "groupBy(limit=max)",
                    "GroupMaxLimit",
                    "effective numeric cap is unknown"
                ]
            ),
            "{query}: {:?}",
            result.findings
        );
        assert!(
            !serde_json::to_string(&result).unwrap().contains("1000000"),
            "{query}"
        );
    }
}

#[test]
fn limit_max_never_asserts_a_numeric_cap_for_any_stage() {
    for (stage, query) in [
        ("groupBy", "groupBy([user], limit=max)"),
        ("sort", "sort(user, limit=max)"),
        ("head", "head(limit=max)"),
        ("tail", "tail(limit=max)"),
        ("table", "table([user], limit=max)"),
        ("bucket", "bucket(limit=max)"),
        ("timeChart", "timeChart(limit=max)"),
        ("collect", "collect([user], limit=max)"),
        ("top", "top(user, limit=max)"),
        ("join", "join({ #repo=fw_repo }, field=user, limit=max)"),
    ] {
        let result = assess(query, rows(1));
        assert_eq!(result.outcome, AssessOutcome::Inconclusive, "{query}");
        assert!(
            result.caps.iter().all(|cap| cap.stage != stage),
            "{query}: {:?}",
            result.caps
        );
        assert!(
            unverified_cap(&result, &[stage]),
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn omitted_documented_default_limits_are_as_unverified_as_explicit_limits() {
    let self_join = "selfJoin(field=user, where=[{x=a}, {x=b}])";
    for query in [
        self_join.to_string(),
        format!("#repo=fw_repo | {self_join} | count()"),
        format!("n := {self_join}"),
    ] {
        let payload = if query.ends_with("count()") {
            json!({"result":[{"_count":5}]})
        } else {
            json!({"result":[{"user":"a","n":"a"}]})
        };
        let omitted = assess(&query, payload.clone());
        assert_eq!(
            omitted.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            omitted.findings
        );
        assert!(
            unverified_cap(&omitted, &["default selfJoin limit"]),
            "{query}: {:?}",
            omitted.findings
        );
        let explicit = assess(&query.replace("}])", "}], limit=5)"), payload);
        assert_eq!(explicit.outcome, AssessOutcome::Inconclusive, "{query}");
        assert!(unverified_cap(&explicit, &["selfJoin limit"]), "{query}");
        assert!(!unverified_cap(&explicit, &["default"]), "{query}");
    }
    for (stage, query) in [
        ("fieldstats", "fieldstats()"),
        ("transpose", "transpose(pivot=x)"),
    ] {
        let result = assess(query, rows(1));
        assert_eq!(result.outcome, AssessOutcome::Inconclusive, "{query}");
        assert!(
            unverified_cap(&result, &[&format!("default {stage} limit")]),
            "{query}: {:?}",
            result.findings
        );
    }
    // Transformations and stages without a documented default limit are unchanged.
    for query in ["hash(user, limit=10)", "count()"] {
        let result = assess(
            query,
            json!({"result":[{"user":"a","_hash":"1","_count":1}]}),
        );
        assert!(
            !result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"),
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn assigned_aggregates_behind_group_max_limit_keep_the_unknown_ceiling() {
    for query in [
        "groupBy([x], limit=max) | n := count()",
        "groupBy([x], limit=max) | count(as=n)",
    ] {
        let result = assess_accepted(query, json!({"result":[{"n":2}]}));
        assert_eq!(result.outcome, AssessOutcome::Inconclusive, "{query}");
        assert!(result.caps.is_empty(), "{query}: {:?}", result.caps);
        assert!(
            unverified_cap(&result, &["groupBy(limit=max)", "GroupMaxLimit"]),
            "{query}: {:?}",
            result.findings
        );
    }
    // Numeric grouping caps keep their value in both forms.
    for query in [
        "groupBy([x], limit=2) | n := count()",
        "groupBy([x], limit=2) | count(as=n)",
    ] {
        let result = assess_accepted(query, json!({"result":[{"n":2}]}));
        assert_eq!(result.outcome, AssessOutcome::Inconclusive, "{query}");
        assert_eq!(result.caps[0].stage, "groupBy");
        assert_eq!(result.caps[0].cap, 2);
        finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Warning);
    }
}

fn unique_key_rows(n: usize) -> Value {
    json!({
        "result": (0..n).map(|i| json!({format!("k{i}"): "v"})).collect::<Vec<_>>()
    })
}

fn group_cap_findings(result: &AssessResult) -> Vec<&str> {
    result
        .findings
        .iter()
        .filter(|finding| {
            matches!(
                finding.code.as_str(),
                "RESULT_CAP_REACHED" | "RESULT_CAP_EXCEEDED"
            ) && finding.message.contains("groupBy")
        })
        .map(|finding| finding.message.as_str())
        .collect()
}

#[test]
fn multi_row_groupings_count_distinct_key_tuples_against_the_group_limit() {
    let one_group = json!({"result":[
        {"a":"one","c":"x","b":"1","_count":1},
        {"a":"one","c":"x","b":"2","_count":1},
        {"a":"one","c":"x","b":"3","_count":1}
    ]});
    for query in [
        "groupBy(a, function=tail(3), limit=2)",
        "groupBy([a, c], function=tail(3), limit=2)",
        "groupBy(a, function=top(b, limit=3), limit=2)",
        "groupBy(a, function=collect([b], multival=false, limit=5), limit=2)",
    ] {
        let result = assess_accepted(query, one_group.clone());
        assert!(
            group_cap_findings(&result).is_empty(),
            "{query}: {:?}",
            result.findings
        );
    }
    // Inline pipelines that cannot write or remove the keys are counted the same way.
    for query in [
        "groupBy(a, function=tail(3), limit=2)",
        "groupBy(a, function={tail(3)}, limit=2)",
        r#"groupBy(a, function={ c="x" | sort(b, limit=10) | tail(3) }, limit=2)"#,
    ] {
        let result = assess_accepted(query, one_group.clone());
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
    }

    // Two rows in one group do not reach a two-group limit.
    let two_rows = assess_accepted(
        "groupBy(a, function=tail(3), limit=2)",
        json!({"result":[{"a":"one","b":"1"},{"a":"one","b":"2"}]}),
    );
    assert!(group_cap_findings(&two_rows).is_empty());
    assert_eq!(two_rows.outcome, AssessOutcome::NoFindings);

    // Rows from two groups reach it, whatever the row count.
    let reached = assess_accepted(
        "groupBy(a, function=tail(3), limit=2)",
        json!({"result":[{"a":"one","b":"1"},{"a":"one","b":"2"},{"a":"two","b":"3"}]}),
    );
    assert_eq!(reached.outcome, AssessOutcome::Inconclusive);
    assert!(
        reached.findings.iter().any(|finding| {
            finding.code == "RESULT_CAP_REACHED"
                && finding.severity == Severity::Warning
                && finding
                    .message
                    .contains("2 groups reached the explicit groupBy cap of 2")
        }),
        "{:?}",
        reached.findings
    );

    // Key tuples, including a missing key, count separate groups.
    let exceeded = assess_accepted(
        "groupBy([a, c], function=tail(3), limit=2)",
        json!({"result":[
            {"a":"one","c":"x","b":"1"},
            {"a":"one","c":"y","b":"2"},
            {"a":"one","b":"3"}
        ]}),
    );
    assert_eq!(exceeded.outcome, AssessOutcome::Inconclusive);
    assert!(
        exceeded.findings.iter().any(|finding| {
            finding.code == "RESULT_CAP_EXCEEDED"
                && finding
                    .message
                    .contains("3 groups exceed the requested groupBy limit of 2")
        }),
        "{:?}",
        exceeded.findings
    );

    // A sub-aggregation that can rewrite or remove a grouping key leaves the group count unverified.
    for query in [
        r#"groupBy([a], function={ a := "x" | tail(3) }, limit=2)"#,
        "groupBy([a], function=[tail(3), count(as=a)], limit=2)",
        "groupBy([a], function={ drop(a) | tail(3) }, limit=2)",
        "groupBy([a], function={ tail(3) | rename(b, as=a) }, limit=2)",
        "groupBy([a], function={ b=/(?<a>x)/ | tail(3) }, limit=2)",
        r#"groupBy([a], function={ case { b="1" | a := "x" ; * } | tail(3) }, limit=2)"#,
    ] {
        let result = assess(
            query,
            json!({"result":[{"a":"x","b":"1"},{"a":"x","b":"2"}]}),
        );
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            unverified_cap(&result, &["explicit groupBy cap of 2", "cannot be checked"]),
            "{query}: {:?}",
            result.findings
        );
        assert!(group_cap_findings(&result).is_empty(), "{query}");
    }
}

#[test]
fn later_aggregates_that_rewrite_a_collected_field_make_its_values_unobservable() {
    for query in [
        "collect([b], limit=2) | count(as=b)",
        "collect([b], limit=2) | b := count()",
        "collect([b], limit=2) | sum(x, as=b)",
        "collect([b], limit=2) | groupBy([b], function=count(as=b))",
        "collect([b], limit=2) | stats(count(as=b))",
        r#"collect([b], limit=2) | stats({ selectLast(b) | b := "z" })"#,
        // Branches are merged conservatively, whatever their order.
        r#"collect([b], limit=2) | case { x=1 | b := "z" ; * }"#,
        r#"collect([b], limit=2) | case { * ; x=1 | b := "z" }"#,
        r#"collect([b], limit=2) | stats({ case { * ; x=1 | b := "z" } | selectLast(b) })"#,
    ] {
        let result = assess(query, json!({"result":[{"b":"1"}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            unverified_cap(
                &result,
                &[
                    "collect() per-group value cap of 2",
                    "cannot be counted in the outer result"
                ]
            ),
            "{query}: {:?}",
            result.findings
        );
    }
    // Aggregates that only select, group by or keep the collected field still let it be counted.
    for (query, value, reached) in [
        ("collect([b], limit=2) | table([b])", json!("one"), false),
        (
            "collect([b], limit=2) | table([b])",
            json!("one\ntwo"),
            true,
        ),
        (
            "collect([b], limit=2) | selectLast([b])",
            json!(["one"]),
            false,
        ),
        ("collect([b], limit=2) | groupBy([b])", json!("one"), false),
        (
            "groupBy([a], function=collect([b], limit=2))",
            json!(["one", "two"]),
            true,
        ),
        // Sub-aggregates that select the existing value keep its lineage.
        (
            "collect([b], limit=2) | stats(selectLast(b))",
            json!("one\ntwo"),
            true,
        ),
        (
            "collect([b], limit=2) | stats({selectLast(b)})",
            json!("one\ntwo"),
            true,
        ),
        (
            "collect([b], limit=2) | groupBy(b, function=selectLast(b))",
            json!("one\ntwo"),
            true,
        ),
    ] {
        let result = assess_accepted(query, json!({"result":[{"a":"x","b":value,"_count":1}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        assert_eq!(
            result.findings.iter().any(|finding| {
                finding.code == "RESULT_CAP_REACHED"
                    && finding
                        .message
                        .contains("requested collect() per-group cap of 2")
            }),
            reached,
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn per_column_statistics_and_findings_are_bounded_with_a_summary() {
    let result = assess("#repo=fw_repo", unique_key_rows(1_500));
    assert_eq!(result.outcome, AssessOutcome::Advisory);
    assert_eq!(result.rows, 1_500);
    assert_eq!(result.columns.len(), 1_000);
    assert_eq!(
        result.columns["k0"],
        ColumnStats {
            present: 1,
            null: 0,
            empty: 0
        }
    );
    assert_eq!(
        result
            .findings
            .iter()
            .filter(|finding| finding.code == "RESULT_NULL_FIELDS")
            .count(),
        1_000
    );
    let summary = result
        .findings
        .iter()
        .filter(|finding| finding.code == "RESULT_COLUMN_REPORT_LIMIT")
        .collect::<Vec<_>>();
    assert_eq!(summary.len(), 1, "{summary:?}");
    assert_eq!(summary[0].severity, Severity::Info);
    assert!(
        summary[0]
            .message
            .contains("1000 of 1500 inspected columns")
            && summary[0]
                .message
                .contains("1000 of 1500 columns with missing, null or empty values"),
        "{}",
        summary[0].message
    );

    let at_bound = assess("#repo=fw_repo", unique_key_rows(1_000));
    assert_eq!(at_bound.outcome, AssessOutcome::Advisory);
    assert_eq!(at_bound.columns.len(), 1_000);
    assert!(
        at_bound
            .findings
            .iter()
            .all(|finding| finding.code != "RESULT_COLUMN_REPORT_LIMIT")
    );

    // Complete columns beyond the bound do not add findings or change the outcome.
    let wide = json!({"result":[(0..1_200)
        .map(|i| (format!("k{i}"), json!("v")))
        .collect::<serde_json::Map<_, _>>()]});
    let wide = assess("#repo=fw_repo", wide);
    assert_eq!(
        wide.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        wide.findings
    );
    assert_eq!(wide.columns.len(), 1_000);
    assert!(wide.findings.iter().any(|finding| {
        finding.code == "RESULT_COLUMN_REPORT_LIMIT"
            && finding.message.contains("1000 of 1200 inspected columns")
            && finding
                .message
                .contains("0 of 0 columns with missing, null or empty values")
    }));
}

#[test]
fn final_row_limits_exceeded_by_returned_rows_are_inconclusive() {
    for (query, returned) in [
        ("#repo=fw_repo | head(5)", 10),
        ("head(0)", 10),
        ("head()", 201),
        ("tail(5)", 6),
        ("sort(x, limit=5)", 6),
        ("table([x], limit=5)", 6),
        ("top(x, limit=3)", 4),
        (r#"top(x, limit=3, rest="true")"#, 5),
        ("groupBy([x], limit=5)", 10),
        ("groupBy([x], limit=2, function=[count(), max(y)])", 3),
    ] {
        let result = assess(query, grouped_x_rows(returned));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            result.findings.iter().any(|finding| {
                finding.code == "RESULT_CAP_EXCEEDED"
                    && finding.severity == Severity::Warning
                    && finding.message.contains(&format!("{returned} rows exceed"))
            }),
            "{query}: {:?}",
            result.findings
        );
        assert!(
            result.findings.iter().all(|finding| {
                !matches!(
                    finding.code.as_str(),
                    "RESULT_CAP_REACHED" | "RESULT_QUERY_COVERAGE"
                )
            }),
            "{query}: {:?}",
            result.findings
        );
    }
    for (query, returned, reached) in [
        ("#repo=fw_repo | head(5)", 5, true),
        ("#repo=fw_repo | head(5)", 4, false),
        (r#"top(x, limit=3, rest="true")"#, 4, true),
        ("groupBy([x], limit=max) | head(5)", 5, true),
    ] {
        let result = assess(query, grouped_x_rows(returned));
        assert!(
            result
                .findings
                .iter()
                .all(|finding| finding.code != "RESULT_CAP_EXCEEDED"),
            "{query}: {:?}",
            result.findings
        );
        assert_eq!(
            result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_CAP_REACHED"),
            reached,
            "{query}"
        );
    }
}

#[test]
fn non_literal_limit_values_are_unverified_instead_of_defaulted() {
    for (stage, query) in [
        ("head", "head(-1)"),
        ("head", "head(limit=?p)"),
        ("sort", "sort(user, limit=?p)"),
        ("groupBy", "groupBy([user], limit=?p)"),
    ] {
        let result = assess(query, rows(1));
        assert_eq!(result.outcome, AssessOutcome::Inconclusive, "{query}");
        assert!(
            result.caps.iter().all(|cap| cap.stage != stage),
            "{query}: {:?}",
            result.caps
        );
        assert!(
            unverified_cap(&result, &[stage, "limit value"]),
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn collect_without_joined_values_is_unverifiable_for_boolean_and_text_flags() {
    for query in [
        "collect([b], multival=false, limit=2)",
        r#"collect([b], multival="false", limit=2)"#,
    ] {
        let result = assess(query, json!({"result":[{"b":"1"}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            unverified_cap(
                &result,
                &[
                    "collect() per-group value cap of 2",
                    "cannot be counted in the outer result"
                ]
            ),
            "{query}: {:?}",
            result.findings
        );
    }
    let joined = assess(
        "collect([b], multival=true, limit=2)",
        json!({"result":[{"b":"1"}]}),
    );
    assert_eq!(
        joined.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        joined.findings
    );
}

#[test]
fn row_limits_followed_only_by_non_expanding_stages_still_bound_the_result() {
    for (query, returned) in [
        ("head(0) | select(a)", 1),
        ("head(5) | select(a)", 10),
        ("head(5) | drop(b)", 6),
        ("head(5) | rename(a, as=c)", 6),
        ("head(5) | n := 1 | c := a", 6),
        (r#"head(5) | a="x*" | !b="y""#, 6),
        ("top(x, limit=3) | select([x, _count])", 4),
        ("groupBy([x], limit=5) | select([x])", 6),
        ("groupBy([x], limit=2) | head(100)", 3),
        ("top(x, limit=3) | sort(_count, limit=10)", 4),
    ] {
        let payload = json!({
            "result": (0..returned)
                .map(|i| json!({"a": format!("x{i}"), "x": format!("x{i}"), "c": "c", "n": 1, "_count": 1}))
                .collect::<Vec<_>>()
        });
        let result = assess(query, payload);
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            result.findings.iter().any(|finding| {
                finding.code == "RESULT_CAP_EXCEEDED"
                    && finding.message.contains(&format!("{returned} rows exceed"))
                    && finding.message.contains("later stages cannot add rows")
            }),
            "{query}: {:?}",
            result.findings
        );
        // The earlier limit keeps its own intermediate finding.
        assert!(
            result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"),
            "{query}: {:?}",
            result.findings
        );
    }
    // Within the bound, or after a stage that can change the row count, nothing new is reported.
    for (query, returned) in [
        ("head(5) | select(a)", 5),
        ("head(5) | groupBy([a])", 10),
        ("head(5) | lower(a)", 10),
        ("head(5) | case { a=x1 | c := 1 ; * }", 10),
        (r#"head(5) | regex("(?<c>x)", field=a)"#, 10),
        ("groupBy([x]) | select([x])", 3),
    ] {
        let payload = json!({
            "result": (0..returned)
                .map(|i| json!({"a": format!("x{i}"), "x": format!("x{i}"), "c": "c", "_count": 1}))
                .collect::<Vec<_>>()
        });
        let result = assess(query, payload);
        assert!(
            result
                .findings
                .iter()
                .all(|finding| finding.code != "RESULT_CAP_EXCEEDED"),
            "{query}: {:?}",
            result.findings
        );
    }
}

fn group_count_unverified(result: &AssessResult) -> bool {
    unverified_cap(result, &["groupBy cap of 2", "cannot be checked"])
}

#[test]
fn group_counts_use_only_listed_key_preserving_sub_aggregations() {
    let one_group = json!({"result":[
        {"a":"x","a[0]":"x","b":"1","c":"x","n":1,"_count":1},
        {"a":"x","a[0]":"x","b":"2","c":"x","n":1,"_count":1},
        {"a":"x","a[0]":"x","b":"3","c":"x","n":1,"_count":1}
    ]});
    // Listed calls and inline pipelines of comparison filters and listed calls count one group.
    for query in [
        "groupBy(a, function=tail(3), limit=2)",
        "groupBy(a, function=head(3), limit=2)",
        "groupBy(a, function=sort(b, limit=3), limit=2)",
        "groupBy(a, function={tail(3)}, limit=2)",
        r#"groupBy(a, function={ c="x" | sort(b, limit=10) | tail(3) }, limit=2)"#,
        "groupBy(a, function=top(b, limit=3), limit=2)",
        "groupBy(a, function=collect([b], multival=false, limit=5), limit=2)",
        "groupBy(a, function=table([b], limit=5), limit=2)",
        "groupBy(a, function=[tail(3), count(as=n)], limit=2)",
        "groupBy(a, function=[tail(3), selectLast(b)], limit=2)",
        "groupBy(a, function={ top(b, limit=3) | tail(3) }, limit=2)",
    ] {
        let result = assess(query, one_group.clone());
        assert!(
            !group_count_unverified(&result) && group_cap_findings(&result).is_empty(),
            "{query}: {:?}",
            result.findings
        );
    }
    // The nearest variants that could write, remove or rename a key, or that are not listed.
    for query in [
        r#"groupBy(a, function={stats({b=/(?<a>x)/ | tail(3)})}, limit=2)"#,
        "groupBy(a, function=stats([tail(3)]), limit=2)",
        r#"groupBy(a, function={ b=/(?<a>x)/ | tail(3) }, limit=2)"#,
        r#"groupBy(a, function={ b=/x/ | tail(3) }, limit=2)"#,
        r#"groupBy(a, function={ a := "x" | tail(3) }, limit=2)"#,
        r#"groupBy(a, function={ case { b="1" | a := "x" ; * } | tail(3) }, limit=2)"#,
        r#"groupBy(a, function={ createEvents(["a=y"]) | tail(3) }, limit=2)"#,
        "groupBy(a, function={ drop(a) | tail(3) }, limit=2)",
        "groupBy(a, function={ rename(b, as=a) | tail(3) }, limit=2)",
        "groupBy(a, function={ lower(b) | tail(3) }, limit=2)",
        "groupBy(a, function=[tail(3), count(as=a)], limit=2)",
        "groupBy(a, function=[tail(3), max(b, as=a)], limit=2)",
        "groupBy(a, function=[tail(3), selectLast(a)], limit=2)",
        "groupBy(a, function=[tail(3), percentile(b)], limit=2)",
        "groupBy(a, function={ tail(3) | top(b, as=a) }, limit=2)",
        "groupBy(a, function={ c=?p | tail(3) }, limit=2)",
        "groupBy(a, function=top(b, as=a), limit=2)",
        "groupBy(a, function=top(a), limit=2)",
        "groupBy(a, function=collect([a], multival=false), limit=2)",
        r#"groupBy(["a[0]"], function={ array:drop("a[]") | tail(3) }, limit=2)"#,
        r#"groupBy(["a[0]"], function={ array:rename("b[]", asArray="a[]") | tail(3) }, limit=2)"#,
    ] {
        let result = assess(query, one_group.clone());
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            group_count_unverified(&result),
            "{query}: {:?}",
            result.findings
        );
        assert!(group_cap_findings(&result).is_empty(), "{query}");
    }
}

#[test]
fn collected_values_survive_only_listed_value_selectors_and_keys() {
    // Keys and value selectors copy the collected value unchanged.
    for query in [
        "collect([b], limit=2) | table([b])",
        "collect([b], limit=2) | selectLast([b])",
        "collect([b], limit=2) | selectFromMax(x, include=[b])",
        "collect([b], limit=2) | groupBy([b])",
        "collect([b], limit=2) | top(b)",
        "collect([b], limit=2) | stats(selectLast(b))",
        "collect([b], limit=2) | stats({selectLast(b)})",
        r#"collect([b], limit=2) | stats({ x="1" | tail(1) | selectLast(b) })"#,
        "collect([b], limit=2) | groupBy(b, function=selectLast(b))",
    ] {
        let result = assess(query, json!({"result":[{"b":"one\ntwo","_count":1}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        finding(&result, "RESULT_CAP_REACHED", Severity::Info);
    }
    // Computed outputs invalidate the list whatever their name, and so does anything unlisted.
    for query in [
        "collect([_count], limit=2) | stats(count(_count, distinct=false))",
        "collect([_count], limit=2) | count(_count)",
        "collect([_sum], limit=2) | stats(sum(_sum))",
        "collect([_count], limit=2) | groupBy([_count])",
        "collect([_count], limit=2) | top(_count)",
        "collect([b], limit=2) | top(b, as=b)",
        "collect([b], limit=2) | collect([b], limit=5)",
        "collect([b], limit=2) | stats(stats(selectLast(b)))",
        "collect([b], limit=2) | stats([selectLast(b), count(as=b)])",
        "collect([b], limit=2) | stats({ b=/(?<b>x)/ | selectLast(b) })",
        r#"collect([b], limit=2) | stats({ createEvents(["b=z"]) | selectLast(b) })"#,
        "collect([b], limit=2) | stats({ lower(b, as=b) | selectLast(b) })",
        "collect([b], limit=2) | groupBy([a], function=collect([b], limit=5))",
        "collect([b], limit=2) | session(function=selectLast(b))",
        "collect([b], limit=2) | stats({ selectLast(b) | rename(b, as=c) | rename(c, as=b) })",
    ] {
        let result = assess(
            query,
            json!({"result":[{"a":"x","b":"1","_count":"1","_sum":"1"}]}),
        );
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            unverified_cap(
                &result,
                &[
                    "collect() per-group value cap of 2",
                    "cannot be counted in the outer result"
                ]
            ),
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn comparison_only_disjunctions_keep_the_row_bound() {
    let rows = |n: usize| json!({"result": (0..n).map(|i| json!({"a": format!("x{i}"), "b": "z"})).collect::<Vec<_>>()});
    for (query, returned) in [
        ("head(0) | (a=x OR a=y)", 1),
        ("head(5) | (a=x OR (a=y AND b=z))", 10),
        ("head(5) | (a=x OR a=y) | select([a])", 10),
        ("head(5) | !(a=x OR a=y)", 10),
    ] {
        let result = assess(query, rows(returned));
        assert!(
            result.findings.iter().any(|finding| {
                finding.code == "RESULT_CAP_EXCEEDED"
                    && finding.message.contains("later stages cannot add rows")
            }),
            "{query}: {:?}",
            result.findings
        );
    }
    // A disjunction with a function call, or any unlisted later stage, drops the bound.
    for query in [
        "head(0) | (a=x OR in(a, values=[y]))",
        "head(0) | (a=x OR a=y) | c := lower(a)",
        "head(0) | case { a=x ; a=y }",
        r#"head(0) | createEvents(["a=x"])"#,
        r#"head(0) | (a=x OR a=y) | createEvents(["a=x"])"#,
        "head(0) | (a=x OR a=y) | groupBy([a])",
    ] {
        let result = assess(query, rows(1));
        assert!(
            result
                .findings
                .iter()
                .all(|finding| finding.code != "RESULT_CAP_EXCEEDED"),
            "{query}: {:?}",
            result.findings
        );
    }
}

#[test]
fn branch_merges_need_every_branch_to_keep_the_collected_list() {
    for query in [
        r#"collect([b], limit=2) | case { b=z | createEvents(["z"]) | b := "changed" ; * }"#,
        r#"collect([b], limit=2) | case { * ; b=z | createEvents(["z"]) | b := "changed" }"#,
        r#"collect([b], limit=2) | b match { z => createEvents(["z"]) | b := "changed" ; * => * }"#,
    ] {
        let result = assess(query, json!({"result":[{"b":"changed"}]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            unverified_cap(
                &result,
                &[
                    "collect() per-group value cap of 2",
                    "cannot be counted in the outer result"
                ]
            ),
            "{query}: {:?}",
            result.findings
        );
    }
    // Every branch keeps the list: still countable.
    let kept = assess(
        r#"collect([b], limit=2) | case { x="1" ; * }"#,
        json!({"result":[{"b":"one\ntwo"}]}),
    );
    assert_eq!(
        kept.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        kept.findings
    );
    finding(&kept, "RESULT_CAP_REACHED", Severity::Info);
    // Every branch replaces the source: the original list is gone, not reported.
    let reset = assess(
        r#"collect([b], limit=2) | case { createEvents(["z"]) | b := "x" ; createEvents(["y"]) | b := "x" }"#,
        json!({"result":[{"b":"x"}]}),
    );
    assert!(
        reset
            .findings
            .iter()
            .all(|finding| !finding.message.contains("collect()")),
        "{:?}",
        reset.findings
    );
}

#[test]
fn collected_lineage_compares_names_by_overlap_not_spelling() {
    let invalid = |result: &AssessResult| {
        unverified_cap(
            result,
            &[
                "collect() per-group value cap of 2",
                "cannot be counted in the outer result",
            ],
        )
    };
    for (query, row) in [
        (
            r#"collect(["b[0]","a[0]"], limit=2) | stats([array:union("a[]", as=b), selectLast("a[0]")])"#,
            json!({"b[0]":"changed","a[0]":"changed"}),
        ),
        (
            r#"collect(["b[0]"], limit=2) | stats(count(as=b))"#,
            json!({"b[0]":"1","b":"1"}),
        ),
        (
            r#"collect(["b[0]"], limit=2) | b := count()"#,
            json!({"b[0]":"1","b":"1"}),
        ),
        (
            r#"collect([b], limit=2) | stats(count(as="b.x"))"#,
            json!({"b":"1","b.x":"1"}),
        ),
        (
            r#"collect([b], limit=2) | groupBy(["b[0]"])"#,
            json!({"b":"1","b[0]":"1","_count":1}),
        ),
        (
            r#"collect(["b[1]"], limit=2) | stats([selectLast("b[1]"), count(as="b[0]")])"#,
            json!({"b[1]":"1","b[0]":"1"}),
        ),
        (
            r#"collect([b], limit=2) | stats(count(as="b*"))"#,
            json!({"b":"1"}),
        ),
        (
            r#"collect(["b.x"], limit=2) | stats(count(as=b))"#,
            json!({"b.x":"1","b":"1"}),
        ),
        (
            r#"collect([b], limit=2) | rename(c, as="b[0]")"#,
            json!({"b":"1"}),
        ),
        (r#"collect([b], limit=2) | "b.x" := 1"#, json!({"b":"1"})),
    ] {
        let result = assess(query, json!({"result":[row]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(invalid(&result), "{query}: {:?}", result.findings);
    }
    // Exact copies of an array element are still counted.
    for (query, row) in [
        (
            r#"collect(["a[0]"], limit=2) | stats(selectLast("a[0]"))"#,
            json!({"a[0]":"one\ntwo"}),
        ),
        (
            r#"collect(["a[0]"], limit=2) | selectLast(["a[0]"])"#,
            json!({"a[0]":"one\ntwo"}),
        ),
        // Copying a sibling element unchanged does not alter the collected element.
        (
            r#"collect(["b[0]"], limit=2) | table(["b[0]", "b[1]"])"#,
            json!({"b[0]":"one\ntwo","b[1]":"z"}),
        ),
    ] {
        let result = assess(query, json!({"result":[row]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        finding(&result, "RESULT_CAP_REACHED", Severity::Info);
    }
}

#[test]
fn group_counts_need_plain_keys_matched_exactly_in_rows() {
    for (query, rows) in [
        (
            r#"groupBy("a[]", function=tail(3), limit=2)"#,
            json!([{"a[0]":"x"},{"a[0]":"y"},{"a[0]":"z"}]),
        ),
        (
            r#"groupBy(["a[0]"], function=tail(3), limit=2)"#,
            json!([{"a[0]":"x"},{"a[0]":"x"},{"a[0]":"x"}]),
        ),
        (
            "groupBy(a, function=tail(3), limit=2)",
            json!([{"a.x":"1"},{"a.x":"2"},{"a.x":"3"}]),
        ),
        (
            r#"groupBy(a, function=[tail(3), count(as="a[0]")], limit=2)"#,
            json!([{"a":"x"},{"a":"x"},{"a":"x"}]),
        ),
        (
            r#"groupBy(a, function=[tail(3), count(as="a.x")], limit=2)"#,
            json!([{"a":"x"},{"a":"x"},{"a":"x"}]),
        ),
        (
            r#"groupBy(a, function=[tail(3), count(as="a*")], limit=2)"#,
            json!([{"a":"x"},{"a":"x"},{"a":"x"}]),
        ),
        (
            r#"groupBy(a, function=top(b, as="a[]"), limit=2)"#,
            json!([{"a":"x"},{"a":"x"},{"a":"x"}]),
        ),
        (
            r#"groupBy(a, function=collect(["a.x"], multival=false), limit=2)"#,
            json!([{"a":"x"},{"a":"x"},{"a":"x"}]),
        ),
        (
            r#"groupBy(["a.b"], function=[tail(3), count(as=a)], limit=2)"#,
            json!([{"a.b":"x"},{"a.b":"x"},{"a.b":"x"}]),
        ),
    ] {
        let result = assess(query, json!({"result":rows}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            group_count_unverified(&result),
            "{query}: {:?}",
            result.findings
        );
        assert!(group_cap_findings(&result).is_empty(), "{query}");
    }
    // A plain key present in every row is still counted, including a dotted name.
    let counted = assess(
        "groupBy(a, function=tail(3), limit=2)",
        json!({"result":[{"a":"x","a.x":"1"},{"a":"x","a.x":"2"},{"a":"x","a.x":"3"}]}),
    );
    let dotted = assess(
        r#"groupBy(["a.b"], function=tail(3), limit=2)"#,
        json!({"result":[{"a.b":"x"},{"a.b":"x"},{"a.b":"x"}]}),
    );
    assert_eq!(
        dotted.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        dotted.findings
    );
    assert_eq!(
        counted.outcome,
        AssessOutcome::NoFindings,
        "{:?}",
        counted.findings
    );
}

#[test]
fn rows_beyond_an_explicit_limit_after_unproven_stages_are_inconclusive() {
    let rows = |n: usize| json!({"result": (0..n).map(|i| json!({"a": format!("x{i}"), "_lower": "x", "_count": 1})).collect::<Vec<_>>()});
    for (query, returned) in [
        ("head(0) | in(a, values=[x])", 1),
        ("head(0) | lower(a)", 1),
        ("head(2) | lower(a)", 3),
        (r#"head(0) | case { createEvents(["a=x"]) ; * }"#, 1),
        ("sort(a, limit=2) | in(a, values=[x0, x1, x2])", 3),
    ] {
        let result = assess(query, rows(returned));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            unverified_cap(&result, &["exceed the requested", "not proven"]),
            "{query}: {:?}",
            result.findings
        );
        assert!(
            result
                .findings
                .iter()
                .all(|finding| finding.code != "RESULT_CAP_EXCEEDED"),
            "{query}"
        );
    }
    // Within the limit, after a complete source replacement, or for a default limit: unchanged.
    for (query, returned) in [
        ("head(2) | in(a, values=[x0, x1])", 2),
        (r#"head(0) | createEvents(["a=x"])"#, 1),
    ] {
        let result = assess(query, rows(returned));
        assert!(
            !unverified_cap(&result, &["not proven"]),
            "{query}: {:?}",
            result.findings
        );
    }
    let default = assess("head() | lower(a)", rows(201));
    assert!(
        !unverified_cap(&default, &["not proven"]),
        "{:?}",
        default.findings
    );
}

#[test]
fn assigned_row_limits_bound_the_result_like_their_call_forms() {
    let rows = |n: usize| json!({"result": (0..n).map(|i| json!({"a": format!("x{i}")})).collect::<Vec<_>>()});
    let exceeded = assess("a := table([a], limit=1) | select(a)", rows(2));
    assert_eq!(
        exceeded.outcome,
        AssessOutcome::Inconclusive,
        "{:?}",
        exceeded.findings
    );
    assert!(
        exceeded.findings.iter().any(|finding| {
            finding.code == "RESULT_CAP_EXCEEDED"
                && finding.message.contains("later stages cannot add rows")
        }),
        "{:?}",
        exceeded.findings
    );
    let unproven = assess("a := table([a], limit=1) | in(a, values=[x0, x1])", rows(2));
    assert_eq!(
        unproven.outcome,
        AssessOutcome::Inconclusive,
        "{:?}",
        unproven.findings
    );
    assert!(
        unverified_cap(&unproven, &["exceed the requested", "not proven"]),
        "{:?}",
        unproven.findings
    );
    // An assigned rename keeps a row bound like the call form.
    for query in ["head(1) | rename(a, as=c)", "head(1) | c := rename(a)"] {
        let result = assess(query, rows(2));
        assert!(
            result.findings.iter().any(|finding| {
                finding.code == "RESULT_CAP_EXCEEDED"
                    && finding.message.contains("later stages cannot add rows")
            }),
            "{query}: {:?}",
            result.findings
        );
    }
    // The final assigned form and results within the limit keep their earlier behaviour.
    let final_form = assess("a := table([a], limit=1)", rows(2));
    finding(&final_form, "RESULT_CAP_EXCEEDED", Severity::Warning);
    let within = assess("a := table([a], limit=2) | select(a)", rows(2));
    assert!(
        within
            .findings
            .iter()
            .all(|finding| finding.code != "RESULT_CAP_EXCEEDED"
                && !finding.message.contains("not proven")),
        "{:?}",
        within.findings
    );
}

#[test]
fn a_collected_field_that_leaves_the_stream_stays_uncountable() {
    for (query, row) in [
        (
            "collect([b, payload], limit=2) | select(payload)",
            json!({"payload":"one"}),
        ),
        (
            "collect([b, payload], limit=2) | drop(b) | kvParse(field=payload)",
            json!({"b":"changed","payload":"b=changed"}),
        ),
        (
            r#"collect(["b[0]", payload], limit=2) | array:drop("b[]") | kvParse(field=payload)"#,
            json!({"b[0]":"changed","payload":"x"}),
        ),
        (
            r#"collect(["b[0]", payload], limit=2) | array:rename("b[]", asArray="c[]") | kvParse(field=payload)"#,
            json!({"b[0]":"changed","payload":"x"}),
        ),
        (
            "collect([B, payload], limit=2) | lowercase(B, include=fields) | kvParse(field=payload)",
            json!({"B":"changed","payload":"B=changed"}),
        ),
        (
            "collect([b, payload], limit=2) | table([payload]) | kvParse(field=payload)",
            json!({"b":"changed","payload":"b=changed"}),
        ),
        (
            r#"collect([b, payload], limit=2) | drop("b.x") | kvParse(field=payload)"#,
            json!({"b":"changed","payload":"b=changed"}),
        ),
    ] {
        let result = assess(query, json!({"result":[row]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            unverified_cap(
                &result,
                &[
                    "collect() per-group value cap of 2",
                    "cannot be counted in the outer result"
                ]
            ),
            "{query}: {:?}",
            result.findings
        );
    }
    // Keeping every collected field, or an exact tracked rename, still counts the list.
    for (query, row) in [
        (
            "collect([b, payload], limit=2) | select([b, payload])",
            json!({"b":"one\ntwo","payload":"one"}),
        ),
        (
            "collect([b], limit=2) | rename(b, as=c)",
            json!({"c":"one\ntwo"}),
        ),
        ("collect([b], limit=2) | drop(c)", json!({"b":"one\ntwo"})),
    ] {
        let result = assess(query, json!({"result":[row]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        finding(&result, "RESULT_CAP_REACHED", Severity::Info);
    }
}

#[test]
fn grouping_keys_and_selector_children_carry_separate_collected_fields() {
    // The key carries one collected field and the selector the other, in either order.
    for query in [
        "collect([b, payload], limit=2) | groupBy(b, function=selectLast(payload), limit=10)",
        "collect([b, payload], limit=2) | groupBy(payload, function=selectLast(b), limit=10)",
    ] {
        let result = assess(
            query,
            json!({"result":[{"b":"one\ntwo","payload":"one\ntwo"}]}),
        );
        assert_eq!(
            result.outcome,
            AssessOutcome::NoFindings,
            "{query}: {:?}",
            result.findings
        );
        finding(&result, "RESULT_CAP_REACHED", Severity::Info);
    }
    // A collected field carried by neither the key nor a child, or a child that computes the
    // value instead of selecting it, still leaves the list uncountable.
    for (query, row) in [
        (
            "collect([b, payload, c], limit=2) | groupBy(b, function=selectLast(payload), limit=10)",
            json!({"b":"one","payload":"one","c":"x"}),
        ),
        (
            "collect([b, payload], limit=2) | groupBy(b, function=count(as=payload), limit=10)",
            json!({"b":"one","payload":"1"}),
        ),
        (
            "collect([b, payload], limit=2) | groupBy(b, function=sum(payload), limit=10)",
            json!({"b":"one","_sum":"1"}),
        ),
    ] {
        let result = assess(query, json!({"result":[row]}));
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{query}: {:?}",
            result.findings
        );
        assert!(
            unverified_cap(
                &result,
                &[
                    "collect() per-group value cap of 2",
                    "cannot be counted in the outer result"
                ]
            ),
            "{query}: {:?}",
            result.findings
        );
    }
}
