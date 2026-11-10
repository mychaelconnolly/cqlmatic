use cqlmatic::{
    contracts::{Cloud, EnvironmentId, MAX_REQUEST_BYTES},
    environment::{Pack, PackData},
    sha256_hex, storage,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

struct TestDir(PathBuf);
impl TestDir {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".local")
            .join(format!(
                "assess-cli-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str, content: impl AsRef<[u8]>) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, content).unwrap();
        path
    }
}
impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn binary() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    command
        .env_remove("FALCON_CLIENT_ID")
        .env_remove("FALCON_CLIENT_SECRET");
    command
}
fn run(mut command: Command, input: &[u8]) -> Output {
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    child.wait_with_output().unwrap()
}
fn result(out: &Output, code: i32) -> Value {
    assert_eq!(
        out.status.code(),
        Some(code),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.stderr.is_empty());
    serde_json::from_slice(&out.stdout).unwrap()
}
fn finding(row: &Value, code: &str, severity: &str) {
    assert!(
        row["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == code && f["severity"] == severity),
        "missing {code} ({severity}): {row}"
    );
}
fn diagnostic(row: &Value, code: &str, severity: &str) {
    assert!(
        row["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == code && d["severity"] == severity),
        "missing {code} ({severity}): {row}"
    );
}
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("examples")
        .join("mcp")
        .join(name)
}
fn mixed_pack(dir: &TestDir) -> PathBuf {
    let pack = Pack::new(
        EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "search-all".into(),
        },
        "2026-01-01T00:00:00Z".into(),
        PackData {
            sources: vec![json!({
                "id": "env:repo:fw_repo",
                "kind": "repo",
                "name": "fw_repo",
                "inventory_status": "observed-only",
                "selectors": {"repo": "fw_repo"},
                "relationship": {"datasets": ["traffic", "event"]},
                "provenance_ids": ["prov:synthetic"]
            })],
            provenance: vec![json!({
                "id": "prov:synthetic",
                "source_type": "ngsiem-api",
                "canonical_source": "synthetic record-kind test",
                "retrieved_at": "2000-01-01T00:00:00Z",
                "redaction_status": "none",
                "coverage_status": "complete"
            })],
            ..PackData::default()
        },
    )
    .unwrap();
    let path = dir.0.join("pack.json");
    storage::Writer::acquire(&path)
        .unwrap()
        .commit(&pack)
        .unwrap();
    path
}

#[test]
fn synthetic_count_file_and_request_return_no_findings() {
    let dir = TestDir::new();
    let query = dir.file("query.cql", "count(as=events)");
    let mut command = binary();
    command
        .args(["assess", "--query-file"])
        .arg(&query)
        .arg("--result")
        .arg(fixture("count.json"))
        .arg("--json");
    let row = result(&run(command, b""), 0);
    assert_eq!(row["outcome"], "no-findings");
    assert_eq!(row["rows"], 1);
    assert_eq!(row["query_sha256"], sha256_hex(b"count(as=events)"));
    finding(&row, "RESULT_COMPLETENESS_UNATTESTED", "info");

    let request = dir.file(
        "request.json",
        json!({"schema_version":1,"id":"saved-count","query":"count()","result":{"result":[{"_count":"24"}]}})
            .to_string(),
    );
    let mut command = binary();
    command
        .args(["assess", "--request"])
        .arg(request)
        .arg("--json");
    let row = result(&run(command, b""), 0);
    assert_eq!(row["id"], "saved-count");
    assert_eq!(row["outcome"], "no-findings");
    assert_eq!(row["rows"], 1);
    finding(&row, "RESULT_COMPLETENESS_UNATTESTED", "info");
}

