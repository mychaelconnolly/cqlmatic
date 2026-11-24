use cqlmatic::{
    environment::{Pack, PackData},
    inventory::InventoryState,
    mcp, storage,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const START: &str = "2026-01-01T00:00:00Z";
const END: &str = "2026-01-02T00:00:00Z";
const EARLIER: &str = "0.1.12";

fn environment() -> Value {
    json!({"cloud":"us1","tenant":"0".repeat(32),"view":"search-all"})
}

fn write(path: &Path, value: &impl serde::Serialize) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn ok(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn refused(output: Output, code: &str) -> Value {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], code, "{diagnostic}");
    diagnostic
}

fn message(diagnostic: &Value) -> &str {
    diagnostic["message"].as_str().unwrap()
}

struct Inventory(PathBuf);

impl Inventory {
    fn new(sources: &[&str]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "cqlmatic-import-guard-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let inventory = Self(path);
        inventory
            .call(json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}));
        inventory.confirm(sources);
        inventory
    }

    fn confirm(&self, sources: &[&str]) {
        let expected: Vec<_> = sources
            .iter()
            .map(|source| json!({"type":source}))
            .collect();
        self.call(json!({"kind":"confirm-target","environment":environment(),
            "scope":"selected-sources","context":"Synthetic sources","expected_sources":expected}));
    }

    fn state(&self) -> PathBuf {
        self.0.join("inventory").join("state.json")
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(self.state()).unwrap()
    }

    fn file(&self, name: &str, value: &impl serde::Serialize) -> PathBuf {
        let path = self.0.join(name);
        write(&path, value);
        path
    }

    fn run(&self, action: Value, projection: &[&str]) -> Output {
        let binary = std::env::var_os("CQLMATIC_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_cqlmatic").into());
        Command::new(binary)
            .args(["env", "inventory", "--json", "--state"])
            .arg(self.state())
            .arg("--request")
            .arg(self.file("request.json", &json!({"schema_version":1,"action":action})))
            .args(projection)
            .output()
            .unwrap()
    }

    fn call(&self, action: Value) -> Value {
        ok(self.run(action, &[]))
    }

    fn prepare_request(max_queries: u32) -> Value {
        json!({"kind":"prepare-discovery","start":START,"end":END,
            "max_queries":max_queries,"max_seconds":600})
    }

    fn prepare(&self, max_queries: u32) -> Run {
        let reply = self.call(Self::prepare_request(max_queries));
        let index = reply["action"]["run_index"].as_u64().unwrap() as usize;
        let run = &reply["status"]["discovery_runs"][index];
        Run {
            index,
            checkpoint: path(&run["state_path"]),
            pack: path(&run["pack_path"]),
            receipt: path(&run["receipt_path"]),
        }
    }

    fn next(&self, run: &Run) -> Value {
        let reply = self.call(json!({"kind":"next-probe","run_index":run.index}));
        assert_eq!(reply["action"]["state"], "probe", "{reply}");
        reply["action"]["probe"]["probe_id"].clone()
    }

    fn answer(&self, run: &Run, probe: &Value, result: Value) -> Value {
        self.call(
            json!({"kind":"discovery-step","run_index":run.index,"answers":[{
            "probe_id":probe,"result_file":self.file("result.json",&result)}]}),
        )
    }

    fn settle(&self, run: &Run, probe: &Value) -> Value {
        self.answer(run, probe, json!({"result":[{"events":10}]}))
    }

    fn poll(&self, run: &Run) -> Value {
        self.call(json!({"kind":"discovery-step","run_index":run.index,"answers":[]}))
    }

    fn pause(&self, run: &Run) -> Value {
        self.call(json!({"kind":"pause-discovery","run_index":run.index}))
    }

    fn resume(&self, run: &Run) -> Value {
        self.call(json!({"kind":"resume-discovery","run_index":run.index}))
    }

    fn import(&self, source: &str, name: &str, update: bool) -> Value {
        let pack = Pack::new(
            serde_json::from_value(environment()).unwrap(),
            START.into(),
            PackData {
                sources: vec![json!({"id":format!("env:repo:{source}"),"kind":"source",
                    "name":name,"inventory_status":"observed-only","selectors":{"type":source},
                    "provenance_ids":[format!("prov:{source}")]})],
                provenance: vec![
                    json!({"id":format!("prov:{source}"),"source_type":"mcp-observation",
                    "canonical_source":"Synthetic pack","retrieved_at":START,
                    "redaction_status":"none","coverage_status":"observed-only"}),
                ],
                ..Default::default()
            },
        )
        .unwrap();
        json!({"kind":"import-pack","pack_file":self.file("incoming.json",&pack),
            "mode":if update {"update"} else {"append"},"supersede_runs":false})
    }

    fn snapshots(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = fs::read_dir(self.state().parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect();
        paths.sort();
        paths
    }

    fn blocked(&self, action: &Value, index: usize) -> Value {
        let before = self.bytes();
        let snapshots = self.snapshots();
        let diagnostic = refused(self.run(action.clone(), &[]), "INVENTORY_INPUT");
        assert_eq!(diagnostic["pointer"], format!("/discovery_runs/{index}"));
        assert_eq!(self.bytes(), before);
        assert_eq!(self.snapshots(), snapshots);
        diagnostic
    }
}

