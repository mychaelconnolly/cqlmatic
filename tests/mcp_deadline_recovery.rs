use cqlmatic::{
    discovery::{self, DiscoverRequest},
    environment::{Pack, PackData},
    mcp::{self, StepOutput},
    sha256_hex, storage,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const EXPIRED_CREATION: &str = "2020-01-01T00:00:00Z";
const COUNT: &[u8] = include_bytes!("../examples/mcp/count.json");
const PARTIAL: &[u8] = br#"{"result":[],"partial":true}"#;

enum Timing {
    Timely,
    Expired,
}

struct Run {
    directory: PathBuf,
    state: PathBuf,
    pack: PathBuf,
    receipt: PathBuf,
    probe_id: String,
}

impl Run {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "cqlmatic-mcp-deadline-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        let state = directory.join("state.json");
        let pack = directory.join("pack.json");
        let receipt = directory.join("receipt.json");
        let request: DiscoverRequest =
            serde_json::from_str(include_str!("../examples/mcp/discover.request.json")).unwrap();
        let initial = mcp::init(request, &state, &pack, &receipt).unwrap();
        assert_eq!(initial.status, "needs-probes");
        Self {
            directory,
            state,
            pack,
            receipt,
            probe_id: initial.probes[0]["probe_id"].as_str().unwrap().into(),
        }
    }

    fn saved(&self) -> Value {
        serde_json::from_slice(&fs::read(&self.state).unwrap()).unwrap()
    }

    fn save(&self, state: &Value) {
        fs::write(&self.state, serde_json::to_vec(state).unwrap()).unwrap();
    }

    fn seed_pack(&self) -> Vec<u8> {
        let environment =
            serde_json::from_value(self.saved()["request"]["environment"].clone()).unwrap();
        let pack = Pack::new(environment, discovery::timestamp(), PackData::default()).unwrap();
        storage::Writer::acquire(&self.pack)
            .unwrap()
            .commit(&pack)
            .unwrap();
        fs::read(&self.pack).unwrap()
    }

    fn ingest(&self, raw: &[u8], timing: Timing) -> StepOutput {
        if matches!(timing, Timing::Expired) {
            let mut state = self.saved();
            state["created_at"] = json!(EXPIRED_CREATION);
            self.save(&state);
        }
        let path = self.directory.join("answer.json");
        fs::write(&path, raw).unwrap();
        mcp::step(&self.state, &[(self.probe_id.clone(), path)]).unwrap()
    }

    fn assert_failed(&self, output: &StepOutput, code: &str) {
        assert_eq!(output.status, "failed", "{:?}", output.diagnostics);
        assert_eq!(output.exit_code(), 2);
        assert_eq!(output.diagnostics[0].code, code);
        assert!(!output.active_pack_updated);
        assert!(output.probes.is_empty());
        assert!(!self.receipt.exists());
    }

    fn assert_capture(&self, raw: &[u8]) {
        let state = self.saved();
        let answer = &state["answers"][&self.probe_id];
        let expected_hex = raw
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(answer["raw_hex"], expected_hex);
        assert_eq!(answer["raw_sha256"], sha256_hex(raw));
        assert_eq!(answer["outcome"], "failed");
        assert!(answer.get("trace_id").is_none());
        assert!(answer.get("rows").is_none());
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn expired_answers_reopen_with_original_deadline() {
    let row_limit = json!({"result": vec![json!({}); 10_000]}).to_string();
    for raw in [
        COUNT,
        PARTIAL,
        b"{",
        b"\xff",
        br#"{"result":[],"error":"synthetic tool error"}"#,
        br#"{"result":[],"error":"deadline exceeded"}"#,
        row_limit.as_bytes(),
    ] {
        let run = Run::new();
        let before = run.seed_pack();
        let failed = run.ingest(raw, Timing::Expired);
        run.assert_failed(&failed, "MCP_DEADLINE");
        run.assert_capture(raw);
        let saved = fs::read(&run.state).unwrap();
        fs::remove_file(run.directory.join("answer.json")).unwrap();
        for _ in 0..2 {
            let reopened = mcp::step(&run.state, &[]).unwrap();
            run.assert_failed(&reopened, "MCP_DEADLINE");
            assert_eq!(
                serde_json::to_value(&reopened.diagnostics).unwrap(),
                serde_json::to_value(&failed.diagnostics).unwrap()
            );
            assert_eq!(fs::read(&run.state).unwrap(), saved);
            assert_eq!(fs::read(&run.pack).unwrap(), before);
        }
    }
}

#[test]
fn timely_failed_answers_reopen_without_promoting_evidence() {
    let row_limit = json!({"result": vec![json!({}); 10_000]}).to_string();
    for (raw, expected) in [
        (PARTIAL, "MCP_INCOMPLETE"),
        (b"{".as_slice(), "MCP_SHAPE"),
        (b"\xff".as_slice(), "MCP_SHAPE"),
        (
            br#"{"result":[],"error":"deadline exceeded"}"#.as_slice(),
            "MCP_DEADLINE",
        ),
        (row_limit.as_bytes(), "SEARCH_ROW_LIMIT"),
    ] {
        let run = Run::new();
        let failed = run.ingest(raw, Timing::Timely);
        run.assert_failed(&failed, expected);
        run.assert_capture(raw);
        let reopened = mcp::step(&run.state, &[]).unwrap();
        run.assert_failed(&reopened, expected);
        assert_eq!(
            serde_json::to_value(&reopened.diagnostics).unwrap(),
            serde_json::to_value(&failed.diagnostics).unwrap()
        );
        assert!(!run.pack.exists());
    }
}

#[test]
fn interrupted_deadline_failure_checkpoint_reopens_without_answer_file() {
    let run = Run::new();
    let failed = run.ingest(PARTIAL, Timing::Expired);
    run.assert_failed(&failed, "MCP_DEADLINE");
    let mut checkpoint = run.saved();
    // Ingestion persists the answer before it persists the terminal failure.
    checkpoint["status"] = json!("needs-probes");
    checkpoint.as_object_mut().unwrap().remove("failure");
    run.save(&checkpoint);
    fs::remove_file(run.directory.join("answer.json")).unwrap();
    let reopened = mcp::step(&run.state, &[]).unwrap();
    run.assert_failed(&reopened, "MCP_DEADLINE");
    run.assert_capture(PARTIAL);
    assert_eq!(run.saved()["status"], "failed");
    assert!(!run.pack.exists());
}

#[test]
fn deadline_state_rejects_changed_capture_or_classification() {
    for edit in [
        "raw",
        "hash",
        "invalid-hex",
        "odd-hex",
        "diagnostic",
        "trace",
        "outcome",
        "timely",
        "before-creation",
    ] {
        let run = Run::new();
        let before = run.seed_pack();
        run.ingest(PARTIAL, Timing::Expired);
        let mut state = run.saved();
        let answer = &mut state["answers"][&run.probe_id];
        match edit {
            "raw" => answer["raw_hex"] = json!("00"),
            "hash" => answer["raw_sha256"] = json!("0".repeat(64)),
            "invalid-hex" => answer["raw_hex"] = json!("zz"),
            "odd-hex" => answer["raw_hex"] = json!("0"),
            "diagnostic" => answer["diagnostic"]["message"] = json!("substituted"),
            "trace" => answer["trace_id"] = json!("substituted"),
            "outcome" => answer["outcome"] = json!("complete"),
            "timely" => answer["ingested_at"] = json!(EXPIRED_CREATION),
            "before-creation" => answer["ingested_at"] = json!("2019-12-31T23:59:59Z"),
            _ => unreachable!(),
        }
        run.save(&state);
        let changed = fs::read(&run.state).unwrap();
        assert_eq!(
            mcp::step(&run.state, &[]).unwrap_err().code,
            "MCP_STATE_TAMPERED",
            "{edit}"
        );
        assert_eq!(fs::read(&run.state).unwrap(), changed);
        assert_eq!(fs::read(&run.pack).unwrap(), before);
        assert!(!run.receipt.exists());
    }
}

#[test]
fn oversized_capture_boundary_reopens_with_fail_closed_diagnostic() {
    let raw = vec![b' '; mcp::MAX_RESULT_BYTES + 1];
    assert_eq!(mcp::normalize(&raw).unwrap_err().code, "MCP_TOO_LARGE");
    for (timing, expected) in [
        (Timing::Timely, "MCP_TOO_LARGE"),
        (Timing::Expired, "MCP_DEADLINE"),
    ] {
        let run = Run::new();
        let failed = run.ingest(&raw, timing);
        run.assert_failed(&failed, expected);
        run.assert_capture(&raw);
        let reopened = mcp::step(&run.state, &[]).unwrap();
        run.assert_failed(&reopened, expected);
        assert!(!run.pack.exists());
    }
}

#[test]
fn persisted_capture_beyond_ingestion_boundary_is_rejected() {
    let run = Run::new();
    let mut raw = vec![b' '; mcp::MAX_RESULT_BYTES + 1];
    run.ingest(&raw, Timing::Expired);
    raw.push(b' ');
    let mut state = run.saved();
    let answer = &mut state["answers"][&run.probe_id];
    let hex = answer["raw_hex"].as_str().unwrap().to_string() + "20";
    answer["raw_hex"] = json!(hex);
    answer["raw_sha256"] = json!(sha256_hex(&raw));
    run.save(&state);
    assert_eq!(
        mcp::step(&run.state, &[]).unwrap_err().code,
        "MCP_STATE_TAMPERED"
    );
    assert!(!run.pack.exists());
    assert!(!run.receipt.exists());
}
