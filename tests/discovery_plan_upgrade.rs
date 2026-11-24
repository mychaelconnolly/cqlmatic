use cqlmatic::{
    discovery::DiscoverRequest,
    mcp::{self, Transport},
    network::SearchBackend,
    sha256_hex,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

const EARLIER_PLAN: &str = "bounded-discovery-v2";
const PLAN: &str = "bounded-discovery-v3";
const EARLIER_PLAN_REFUSAL: &str = "Discovery checkpoint was prepared under an earlier discovery plan; this build can inspect it but not continue it. Preserve the inventory and checkpoint files.";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-plan-upgrade-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn environment() -> Value {
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"})
}
fn state(dir: &Path) -> PathBuf {
    dir.join("workflow").join("state.json")
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
fn inventory(dir: &Path, action: Value) -> Output {
    let request = dir.join("request.json");
    write(&request, &json!({"schema_version":1,"action":action}));
    Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .env("HOME", dir)
        .args(["env", "inventory", "--json", "--state"])
        .arg(state(dir))
        .arg("--request")
        .arg(request)
        .output()
        .unwrap()
}
fn call(dir: &Path, action: Value) -> Value {
    let output = inventory(dir, action);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn status(dir: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .env("HOME", dir)
        .args(["status", "--json", "--state"])
        .arg(state(dir))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn fixture(dir: &Path, name: &str, value: Value) -> String {
    let path = dir.join(name);
    write(&path, &value);
    path.to_string_lossy().into_owned()
}
fn confirm(dir: &Path) {
    call(
        dir,
        json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
    );
    call(
        dir,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic discovery target","expected_sources":[{"type":"synthetic"}]}),
    );
}
fn prepare_request(max_queries: u32) -> Value {
    json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","max_queries":max_queries,"max_seconds":120})
}
fn checkpoint(dir: &Path, index: usize) -> PathBuf {
    PathBuf::from(
        read(&state(dir))["discovery_runs"][index]["state_path"]
            .as_str()
            .unwrap(),
    )
}
fn runs(dir: &Path) -> usize {
    read(&state(dir))["discovery_runs"]
        .as_array()
        .unwrap()
        .len()
}

/// The session binding as a reader recomputes it for each plan.
fn binding(checkpoint: &Value) -> String {
    let session = &checkpoint["session"];
    let mut bytes = serde_json::to_vec(&(
        &checkpoint["request_sha256"],
        &checkpoint["created_at"],
        &checkpoint["pack"],
        &checkpoint["receipt"],
        &session["transport"],
        &session["predecessor_sha256"],
    ))
    .unwrap();
    if session["sparse_fieldset_fallback"] == true {
        bytes.extend_from_slice(b"\0sparse-fieldset-fallback-v1");
    }
    if let Some(owner) = session.get("inventory_owner") {
        bytes.extend_from_slice(b"\0inventory-owner-v1\0");
        bytes.extend_from_slice(&serde_json::to_vec(owner).unwrap());
    }
    if checkpoint["discovery_algorithm"] == PLAN {
        bytes.extend_from_slice(b"\0bounded-discovery-v3");
    }
    sha256_hex(&bytes)
}

/// Rewrite a checkpoint as an earlier build wrote it: the earlier plan label and its binding.
fn as_earlier_plan(path: &Path, owned: bool) {
    let mut saved = read(path);
    assert_eq!(saved["discovery_algorithm"], PLAN);
    assert_eq!(saved["session"]["binding_sha256"], json!(binding(&saved)));
    if !owned {
        saved["session"]
            .as_object_mut()
            .unwrap()
            .remove("inventory_owner");
    }
    saved["discovery_algorithm"] = json!(EARLIER_PLAN);
    saved["session"]["binding_sha256"] = json!(binding(&saved));
    write(path, &saved);
    mcp::inspect_state(path).unwrap();
}

/// Every file an inventory action could change.
fn snapshot(dir: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let saved = read(&state(dir));
    let mut paths = vec![state(dir)];
    for run in saved["discovery_runs"].as_array().unwrap() {
        for name in ["state_path", "pack_path", "receipt_path"] {
            paths.push(PathBuf::from(run[name].as_str().unwrap()));
        }
    }
    paths
        .into_iter()
        .map(|path| {
            let bytes = fs::read(&path).ok();
            (path, bytes)
        })
        .collect()
}
fn unchanged(before: &[(PathBuf, Option<Vec<u8>>)], label: &str) {
    for (path, bytes) in before {
        assert!(
            fs::read(path).ok() == *bytes,
            "{label}: changed {}",
            path.display()
        );
    }
}
fn refused(dir: &Path, action: Value) -> Value {
    let output = inventory(dir, action.clone());
    assert_eq!(output.status.code(), Some(2), "{action}: {output:?}");
    serde_json::from_slice(&output.stderr).unwrap()
}

/// Leave the only run of a fresh inventory in the named phase.
fn run_in_phase(dir: &Path, phase: &str) -> PathBuf {
    confirm(dir);
    let prepared = call(dir, prepare_request(4));
    let probe = prepared["action"]["relay"]["probes"][0]["probe_id"].clone();
    match phase {
        "idle" => {}
        "reserved" => {
            call(dir, json!({"kind":"dispatch-discovery","probe_id":probe}));
        }
        "paused" => {
            let reserved = call(dir, json!({"kind":"next-probe"}));
            let file = fixture(
                dir,
                "unusable.json",
                json!({"result":[{"field":"shape-mismatch"}]}),
            );
            let paused = call(
                dir,
                json!({"kind":"next-probe","answer":{"probe_id":reserved["action"]["probe"]["probe_id"],"result_file":file}}),
            );
            assert_eq!(paused["action"]["state"], "paused", "{paused}");
        }
        "activated" | "failed" => {
            let events = if phase == "failed" { 0 } else { 10 };
            call(dir, json!({"kind":"dispatch-discovery","probe_id":probe}));
            let file = fixture(dir, "volume.json", json!({"result":[{"events":events}]}));
            let result = call(
                dir,
                json!({"kind":"discovery-step","answers":[{"probe_id":probe,"result_file":file}]}),
            );
            assert_eq!(
                result["action"]["relay"]["status"],
                if phase == "failed" {
                    "failed"
                } else {
                    "complete"
                },
                "{result}"
            );
        }
        other => panic!("unknown phase {other}"),
    }
    checkpoint(dir, 0)
}

#[test]
fn new_checkpoints_name_the_new_plan_and_commit_it_to_the_binding() {
    let dir = Temp::new();
    let path = run_in_phase(&dir.0, "idle");
    let saved = read(&path);
    assert_eq!(saved["discovery_algorithm"], PLAN);
    assert_eq!(saved["session"]["binding_sha256"], json!(binding(&saved)));
}

#[test]
fn earlier_plan_checkpoints_stay_readable_but_cannot_continue() {
    for phase in ["idle", "reserved", "paused"] {
        let dir = Temp::new();
        let path = run_in_phase(&dir.0, phase);
        as_earlier_plan(&path, true);
        let before = snapshot(&dir.0);
        let inspected = mcp::inspect_state(&path).unwrap();
        assert_eq!(
            mcp::require_current_producer(&inspected)
                .unwrap_err()
                .message,
            EARLIER_PLAN_REFUSAL
        );
        status(&dir.0);
        unchanged(&before, phase);
        let file = fixture(&dir.0, "answer.json", json!({"result":[{"events":10}]}));
        let probe = read(&path)["session"]["attempts"]
            .as_array()
            .unwrap()
            .first()
            .map_or(json!("p-unreserved"), |attempt| {
                json!(cqlmatic::discovery::probe_id(
                    "search-all",
                    &serde_json::from_value(attempt["probe"].clone()).unwrap()
                ))
            });
        let before = snapshot(&dir.0);
        for action in [
            json!({"kind":"next-probe","run_index":0}),
            json!({"kind":"next-probe","run_index":0,"answer":{"probe_id":probe,"result_file":file}}),
            json!({"kind":"pause-discovery","run_index":0}),
            json!({"kind":"resume-discovery","run_index":0}),
            json!({"kind":"discovery-step","run_index":0}),
            json!({"kind":"discovery-step","run_index":0,"answers":[{"probe_id":probe,"result_file":file}]}),
            json!({"kind":"dispatch-discovery","run_index":0,"probe_id":probe}),
        ] {
            let diagnostic = refused(&dir.0, action.clone());
            assert_eq!(diagnostic["code"], "MCP_STATE_PRODUCER", "{phase} {action}");
            assert!(
                diagnostic["message"]
                    .as_str()
                    .unwrap()
                    .contains(EARLIER_PLAN_REFUSAL),
                "{phase} {action}: {diagnostic}"
            );
            unchanged(&before, &format!("{phase} {action}"));
        }
    }
}

#[test]
fn activated_earlier_plan_evidence_still_resolves_open_work() {
    let dir = Temp::new();
    confirm(&dir.0);
    call(
        &dir.0,
        json!({"kind":"record-inventory-hints","environment":environment(),"hints":[{"source":{"type":"new_source"},"fields":[]}]}),
    );
    call(
        &dir.0,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic discovery target","expected_sources":[{"type":"new_source"}]}),
    );
    let path = {
        let prepared = call(&dir.0, prepare_request(4));
        let probe = prepared["action"]["relay"]["probes"][0]["probe_id"].clone();
        call(
            &dir.0,
            json!({"kind":"dispatch-discovery","probe_id":probe}),
        );
        let file = fixture(&dir.0, "volume.json", json!({"result":[{"events":10}]}));
        call(
            &dir.0,
            json!({"kind":"discovery-step","answers":[{"probe_id":probe,"result_file":file}]}),
        );
        checkpoint(&dir.0, 0)
    };
    let mut saved = read(&state(&dir.0));
    assert_eq!(
        saved["pending_inventory"][0]["resolution"]["kind"],
        "inventoried"
    );
    // Reopen the work so only the earlier-plan run can resolve it.
    saved["pending_inventory"][0]
        .as_object_mut()
        .unwrap()
        .remove("resolution");
    write(&state(&dir.0), &saved);
    as_earlier_plan(&path, true);
    let checkpoint_bytes = fs::read(&path).unwrap();
    status(&dir.0);
    call(
        &dir.0,
        json!({"kind":"reconcile","reviewed_at":"2026-01-03T00:00:00Z"}),
    );
    assert_eq!(
        read(&state(&dir.0))["pending_inventory"][0]["resolution"]["kind"],
        "inventoried"
    );
    assert_eq!(fs::read(&path).unwrap(), checkpoint_bytes);
}

#[test]
fn relabelling_an_earlier_plan_checkpoint_fails_its_binding() {
    let dir = Temp::new();
    let path = run_in_phase(&dir.0, "paused");
    as_earlier_plan(&path, true);
    let mut saved = read(&path);
    saved["discovery_algorithm"] = json!(PLAN);
    write(&path, &saved);
    assert_eq!(
        mcp::inspect_state(&path).unwrap_err().code,
        "MCP_STATE_TAMPERED"
    );
}

#[test]
fn an_earlier_producer_cannot_claim_the_new_plan() {
    let dir = Temp::new();
    let path = run_in_phase(&dir.0, "paused");
    let mut saved = read(&path);
    saved["producer_version"] = json!("0.1.12");
    write(&path, &saved);
    assert_eq!(
        mcp::inspect_state(&path).unwrap_err().code,
        "MCP_STATE_TAMPERED"
    );
    // The same history under the plan that release wrote stays readable.
    saved["discovery_algorithm"] = json!(EARLIER_PLAN);
    saved["session"]["binding_sha256"] = json!(binding(&saved));
    write(&path, &saved);
    assert_eq!(
        mcp::inspect_state(&path).unwrap().producer_version,
        "0.1.12"
    );
}

#[test]
fn preparing_beside_an_unowned_earlier_plan_run_leaves_it_untouched() {
    for phase in ["activated", "failed"] {
        let dir = Temp::new();
        let path = run_in_phase(&dir.0, phase);
        as_earlier_plan(&path, false);
        let retained = fs::read(&path).unwrap();
        status(&dir.0);
        call(&dir.0, prepare_request(5));
        assert_eq!(runs(&dir.0), 2, "{phase}");
        assert_eq!(fs::read(&path).unwrap(), retained, "{phase}");
        assert_eq!(read(&checkpoint(&dir.0, 1))["discovery_algorithm"], PLAN);
    }
}

#[test]
fn retained_run_guards_ignore_the_plan() {
    let dir = Temp::new();
    let path = run_in_phase(&dir.0, "paused");
    as_earlier_plan(&path, true);
    let before = snapshot(&dir.0);
    let diagnostic = refused(&dir.0, prepare_request(4));
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("An identical discovery request is safely paused"),
        "{diagnostic}"
    );
    unchanged(&before, "identical request");
    assert_eq!(runs(&dir.0), 1);
    let retained = fs::read(&path).unwrap();
    call(&dir.0, prepare_request(5));
    assert_eq!(runs(&dir.0), 2);
    assert_eq!(fs::read(&path).unwrap(), retained);

    for (phase, message) in [
        ("idle", "A retained discovery run is unfinished"),
        (
            "reserved",
            "A retained discovery search has unresolved termination",
        ),
    ] {
        let dir = Temp::new();
        let path = run_in_phase(&dir.0, phase);
        as_earlier_plan(&path, true);
        let before = snapshot(&dir.0);
        let diagnostic = refused(&dir.0, prepare_request(5));
        assert!(
            diagnostic["message"].as_str().unwrap().contains(message),
            "{phase}: {diagnostic}"
        );
        unchanged(&before, phase);
    }
}

struct NoSearch;
impl SearchBackend for NoSearch {
    fn start(
        &mut self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: Instant,
    ) -> Result<String, cqlmatic::contracts::Diagnostic> {
        panic!("An earlier-plan checkpoint must not start a search")
    }
    fn poll(
        &mut self,
        _: &str,
        _: &str,
        _: Instant,
    ) -> Result<Value, cqlmatic::contracts::Diagnostic> {
        panic!("An earlier-plan checkpoint must not poll a search")
    }
    fn stop(
        &mut self,
        _: &str,
        _: &str,
        _: Instant,
    ) -> Result<(), cqlmatic::contracts::Diagnostic> {
        panic!("An earlier-plan checkpoint must not stop a search")
    }
}

#[test]
fn standalone_earlier_plan_checkpoints_refuse_every_lifecycle_step() {
    for transport in [Transport::Mcp, Transport::Native] {
        let dir = Temp::new();
        let request: DiscoverRequest = serde_json::from_value(json!({"schema_version":1,
            "environment":environment(),"start":"2026-01-01T00:00:00Z","end":"2026-01-01T01:00:00Z",
            "sources":[{"type":"synthetic"}],"depth":"sources-only","max_queries":4,"max_seconds":120}))
        .unwrap();
        let path = dir.0.join("checkpoint.json");
        let initial = mcp::init_resumable(
            request,
            &path,
            &dir.0.join("pack.json"),
            &dir.0.join("receipt.json"),
            transport,
        )
        .unwrap();
        let probe = initial.probes[0]["probe_id"].as_str().unwrap().to_string();
        as_earlier_plan(&path, true);
        let retained = fs::read(&path).unwrap();
        let answer = PathBuf::from(fixture(
            &dir.0,
            "answer.json",
            json!({"result":[{"events":10}]}),
        ));
        let outcomes = [
            mcp::step_resumable(&path, &[]).map(|_| ()),
            mcp::step_resumable(&path, &[(probe.clone(), answer)]).map(|_| ()),
            mcp::dispatch(&path, &probe).map(|_| ()),
            mcp::pause(&path).map(|_| ()),
            mcp::resume(&path).map(|_| ()),
            mcp::run_native(&path, &mut NoSearch).map(|_| ()),
        ];
        for outcome in outcomes {
            let diagnostic = outcome.unwrap_err();
            assert_eq!(diagnostic.code, "MCP_STATE_PRODUCER", "{diagnostic:?}");
            assert_eq!(diagnostic.message, EARLIER_PLAN_REFUSAL);
        }
        assert_eq!(fs::read(&path).unwrap(), retained);
    }
}