impl Drop for Inventory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn path(value: &Value) -> PathBuf {
    value.as_str().unwrap().into()
}

fn override_import(mut action: Value) -> Value {
    action["supersede_runs"] = json!(true);
    action
}

struct Run {
    index: usize,
    checkpoint: PathBuf,
    pack: PathBuf,
    receipt: PathBuf,
}

impl Run {
    fn bytes(&self) -> Vec<u8> {
        fs::read(&self.checkpoint).unwrap()
    }

    fn edit(&self, edit: impl FnOnce(&mut Value)) {
        let mut value = read(&self.checkpoint);
        edit(&mut value);
        write(&self.checkpoint, &value);
    }

    fn status(&self) -> String {
        mcp::inspect_state(&self.checkpoint).unwrap().status
    }

    fn activation_phase(&self, phase: &str) {
        self.edit(|state| {
            state["status"] = json!(phase);
            state.as_object_mut().unwrap().remove("pack_sha256");
        });
    }
}

#[test]
fn replay_and_update_noop_leave_reserved_and_unpromoted_runs_untouched() {
    for activated in [false, true] {
        let inv = Inventory::new(&["expected"]);
        let replay = inv.import("imported", "Original", false);
        let seeded = inv.call(replay.clone());
        assert_eq!(seeded["action"]["pack_updated"], true);
        let mut noop = replay.clone();
        noop["mode"] = json!("update");
        let run = inv.prepare(4);
        let probe = inv.next(&run);
        if activated {
            let before = inv.bytes();
            assert_eq!(
                inv.settle(&run, &probe)["action"]["relay"]["status"],
                "complete"
            );
            fs::write(inv.state(), before).unwrap();
        }
        let checkpoint = run.bytes();
        let before = inv.bytes();
        assert_eq!(inv.call(replay.clone())["action"]["already_imported"], true);
        assert_eq!(inv.call(noop)["action"]["pack_updated"], false);
        assert_eq!(inv.bytes(), before);
        assert_eq!(run.bytes(), checkpoint);

        let mut missing_source = read(&inv.state());
        missing_source["sources"]
            .as_array_mut()
            .unwrap()
            .retain(|source| source["selectors"]["type"] != "imported");
        write(&inv.state(), &missing_source);
        assert_eq!(inv.call(replay.clone())["action"]["already_imported"], true);
        assert!(
            read(&inv.state())["sources"]
                .as_array()
                .unwrap()
                .iter()
                .any(|source| source["selectors"]["type"] == "imported")
        );
        assert_eq!(run.bytes(), checkpoint);

        let active = path(&seeded["status"]["active_pack"]["path"]);
        let pack_bytes = fs::read(&active).unwrap();
        fs::remove_file(&active).unwrap();
        let before = inv.bytes();
        refused(inv.run(replay, &[]), "INVENTORY_INPUT");
        assert_eq!(inv.bytes(), before);
        assert_eq!(run.bytes(), checkpoint);
        fs::write(active, pack_bytes).unwrap();

        let completed = if activated {
            inv.poll(&run)
        } else {
            inv.settle(&run, &probe)
        };
        assert_eq!(completed["action"]["relay"]["status"], "complete");
        assert_eq!(path(&completed["status"]["active_pack"]["path"]), run.pack);
    }
}

#[test]
fn finished_runs_do_not_require_their_checkpoints() {
    for status in ["failed", "superseded", "complete"] {
        let inv = Inventory::new(&["expected"]);
        let run = inv.prepare(4);
        if status == "complete" {
            let probe = inv.next(&run);
            inv.settle(&run, &probe);
            fs::remove_file(&run.checkpoint).unwrap();
            fs::remove_file(&run.receipt).unwrap();
            assert!(run.pack.is_file());
        } else {
            let mut state = read(&inv.state());
            state["discovery_runs"][0]["status"] = json!(status);
            write(&inv.state(), &state);
            fs::rename(run.checkpoint.parent().unwrap(), inv.0.join("held")).unwrap();
        }
        let applied = inv.call(inv.import("imported", "Extra", false));
        assert_eq!(applied["action"]["pack_updated"], true);
        assert!(applied["action"].get("superseded_runs").is_none());
        assert!(!run.checkpoint.exists());
        if status == "complete" {
            assert!(run.pack.is_file());
            assert!(!run.receipt.exists());
        } else {
            assert!(!run.checkpoint.parent().unwrap().exists());
        }
    }
}

