use cqlmatic::{
    contracts::{Cloud, EnvironmentId, Scope},
    environment::{Pack, PackData},
    inventory::{
        DiscoveryMode, InventoryState, MAX_STATE_BYTES, PackReference, TargetScope,
        UnverifiedFieldLead,
    },
    storage::Writer,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-quick-{}-{}",
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
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn source() -> Scope {
    Scope {
        repo: Some("example".into()),
        ..Scope::default()
    }
}

fn environment() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "search-all".into(),
    }
}

fn state(path: &Path, full_leads: bool) {
    let mut state = InventoryState::new("2026-01-01T00:00:00Z").unwrap();
    state.discovery_mode = DiscoveryMode::Quick;
    state
        .confirm(
            environment(),
            TargetScope::SelectedSources,
            "Synthetic Quick target".into(),
            vec![source()],
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
    if full_leads {
        state.unverified_leads = (0..1000)
            .map(|index| UnverifiedFieldLead {
                source: source(),
                field: format!("lead{index}"),
            })
            .collect();
    }
    state.validate().unwrap();
    Writer::acquire(path)
        .unwrap()
        .commit_bytes(&serde_json::to_vec(&state).unwrap(), MAX_STATE_BYTES)
        .unwrap();
}

fn run(args: &[&str]) -> (i32, Value) {
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(args)
        .output()
        .unwrap();
    let json: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    (output.status.code().unwrap_or(-1), json)
}

fn diagnostic(result: &Value, code: &str) -> bool {
    result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == code)
}