fn duplicate_key_requests() -> Vec<String> {
    [
        r#"{"result":[{"_count":"24"}],"partial":true,"partial":false}"#,
        r#"{"result":[{"_count":"24"}],"partial":false,"partial":false}"#,
        r#"{"result":[{"_count":"24"}],"partial":true,"\u0070artial":false}"#,
        r#"{"result":[{"_count":"24"}],"metadata":{"partial":true,"partial":false}}"#,
        r#"{"result":[{"_count":"24"}],"metaData":{"nested":[{"partial":true,"partial":false}]}}"#,
        r#"{"result":[{"_count":"0","_count":"24"}]}"#,
        r#"{"result":[{"_count":"24","nested":{"key":1,"key":2}}]}"#,
        r#"{"result":[{"_count":"24","nested":[{"key":1,"key":2}]}]}"#,
        r#"{"result":[],"result":[{"_count":"24"}]}"#,
    ]
    .iter()
    .map(|payload| {
        format!(
            r#"{{"schema_version":1,"id":"untrusted","query":"count()","result":{payload},"context":{{}}}}"#
        )
    })
    .collect()
}

#[test]
fn request_file_and_stdin_reject_duplicate_keys_before_result_assessment() {
    let dir = TestDir::new();
    for request in duplicate_key_requests() {
        let path = dir.file("duplicate.request.json", &request);
        for source in [path.as_path(), Path::new("-")] {
            let mut command = binary();
            command
                .args(["assess", "--request"])
                .arg(source)
                .arg("--json");
            let input = if source == Path::new("-") {
                request.as_bytes()
            } else {
                b""
            };
            let out = run(command, input);
            assert_eq!(out.status.code(), Some(2), "{request}");
            assert!(out.stdout.is_empty(), "{request}");
            let diagnostic: Value = serde_json::from_slice(&out.stderr).unwrap();
            assert_eq!(diagnostic["code"], "REQUEST_JSON", "{request}");
        }
    }
}

#[test]
fn jsonl_rejects_duplicate_keys_and_recovers_for_each_following_request() {
    let valid = r#"{"schema_version":1,"id":"after","query":"count()","result":{"result":[{"_count":24}]}}"#;
    let requests = duplicate_key_requests();
    let input = requests
        .iter()
        .map(|request| format!("{request}\n{valid}\n"))
        .collect::<String>();
    let mut command = binary();
    command.args(["assess", "--jsonl"]);
    let out = run(command, input.as_bytes());
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stderr.is_empty());
    let rows = out
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), requests.len() * 2);
    for (index, pair) in rows.as_chunks::<2>().0.iter().enumerate() {
        assert_eq!(pair[0]["outcome"], "error", "{}", requests[index]);
        assert!(pair[0]["id"].is_null());
        finding(&pair[0], "REQUEST_JSON", "error");
        assert_eq!(
            pair[0]["findings"][0]["pointer"],
            format!("/line/{}", index * 2 + 1)
        );
        assert_eq!(pair[1]["outcome"], "no-findings");
        assert_eq!(pair[1]["id"], "after");
    }
}

#[test]
fn request_framing_rejects_repeated_documents_in_files_stdin_and_jsonl() {
    let dir = TestDir::new();
    let request = r#"{"schema_version":1,"query":"count()","result":{"result":[{"_count":24}]}}"#;
    let repeated = format!("{request} {request}");
    let path = dir.file("repeated.request.json", &repeated);
    for source in [path.as_path(), Path::new("-")] {
        let mut command = binary();
        command
            .args(["assess", "--request"])
            .arg(source)
            .arg("--json");
        let input = if source == Path::new("-") {
            repeated.as_bytes()
        } else {
            b""
        };
        let out = run(command, input);
        assert_eq!(out.status.code(), Some(2));
        assert!(out.stdout.is_empty());
        let diagnostic: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert_eq!(diagnostic["code"], "REQUEST_JSON");
    }
    let mut command = binary();
    command.args(["assess", "--jsonl"]);
    let out = run(command, format!("{repeated}\n{request}\n").as_bytes());
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stderr.is_empty());
    let rows = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 2);
    finding(&rows[0], "REQUEST_JSON", "error");
    assert_eq!(rows[1]["outcome"], "no-findings");
}