#[test]
fn missing_unfinished_checkpoint_refuses_before_creating_a_lock_directory() {
    let inv = Inventory::new(&["expected"]);
    let run = inv.prepare(4);
    let parent = run.checkpoint.parent().unwrap();
    let moved = inv.0.join("held");
    fs::rename(parent, &moved).unwrap();
    let request = override_import(inv.import("imported", "Extra", false));
    let diagnostic = inv.blocked(&request, 0);
    assert!(message(&diagnostic).contains(run.checkpoint.to_str().unwrap()));
    assert!(message(&diagnostic).contains("Restore the checkpoint folder"));
    assert!(!parent.exists());
    fs::rename(&moved, parent).unwrap();
    assert_eq!(inv.call(request)["action"]["superseded_runs"], json!([0]));
}

#[test]
fn inconsistent_unfinished_artifacts_cannot_be_overridden() {
    for problem in [
        "checkpoint",
        "reference",
        "pack",
        "receipt",
        "receipt-before-pack",
    ] {
        let inv = Inventory::new(&["expected"]);
        let run = inv.prepare(4);
        match problem {
            "checkpoint" => run.edit(|state| state["request_sha256"] = json!("invalid")),
            "reference" => {
                let mut state = read(&inv.state());
                state["discovery_runs"][0]["max_queries"] = json!(5);
                write(&inv.state(), &state);
            }
            _ => {
                let probe = inv.next(&run);
                let before = inv.bytes();
                inv.settle(&run, &probe);
                fs::write(inv.state(), before).unwrap();
                run.activation_phase("activating");
                match problem {
                    "pack" => {
                        fs::remove_file(&run.receipt).unwrap();
                        fs::write(&run.pack, b"unrelated bytes").unwrap();
                    }
                    "receipt" => fs::write(&run.receipt, b"unrelated receipt").unwrap(),
                    "receipt-before-pack" => fs::remove_file(&run.pack).unwrap(),
                    _ => unreachable!(),
                }
                assert_eq!(run.status(), "activating");
            }
        }
        let checkpoint = run.bytes();
        let request = inv.import("imported", "Extra", false);
        for action in [request.clone(), override_import(request)] {
            let diagnostic = inv.blocked(&action, 0);
            assert!(
                message(&diagnostic).contains("Restore the checkpoint folder"),
                "{diagnostic}"
            );
            assert!(
                !message(&diagnostic).contains("discovery-step"),
                "{diagnostic}"
            );
            assert_eq!(run.bytes(), checkpoint);
        }
    }
}

#[test]
fn complete_label_without_a_unique_matching_pack_still_requires_promotion() {
    let inv = Inventory::new(&["expected"]);
    let run = inv.prepare(4);
    let probe = inv.next(&run);
    let before = inv.bytes();
    inv.settle(&run, &probe);

    let mut state: Value = serde_json::from_slice(&before).unwrap();
    state["discovery_runs"][0]["status"] = json!("complete");
    write(&inv.state(), &state);
    let request = inv.import("imported", "Extra", false);
    assert!(message(&inv.blocked(&request, 0)).contains("discovery-step"));
    let promoted = inv.poll(&run);
    assert_eq!(path(&promoted["status"]["active_pack"]["path"]), run.pack);
    assert_eq!(inv.call(request)["action"]["pack_updated"], true);
}

#[test]
fn pending_work_can_read_a_completed_relay_after_the_import_guard_releases_it() {
    for registered in [true, false] {
        let inv = Inventory::new(&["baseline"]);
        inv.call(
            json!({"kind":"record-inventory-hints","environment":environment(),
            "hints":[{"source":{"type":"expected"}}]}),
        );
        inv.confirm(&["expected"]);
        let run = inv.prepare(4);
        let probe = inv.next(&run);
        inv.settle(&run, &probe);
        let mut state = read(&inv.state());
        assert!(state["pending_inventory"][0]["resolution"].is_object());
        state["pending_inventory"][0]["resolution"] = Value::Null;
        if !registered {
            state["packs"] = json!([]);
            state["active_pack"] = Value::Null;
        }
        write(&inv.state(), &state);
        let request = inv.import("imported", "Extra", false);
        let result = inv.call(if registered {
            request
        } else {
            override_import(request)
        });
        assert_eq!(result["action"]["pack_updated"], true);
        if registered {
            assert!(result["action"].get("superseded_runs").is_none());
        } else {
            assert_eq!(result["action"]["superseded_runs"], json!([0]));
        }
    }
}

fn stage(inv: &Inventory, run: &Run, phase: &str) -> Option<Value> {
    match phase {
        "needs-probes" => None,
        "paused" => {
            inv.pause(run);
            None
        }
        "failed" => {
            let before = inv.bytes();
            let reply = inv.call(json!({"kind":"next-probe","run_index":run.index}));
            let probe = &reply["action"]["probe"]["probe_id"];
            // Format 2 has no reservation ledger and records a terminal relay failure.
            run.edit(|state| {
                state["schema_version"] = json!(2);
                state.as_object_mut().unwrap().remove("session");
            });
            let result = inv.answer(run, probe, json!({"result":[],"error":"Synthetic failure"}));
            assert_eq!(result["action"]["relay"]["status"], "failed");
            fs::write(inv.state(), before).unwrap();
            None
        }
        _ => {
            let probe = inv.next(run);
            match phase {
                "reserved" => {}
                "cancel-requested" => {
                    inv.pause(run);
                }
                "cleanup-pending" => {
                    inv.answer(
                        run,
                        &probe,
                        json!({"result":[],"error":"Synthetic interruption"}),
                    );
                }
                "activation-blocked" => {
                    let _lock = storage::Writer::acquire(&run.pack).unwrap();
                    inv.settle(run, &probe);
                }
                "activated" | "activating" => {
                    let before = inv.bytes();
                    inv.settle(run, &probe);
                    fs::write(inv.state(), before).unwrap();
                    if phase == "activating" {
                        run.activation_phase(phase);
                        fs::remove_file(&run.receipt).unwrap();
                    }
                }
                _ => unreachable!(),
            }
            Some(probe)
        }
    }
}