#[test]
fn pack_free_quick_check_applies_target_and_saves_only_input_names() {
    let dir = Temp::new();
    let path = dir.path().join("state.json");
    state(&path, false);
    let (code, result) = run(&[
        "check",
        "#repo=example user=\"secret-value\" | top(user)",
        "--inventory-state",
        path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{result}");
    assert_eq!(result["outcome"], "ready");
    assert!(diagnostic(&result, "CQL_QUICK_TARGET"));
    assert!(diagnostic(&result, "CQL_QUICK_LEADS"));
    let saved = fs::read(&path).unwrap();
    assert!(
        !saved
            .windows(b"secret-value".len())
            .any(|bytes| bytes == b"secret-value")
    );
    let state: InventoryState = serde_json::from_slice(&saved).unwrap();
    assert!(
        state
            .unverified_leads
            .iter()
            .any(|lead| lead.source == source() && lead.field == "user")
    );
}

#[test]
fn pack_free_quick_generate_applies_target_and_captures_input_field() {
    let dir = Temp::new();
    let path = dir.path().join("state.json");
    state(&path, false);
    let (code, result) = run(&[
        "generate",
        "--intent",
        "count events where account equals \"synthetic-secret\"",
        "--repo",
        "example",
        "--inventory-state",
        path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{result}");
    assert_eq!(result["outcome"], "ready");
    assert!(diagnostic(&result, "CQL_QUICK_TARGET"));
    assert!(diagnostic(&result, "CQL_QUICK_LEADS"));
    let saved = fs::read(&path).unwrap();
    assert!(
        !saved
            .windows(b"synthetic-secret".len())
            .any(|bytes| bytes == b"synthetic-secret")
    );
    let state: InventoryState = serde_json::from_slice(&saved).unwrap();
    assert!(
        state
            .unverified_leads
            .iter()
            .any(|lead| lead.source == source() && lead.field == "account")
    );
}

#[test]
fn quick_target_mismatch_and_lead_save_failure_are_visible_without_breaking_json() {
    let dir = Temp::new();
    let path = dir.path().join("state.json");
    state(&path, false);
    let request = dir.path().join("request.json");
    let mut other = environment();
    other.tenant = "1".repeat(32);
    fs::write(
        &request,
        serde_json::to_vec(&json!({"schema_version":1,
        "query":"#repo=example user=* | head(1)","context":{"environment":other}}))
        .unwrap(),
    )
    .unwrap();
    let (code, result) = run(&[
        "check",
        "--request",
        request.to_str().unwrap(),
        "--inventory-state",
        path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{result}");
    assert!(diagnostic(&result, "CQL_QUICK_TARGET_MISMATCH"));
    assert!(
        serde_json::from_slice::<InventoryState>(&fs::read(&path).unwrap())
            .unwrap()
            .unverified_leads
            .is_empty()
    );

    state(&path, true);
    let (code, result) = run(&[
        "check",
        "#repo=example new_field=* | head(1)",
        "--inventory-state",
        path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{result}");
    assert!(diagnostic(&result, "CQL_QUICK_LEAD_SAVE"));
    assert_eq!(
        serde_json::from_slice::<InventoryState>(&fs::read(&path).unwrap())
            .unwrap()
            .unverified_leads
            .len(),
        1000
    );
}

#[test]
fn quick_jsonl_keeps_one_result_per_line_and_captures_each_input_field() {
    let dir = Temp::new();
    let path = dir.path().join("state.json");
    state(&path, false);
    let mut child = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args([
            "check",
            "--jsonl",
            "--inventory-state",
            path.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(
        b"{\"schema_version\":1,\"query\":\"#repo=example alpha=* | head(1)\"}\n{\"schema_version\":1,\"query\":\"#repo=example beta=* | head(1)\"}\n"
    ).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let lines = output
        .stdout
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    assert_eq!(lines.len(), 2);
    for line in lines {
        let result: Value = serde_json::from_slice(line).unwrap();
        assert!(diagnostic(&result, "CQL_QUICK_TARGET"));
    }
    let state: InventoryState = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert!(
        state
            .unverified_leads
            .iter()
            .any(|lead| lead.field == "alpha")
    );
    assert!(
        state
            .unverified_leads
            .iter()
            .any(|lead| lead.field == "beta")
    );
}

#[test]
fn strict_grounding_fails_closed_when_selected_active_pack_disappears() {
    let dir = Temp::new();
    let state_path = dir.path().join("state.json");
    state(&state_path, false);
    let pack_path = dir.path().join("old-pack.json");
    let source_id = cqlmatic::discovery::source_id(&source());
    let pack = Pack::new(
        environment(),
        "2026-01-01T00:00:00Z".into(),
        PackData {
            sources: vec![json!({"id":source_id,"kind":"repo","name":"example",
                "inventory_status":"observed-only","selectors":{"repo":"example"},
                "evidence":{"observed":true},"provenance_ids":["prov:old"]})],
            fields: vec![json!({"id":"env:field:example:user","source_id":source_id,
                "path":"user","data_type":"string","is_tag":false,
                "evidence_kind":"observed","coverage":{"observed":true},
                "provenance_ids":["prov:old"]})],
            provenance: vec![json!({"id":"prov:old","source_type":"ngsiem-api",
                "canonical_source":"Synthetic old measurement",
                "retrieved_at":"2026-01-01T00:00:00Z","redaction_status":"none",
                "coverage_status":"complete"})],
            ..PackData::default()
        },
    )
    .unwrap();
    Writer::acquire(&pack_path).unwrap().commit(&pack).unwrap();
    let query = "#repo=example | user=* | head(1)";
    let (code, baseline) = run(&[
        "check",
        query,
        "--pack",
        pack_path.to_str().unwrap(),
        "--require-observed",
        "--json",
    ]);
    assert_eq!(code, 0, "{baseline}");
    assert_eq!(baseline["outcome"], "ready");

    let mut inventory: InventoryState =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let active = PackReference {
        path: dir
            .path()
            .join("missing-new-pack.json")
            .to_string_lossy()
            .into_owned(),
        content_digest: "0".repeat(64),
        imported_at: "2026-01-02T00:00:00Z".into(),
        discovery_run: None,
    };
    inventory.packs.push(active.clone());
    inventory.active_pack = Some(active);
    inventory.validate().unwrap();
    Writer::acquire(&state_path)
        .unwrap()
        .commit_bytes(&serde_json::to_vec(&inventory).unwrap(), MAX_STATE_BYTES)
        .unwrap();
    let (code, result) = run(&[
        "check",
        query,
        "--pack",
        pack_path.to_str().unwrap(),
        "--inventory-state",
        state_path.to_str().unwrap(),
        "--require-observed",
        "--json",
    ]);
    assert_eq!(code, 1, "{result}");
    assert_eq!(result["outcome"], "needs-context");
    assert!(diagnostic(&result, "CQL_QUICK_INVENTORY"));
    assert_eq!(result["grounding"]["policy"]["require_observed"], true);
}