#[test]
fn request_allows_reused_keys_in_separate_objects_and_key_text_in_strings() {
    let request = r#"{"schema_version":1,"query":"count()","result":{"result":[{"_count":24,"nested":[{"key":1},{"key":2}],"text":"\"key\":1,\"key\":2"}]}}"#;
    for args in [
        vec!["assess", "--request", "-", "--json"],
        vec!["assess", "--jsonl"],
    ] {
        let mut command = binary();
        command.args(args);
        let row = result(&run(command, request.as_bytes()), 0);
        assert_eq!(row["outcome"], "no-findings");
        assert_eq!(row["rows"], 1);
    }
}

#[test]
fn empty_and_non_json_saved_results_have_structured_outcomes() {
    let dir = TestDir::new();
    let query = dir.file("query.cql", "count()");
    let empty = dir.file("empty.json", r#"{"result":[]}"#);
    let mut command = binary();
    command
        .args(["assess", "--query-file"])
        .arg(&query)
        .arg("--result")
        .arg(empty)
        .arg("--json");
    let row = result(&run(command, b""), 1);
    assert_eq!(row["outcome"], "inconclusive");
    assert_eq!(row["rows"], 0);
    finding(&row, "RESULT_EMPTY", "warning");

    let mut command = binary();
    command
        .args(["assess", "--query-file"])
        .arg(&query)
        .arg("--result")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
        .arg("--json");
    let row = result(&run(command, b""), 2);
    assert_eq!(row["outcome"], "error");
    assert_eq!(row["rows"], 0);
    assert_eq!(row["query_sha256"], sha256_hex(b"count()"));
    finding(&row, "RESULT_JSON", "error");
}

#[test]
fn host_rendered_repeat_of_one_result_is_assessed_not_rejected() {
    // GitHub Copilot CLI shows an MCP result as text content followed by identical structuredContent.
    let dir = TestDir::new();
    let query = dir.file("query.cql", "count()");
    let one = r#"{"result":[{"_count":"24"}],"trace_id":"t"}"#;
    let repeated = dir.file("repeated.json", format!("{one}\r\n\r\n\r\n{one}\r\n"));
    let mut command = binary();
    command
        .args(["assess", "--query-file"])
        .arg(&query)
        .arg("--result")
        .arg(repeated)
        .arg("--json");
    let row = result(&run(command, b""), 0);
    assert_eq!(row["outcome"], "no-findings");
    assert_eq!(row["rows"], 1);

    let differing = dir.file(
        "differing.json",
        format!("{one}\n\n{}", one.replace("24", "25")),
    );
    let mut command = binary();
    command
        .args(["assess", "--query-file"])
        .arg(&query)
        .arg("--result")
        .arg(differing)
        .arg("--json");
    let row = result(&run(command, b""), 2);
    assert_eq!(row["outcome"], "error");
}

#[test]
fn jsonl_orders_rows_echoes_ids_and_returns_highest_exit() {
    let input = [
        json!({"schema_version":1,"id":"ok","query":"count()","result":{"result":[{"_count":3}]}}),
        json!({"schema_version":1,"id":"empty","query":"count()","result":{"result":[]}}),
        json!({"schema_version":2,"id":"malformed","query":"count()","result":{"result":[{"_count":3}]}}),
    ]
    .iter()
    .map(Value::to_string)
    .collect::<Vec<_>>()
    .join("\n");
    let mut command = binary();
    command.args(["assess", "--jsonl"]);
    let out = run(command, input.as_bytes());
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stderr.is_empty());
    let rows = out
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["id"], "ok");
    assert_eq!(rows[0]["outcome"], "no-findings");
    finding(&rows[0], "RESULT_COMPLETENESS_UNATTESTED", "info");
    assert_eq!(rows[1]["id"], "empty");
    assert_eq!(rows[1]["outcome"], "inconclusive");
    finding(&rows[1], "RESULT_EMPTY", "warning");
    assert_eq!(rows[2]["id"], "malformed");
    assert_eq!(rows[2]["outcome"], "error");
    finding(&rows[2], "REQUEST_VERSION", "error");
}