#[test]
fn changed_import_classifies_each_relay_phase_for_current_and_earlier_producers() {
    for phase in [
        "activated",
        "activating",
        "activation-blocked",
        "needs-probes",
        "reserved",
        "paused",
        "cancel-requested",
        "cleanup-pending",
        "failed",
    ] {
        for earlier in [false, true] {
            let inv = Inventory::new(&["expected"]);
            let run = inv.prepare(4);
            let probe = stage(&inv, &run, phase);
            if earlier {
                run.edit(|state| state["producer_version"] = json!(EARLIER));
            }
            let relay_status = if phase == "reserved" {
                "needs-probes"
            } else {
                phase
            };
            assert_eq!(run.status(), relay_status);
            let request = inv.import("imported", "Extra", false);
            let checkpoint = run.bytes();
            let pack_before = fs::read(&run.pack).ok();
            let receipt_before = fs::read(&run.receipt).ok();
            if phase == "failed" {
                // A relay can have failed before its inventory status was updated.
                assert!(inv.call(request)["action"].get("superseded_runs").is_none());
            } else {
                let diagnostic = inv.blocked(&request, 0);
                let advice = message(&diagnostic);
                assert!(
                    advice.contains(&format!("run_index 0 ({relay_status})")),
                    "{advice}"
                );
                assert!(advice.contains("supersede_runs: true"), "{advice}");
                if earlier {
                    assert!(advice.contains(EARLIER), "{advice}");
                    assert_only_override(advice);
                } else if matches!(phase, "reserved" | "cancel-requested" | "cleanup-pending") {
                    assert!(
                        advice.contains(probe.as_ref().unwrap().as_str().unwrap()),
                        "{advice}"
                    );
                    assert!(advice.contains("confirmed cancellation"), "{advice}");
                    assert!(
                        advice.contains("before another discovery search"),
                        "{advice}"
                    );
                    assert!(
                        !advice.contains("pause-discovery") && !advice.contains("pause it"),
                        "{advice}"
                    );
                }
                assert_eq!(run.bytes(), checkpoint);
                assert_eq!(
                    inv.call(override_import(request))["action"]["superseded_runs"],
                    json!([0])
                );
            }
            assert_eq!(run.bytes(), checkpoint);
            assert_eq!(fs::read(&run.pack).ok(), pack_before);
            assert_eq!(fs::read(&run.receipt).ok(), receipt_before);

            // The first import moved the predecessor. A second distinct import can proceed.
            let displaced = inv.import("another", "Different", false);
            assert!(
                inv.call(displaced)["action"]
                    .get("superseded_runs")
                    .is_none()
            );
            assert_eq!(run.bytes(), checkpoint);
        }
    }
}

#[test]
fn current_run_recovery_advice_can_be_followed() {
    for phase in [
        "activated",
        "activating",
        "activation-blocked",
        "needs-probes",
        "reserved",
        "paused",
        "cancel-requested",
        "cleanup-pending",
    ] {
        let inv = Inventory::new(&["expected"]);
        let run = inv.prepare(4);
        let probe = stage(&inv, &run, phase);
        let request = inv.import("imported", "Extra", false);
        let diagnostic = inv.blocked(&request, 0);
        let advice = message(&diagnostic);
        let completed = match phase {
            "activated" | "activating" | "activation-blocked" => {
                assert!(
                    advice.contains("discovery-step (run_index: 0, answers: [])"),
                    "{advice}"
                );
                inv.poll(&run)
            }
            "needs-probes" | "paused" => {
                if phase == "paused" {
                    assert!(
                        advice.contains("resume-discovery (run_index: 0)"),
                        "{advice}"
                    );
                    assert_eq!(
                        inv.resume(&run)["action"]["relay"]["status"],
                        "needs-probes"
                    );
                } else {
                    assert!(advice.contains("next-probe (run_index: 0)"), "{advice}");
                }
                let probe = inv.next(&run);
                inv.settle(&run, &probe)
            }
            _ => {
                assert!(
                    advice.contains("original result via next-probe or discovery-step"),
                    "{advice}"
                );
                let settled = inv.settle(&run, &probe.unwrap());
                if settled["action"]["relay"]["status"] == "paused" {
                    inv.resume(&run)
                } else {
                    settled
                }
            }
        };
        assert_eq!(
            completed["action"]["relay"]["status"], "complete",
            "{phase}: {completed}"
        );
        assert_eq!(path(&completed["status"]["active_pack"]["path"]), run.pack);
        assert_eq!(inv.call(request)["action"]["pack_updated"], true);
    }
}

