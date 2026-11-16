use cqlmatic::{
    assess::{self, AssessOutcome, AssessRequest},
    catalog,
    contracts::Context,
};

#[test]
fn series_memory_default_matches_the_attributed_function_description() {
    let record = catalog::records()
        .iter()
        .find(|record| record.id == "cql:function:series")
        .unwrap();
    assert!(record.body.contains("default '1KiB'"));
    assert_eq!(record.source_path, "references/cql/correlation.md");
    let rule = catalog::parameter_rules("series")
        .iter()
        .find(|rule| rule.name == "memlimit")
        .unwrap();
    assert_eq!(rule.default.as_deref(), Some("1KiB"));
    assert!(rule.minimum.is_none() && rule.maximum.is_none());
}

#[test]
fn series_default_and_explicit_memory_caps_both_remain_unverified() {
    let evaluate = |query: &str| {
        assess::assess(
            AssessRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                result: serde_json::json!({"result":[{"a":"example"}]}),
                context: Context::default(),
            },
            None,
        )
    };
    let omitted = evaluate("series(collect=[a])");
    let explicit = evaluate(r#"series(collect=[a],memlimit="1KiB")"#);
    for result in [&explicit, &omitted] {
        assert_eq!(result.outcome, AssessOutcome::Inconclusive, "{result:?}");
        assert!(
            result
                .findings
                .iter()
                .any(|finding| finding.code == "RESULT_UPSTREAM_CAP_UNVERIFIED"
                    && finding.message.contains("series")),
            "{result:?}"
        );
    }
}
