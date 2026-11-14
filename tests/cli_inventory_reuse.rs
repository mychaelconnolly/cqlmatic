use cqlmatic::{
    contracts::{Cloud, EnvironmentId, Scope},
    discovery,
    environment::{Pack, PackData},
    inventory::{
        DiscoveryMode, InventoryState, MAX_STATE_BYTES, PackReference, QueryObservation,
        QueryResultAssertion, TargetScope,
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
            "cqlmatic-inventory-reuse-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        private_directory(&path);
        Self(path)
    }

    fn default_state(&self) -> PathBuf {
        self.0.join("data/cqlmatic/inventory/state.json")
    }

    fn binary(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
        command
            .env_clear()
            .envs(std::env::vars_os().filter(|(key, _)| {
                matches!(
                    key.to_str(),
                    Some("PATH" | "HOME" | "SystemRoot" | "SYSTEMROOT")
                )
            }));
        command
            .env("LOCALAPPDATA", self.0.join("data"))
            .env("XDG_DATA_HOME", self.0.join("data"))
            .env("XDG_CONFIG_HOME", self.0.join("config"))
            .env("XDG_STATE_HOME", self.0.join("state"))
            .env("TMPDIR", &self.0)
            .env("TMP", &self.0)
            .env("TEMP", &self.0);
        command
    }

    fn run(&self, args: &[&str], input: &[u8]) -> (i32, Vec<Value>) {
        let mut child = self
            .binary()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows = output
            .stdout
            .split(|byte| *byte == b'\n')
            .filter(|row| !row.is_empty())
            .map(|row| serde_json::from_slice(row).unwrap())
            .collect();
        (output.status.code().unwrap_or(-1), rows)
    }

    fn one(&self, args: &[&str]) -> (i32, Value) {
        let (code, mut rows) = self.run(args, b"");
        assert_eq!(rows.len(), 1);
        (code, rows.remove(0))
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn private_directory(path: &Path) {
    fs::create_dir_all(path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

fn environment() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "search-all".into(),
    }
}

fn source() -> Scope {
    Scope {
        repo: Some("sample".into()),
        ..Scope::default()
    }
}

fn pack(observed: bool, date: &str) -> Pack {
    let source_id = discovery::source_id(&source());
    Pack::new(
        environment(),
        date.into(),
        PackData {
            sources: vec![json!({"id":source_id,"kind":"repo","name":"sample",
                "selectors":{"repo":"sample"},"inventory_status":"observed-only",
                "evidence":{"observed":true},"provenance_ids":["prov:sample"]})],
            fields: vec![json!({"id":"env:field:sample:user","source_id":source_id,
                "path":"user","data_type":"string","is_tag":false,"evidence_kind":"observed",
                "coverage":{"observed":observed},"provenance_ids":["prov:sample"]})],
            provenance: vec![json!({"id":"prov:sample","source_type":"ngsiem-api",
                "canonical_source":"Synthetic source measurement","retrieved_at":date,
                "redaction_status":"none","coverage_status":"complete"})],
            ..PackData::default()
        },
    )
    .unwrap()
}

fn save_pack(path: &Path, pack: &Pack) {
    private_directory(path.parent().unwrap());
    Writer::acquire(path).unwrap().commit(pack).unwrap();
}

fn state(mode: DiscoveryMode) -> InventoryState {
    let mut state = InventoryState::new("2026-01-01T00:00:00Z").unwrap();
    state.discovery_mode = mode;
    state
        .confirm(
            environment(),
            TargetScope::SelectedSources,
            "Synthetic source".into(),
            vec![source()],
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
    state
}

fn attach(state: &mut InventoryState, path: &Path, digest: String) {
    let reference = PackReference {
        path: path.to_string_lossy().into_owned(),
        content_digest: digest,
        imported_at: "2026-01-03T00:00:00Z".into(),
        discovery_run: None,
    };
    state.packs.push(reference.clone());
    state.active_pack = Some(reference);
}

fn save_state(path: &Path, state: &InventoryState) {
    state.validate().unwrap();
    private_directory(path.parent().unwrap());
    Writer::acquire(path)
        .unwrap()
        .commit_bytes(&serde_json::to_vec(state).unwrap(), MAX_STATE_BYTES)
        .unwrap();
}

fn diagnostic(result: &Value, code: &str, severity: &str) -> bool {
    result["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == code && d["severity"] == severity)
}

fn finding(result: &Value, code: &str) -> bool {
    result["findings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == code)
}

fn assert_inventory_target_message(result: &Value, key: &str, mode: DiscoveryMode) {
    let diagnostic = result[key]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "CQL_QUICK_TARGET")
        .unwrap_or_else(|| panic!("{result}"));
    assert_eq!(diagnostic["severity"], "info", "{result}");
    if key == "findings" {
        assert_eq!(
            diagnostic["message"],
            format!(
                "Confirmed {mode:?} inventory supplied the otherwise unspecified assessment target."
            )
        );
    } else {
        assert!(
            diagnostic["message"]
                .as_str()
                .unwrap()
                .starts_with(&format!("Confirmed {mode:?} inventory target supplied: ")),
            "{result}"
        );
    }
}

#[test]
fn both_modes_reuse_default_and_explicit_active_packs_for_generate_and_check() {
    for mode in [DiscoveryMode::Full, DiscoveryMode::Quick] {
        for explicit in [false, true] {
            let dir = Temp::new();
            let path = if explicit {
                dir.0.join("chosen/state.json")
            } else {
                dir.default_state()
            };
            let pack_path = dir.0.join("snapshot/pack.json");
            let pack = pack(true, "2026-01-02T00:00:00Z");
            save_pack(&pack_path, &pack);
            let mut state = state(mode);
            attach(&mut state, &pack_path, pack.content_digest.clone());
            save_state(&path, &state);
            let before = fs::read(&path).unwrap();
            let request = dir.0.join("generate.json");
            fs::write(
                &request,
                json!({"schema_version":1,"input":{"mode":"plan",
                "plan":{"source":{"repo":"sample"},"filters":[{"op":"exists","field":"user"}]}}})
                .to_string(),
            )
            .unwrap();
            for mut args in [
                vec!["check", "#repo=sample | user=* | count()", "--json"],
                vec!["generate", "--request", request.to_str().unwrap(), "--json"],
            ] {
                if explicit {
                    args.extend(["--inventory-state", path.to_str().unwrap()]);
                }
                for strict in [false, true] {
                    if strict {
                        args.push("--require-observed");
                    }
                    let (code, result) = dir.one(&args);
                    assert_eq!(code, 0, "{result}");
                    assert_eq!(result["outcome"], "ready", "{result}");
                    assert_eq!(result["grounding"]["pack_id"], pack.content_digest);
                    assert_inventory_target_message(&result, "diagnostics", mode);
                    assert!(
                        !diagnostic(&result, "CQL_INVENTORY_PENDING", "info"),
                        "{result}"
                    );
                }
            }
            if mode == DiscoveryMode::Full {
                assert_eq!(fs::read(&path).unwrap(), before);
            } else {
                let saved: InventoryState =
                    serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
                assert!(
                    saved
                        .unverified_leads
                        .iter()
                        .any(|lead| lead.field == "user")
                );
            }
            let saved: InventoryState = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert!(saved.pending_inventory.is_empty());
        }
    }
}

#[test]
fn both_modes_queue_source_only_hints_once_without_promoting_grounding() {
    for mode in [DiscoveryMode::Full, DiscoveryMode::Quick] {
        for (query, selectors) in [
            ("#repo=unseen | count()", json!({"repo":"unseen"})),
            ("#type=unseen-type | count()", json!({"type":"unseen-type"})),
            (
                "#repo=sample #type=unseen-type | count()",
                json!({"repo":"sample","type":"unseen-type"}),
            ),
        ] {
            let dir = Temp::new();
            let path = dir.default_state();
            let initial = state(mode);
            save_state(&path, &initial);
            let (code, result) = dir.one(&["check", query, "--json"]);
            assert_eq!(code, 0, "{result}");
            assert!(
                diagnostic(&result, "CQL_INVENTORY_PENDING", "info"),
                "{result}"
            );
            let saved: InventoryState = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved.pending_inventory.len(), 1);
            let work = &saved.pending_inventory[0];
            assert_eq!(serde_json::to_value(&work.source).unwrap(), selectors);
            assert!(work.fields.is_empty());
            assert!(work.new_source);
            assert_eq!(work.revision, 1);
            assert_eq!(work.notified_revision, 0);
            assert_eq!(saved.inventory_notifications().len(), 1);
            assert_eq!(
                serde_json::to_value(&saved.sources).unwrap(),
                serde_json::to_value(&initial.sources).unwrap()
            );
            assert!(saved.active_pack.is_none());
            assert!(saved.query_observations.is_empty());
            assert!(saved.unverified_leads.is_empty());
            let before = fs::read(&path).unwrap();
            let (_, again) = dir.one(&["check", query, "--json"]);
            assert!(
                diagnostic(&again, "CQL_INVENTORY_PENDING", "info"),
                "{again}"
            );
            assert_eq!(fs::read(&path).unwrap(), before);

            let request = dir.0.join("source-only.json");
            fs::write(&request, json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":selectors,"filters":[],"steps":[]}}}).to_string()).unwrap();
            let (_, generated) =
                dir.one(&["generate", "--request", request.to_str().unwrap(), "--json"]);
            assert_eq!(generated["validation"]["status"], "accepted", "{generated}");
            assert!(
                diagnostic(&generated, "CQL_INVENTORY_PENDING", "info"),
                "{generated}"
            );
            assert_eq!(fs::read(&path).unwrap(), before);

            let (code, strict) = dir.one(&["check", query, "--json", "--require-observed"]);
            assert_eq!(code, 1, "{strict}");
            assert_eq!(strict["outcome"], "needs-context", "{strict}");
            assert_eq!(fs::read(&path).unwrap(), before);
        }
    }
}

#[test]
fn deferred_on_use_work_keeps_settings_attention_after_restart_and_new_fields_notify_again() {
    for mode in [DiscoveryMode::Full, DiscoveryMode::Quick] {
        let dir = Temp::new();
        let path = dir.default_state();
        save_state(&path, &state(mode));
        let (_, result) = dir.one(&["check", "#repo=unseen | count()", "--json"]);
        assert!(diagnostic(&result, "CQL_INVENTORY_PENDING", "info"));
        let saved: InventoryState = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let work = &saved.pending_inventory[0];
        let request = dir.0.join("defer.json");
        fs::write(&request, json!({"schema_version":1,"action":{"kind":"acknowledge-inventory-work","work":[{"id":work.id,"revision":work.revision}],"defer":true}}).to_string()).unwrap();
        let (code, deferred) = dir.one(&[
            "env",
            "inventory",
            "--request",
            request.to_str().unwrap(),
            "--json",
        ]);
        assert_eq!(code, 0, "{deferred}");
        assert_eq!(deferred["status"]["settings_attention"], true);
        assert_eq!(deferred["status"]["pending_inventory"][0]["deferred"], true);
        let before = fs::read(&path).unwrap();

        let (_, status) = dir.one(&["status", "--json", "--summary"]);
        assert_eq!(status["settings_attention"], true);
        assert_eq!(status["counts"]["inventory_notifications"], 0);
        let text = dir.binary().arg("status").output().unwrap();
        assert!(text.status.success());
        assert!(String::from_utf8_lossy(&text.stdout).contains("Settings *"));
        let (_, replay) = dir.one(&["check", "#repo=unseen | count()", "--json"]);
        assert!(!diagnostic(&replay, "CQL_INVENTORY_PENDING", "info"));
        assert_eq!(fs::read(&path).unwrap(), before);

        let (_, novel) = dir.one(&["check", "#repo=unseen | new_field=* | count()", "--json"]);
        assert!(
            diagnostic(&novel, "CQL_INVENTORY_PENDING", "info"),
            "{novel}"
        );
        let saved: InventoryState = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.pending_inventory[0].revision, 2);
        assert_eq!(saved.pending_inventory[0].notified_revision, 1);
        assert_eq!(saved.pending_inventory[0].fields, ["new_field"]);
        assert!(!saved.pending_inventory[0].deferred);
        assert!(saved.query_observations.is_empty());
        assert!(saved.packs.is_empty());
        assert_eq!(saved.inventory_notifications().len(), 1);
        let before = fs::read(&path).unwrap();
        let (_, replay) = dir.one(&["check", "#repo=unseen | new_field=* | count()", "--json"]);
        assert!(diagnostic(&replay, "CQL_INVENTORY_PENDING", "info"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
}

#[test]
fn jsonl_source_only_notices_remain_one_result_per_request_without_acknowledging_delivery() {
    for mode in [DiscoveryMode::Full, DiscoveryMode::Quick] {
        let dir = Temp::new();
        let path = dir.default_state();
        save_state(&path, &state(mode));
        let input = [
            "#repo=unseen | count()",
            "#repo=unseen | count()",
            "#type=unseen-type | count()",
        ]
        .iter()
        .enumerate()
        .map(|(id, query)| {
            json!({"schema_version":1,"id":id.to_string(),"query":query}).to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
        let (code, rows) = dir.run(&["check", "--jsonl"], input.as_bytes());
        assert_eq!(code, 0, "{rows:?}");
        assert_eq!(rows.len(), 3);
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(row["id"], index.to_string());
            assert!(diagnostic(row, "CQL_INVENTORY_PENDING", "info"), "{row}");
        }
        let saved: InventoryState = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.pending_inventory.len(), 2);
        assert!(saved.pending_inventory.iter().all(|work| work.revision == 1
            && work.notified_revision == 0
            && work.fields.is_empty()));
        assert_eq!(saved.inventory_notifications().len(), 2);
        assert!(saved.query_observations.is_empty());
        assert!(saved.packs.is_empty());
    }
}

#[test]
fn generation_jsonl_and_failed_output_do_not_acknowledge_inventory_work() {
    for mode in [DiscoveryMode::Full, DiscoveryMode::Quick] {
        let dir = Temp::new();
        let path = dir.default_state();
        save_state(&path, &state(mode));
        let input = (0..2)
            .map(|index| {
                json!({"schema_version":1,"id":index.to_string(),
            "input":{"mode":"plan","plan":{"source":{"repo":"unseen"},"filters":[],"steps":[]}}})
                .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (code, rows) = dir.run(&["generate", "--jsonl"], input.as_bytes());
        assert_eq!(code, 0, "{rows:?}");
        assert_eq!(rows.len(), 2);
        for (index, row) in rows.iter().enumerate() {
            assert_eq!(row["id"], index.to_string());
            assert!(diagnostic(row, "CQL_INVENTORY_PENDING", "info"), "{row}");
        }
        let saved: InventoryState = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved.pending_inventory.len(), 1);
        assert_eq!(saved.pending_inventory[0].notified_revision, 0);
        let before = fs::read(&path).unwrap();
        let (code, _) = dir.run(&["generate", "--jsonl"], input.as_bytes());
        assert_eq!(code, 0);
        assert_eq!(fs::read(&path).unwrap(), before);

        let request = dir.0.join("generate.json");
        fs::write(&request, json!({"schema_version":1,
            "input":{"mode":"plan","plan":{"source":{"repo":"output-failure"},"filters":[],"steps":[]}}}).to_string()).unwrap();
        for args in [
            vec!["generate", "--request", request.to_str().unwrap(), "--json"],
            vec!["check", "#repo=output-failure | count()", "--json"],
        ] {
            let (reader, writer) = std::io::pipe().unwrap();
            drop(reader);
            let failed = dir
                .binary()
                .args(args)
                .stdout(Stdio::from(writer))
                .output()
                .unwrap();
            assert_eq!(failed.status.code(), Some(2), "{failed:?}");
            let error: Value = serde_json::from_slice(&failed.stderr).unwrap();
            assert_eq!(error["code"], "OUTPUT_IO");
            let saved: InventoryState = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(saved.pending_inventory.len(), 2);
            assert!(
                saved
                    .pending_inventory
                    .iter()
                    .all(|work| work.notified_revision == 0)
            );
        }
    }
}

#[test]
fn foreground_queries_reuse_the_saved_pack_while_refresh_is_dispatched() {
    let dir = Temp::new();
    let mut state = state(DiscoveryMode::Full);
    let pack = pack(true, "2026-01-02T00:00:00Z");
    let pack_path = dir.0.join("snapshot/pack.json");
    save_pack(&pack_path, &pack);
    attach(&mut state, &pack_path, pack.content_digest.clone());
    let now = cqlmatic::discovery::timestamp();
    let host = serde_json::to_vec(&json!({"schema_version":1,"environment":environment(),
        "session_id":"synthetic-session","verified_at":now,
        "mechanism":"provider-enforced-search-lifetime","max_search_seconds":300,
        "capability_reference":"Synthetic provider capability",
        "verification_evidence":"Synthetic session verification of provider search lifetime"}))
    .unwrap();
    let id = state
        .prepare_refresh("synthetic-session".into(), &host, false, &now)
        .unwrap();
    state
        .dispatch_refresh(&id, "synthetic-session", &now)
        .unwrap();
    let path = dir.default_state();
    save_state(&path, &state);
    let before = fs::read(&path).unwrap();
    let (code, checked) = dir.one(&[
        "check",
        "#repo=sample | user=* | count()",
        "--json",
        "--require-observed",
    ]);
    assert_eq!(code, 0, "{checked}");
    assert_eq!(checked["outcome"], "ready");
    assert_eq!(checked["grounding"]["pack_id"], pack.content_digest);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn source_only_inventory_supplies_mixed_kind_notices_to_normal_queries() {
    let dir = Temp::new();
    let mut pack = pack(true, "2026-01-02T00:00:00Z");
    pack.data.fields.clear();
    pack.data.sources[0]["relationship"] = json!({"datasets":["activity","network"]});
    pack.content_digest = pack.digest().unwrap();
    let pack_path = dir.0.join("snapshot/pack.json");
    save_pack(&pack_path, &pack);
    let mut state = state(DiscoveryMode::Full);
    attach(&mut state, &pack_path, pack.content_digest.clone());
    save_state(&dir.default_state(), &state);
    for query in ["#repo=sample | count()", "#repo=sample | top(user)"] {
        let (code, result) = dir.one(&["check", query, "--json"]);
        assert_eq!(code, 0, "{result}");
        assert_eq!(result["outcome"], "ready");
        assert!(
            diagnostic(&result, "CQL_RECORD_KIND_NOTICE", "info"),
            "{result}"
        );
        assert_eq!(result["grounding"]["pack_id"], pack.content_digest);
    }
}

#[test]
fn explicit_pack_stays_primary_and_cannot_bypass_newer_active_negative() {
    let dir = Temp::new();
    let explicit = pack(true, "2026-01-02T00:00:00Z");
    let explicit_path = dir.0.join("explicit/pack.json");
    save_pack(&explicit_path, &explicit);
    let active_path = dir.0.join("snapshot/pack.json");
    let mut state = state(DiscoveryMode::Full);
    for observed in [true, false] {
        let active = pack(observed, "2026-02-02T00:00:00Z");
        save_pack(&active_path, &active);
        state.packs.clear();
        attach(&mut state, &active_path, active.content_digest);
        save_state(&dir.default_state(), &state);
        let (code, result) = dir.one(&[
            "check",
            "#repo=sample | user=* | count()",
            "--pack",
            explicit_path.to_str().unwrap(),
            "--require-observed",
            "--json",
        ]);
        assert_eq!(result["grounding"]["pack_id"], explicit.content_digest);
        assert_eq!(code, if observed { 0 } else { 1 }, "{result}");
        assert_eq!(
            result["outcome"],
            if observed { "ready" } else { "needs-context" },
            "{result}"
        );
    }
}

#[test]
fn jsonl_skips_foreign_corrupt_snapshot_but_blocks_the_matching_row() {
    let dir = Temp::new();
    let active = dir.0.join("snapshot/pack.json");
    private_directory(active.parent().unwrap());
    Writer::acquire(&active)
        .unwrap()
        .commit_bytes(b"invalid", MAX_STATE_BYTES)
        .unwrap();
    let mut state = state(DiscoveryMode::Quick);
    attach(&mut state, &active, "0".repeat(64));
    let path = dir.default_state();
    save_state(&path, &state);
    let before = fs::read(&path).unwrap();
    let mut foreign = environment();
    foreign.view = "other-view".into();
    for command in ["check", "generate", "assess"] {
        let mut lines = Vec::new();
        for environment in [foreign.clone(), environment()] {
            let mut request = json!({"schema_version":1,"context":{"environment":environment}});
            if command == "check" {
                request["query"] = json!("#repo=sample | user=* | count()");
            } else if command == "generate" {
                request["input"] = json!({"mode":"plan","plan":{"source":{"repo":"sample"}}});
            } else {
                request["query"] = json!("#repo=sample | count()");
                request["result"] = json!({"result":[{"_count":1}]});
            }
            lines.extend(serde_json::to_vec(&request).unwrap());
            lines.push(b'\n');
        }
        let (code, rows) = dir.run(&[command, "--jsonl"], &lines);
        assert_eq!(code, if command == "assess" { 2 } else { 1 }, "{rows:?}");
        assert_eq!(rows.len(), 2);
        if command == "assess" {
            assert_eq!(rows[0]["outcome"], "advisory", "{}", rows[0]);
            assert!(finding(&rows[0], "CQL_QUICK_TARGET_MISMATCH"));
            assert_eq!(rows[1]["outcome"], "error", "{}", rows[1]);
            assert!(finding(&rows[1], "CQL_QUICK_INVENTORY"));
            continue;
        }
        assert_eq!(rows[0]["outcome"], "ready", "{}", rows[0]);
        assert!(diagnostic(&rows[0], "CQL_QUICK_TARGET_MISMATCH", "warning"));
        assert_eq!(rows[1]["outcome"], "needs-context", "{}", rows[1]);
        assert!(diagnostic(&rows[1], "CQL_QUICK_INVENTORY", "error"));
    }
    assert_eq!(fs::read(path).unwrap(), before);

    let mut explicit = pack(true, "2026-01-02T00:00:00Z");
    explicit.environment = foreign;
    explicit.content_digest = explicit.digest().unwrap();
    let explicit_path = dir.0.join("explicit/pack.json");
    save_pack(&explicit_path, &explicit);
    let (code, result) = dir.one(&[
        "check",
        "count()",
        "--pack",
        explicit_path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{result}");
    assert_eq!(result["grounding"]["pack_id"], explicit.content_digest);
    assert!(diagnostic(&result, "CQL_QUICK_TARGET_MISMATCH", "warning"));
}

#[test]
fn unavailable_active_snapshot_blocks_lenient_queries_without_creating_directories() {
    let dir = Temp::new();
    let explicit_path = dir.0.join("explicit/pack.json");
    save_pack(&explicit_path, &pack(true, "2026-01-02T00:00:00Z"));
    let active = dir.0.join("absent/nested/pack.json");
    let mut state = state(DiscoveryMode::Full);
    attach(&mut state, &active, "0".repeat(64));
    save_state(&dir.default_state(), &state);
    for mut args in [
        vec!["check", "count()", "--json"],
        vec![
            "check",
            "count()",
            "--pack",
            explicit_path.to_str().unwrap(),
            "--json",
        ],
    ] {
        for strict in [false, true] {
            if strict {
                args.push("--require-observed");
            }
            let (code, result) = dir.one(&args);
            assert_eq!(code, 1, "{result}");
            assert_eq!(result["outcome"], "needs-context");
            assert!(diagnostic(&result, "CQL_QUICK_INVENTORY", "error"));
        }
    }
    assert!(!dir.0.join("absent").exists());
}

#[test]
fn missing_default_is_a_noop_but_missing_explicit_and_retained_without_active_fail() {
    let dir = Temp::new();
    let path = dir.default_state();
    let (code, result) = dir.one(&["check", "count()", "--json"]);
    assert_eq!(code, 0, "{result}");
    assert!(!dir.0.join("data").exists());
    let (code, result) = dir.one(&[
        "check",
        "count()",
        "--inventory-state",
        path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 1, "{result}");
    assert!(diagnostic(&result, "CQL_QUICK_INVENTORY", "error"));
    assert!(!dir.0.join("data").exists());
    let mut state = state(DiscoveryMode::Full);
    attach(&mut state, &dir.0.join("missing/pack.json"), "0".repeat(64));
    state.active_pack = None;
    save_state(&path, &state);
    let (code, result) = dir.one(&["check", "count()", "--json"]);
    assert_eq!(code, 1, "{result}");
    assert!(diagnostic(&result, "CQL_QUICK_INVENTORY", "error"));
    assert!(!dir.0.join("missing").exists());
}

#[test]
fn only_a_malformed_default_can_be_ignored_for_an_explicit_pack() {
    let dir = Temp::new();
    let pack_path = dir.0.join("snapshot/pack.json");
    save_pack(&pack_path, &pack(true, "2026-01-02T00:00:00Z"));
    let path = dir.default_state();
    private_directory(path.parent().unwrap());
    Writer::acquire(&path)
        .unwrap()
        .commit_bytes(b"invalid", MAX_STATE_BYTES)
        .unwrap();
    let (code, result) = dir.one(&["check", "count()", "--json"]);
    assert_eq!(code, 1, "{result}");
    let (code, result) = dir.one(&[
        "check",
        "count()",
        "--pack",
        pack_path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 0, "{result}");
    assert!(diagnostic(
        &result,
        "CQL_QUICK_INVENTORY_IGNORED",
        "warning"
    ));
    let (code, result) = dir.one(&[
        "check",
        "count()",
        "--pack",
        pack_path.to_str().unwrap(),
        "--inventory-state",
        path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 1, "{result}");
    assert!(diagnostic(&result, "CQL_QUICK_INVENTORY", "error"));
}

#[cfg(unix)]
#[test]
fn unreadable_default_inventory_blocks_explicit_pack_for_query_and_assessment() {
    use std::os::unix::fs::PermissionsExt;

    let dir = Temp::new();
    let pack_path = dir.0.join("snapshot/pack.json");
    save_pack(&pack_path, &pack(true, "2026-01-02T00:00:00Z"));
    let path = dir.default_state();
    save_state(&path, &state(DiscoveryMode::Full));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        fs::read(&path).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );

    for args in [
        vec![
            "check",
            "count()",
            "--pack",
            pack_path.to_str().unwrap(),
            "--json",
        ],
        vec![
            "generate",
            "--intent",
            "count events",
            "--repo",
            "sample",
            "--pack",
            pack_path.to_str().unwrap(),
            "--json",
        ],
    ] {
        let (code, result) = dir.one(&args);
        assert_eq!(code, 1, "{result}");
        assert_eq!(result["outcome"], "needs-context");
        assert!(result["query"].is_null());
        assert!(diagnostic(&result, "CQL_QUICK_INVENTORY", "error"));
        assert!(!diagnostic(
            &result,
            "CQL_QUICK_INVENTORY_IGNORED",
            "warning"
        ));
    }

    let request = json!({"schema_version":1,"query":"count()","result":{"result":[{"_count":1}]}});
    let (code, rows) = dir.run(
        &["assess", "--jsonl", "--pack", pack_path.to_str().unwrap()],
        format!("{request}\n").as_bytes(),
    );
    assert_eq!(code, 2, "{rows:?}");
    assert_eq!(rows[0]["outcome"], "error");
    assert!(finding(&rows[0], "CQL_QUICK_INVENTORY"));
    assert!(!finding(&rows[0], "CQL_QUICK_INVENTORY_IGNORED"));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn oversized_default_inventory_cannot_be_ignored_for_an_explicit_pack() {
    let dir = Temp::new();
    let pack_path = dir.0.join("snapshot/pack.json");
    save_pack(&pack_path, &pack(true, "2026-01-02T00:00:00Z"));
    let path = dir.default_state();
    save_state(&path, &state(DiscoveryMode::Full));
    fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_STATE_BYTES + 1)
        .unwrap();
    let (code, result) = dir.one(&[
        "check",
        "count()",
        "--pack",
        pack_path.to_str().unwrap(),
        "--json",
    ]);
    assert_eq!(code, 1, "{result}");
    assert_eq!(result["outcome"], "needs-context");
    assert!(diagnostic(&result, "CQL_QUICK_INVENTORY", "error"));
    assert!(!diagnostic(
        &result,
        "CQL_QUICK_INVENTORY_IGNORED",
        "warning"
    ));
}

#[test]
fn unconfirmed_preferences_are_benign_only_without_evidence() {
    let dir = Temp::new();
    let path = dir.default_state();
    for mode in [DiscoveryMode::Full, DiscoveryMode::Quick] {
        let mut preferences = InventoryState::new("2026-01-01T00:00:00Z").unwrap();
        preferences.discovery_mode = mode;
        save_state(&path, &preferences);
        let before = fs::read(&path).unwrap();
        let (code, result) = dir.one(&["check", "count()", "--json"]);
        assert_eq!(code, 0, "{result}");
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let mut contradictory = state(DiscoveryMode::Quick);
    contradictory.target = None;
    contradictory.refresh_reminder = None;
    save_state(&path, &contradictory);
    let (code, result) = dir.one(&["check", "count()", "--json"]);
    assert_eq!(code, 1, "{result}");
    assert!(diagnostic(&result, "CQL_QUICK_INVENTORY", "error"));
}

#[test]
fn missing_observation_input_does_not_create_its_parent() {
    let dir = Temp::new();
    let path = dir.default_state();
    save_state(&path, &state(DiscoveryMode::Full));
    let query = dir.0.join("absent/query.cql");
    let request = dir.0.join("request.json");
    fs::write(&request, json!({"schema_version":1,"action":{"kind":"import-query-observations",
        "query_file":query,"result_file":dir.0.join("absent/result.json"),"environment":environment(),
        "source":source(),"start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z",
        "retrieved_at":"2026-01-03T00:00:00Z","caller_assertion":"exact-query-result-target-window"}}).to_string()).unwrap();
    let before = fs::read(&path).unwrap();
    let output = dir
        .binary()
        .args(["env", "inventory", "--json", "--state"])
        .arg(&path)
        .arg("--request")
        .arg(request)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(!dir.0.join("absent").exists());
    assert_eq!(fs::read(path).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn dangling_snapshot_and_default_state_symlinks_are_errors() {
    use std::os::unix::fs::symlink;
    let dir = Temp::new();
    let active = dir.0.join("snapshot/pack.json");
    private_directory(active.parent().unwrap());
    symlink(dir.0.join("missing/pack.json"), &active).unwrap();
    let mut state = state(DiscoveryMode::Full);
    attach(&mut state, &active, "0".repeat(64));
    let path = dir.default_state();
    save_state(&path, &state);
    let (code, result) = dir.one(&["check", "count()", "--json"]);
    assert_eq!(code, 1, "{result}");
    assert!(diagnostic(&result, "CQL_QUICK_INVENTORY", "error"));
    assert!(!dir.0.join("missing").exists());
    fs::remove_file(&path).unwrap();
    symlink(dir.0.join("missing/state.json"), &path).unwrap();
    let (code, result) = dir.one(&["check", "count()", "--json"]);
    assert_eq!(code, 1, "{result}");
    assert!(diagnostic(&result, "CQL_QUICK_INVENTORY", "error"));
}

#[test]
fn assessment_reuses_pack_in_all_input_modes_without_writing_leads() {
    let dir = Temp::new();
    let mut pack = pack(true, "2026-01-02T00:00:00Z");
    pack.data.fields[0]["coverage"]["rate"] = json!(0.5);
    pack.content_digest = pack.digest().unwrap();
    let pack_path = dir.0.join("snapshot/pack.json");
    save_pack(&pack_path, &pack);
    let mut state = state(DiscoveryMode::Quick);
    attach(&mut state, &pack_path, pack.content_digest);
    let path = dir.default_state();
    save_state(&path, &state);
    let before = fs::read(&path).unwrap();
    let request = json!({"schema_version":1,"query":"#repo=sample | groupBy([user], limit=10)",
        "result":{"result":[{"user":"example","_count":1}]}});
    let request_path = dir.0.join("assess.json");
    fs::write(&request_path, request.to_string()).unwrap();
    let query_path = dir.0.join("query.cql");
    fs::write(&query_path, "#repo=sample | groupBy([user], limit=10)").unwrap();
    let result_path = dir.0.join("result.json");
    fs::write(&result_path, request["result"].to_string()).unwrap();
    for args in [
        vec![
            "assess",
            "--request",
            request_path.to_str().unwrap(),
            "--json",
        ],
        vec![
            "assess",
            "--query-file",
            query_path.to_str().unwrap(),
            "--result",
            result_path.to_str().unwrap(),
            "--inventory-state",
            path.to_str().unwrap(),
            "--json",
        ],
    ] {
        let (code, result) = dir.one(&args);
        assert_eq!(code, 1, "{result}");
        assert!(finding(&result, "RESULT_KEY_COVERAGE"), "{result}");
        assert_inventory_target_message(&result, "findings", DiscoveryMode::Quick);
    }
    let (code, rows) = dir.run(&["assess", "--jsonl"], format!("{request}\n").as_bytes());
    assert_eq!(code, 1, "{rows:?}");
    assert!(finding(&rows[0], "RESULT_KEY_COVERAGE"));
    assert_inventory_target_message(&rows[0], "findings", DiscoveryMode::Quick);
    assert_eq!(fs::read(path).unwrap(), before);
}

#[test]
fn assessment_does_not_turn_the_query_ledger_into_population_rates() {
    let dir = Temp::new();
    let mut state = state(DiscoveryMode::Full);
    state.query_observations.push(
        QueryObservation::from_result(
            b"#repo=sample | select([user]) | head(2)",
            br#"{"result":[{"user":"example"}]}"#,
            environment(),
            source(),
            "2026-02-01T00:00:00Z".into(),
            "2026-02-02T00:00:00Z".into(),
            "2026-02-03T00:00:00Z".into(),
            QueryResultAssertion::ExactQueryResultTargetWindow,
            dir.0.join("audit.json").to_string_lossy().into_owned(),
        )
        .unwrap(),
    );
    let path = dir.default_state();
    save_state(&path, &state);
    let before = fs::read(&path).unwrap();
    let (code, checked) = dir.one(&[
        "check",
        "#repo=sample | user=* | count()",
        "--require-observed",
        "--json",
    ]);
    assert_eq!(code, 0, "{checked}");
    assert_eq!(checked["outcome"], "ready");
    assert!(diagnostic(&checked, "CQL_QUERY_OBSERVATION", "warning"));
    let request = json!({"schema_version":1,"query":"#repo=sample | groupBy([user], limit=10)",
        "result":{"result":[{"user":"example","_count":1}]}});
    let (code, rows) = dir.run(&["assess", "--jsonl"], format!("{request}\n").as_bytes());
    assert_eq!(code, 0, "{rows:?}");
    assert!(!finding(&rows[0], "RESULT_KEY_COVERAGE"));
    assert_inventory_target_message(&rows[0], "findings", DiscoveryMode::Full);
    assert_eq!(fs::read(path).unwrap(), before);
}