fn assert_only_override(advice: &str) {
    for command in [
        "discovery-step",
        "pause-discovery",
        "next-probe",
        "resume-discovery",
    ] {
        assert!(!advice.contains(command), "{advice}");
    }
    assert!(advice.contains("supersede_runs: true"), "{advice}");
}

#[test]
fn owner_format_plan_and_allowance_blockers_only_offer_override() {
    for (problem, expected) in [
        ("owner", "different inventory"),
        ("format-2", "format or plan"),
        ("format-unknown", "format or plan"),
        ("unknown-plan", "format or plan"),
        ("query-budget", "no remaining search allowance"),
        ("time-budget", "no remaining search allowance"),
    ] {
        let inv = Inventory::new(&["expected"]);
        let run = inv.prepare(1);
        let original_state = inv.state();
        let inv = if problem == "owner" {
            let copy = Inventory::new(&["expected"]);
            fs::copy(&original_state, copy.state()).unwrap();
            // Keep the original owner directory available for identity validation.
            check_noncontinuable(&copy, &run, expected);
            continue;
        } else {
            inv
        };
        match problem {
            "format-2" => run.edit(|state| {
                state["schema_version"] = json!(2);
                state.as_object_mut().unwrap().remove("session");
            }),
            "format-unknown" => run.edit(|state| state["schema_version"] = json!(99)),
            "unknown-plan" => run.edit(|state| state["discovery_algorithm"] = json!("unknown")),
            "query-budget" => {
                let probe = inv.next(&run);
                assert_eq!(
                    inv.answer(&run, &probe, json!({"result":[{"field":"unused"}]}))["action"]["relay"]
                        ["status"],
                    "paused"
                );
            }
            "time-budget" => {
                inv.pause(&run);
                run.edit(|state| {
                    // Periods are not part of the binding hash. Keep the start after creation,
                    // and consume the active-time limit without a clock-dependent wait.
                    state["created_at"] = json!(START);
                    state["session"]["periods"] = json!([{"start":START,"end":END}]);
                    let mut binding = serde_json::to_vec(&(
                        &state["request_sha256"],
                        &state["created_at"],
                        &state["pack"],
                        &state["receipt"],
                        &state["session"]["transport"],
                        &state["session"]["predecessor_sha256"],
                    ))
                    .unwrap();
                    if state["session"]["sparse_fieldset_fallback"] == true {
                        binding.extend_from_slice(b"\0sparse-fieldset-fallback-v1");
                    }
                    binding.extend_from_slice(b"\0inventory-owner-v1\0");
                    binding.extend_from_slice(
                        &serde_json::to_vec(&state["session"]["inventory_owner"]).unwrap(),
                    );
                    state["session"]["binding_sha256"] = json!(cqlmatic::sha256_hex(&binding));
                });
            }
            _ => unreachable!(),
        }
        check_noncontinuable(&inv, &run, expected);
    }
}

fn check_noncontinuable(inv: &Inventory, run: &Run, expected: &str) {
    let request = inv.import("imported", "Extra", false);
    let checkpoint = run.bytes();
    let diagnostic = inv.blocked(&request, 0);
    assert!(message(&diagnostic).contains(expected), "{diagnostic}");
    assert_only_override(message(&diagnostic));
    assert_eq!(
        inv.call(override_import(request))["action"]["superseded_runs"],
        json!([0])
    );
    assert_eq!(run.bytes(), checkpoint);
}

#[test]
fn override_keeps_reserved_search_unsettled_and_late_final_answer_records_superseded() {
    let inv = Inventory::new(&["expected"]);
    let run = inv.prepare(1);
    let probe = inv.next(&run);
    let request = inv.import("imported", "Extra", false);
    let full = inv.blocked(&request, 0);
    assert!(message(&full).contains(probe.as_str().unwrap()));
    let brief = refused(inv.run(request.clone(), &["--brief"]), "INVENTORY_INPUT");
    assert_eq!(brief["pointer"], "/discovery_runs/0");
    let advice = message(&brief);
    assert!(
        advice.contains("supersede_runs: true") && advice.contains("full output"),
        "{brief}"
    );
    for detail in [probe.as_str().unwrap(), inv.0.to_str().unwrap(), "expected"] {
        assert!(!advice.contains(detail), "{brief}");
    }
    let checkpoint = run.bytes();
    let before = read(&inv.state());
    let imported = inv.call(override_import(request));
    assert_eq!(imported["action"]["superseded_runs"], json!([0]));
    assert_eq!(run.bytes(), checkpoint);
    let after = read(&inv.state());
    assert!(after.get("supersede_runs").is_none());
    assert_eq!(
        serde_json::to_vec(&after["discovery_runs"]).unwrap(),
        serde_json::to_vec(&before["discovery_runs"]).unwrap()
    );
    let denied = refused(
        inv.run(Inventory::prepare_request(4), &[]),
        "INVENTORY_INPUT",
    );
    assert!(
        message(&denied).contains("unresolved termination or cleanup"),
        "{denied}"
    );
    assert_eq!(run.bytes(), checkpoint);

    let settled = inv.settle(&run, &probe);
    assert_eq!(settled["action"]["status"], "incomplete");
    assert_eq!(
        settled["action"]["diagnostics"][0]["code"],
        "DISCOVERY_SUPERSEDED"
    );
    assert_eq!(
        settled["status"]["discovery_runs"][0]["status"],
        "superseded"
    );
    assert_eq!(
        settled["status"]["active_pack"],
        imported["status"]["active_pack"]
    );
    assert_eq!(inv.prepare(4).index, 1);
}

