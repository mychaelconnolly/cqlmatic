use cqlmatic::{
    assess::{self, AssessOutcome, AssessRequest},
    contracts::Context,
    mcp, sha256_hex,
};
use serde_json::{Value, json};

const BARE: &[u8] = include_bytes!("fixtures/mcp/bare.json");
const ENVELOPE: &[u8] = include_bytes!("fixtures/mcp/envelope.json");

fn assert_code(raw: &[u8], code: &str) {
    let diagnostic = mcp::normalize(raw).unwrap_err();
    assert_eq!(diagnostic.code, code);
}

fn envelope(text: &str, is_error: bool) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "content": [{"type": "text", "text": text}],
        "isError": is_error
    }))
    .unwrap()
}

fn assess_saved(result: Value) -> assess::AssessResult {
    assess::assess(
        AssessRequest {
            schema_version: 1,
            id: None,
            query: "#repo=synthetic_repo | count(as=events)".into(),
            result,
            context: Context::default(),
        },
        None,
    )
}

#[test]
fn bare_result_keeps_string_counts_and_row_values() {
    let normalized = mcp::normalize(BARE).unwrap();
    assert_eq!(normalized.rows.len(), 1);
    assert_eq!(normalized.rows[0]["_count"], "24");
    assert_eq!(normalized.rows[0]["user"], "example_user");
    assert_eq!(normalized.rows[0]["#repo"], "example_repo");
    assert_eq!(normalized.trace_id.as_deref(), Some("trace-example-1"));
}

#[test]
fn envelope_round_trip_matches_bare_rows_and_trace() {
    let bare = mcp::normalize(BARE).unwrap();
    let wrapped = mcp::normalize(ENVELOPE).unwrap();
    assert_eq!(wrapped.rows, bare.rows);
    assert_eq!(wrapped.trace_id, bare.trace_id);
    assert_eq!(wrapped.raw_sha256, sha256_hex(ENVELOPE));
    assert_ne!(wrapped.raw_sha256, bare.raw_sha256);
}

#[test]
fn standard_envelope_accepts_omitted_optional_error_flag_without_rewriting_bytes() {
    let bare = mcp::normalize(BARE).unwrap();
    for with_structured in [false, true] {
        let mut value =
            json!({"content": [{"type": "text", "text": std::str::from_utf8(BARE).unwrap()}]});
        if with_structured {
            value["structuredContent"] = serde_json::from_slice::<Value>(BARE).unwrap();
        }
        let raw = serde_json::to_vec(&value).unwrap();
        let normalized = mcp::normalize(&raw).unwrap();
        assert_eq!(normalized.rows, bare.rows);
        assert_eq!(normalized.trace_id, bare.trace_id);
        assert_eq!(normalized.raw_sha256, sha256_hex(&raw));
        assert_ne!(normalized.raw_sha256, bare.raw_sha256);
    }
}

