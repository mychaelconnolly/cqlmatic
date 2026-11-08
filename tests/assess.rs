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
fn max_groupby_upstream_and_requested_head_size_are_informational() {
    let result = assess("groupBy([user], limit=max) | head(10)", rows(10));
    assert_eq!(result.outcome, AssessOutcome::NoFindings);
    assert_eq!(
        result
            .caps
            .iter()
            .map(|cap| (cap.stage.as_str(), cap.cap))
            .collect::<Vec<_>>(),
        [("groupBy", 1_000_000), ("head", 10)]
    );
    finding(&result, "RESULT_CAP_REACHED", Severity::Info);
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Info);
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
    assert_eq!(table_max.caps[0].cap, 200_000);
    assert_eq!(table_max.outcome, AssessOutcome::NoFindings);
    let group_max = assess("groupBy([user], limit=max)", rows(1));
    assert_eq!(group_max.caps[0].cap, 1_000_000);
    assert_eq!(group_max.outcome, AssessOutcome::NoFindings);
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
fn groupby_max_upstream_and_requested_sort_size_are_information_only() {
    let result = assess_accepted(
        "groupBy([x], limit=max) | sort(_count, limit=10)",
        grouped_x_rows(10),
    );
    assert_eq!(result.outcome, AssessOutcome::NoFindings);
    finding(&result, "RESULT_UPSTREAM_CAP_UNVERIFIED", Severity::Info);
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
fn sort_default_cap_message_does_not_recommend_unsupported_max() {
    let result = assess_accepted("sort(user)", rows(200));
    assert_eq!(result.outcome, AssessOutcome::Inconclusive);
    finding(&result, "RESULT_CAP_REACHED", Severity::Warning);
    let message = &result
        .findings
        .iter()
        .find(|f| f.code == "RESULT_CAP_REACHED")
        .unwrap()
        .message;
    assert!(!message.contains("limit=max is"), "{message}");
    assert!(message.contains("sort has no limit=max"), "{message}");
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
    let result = assess(
        r#"top(user, rest="others")"#,
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
    let explicit = assess(
        r#"top(user, limit=1, rest="others")"#,
        json!({"result":[{"user":"a","_count":1},{"user":"others","_count":1}]}),
    );
    assert_eq!(explicit.outcome, AssessOutcome::NoFindings);
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