#[test]
fn late_nonfinal_answer_is_saved_without_promoting() {
    let inv = Inventory::new(&["expected-a", "expected-b"]);
    let run = inv.prepare(4);
    let probe = inv.next(&run);
    let imported = inv.call(override_import(inv.import("imported", "Extra", false)));
    let settled = inv.settle(&run, &probe);
    assert_eq!(settled["action"]["relay"]["answered"], 1);
    assert_eq!(run.status(), "needs-probes");
    assert_eq!(
        settled["status"]["discovery_runs"][0]["status"],
        "needs-probes"
    );
    assert_eq!(
        settled["status"]["active_pack"],
        imported["status"]["active_pack"]
    );
    let relay = mcp::inspect_state(&run.checkpoint).unwrap();
    assert!(!mcp::has_in_flight(&relay));
}

#[test]
fn restoring_the_predecessor_bytes_allows_promotion() {
    let inv = Inventory::new(&["expected"]);
    let a = inv.import("imported", "A", false);
    let seed = inv.call(a);
    let base = fs::read(path(&seed["status"]["active_pack"]["path"])).unwrap();
    let run = inv.prepare(4);
    stage(&inv, &run, "activated");
    let checkpoint = run.bytes();

    let b = inv.import("imported", "B", true);
    inv.blocked(&b, 0);
    let changed = ok(inv.run(override_import(b), &["--summary"]));
    assert_eq!(changed["action"]["superseded_runs"], json!([0]));
    assert_ne!(
        fs::read(path(&changed["status"]["active_pack"]["path"])).unwrap(),
        base
    );

    let a_again = inv.import("imported", "A", true);
    let restored = inv.call(a_again);
    assert!(restored["action"].get("superseded_runs").is_none());
    assert_eq!(
        fs::read(path(&restored["status"]["active_pack"]["path"])).unwrap(),
        base
    );
    assert_eq!(run.bytes(), checkpoint);
    let promoted = inv.poll(&run);
    assert_eq!(
        promoted["status"]["discovery_runs"][0]["status"],
        "complete"
    );
    assert_eq!(path(&promoted["status"]["active_pack"]["path"]), run.pack);
}

#[test]
fn override_lists_only_matching_indices_and_brief_reports_the_count() {
    for projection in ["--summary", "--brief"] {
        let inv = Inventory::new(&["expected"]);
        let original = inv.bytes();
        let old = inv.prepare(4);
        stage(&inv, &old, "activated");
        let old_run = read(&inv.state())["discovery_runs"][0].clone();
        // Construct a second legitimate retained checkpoint from the same inventory state.
        fs::write(inv.state(), original).unwrap();
        let next = inv.prepare(5);
        assert_ne!(old.checkpoint, next.checkpoint);
        stage(&inv, &next, "activated");
        let mut state = read(&inv.state());
        state["discovery_runs"]
            .as_array_mut()
            .unwrap()
            .push(old_run);
        write(&inv.state(), &state);
        let checkpoints = [next.bytes(), old.bytes()];
        let request = inv.import("imported", "Extra", false);
        let denied = inv.blocked(&request, 0);
        assert!(message(&denied).contains("run_index 0 (activated)"));
        assert!(message(&denied).contains("run_index 1 (activated)"));
        let changed = inv.run(override_import(request), &[projection]);
        if projection == "--brief" {
            assert!(changed.stdout.len() <= 4096);
            let brief = ok(changed);
            assert_eq!(brief["action"]["superseded_run_count"], 2);
            assert!(brief["action"].get("superseded_runs").is_none());
        } else {
            assert_eq!(ok(changed)["action"]["superseded_runs"], json!([0, 1]));
        }
        assert_eq!([next.bytes(), old.bytes()], checkpoints);
    }
}