#[test]
fn jsonl_recovers_after_malformed_and_oversized_records() {
    let valid = json!({"schema_version":1,"id":"after","query":"count()","result":{"result":[{"_count":1}]}});
    let mut input = b"{broken\n".to_vec();
    input.extend(vec![b'x'; MAX_REQUEST_BYTES + 1]);
    input.extend(format!("\n{valid}\n").as_bytes());
    let mut command = binary();
    command.args(["assess", "--jsonl"]);
    let out = run(command, &input);
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stderr.is_empty());
    let rows = out
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 3);
    assert!(rows[0]["id"].is_null());
    assert_eq!(rows[0]["outcome"], "error");
    finding(&rows[0], "REQUEST_JSON", "error");
    assert_eq!(rows[0]["findings"][0]["pointer"], "/line/1");
    assert_eq!(rows[1]["outcome"], "error");
    finding(&rows[1], "CLI_INPUT", "error");
    assert_eq!(rows[1]["findings"][0]["pointer"], "/line/2");
    assert_eq!(rows[2]["id"], "after");
    assert_eq!(rows[2]["outcome"], "no-findings");
}

#[test]
fn assess_schema_matches_bundled_contract() {
    let mut command = binary();
    command.args(["--schema", "assess"]);
    let row = result(&run(command, b""), 0);
    let bundled: Value = serde_json::from_slice(
        &fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("schemas")
                .join("assess.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(row, bundled);
    assert_eq!(row["title"], "AssessContract");
}

#[test]
fn mixed_record_kind_flag_changes_generate_outcome_and_warning() {
    let dir = TestDir::new();
    let pack = mixed_pack(&dir);
    let mut command = binary();
    command
        .args(["generate", "--intent", "top 10 dst", "--repo", "fw_repo"])
        .arg("--pack")
        .arg(&pack)
        .arg("--json");
    let blocked = result(&run(command, b""), 1);
    assert_eq!(blocked["outcome"], "needs-context");
    assert!(blocked["query"].is_null());
    diagnostic(&blocked, "CQL_RECORD_KIND_AMBIGUOUS", "error");

    let mut command = binary();
    command
        .args(["generate", "--intent", "top 10 dst", "--repo", "fw_repo"])
        .arg("--pack")
        .arg(&pack)
        .args(["--accept-mixed-record-kinds", "--json"]);
    let accepted = result(&run(command, b""), 0);
    assert_eq!(accepted["outcome"], "ready");
    assert!(accepted["query"].as_str().is_some());
    diagnostic(&accepted, "CQL_RECORD_KIND_AMBIGUOUS", "warning");
}

#[test]
fn mixed_record_kind_flag_applies_to_check_and_request_modes() {
    let dir = TestDir::new();
    let pack = mixed_pack(&dir);
    let query = dir.file("query.cql", "#repo=fw_repo | top(dst)");
    let request = dir.file(
        "request.json",
        json!({"schema_version":1,"id":"check-request","query":"#repo=fw_repo | top(dst)"})
            .to_string(),
    );
    for mode in [query, request] {
        let flag = if mode.ends_with("query.cql") {
            "--file"
        } else {
            "--request"
        };
        let mut command = binary();
        command
            .args(["check", flag])
            .arg(mode)
            .arg("--pack")
            .arg(&pack)
            .args(["--accept-mixed-record-kinds", "--json"]);
        let row = result(&run(command, b""), 0);
        assert_eq!(row["outcome"], "ready");
        diagnostic(&row, "CQL_RECORD_KIND_AMBIGUOUS", "warning");
    }
}

#[test]
fn human_assessment_reports_findings_and_summary_without_row_values() {
    let dir = TestDir::new();
    let query = dir.file("query.cql", "select([user])");
    let saved = dir.file(
        "result.json",
        r#"{"result":[{"user":"private-row-token"}]}"#,
    );
    let mut command = binary();
    command
        .args(["assess", "--query-file"])
        .arg(query)
        .arg("--result")
        .arg(saved);
    let out = run(command, b"");
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stderr.is_empty());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("private-row-token"));
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        [
            "RESULT_COMPLETENESS_UNATTESTED (info): The MCP result carries no completeness attestation; these checks cannot certify the answer.",
            "no-findings: 1 row(s), 1 finding(s)"
        ]
    );
}