#[test]
fn optional_error_flag_rejects_explicit_non_boolean_and_true_values() {
    for value in [Value::Null, json!("false"), json!(0), json!([]), json!({})] {
        let input =
            json!({"content": [{"type": "text", "text": "{\"result\":[]}"}], "isError": value});
        assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_SHAPE");
    }
    assert_code(&envelope(r#"{"result":[]}"#, true), "MCP_TOOL_ERROR");
}

#[test]
fn tool_error_includes_at_most_300_text_characters() {
    let text = "é".repeat(350);
    let diagnostic = mcp::normalize(&envelope(&text, true)).unwrap_err();
    assert_eq!(diagnostic.code, "MCP_TOOL_ERROR");
    assert!(diagnostic.message.contains(&"é".repeat(300)));
    assert!(!diagnostic.message.contains(&"é".repeat(301)));
    assert_code(&envelope("tool failed", true), "MCP_TOOL_ERROR");
}

#[test]
fn malformed_envelopes_fail_closed() {
    for envelope in [
        json!({"content": [], "isError": false}),
        json!({"content": [{"type": "text", "text": "{}"}, {"type": "text", "text": "{}"}], "isError": false}),
        json!({"content": [{"type": "image", "text": "{}"}], "isError": false}),
        json!({"content": [{"type": "text", "text": 4}], "isError": false}),
        json!({"content": ["text"], "isError": false}),
        json!({"content": [{"type": "text", "text": "{}"}], "isError": null}),
        json!({"content": [{"type": "text", "text": "{}"}], "isError": false, "extra": 1}),
        json!({"content": [{"type": "text", "text": "{}"}], "isError": "false"}),
    ] {
        assert_code(&serde_json::to_vec(&envelope).unwrap(), "MCP_SHAPE");
    }
    for text in ["not JSON", "[]", "null", "{\"result\":[]}\n{}"] {
        assert_code(&envelope(text, false), "MCP_SHAPE");
    }
}

#[test]
fn nested_envelope_is_rejected_without_parsing_text_again() {
    let nested = envelope(r#"{"result":[]}"#, false);
    let nested_text = std::str::from_utf8(&nested).unwrap();
    assert_code(&envelope(nested_text, false), "MCP_SHAPE");
    // An error envelope must not hide a nested envelope behind MCP_TOOL_ERROR.
    assert_code(&envelope(nested_text, true), "MCP_SHAPE");
}

#[test]
fn top_level_error_keys_fail_even_when_not_strings() {
    for key in ["error", "errors"] {
        for value in [
            json!("failed"),
            json!(false),
            json!(0),
            json!([]),
            json!({}),
        ] {
            let mut input = json!({"result": []});
            input.as_object_mut().unwrap().insert(key.into(), value);
            assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_TOOL_ERROR");
        }
    }
    assert_eq!(
        mcp::normalize(br#"{"result":[],"error":null,"errors":null}"#)
            .unwrap()
            .rows
            .len(),
        0
    );
}

#[test]
fn timeout_wording_takes_precedence_over_tool_errors() {
    for input in [
        json!({"result": [], "trace_id": "TIMEOUT"}),
        json!({"result": [], "error": "TimEd OuT", "warnings": ["incomplete"]}),
        json!({"result": [], "errors": [{"message": "Deadline exceeded"}]}),
        json!({"result": [], "status": "timeout"}),
    ] {
        assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_DEADLINE");
    }
    for text in ["TIMEOUT", "timed out", "deadline exceeded"] {
        assert_code(&envelope(text, true), "MCP_DEADLINE");
    }
    assert_code(
        br#"{"result":[],"error":"failed","warnings":["incomplete"]}"#,
        "MCP_TOOL_ERROR",
    );
}

#[test]
fn every_completeness_flag_rejects_truthy_top_level_values() {
    for flag in [
        "hasMoreEvents",
        "truncated",
        "partial",
        "limitReached",
        "eventsSkipped",
        "skippedEvents",
        "timedOut",
        "warnings",
    ] {
        for value in [
            json!(true),
            json!(1),
            json!(-1),
            json!(0.5),
            json!("yes"),
            json!(["notice"]),
            json!({"notice": true}),
        ] {
            let mut input = json!({"result": []});
            input.as_object_mut().unwrap().insert(flag.into(), value);
            assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_INCOMPLETE");
        }
    }
}

#[test]
fn every_completeness_flag_allows_falsy_top_level_values() {
    for flag in [
        "hasMoreEvents",
        "truncated",
        "partial",
        "limitReached",
        "eventsSkipped",
        "skippedEvents",
        "timedOut",
        "warnings",
    ] {
        for value in [
            Value::Null,
            json!(false),
            json!(0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            let mut input = json!({"result": []});
            input.as_object_mut().unwrap().insert(flag.into(), value);
            assert_eq!(
                mcp::normalize(&serde_json::to_vec(&input).unwrap())
                    .unwrap()
                    .rows
                    .len(),
                0
            );
        }
    }
}

#[test]
fn floating_completeness_flags_fail_closed_after_json_underflow() {
    for number in ["1e-10000", "-1e-10000", "0.0", "-0.0", "0e0"] {
        let raw = format!(r#"{{"result":[{{"_count":"1"}}],"partial":{number}}}"#);
        assert_code(raw.as_bytes(), "MCP_INCOMPLETE");
        let metadata = format!(
            r#"{{"result":[{{"_count":"1"}}],"metadata":{{"extraData":{{"partial":{number}}}}}}}"#
        );
        assert_code(metadata.as_bytes(), "MCP_INCOMPLETE");
        let live = format!(
            r#"{{"result":[{{"_count":"1"}}],"metadata":{{"filterQuery":{{"isLive":{number}}}}}}}"#
        );
        assert_code(live.as_bytes(), "MCP_INCOMPLETE");
    }
    assert!(mcp::normalize(br#"{"result":[],"partial":0}"#).is_ok());
}

#[test]
fn metadata_flags_follow_the_same_completeness_rules() {
    for metadata_key in ["metaData", "metadata"] {
        for flag in [
            "hasMoreEvents",
            "truncated",
            "partial",
            "limitReached",
            "eventsSkipped",
            "skippedEvents",
            "timedOut",
            "warnings",
        ] {
            let mut truthy = json!({"result": []});
            let mut truthy_metadata = json!({"other": "ignored"});
            truthy_metadata
                .as_object_mut()
                .unwrap()
                .insert(flag.into(), json!(true));
            truthy
                .as_object_mut()
                .unwrap()
                .insert(metadata_key.into(), truthy_metadata);
            assert_code(&serde_json::to_vec(&truthy).unwrap(), "MCP_INCOMPLETE");

            let mut falsy = json!({"result": []});
            let mut falsy_metadata = json!({"other": "ignored"});
            falsy_metadata
                .as_object_mut()
                .unwrap()
                .insert(flag.into(), json!(0));
            falsy
                .as_object_mut()
                .unwrap()
                .insert(metadata_key.into(), falsy_metadata);
            assert_eq!(
                mcp::normalize(&serde_json::to_vec(&falsy).unwrap())
                    .unwrap()
                    .rows
                    .len(),
                0
            );
        }
    }
}

#[test]
fn nested_search_errors_must_not_be_reported_as_no_findings() {
    for metadata_key in ["metaData", "metadata"] {
        for error_key in ["error", "errors"] {
            let mut input = json!({"result": [{"events": 7}]});
            input[metadata_key] = json!({});
            input[metadata_key][error_key] = json!([{"message": "synthetic query error"}]);
            let result = assess_saved(input.clone());
            assert_eq!(
                result.outcome,
                AssessOutcome::Inconclusive,
                "{:?}",
                result.findings
            );
            assert!(result.findings.iter().any(|d| d.code == "RESULT_ERROR"));
            let raw = serde_json::to_vec(&input).unwrap();
            assert_code(&raw, "MCP_TOOL_ERROR");
            assert_code(
                &envelope(std::str::from_utf8(&raw).unwrap(), false),
                "MCP_TOOL_ERROR",
            );

            input[metadata_key][error_key] = Value::Null;
            assert_eq!(
                mcp::normalize(&serde_json::to_vec(&input).unwrap())
                    .unwrap()
                    .rows
                    .len(),
                1
            );
            input[metadata_key][error_key] = json!({"message": "deadline exceeded"});
            assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_DEADLINE");
        }
    }
}

#[test]
fn nested_skipped_events_must_not_be_reported_as_no_findings() {
    for metadata_key in ["metaData", "metadata"] {
        let mut input = json!({"result": [{"events": 7}]});
        input[metadata_key] = json!({"extraData": {"eventsSkipped": 5}});
        let result = assess_saved(input.clone());
        assert_eq!(
            result.outcome,
            AssessOutcome::Inconclusive,
            "{:?}",
            result.findings
        );
        assert!(
            result
                .findings
                .iter()
                .any(|d| d.code == "RESULT_INCOMPLETE")
        );
        let raw = serde_json::to_vec(&input).unwrap();
        assert_code(&raw, "MCP_INCOMPLETE");
        assert_code(
            &envelope(std::str::from_utf8(&raw).unwrap(), false),
            "MCP_INCOMPLETE",
        );
    }
}

#[test]
fn nested_extra_data_flags_follow_top_level_truthiness() {
    for metadata_key in ["metaData", "metadata"] {
        for flag in [
            "hasMoreEvents",
            "truncated",
            "partial",
            "limitReached",
            "eventsSkipped",
            "skippedEvents",
            "timedOut",
        ] {
            for value in [json!(true), json!(1), json!("false"), json!(["notice"])] {
                let mut input = json!({"result": []});
                input[metadata_key] = json!({"extraData": {}});
                input[metadata_key]["extraData"][flag] = value;
                assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_INCOMPLETE");
            }
            for value in [
                Value::Null,
                json!(false),
                json!(0),
                json!(""),
                json!([]),
                json!({}),
            ] {
                let mut input = json!({"result": []});
                input[metadata_key] = json!({"extraData": {}});
                input[metadata_key]["extraData"][flag] = value;
                assert!(
                    mcp::normalize(&serde_json::to_vec(&input).unwrap())
                        .unwrap()
                        .rows
                        .is_empty()
                );
            }
        }
    }
}

#[test]
fn explicit_live_flag_must_not_be_accepted_as_complete_mcp_measurement() {
    for metadata_key in ["metaData", "metadata"] {
        let mut input = json!({"result": [{"events": 7}]});
        input[metadata_key] = json!({"filterQuery": {"isLive": true}});
        assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_INCOMPLETE");
        assert_code(&envelope(&input.to_string(), false), "MCP_INCOMPLETE");
        input[metadata_key]["filterQuery"]["isLive"] = json!(false);
        assert_eq!(
            mcp::normalize(&serde_json::to_vec(&input).unwrap())
                .unwrap()
                .rows
                .len(),
            1
        );
    }
}

#[test]
fn unknown_top_level_keys_and_non_object_metadata_fail_closed() {
    for input in [
        json!({"result": [], "jobStatus": "done"}),
        json!({"result": [], "warning": "none"}),
        json!({"result": [], "metadata": null}),
        json!({"result": [], "metaData": []}),
        json!({"result": [], "isError": false}),
    ] {
        assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_SHAPE");
    }
}

#[test]
fn result_must_be_an_array_of_objects() {
    for input in [
        json!({}),
        json!({"result": null}),
        json!({"result": {}}),
        json!({"result": "[]"}),
        json!({"result": [null]}),
        json!({"result": [1]}),
        json!({"result": [[]]}),
    ] {
        assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_SHAPE");
    }
}

#[test]
fn empty_result_is_valid_without_a_count_or_trace_id() {
    let normalized = mcp::normalize(br#"{"result":[]}"#).unwrap();
    assert!(normalized.rows.is_empty());
    assert_eq!(normalized.trace_id, None);
}

#[test]
fn trace_id_must_be_a_string_when_present() {
    for trace_id in [Value::Null, json!(1), json!(false), json!([]), json!({})] {
        let input = json!({"result": [], "trace_id": trace_id});
        assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_SHAPE");
    }
    let normalized = mcp::normalize(br#"{"result":[],"trace_id":""}"#).unwrap();
    assert_eq!(normalized.trace_id.as_deref(), Some(""));
}

#[test]
fn oversized_input_is_rejected_before_json_parsing() {
    let oversized = vec![b' '; mcp::MAX_RESULT_BYTES + 1];
    assert_code(&oversized, "MCP_TOO_LARGE");

    let mut at_limit = br#"{"result":[]}"#.to_vec();
    at_limit.resize(mcp::MAX_RESULT_BYTES, b' ');
    assert!(mcp::normalize(&at_limit).unwrap().rows.is_empty());
}

#[test]
fn invalid_utf8_json_and_non_object_roots_are_shape_errors() {
    for raw in [
        b"".as_slice(),
        b"not JSON".as_slice(),
        b"{}".as_slice(),
        b"[]".as_slice(),
        b"null".as_slice(),
        b"4".as_slice(),
        b"\xff".as_slice(),
    ] {
        assert_code(raw, "MCP_SHAPE");
    }
}

#[test]
fn raw_sha256_is_stable_over_exact_input_bytes() {
    let first = mcp::normalize(BARE).unwrap();
    let second = mcp::normalize(BARE).unwrap();
    assert_eq!(first, second);
    assert_eq!(first.raw_sha256, sha256_hex(BARE));

    let with_whitespace = [BARE, b" "].concat();
    let changed = mcp::normalize(&with_whitespace).unwrap();
    assert_eq!(changed.rows, first.rows);
    assert_eq!(changed.trace_id, first.trace_id);
    assert_eq!(changed.raw_sha256, sha256_hex(&with_whitespace));
    assert_ne!(changed.raw_sha256, first.raw_sha256);
}

#[test]
fn duplicate_top_level_keys_fail_closed() {
    assert_code(
        br#"{"result":[],"truncated":true,"truncated":false}"#,
        "MCP_SHAPE",
    );
    assert_code(br#"{"result":[],"result":[]}"#, "MCP_SHAPE");
    assert_code(
        br#"{"result":[],"truncated":false,"trunca\u0074ed":true}"#,
        "MCP_SHAPE",
    );
}

#[test]
fn duplicate_envelope_keys_fail_closed() {
    assert_code(
        br#"{"content":[{"type":"text","text":"{\"result\":[]}"}],"isError":true,"isError":false}"#,
        "MCP_SHAPE",
    );
    assert_code(
        br#"{"content":[{"type":"text","text":"{\"result\":[]}"}],"isError":false,"isError":true}"#,
        "MCP_SHAPE",
    );
}

#[test]
fn duplicate_content_item_keys_fail_closed() {
    assert_code(
        br#"{"content":[{"type":"image","type":"text","text":"{\"result\":[]}"}],"isError":false}"#,
        "MCP_SHAPE",
    );
}

#[test]
fn duplicate_keys_inside_envelope_text_fail_closed() {
    assert_code(
        &envelope(r#"{"result":[],"truncated":true,"truncated":false}"#, false),
        "MCP_SHAPE",
    );
    assert_code(
        &envelope(r#"{"result":[{"user":"first","user":"last"}]}"#, false),
        "MCP_SHAPE",
    );
    assert_code(
        &envelope(r#"{"error":"first","error":"last"}"#, true),
        "MCP_SHAPE",
    );
    assert_code(&envelope(r#"{"error":"failed"}"#, true), "MCP_TOOL_ERROR");
}

#[test]
fn duplicate_metadata_keys_fail_closed() {
    for metadata_key in ["metaData", "metadata"] {
        let input =
            format!(r#"{{"result":[],"{metadata_key}":{{"warnings":["skipped"],"warnings":[]}}}}"#);
        assert_code(input.as_bytes(), "MCP_SHAPE");
    }
}

#[test]
fn duplicate_row_keys_fail_closed() {
    assert_code(
        br#"{"result":[{"user":"first","user":"last"}]}"#,
        "MCP_SHAPE",
    );
    assert_code(
        br#"{"result":[{"user":{"role":"first","role":"last"}}]}"#,
        "MCP_SHAPE",
    );
    assert_code(
        br#"{"result":[{"user":[{"role":1,"role":2}]}]}"#,
        "MCP_SHAPE",
    );
}

#[test]
fn duplicate_keys_inside_error_arrays_fail_closed() {
    assert_code(
        br#"{"result":[],"errors":[{"message":"first","message":"last"}]}"#,
        "MCP_SHAPE",
    );
}

#[test]
fn same_key_in_distinct_rows_is_not_a_duplicate() {
    let normalized = mcp::normalize(
        br#"{"result":[{"user":"first","_count":"1"},{"user":"second","_count":"2"}]}"#,
    )
    .unwrap();
    assert_eq!(normalized.rows.len(), 2);
    assert_eq!(normalized.rows[0]["user"], "first");
    assert_eq!(normalized.rows[1]["user"], "second");
    assert_eq!(normalized.rows[0]["_count"], "1");
    assert_eq!(normalized.rows[1]["_count"], "2");
}

// GitHub Copilot CLI shows the model an MCP result as the text content followed by the identical
// structuredContent, separated by blank lines; agents that save "exactly what they received" save both.
const HOST_RENDERED: &[u8] = b"{\"result\":[{\"_count\":\"24\"}],\"trace_id\":\"t-1\"}\r\n\r\n\r\n{\"result\":[{\"_count\":\"24\"}],\"trace_id\":\"t-1\"}\r\n";

#[test]
fn identical_host_rendered_repeats_normalize_as_one_result() {
    let normalized = mcp::normalize(HOST_RENDERED).unwrap();
    assert_eq!(normalized.rows.len(), 1);
    assert_eq!(normalized.rows[0]["_count"], json!("24"));
    assert_eq!(normalized.trace_id.as_deref(), Some("t-1"));
    assert_eq!(normalized.raw_sha256, sha256_hex(HOST_RENDERED));

    let three = b"{\"result\":[],\"trace_id\":\"t\"}\n{\"result\":[],\"trace_id\":\"t\"}\n{\"result\":[],\"trace_id\":\"t\"}";
    assert!(mcp::normalize(three).unwrap().rows.is_empty());
}

#[test]
fn differing_or_invalid_repeats_fail_closed() {
    for raw in [
        &b"{\"result\":[{\"_count\":\"24\"}],\"trace_id\":\"t\"}\n\n{\"result\":[{\"_count\":\"25\"}],\"trace_id\":\"t\"}"[..],
        &b"{\"result\":[],\"trace_id\":\"t\"}\n\n{\"result\":[],\"trace_id\":\"t\",\"truncated\":true}"[..],
        &b"{\"result\":[],\"trace_id\":\"t\"}\n\n{\"result\":[],\"result\":[],\"trace_id\":\"t\"}"[..],
        &b"{\"result\":[],\"trace_id\":\"t\"}\n\nnot json"[..],
        &b"{\"result\":[],\"trace_id\":\"t\"}\n\n[]"[..],
    ] {
        assert_code(raw, "MCP_SHAPE");
    }
}

#[test]
fn envelope_structured_content_must_match_its_text() {
    let text = r#"{"result":[{"_count":"24"}],"trace_id":"t"}"#;
    let matching = serde_json::to_vec(&json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": {"result": [{"_count": "24"}], "trace_id": "t"},
        "isError": false
    }))
    .unwrap();
    assert_eq!(mcp::normalize(&matching).unwrap().rows.len(), 1);

    let mismatched = serde_json::to_vec(&json!({
        "content": [{"type": "text", "text": text}],
        "structuredContent": {"result": [{"_count": "99"}], "trace_id": "t"},
        "isError": false
    }))
    .unwrap();
    assert_code(&mismatched, "MCP_SHAPE");

    let extra = serde_json::to_vec(&json!({
        "content": [{"type": "text", "text": text}],
        "contents": [],
        "isError": false
    }))
    .unwrap();
    assert_code(&extra, "MCP_SHAPE");
}

#[test]
fn live_metadata_uses_completeness_flag_truthiness() {
    for key in ["metaData", "metadata"] {
        for value in [
            json!("true"),
            json!("1"),
            json!(1),
            json!(-1),
            json!(["live"]),
            json!({"live":true}),
        ] {
            let mut input = json!({"result":[{"events":7}]});
            input[key] = json!({"filterQuery":{"isLive":value}});
            assert_code(&serde_json::to_vec(&input).unwrap(), "MCP_INCOMPLETE");
            let assessed = assess_saved(input);
            assert_eq!(
                assessed.outcome,
                AssessOutcome::Inconclusive,
                "{:?}",
                assessed.findings
            );
            assert!(
                assessed
                    .findings
                    .iter()
                    .any(|d| d.code == "RESULT_INCOMPLETE")
            );
        }
        for value in [
            Value::Null,
            json!(false),
            json!(0),
            json!(""),
            json!([]),
            json!({}),
        ] {
            let mut input = json!({"result":[{"events":7}]});
            input[key] = json!({"filterQuery":{"isLive":value}});
            assert_eq!(
                mcp::normalize(&serde_json::to_vec(&input).unwrap())
                    .unwrap()
                    .rows
                    .len(),
                1
            );
        }
    }
}