#[test]
fn a_second_promotion_displaces_an_older_unpromoted_checkpoint() {
    let inv = Inventory::new(&["expected"]);
    let original = inv.bytes();
    let old = inv.prepare(4);
    stage(&inv, &old, "activated");
    let old_run = read(&inv.state())["discovery_runs"][0].clone();
    fs::write(inv.state(), original).unwrap();
    let current = inv.prepare(5);
    let probe = inv.next(&current);
    inv.settle(&current, &probe);
    let mut state = read(&inv.state());
    state["discovery_runs"]
        .as_array_mut()
        .unwrap()
        .insert(0, old_run);
    write(&inv.state(), &state);
    let checkpoint = old.bytes();
    let imported = inv.call(inv.import("imported", "Extra", false));
    assert_eq!(imported["action"]["pack_updated"], true);
    assert!(imported["action"].get("superseded_runs").is_none());
    assert_eq!(old.bytes(), checkpoint);
}

#[test]
fn inventory_v4_fixture_round_trips_without_request_fields() {
    let fixture = json!({
        "format":"cqlmatic-inventory-state","schema_version":4,"created_at":START,
        "updated_at":END,"discovery_mode":"full","quick_depth":"sources-and-fields",
        "unverified_leads":[],"query_observations":[],"refresh_settings":{"automatic":false,"lookback":"24h"},
        "refresh_attempts":[],"source_metadata":[],"pending_inventory":[],"scout_plan":null,
        "scouting":[],"target":null,"sources":[],"parser_pages":[],"parsers":[],
        "parser_exclusions":[],"packs":[],"active_pack":null,"discovery_runs":[],
        "parser_runs":[],"failures":[],"reconciled_evidence_sha256":null,"reconciled_at":null
    });
    let stored: InventoryState = serde_json::from_value(fixture.clone()).unwrap();
    stored.validate().unwrap();
    assert_eq!(serde_json::to_value(stored).unwrap(), fixture);
}

#[test]
fn resume_can_promote_a_paused_final_answer_after_the_last_query() {
    let inv = Inventory::new(&["expected"]);
    let run = inv.prepare(1);
    let probe = inv.next(&run);
    inv.pause(&run);
    assert_eq!(
        inv.settle(&run, &probe)["action"]["relay"]["status"],
        "paused"
    );
    let request = inv.import("imported", "Extra", false);
    let denied = inv.blocked(&request, 0);
    assert!(message(&denied).contains("resume-discovery"), "{denied}");
    assert_eq!(inv.resume(&run)["action"]["relay"]["status"], "complete");
    assert_eq!(inv.call(request)["action"]["pack_updated"], true);
}

#[test]
fn duplicate_pack_references_do_not_mark_a_checkpoint_finished() {
    let inv = Inventory::new(&["expected"]);
    let run = inv.prepare(4);
    let probe = inv.next(&run);
    let before = inv.bytes();
    inv.settle(&run, &probe);
    let mut state: Value = serde_json::from_slice(&before).unwrap();
    state["discovery_runs"][0]["status"] = json!("complete");
    let reference = json!({"path":run.pack,"content_digest":read(&run.pack)["content_digest"],
        "imported_at":END,"discovery_run":run.checkpoint});
    state["active_pack"] = reference.clone();
    state["packs"] = json!([reference, reference]);
    write(&inv.state(), &state);
    let request = inv.import("imported", "Extra", true);
    fs::remove_file(&run.checkpoint).unwrap();
    let denied = inv.blocked(&request, 0);
    assert!(
        message(&denied).contains("The checkpoint is missing"),
        "{denied}"
    );
}

#[test]
fn corrupted_frozen_predecessor_refuses_import_for_activating_and_activated_runs() {
    for phase in ["activated", "activating"] {
        let inv = Inventory::new(&["expected"]);
        inv.call(inv.import("seed", "Seed", false));
        let run = inv.prepare(4);
        stage(&inv, &run, phase);

        // Corrupt one hex digit of activation.predecessor_pack_sha256 while keeping
        // session.predecessor_sha256, pack bytes, and receipt intact.
        run.edit(|state| {
            let digest = state["activation"]["predecessor_pack_sha256"]
                .as_str()
                .unwrap()
                .to_owned();
            let first = if digest.starts_with('0') { '1' } else { '0' };
            state["activation"]["predecessor_pack_sha256"] =
                json!(format!("{first}{}", &digest[1..]));
        });

        for supersede in [false, true] {
            let mut request = inv.import("imported", "Extra", false);
            request["supersede_runs"] = json!(supersede);
            let denied = inv.blocked(&request, 0);
            assert!(
                message(&denied).contains("disagrees with the bound session predecessor"),
                "phase={phase} supersede={supersede} denied={denied}"
            );
            assert!(
                message(&denied).contains("Restore the checkpoint folder"),
                "{denied}"
            );
        }
    }
}

#[test]
fn activation_blocked_with_stale_receipt_refuses_under_both_flags() {
    let inv = Inventory::new(&["expected"]);
    let run = inv.prepare(4);
    stage(&inv, &run, "activation-blocked");
    fs::write(&run.receipt, b"{\"stale\":true}\n").unwrap();

    for supersede in [false, true] {
        let mut request = inv.import("imported", "Extra", false);
        request["supersede_runs"] = json!(supersede);
        let denied = inv.blocked(&request, 0);
        assert!(
            message(&denied).contains("MCP receipt path must remain absent before activation"),
            "supersede={supersede} denied={denied}"
        );
        assert!(!message(&denied).contains("discovery-step"), "{denied}");
    }
}