#[test]
fn assess_rejects_conflicting_or_incomplete_input_modes() {
    for args in [
        vec!["assess", "--query-file", "query.cql", "--json"],
        vec!["assess", "--jsonl", "--request", "request.json"],
        vec!["assess", "--fail-on-warning"],
    ] {
        let mut command = binary();
        command.args(&args);
        let out = run(command, b"");
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert!(out.stdout.is_empty(), "{args:?}");
        if args.contains(&"--fail-on-warning") {
            assert!(String::from_utf8_lossy(&out.stderr).contains("unexpected argument"));
        } else {
            let diagnostic: Value = serde_json::from_slice(&out.stderr).unwrap();
            assert_eq!(diagnostic["code"], "CLI_INPUT");
        }
    }
}

#[test]
fn synthetic_mcp_replay_generates_observed_query_and_assesses_saved_answer() {
    let dir = TestDir::new();
    let state = dir.0.join("state.json");
    let pack = dir.0.join("pack.json");
    let receipt = dir.0.join("receipt.json");
    let mut command = binary();
    command
        .args(["env", "discover", "--mcp", "--request"])
        .arg(fixture("discover.request.json"))
        .arg("--state")
        .arg(&state)
        .arg("--pack")
        .arg(&pack)
        .arg("--receipt")
        .arg(&receipt);
    let mut step = result(&run(command, b""), 3);
    let mut activated = false;
    for _ in 0..4 {
        assert_eq!(step["status"], "needs-probes");
        let probes = step["probes"].as_array().unwrap();
        assert!(!probes.is_empty());
        let mut command = binary();
        command
            .args(["env", "discover", "--mcp", "--state"])
            .arg(&state);
        for probe in probes {
            // Same probe-to-answer mapping as tests/mcp_discovery.rs fixture().
            let field = probe["field"].as_str();
            let file = match probe["kind"].as_str().unwrap() {
                "census" => "census.json",
                "volume" => "count.json",
                "tag-detect" if field == Some("event_simpleName") => "count.json",
                "tag-detect" => "zero.json",
                "dataset-facet" => "dataset.json",
                "fieldset" => "fieldset.json",
                "population" => "population.json",
                "value-facet" => "value.json",
                "record-kind-facet" if field == Some("#type") => "kind.json",
                "record-kind-facet" if field == Some("#event_simpleName") => "simple-name.json",
                other => panic!("unexpected probe kind: {other} {field:?}"),
            };
            command.arg("--answer").arg(format!(
                "{}={}",
                probe["probe_id"].as_str().unwrap(),
                fixture(file).display()
            ));
        }
        let out = run(command, b"");
        if out.status.code() == Some(0) {
            step = result(&out, 0);
            activated = true;
            break;
        }
        step = result(&out, 3);
    }
    assert!(activated, "{step}");
    assert_eq!(step["status"], "complete");
    assert!(pack.exists());
    assert!(receipt.exists());

    // The synthetic source holds two #type record kinds: unconstrained generation needs context ...
    let intent =
        "show events from synthetic_repo where user equals \"synthetic-user\" select user limit 20";
    let mut command = binary();
    command
        .args(["generate", "--intent", intent])
        .arg("--pack")
        .arg(&pack)
        .args(["--require-observed", "--json"]);
    let ambiguous = result(&run(command, b""), 1);
    assert_eq!(ambiguous["outcome"], "needs-context", "{ambiguous}");
    diagnostic(&ambiguous, "CQL_RECORD_KIND_AMBIGUOUS", "error");

    // ... and explicit acceptance of mixed kinds is ready with the warning kept.
    let mut command = binary();
    command
        .args(["generate", "--intent", intent])
        .arg("--pack")
        .arg(&pack)
        .args([
            "--require-observed",
            "--accept-mixed-record-kinds",
            "--json",
        ]);
    let generated_out = run(command, b"");
    assert_eq!(
        generated_out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&generated_out.stdout)
    );
    let generated = result(&generated_out, 0);
    assert_eq!(generated["outcome"], "ready", "{generated}");
    diagnostic(&generated, "CQL_RECORD_KIND_AMBIGUOUS", "warning");
    let query = dir.file("generated.cql", generated["query"].as_str().unwrap());
    let mut command = binary();
    command
        .args(["assess", "--query-file"])
        .arg(query)
        .arg("--result")
        .arg(fixture("value.json"))
        .arg("--pack")
        .arg(pack)
        .arg("--json");
    let assessed_out = run(command, b"");
    assert_eq!(
        assessed_out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&assessed_out.stdout)
    );
    let assessed = result(&assessed_out, 1);
    assert_eq!(assessed["outcome"], "advisory", "{assessed}");
    assert_eq!(assessed["rows"], 1);
    finding(&assessed, "RESULT_COMPLETENESS_UNATTESTED", "info");
    finding(&assessed, "RESULT_KEY_COVERAGE", "warning");
}