#[test]
fn paused_run_withholds_resume_advice_when_sibling_blocks_continuing() {
    // Sibling with an outstanding reservation blocks resuming run 0.
    let inv = Inventory::new(&["expected"]);
    let run0 = inv.prepare(3);
    inv.pause(&run0);
    let run1 = inv.prepare(4);
    let _probe1 = inv.next(&run1);
    let request = inv.import("imported", "Extra", false);
    let denied = inv.blocked(&request, 0);
    let text = message(&denied);
    assert!(
        !text.contains("resume-discovery"),
        "must withhold blocked resume-discovery: {text}"
    );
    assert!(text.contains("run_index 1"), "{text}");
    assert!(text.contains("supersede_runs: true"), "{text}");

    // Same when sibling run 1 has an earlier producer version.
    let inv_earlier = Inventory::new(&["expected"]);
    let r0 = inv_earlier.prepare(3);
    inv_earlier.pause(&r0);
    let r1 = inv_earlier.prepare(4);
    let _p1 = inv_earlier.next(&r1);
    r1.edit(|state| {
        state["producer_version"] = json!(EARLIER);
    });
    let req_earlier = inv_earlier.import("imported", "Extra", false);
    let denied_earlier = inv_earlier.blocked(&req_earlier, 0);
    let text_earlier = message(&denied_earlier);
    assert!(!text_earlier.contains("resume-discovery"), "{text_earlier}");
    assert!(text_earlier.contains("run_index 1"), "{text_earlier}");
    let forced = inv_earlier.call(override_import(req_earlier));
    assert_eq!(forced["action"]["superseded_runs"], json!([0, 1]));
}

#[test]
fn replay_and_update_noop_refuse_missing_completed_pending_evidence() {
    let inv = Inventory::new(&["expected"]);
    let seed = inv.import("seed", "Seed", false);
    inv.call(seed.clone());
    inv.call(json!({
        "kind":"record-inventory-hints",
        "environment":environment(),
        "hints":[{"source":{"type":"expected"},"fields":["pending_field"]}]
    }));
    let run = inv.prepare(4);
    let probe = inv.next(&run);
    inv.settle(&run, &probe);
    assert_eq!(
        read(&inv.state())["discovery_runs"][0]["status"],
        "complete"
    );
    assert!(read(&inv.state())["pending_inventory"][0]["resolution"].is_null());

    // Remove only the completed run's checkpoint while keeping its pack and receipt.
    let backup = run.bytes();
    fs::remove_file(&run.checkpoint).unwrap();

    for update in [false, true] {
        let mut req = seed.clone();
        req["mode"] = json!(if update { "update" } else { "append" });
        let denied = inv.blocked(&req, 0);
        assert!(
            message(&denied).contains("Completed discovery evidence is missing"),
            "update={update} denied={denied}"
        );
    }

    // Restoring the checkpoint restores clean replay and update no-op.
    fs::write(&run.checkpoint, backup).unwrap();
    let replayed = inv.call(seed.clone());
    assert_eq!(replayed["action"]["pack_updated"], false);
    let mut noop_update = seed;
    noop_update["mode"] = json!("update");
    let unchanged = inv.call(noop_update);
    assert_eq!(unchanged["action"]["pack_updated"], false);
}

#[test]
fn late_settlement_during_cancel_or_cleanup_stays_paused_until_resumed_on_original_predecessor() {
    for (phase, sources) in [
        ("cancel-requested", vec!["expected"]),
        ("cancel-requested", vec!["first", "second"]),
        ("cleanup-pending", vec!["expected"]),
        ("cleanup-pending", vec!["first", "second"]),
    ] {
        let inv = Inventory::new(&sources);
        let seed_a = inv.import("seed_a", "Seed A", false);
        inv.call(seed_a.clone());
        let run = inv.prepare(4);
        let probe = inv.next(&run);
        if phase == "cancel-requested" {
            inv.pause(&run);
        } else {
            inv.answer(&run, &probe, json!({"error":"simulated timeout"}));
        }
        assert_eq!(run.status(), phase);

        // Displace with B using override.
        let mut import_b = inv.import("seed_b", "Seed B", false);
        import_b["supersede_runs"] = json!(true);
        let displaced = inv.call(import_b);
        assert_eq!(displaced["action"]["superseded_runs"], json!([0]));

        // Settling the outstanding attempt with its original result saves the answer
        // and leaves the run paused (preserving the pause/cleanup transition).
        let settled = inv.settle(&run, &probe);
        assert_eq!(
            settled["action"]["relay"]["status"], "paused",
            "phase={phase} sources={sources:?} settled={settled}"
        );
        assert_eq!(run.status(), "paused");

        // Resuming under B is refused because the predecessor changed.
        refused(
            inv.run(json!({"kind":"resume-discovery","run_index":0}), &[]),
            "INVENTORY_INPUT",
        );
    }
}