#[test]
fn full_env_inspect_filters_synthetic_pack_and_preserves_metadata() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("legacy-core-v1-pack.json");
    let original: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    for (source, field, expected) in [
        ("example", "user", true),
        ("other-repo", "user", false),
        ("example", "missing", false),
    ] {
        let mut command = binary();
        command
            .args(["env", "inspect"])
            .arg(&path)
            .args(["--full", "--source", source, "--field", field]);
        let inspected = result(&run(command, b""), 0);
        for key in [
            "format_version",
            "environment",
            "producer_version",
            "profile_id",
            "knowledge_revision",
            "created_at",
            "content_digest",
        ] {
            assert_eq!(inspected[key], original[key], "{source}: {field}: {key}");
        }
        for key in ["sources", "fields", "provenance"] {
            assert_eq!(
                inspected["data"][key].as_array().unwrap().len(),
                usize::from(expected),
                "{source}: {field}: {key}"
            );
        }
        assert_eq!(inspected["data"]["values"], json!([]));
        assert_eq!(inspected["data"]["annotations"], json!([]));
        if expected {
            assert_eq!(inspected["data"]["fields"][0]["path"], "user");
            assert_eq!(
                inspected["data"]["sources"][0]["selectors"]["repo"],
                "example"
            );
            assert_eq!(inspected["data"]["provenance"][0]["id"], "prov:historical");
        }
    }

    let mut command = binary();
    command
        .args(["env", "inspect"])
        .arg(&path)
        .args(["--full", "--source", "absent"]);
    let missing = result(&run(command, b""), 0);
    for key in ["sources", "fields", "values", "provenance", "annotations"] {
        assert_eq!(missing["data"][key], json!([]), "{key}");
    }
}

#[test]
fn env_inspect_summary_is_opt_in_and_default_json_stays_detailed() {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legacy-core-v1-pack.json");
    let mut command = binary();
    command.args(["env", "inspect"]).arg(&path);
    let default = result(&run(command, b""), 0);
    assert!(default.get("summary").is_none());
    assert!(default["sources"].is_array());
    assert!(default["annotations"].is_array());

    let mut command = binary();
    command.args(["env", "inspect"]).arg(&path).arg("--summary");
    let summary = result(&run(command, b""), 0);
    assert_eq!(summary["summary"], true);
    assert_eq!(summary["pack_id"], default["pack_id"]);
    assert_eq!(
        summary["counts"]["sources"],
        default["sources"].as_array().unwrap().len()
    );
    assert!(summary.get("annotations").is_none());
    assert!(summary["sources"][0].get("field_count").is_some());
}
