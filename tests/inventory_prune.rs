use cqlmatic::{
    environment::{Pack, PackData},
    inventory::{DiscoveryRun, InventoryCaptureBoundary, InventoryRequest, InventoryState},
    mcp, storage,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const NOW: &str = "2026-01-02T00:00:00Z";

struct Inventory {
    root: PathBuf,
    state: PathBuf,
}

impl Inventory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "cqlmatic-retention-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        Self {
            state: root.join("inventory").join("state.json"),
            root,
        }
    }

    fn request(&self, action: Value, flags: &[&str]) -> Output {
        let request = self.root.join("request.json");
        fs::write(
            &request,
            serde_json::to_vec(&json!({"schema_version":1,"action":action})).unwrap(),
        )
        .unwrap();
        let binary = std::env::var_os("CQLMATIC_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_cqlmatic").into());
        Command::new(binary)
            .args(["env", "inventory", "--json", "--state"])
            .arg(&self.state)
            .arg("--request")
            .arg(request)
            .args(flags)
            .output()
            .unwrap()
    }

    fn read(&self) -> InventoryState {
        serde_json::from_slice(&fs::read(&self.state).unwrap()).unwrap()
    }

    fn write(&self, state: &InventoryState) {
        fs::create_dir_all(self.state.parent().unwrap()).unwrap();
        fs::write(&self.state, serde_json::to_vec_pretty(state).unwrap()).unwrap();
    }

    fn import_action(&self, day: u32) -> Value {
        let pack = Pack::new(
            serde_json::from_value(json!({"cloud":"us1","tenant":"0".repeat(32),"view":"search-all"})).unwrap(),
            format!("2026-01-{day:02}T00:00:00Z"),
            PackData {
                sources: vec![json!({"id":format!("env:repo:source-{day}"),"kind":"source",
                    "name":format!("Source {day}"),"inventory_status":"observed-only","selectors":{"type":format!("source-{day}")},
                    "provenance_ids":["prov:synthetic"]})],
                provenance: vec![json!({"id":"prov:synthetic","source_type":"mcp-observation",
                    "canonical_source":"Synthetic pack","retrieved_at":NOW,
                    "redaction_status":"none","coverage_status":"observed-only"})],
                ..Default::default()
            },
        ).unwrap();
        let input = self.root.join("pack.json");
        fs::write(&input, serde_json::to_vec(&pack).unwrap()).unwrap();
        json!({"kind":"import-pack","pack_file":input})
    }

    fn confirm(&self) {
        ok(self.request(
            json!({
                "kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"
            }),
            &[],
        ));
        ok(self.request(
            json!({
                "kind":"confirm-target",
                "environment":{"cloud":"us1","tenant":"0".repeat(32),"view":"search-all"},
                "scope":"selected-sources","context":"Synthetic sources",
                "expected_sources":[{"type":"discovered-source"}]
            }),
            &[],
        ));
    }

    fn prepare(&self) -> DiscoveryRun {
        let mut action = json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z",
            "end":NOW,"max_queries":4,"max_seconds":600});
        let index = self.read().discovery_runs.len();
        action["max_queries"] = json!(index + 4);
        ok(self.request(action, &[]));
        self.read().discovery_runs[index].clone()
    }

    fn settle(&self, index: usize, events: u64) -> Value {
        let next = ok(self.request(json!({"kind":"next-probe","run_index":index}), &[]));
        let id = &next["action"]["probe"]["probe_id"];
        assert!(id.is_string(), "{next}");
        let result = self.root.join("result.json");
        fs::write(&result, json!({"result":[{"events":events}]}).to_string()).unwrap();
        ok(self.request(
            json!({"kind":"discovery-step","run_index":index,"answers":[
                {"probe_id":id,"result_file":result}
            ]}),
            &[],
        ))
    }

    fn historical_run(&self) -> DiscoveryRun {
        self.confirm();
        let run = self.prepare();
        let reply = self.settle(0, 10);
        assert_eq!(
            reply["status"]["discovery_runs"][0]["status"], "complete",
            "{reply}"
        );
        ok(self.request(self.import_action(1), &[]));
        run
    }

    fn imports(&self) -> PathBuf {
        self.confirm();
        ok(self.request(self.import_action(1), &[]));
        ok(self.request(self.import_action(2), &[]));
        self.read().packs[0].path.clone().into()
    }
}

impl Drop for Inventory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn ok(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn prune(dry_run: bool) -> Value {
    json!({"kind":"prune","dry_run":dry_run})
}

#[test]
fn prune_contract() {
    let parse = |action| {
        serde_json::from_value::<InventoryRequest>(json!({"schema_version":1,"action":action}))
    };
    assert!(parse(json!({"kind":"prune","dry_run":true,"keep_last":1000})).is_ok());
    assert!(parse(json!({"kind":"prune","dry_run":true,"keep_last":1001})).is_err());
    assert!(parse(json!({"kind":"prune","dryrun":true})).is_err());
    assert!(parse(json!({"kind":"prune"})).is_err());
    assert!(parse(json!({"kind":"prune","dry_run":true,"extra":false})).is_err());
    let schema: Value = serde_json::from_str(include_str!("../schemas/inventory.json")).unwrap();
    let schema = schema["$defs"]["InventoryAction"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["kind"]["const"] == "prune")
        .unwrap();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["keep_last"]["maximum"], 1000);
    assert!(
        schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("dry_run"))
    );
    let inventory = Inventory::new();
    for dry in [true, false] {
        assert!(!inventory.request(prune(dry), &[]).status.success());
        assert!(!inventory.state.parent().unwrap().exists());
    }
    let old = inventory.imports();
    let before = fs::read(&inventory.state).unwrap();
    fs::remove_file(inventory.state.with_file_name(".state.json.lock")).unwrap();
    let preview = inventory.request(prune(true), &["--brief"]);
    assert!(preview.stdout.len() <= 4096);
    assert!(!String::from_utf8_lossy(&preview.stdout).contains(old.to_str().unwrap()));
    let preview = ok(preview);
    assert_eq!(preview["action"]["outcome"], "preview");
    assert_eq!(preview["action"]["reclaimed_bytes"], 0);
    assert!(preview["action"]["reclaimable_bytes"].as_u64().unwrap() > 0);
    assert_eq!(fs::read(&inventory.state).unwrap(), before);
    assert!(!inventory.state.with_file_name(".state.json.lock").exists());
    let applied = ok(inventory.request(prune(false), &["--summary"]));
    assert!(applied["action"].get("items").is_none());
    assert_eq!(
        applied["action"]["reclaimed_bytes"],
        preview["action"]["reclaimable_bytes"]
    );
    assert!(!old.exists());
}

#[test]
fn prune_liveness() {
    let inventory = Inventory::new();
    let old = inventory.imports();
    let mut keep = prune(false);
    keep["keep_last"] = json!(1);
    assert_eq!(
        ok(inventory.request(keep, &[]))["action"]["state_updated"],
        false
    );
    assert!(old.exists());
    let current = inventory.read().active_pack.unwrap().path;
    ok(inventory.request(prune(false), &[]));
    assert!(Path::new(&current).exists());

    let inventory = Inventory::new();
    inventory.confirm();
    let run = inventory.prepare();
    let checkpoint = Path::new(&run.state_path);
    let relay = mcp::inspect_state(checkpoint).unwrap();
    let boundary = InventoryCaptureBoundary::from_checkpoint(&run.state_path, &relay).unwrap();
    let mut state = inventory.read();
    // The before-answer boundary applies to run zero, even though after_run is one.
    let source = relay.request.sources[0].clone();
    let target = relay.request.environment.clone();
    state
        .record_inventory_hints_with_boundary(
            &target,
            vec![cqlmatic::inventory::InventoryHint {
                source: source.clone(),
                fields: vec!["later".into()],
            }],
            Some(boundary.clone()),
        )
        .unwrap();
    state.pending_inventory[0].fields.clear();
    state.pending_inventory[0].new_source = true;
    assert_eq!(state.pending_inventory[0].after_run, 1);
    inventory.write(&state);
    inventory.settle(0, 10);
    let proof = inventory.read().pending_inventory[0]
        .resolution
        .clone()
        .expect("post-boundary volume proof");
    ok(inventory.request(inventory.import_action(1), &[]));
    let mut pending = inventory.read();
    pending.pending_inventory[0].resolution = None;
    inventory.write(&pending);
    let kept = ok(inventory.request(prune(false), &[]));
    assert_eq!(kept["action"]["marked_count"], 0);
    assert!(
        kept["action"]["kept"]
            .as_array()
            .unwrap()
            .iter()
            .any(|unit| unit["reason"] == "pending-inventory-work")
    );
    assert!(checkpoint.exists());
    let mut wrong_depth = pending.clone();
    wrong_depth.pending_inventory[0].fields = vec!["later".into()];
    let mut invalid = pending.clone();
    invalid.discovery_runs[0].pruned_at = Some(NOW.into());
    invalid.packs[0].pruned_at = Some(NOW.into());
    assert!(invalid.validate().is_err());
    // Sources-only cannot supply field evidence.
    inventory.write(&wrong_depth);
    assert_eq!(
        ok(inventory.request(prune(true), &[]))["action"]["eligible_count"],
        1
    );
    inventory.write(&pending);
    ok(inventory.request(
        json!({
            "kind":"review-source","source_id":pending.pending_inventory[0].id,
            "purpose":"Synthetic volume","data_model":"Synthetic model",
            "reviewed_at":NOW,"evidence":"Reviewed the saved measurement"
        }),
        &[],
    ));
    let resolved = inventory.read();
    assert_eq!(
        serde_json::to_value(&resolved.pending_inventory[0].resolution).unwrap(),
        serde_json::to_value(Some(proof)).unwrap()
    );
    assert_eq!(
        ok(inventory.request(prune(false), &[]))["action"]["marked_count"],
        1
    );
    assert!(!checkpoint.exists());
    let after = inventory.read();
    after.validate().unwrap();
    assert_eq!(
        after.evidence_digest().unwrap(),
        resolved.evidence_digest().unwrap()
    );

    let inventory = Inventory::new();
    inventory.confirm();
    let run = inventory.prepare();
    let checkpoint = Path::new(&run.state_path);
    let original = mcp::inspect_state(checkpoint).unwrap();
    fs::remove_file(checkpoint).unwrap();
    mcp::init_resumable(
        original.request,
        checkpoint,
        Path::new(&run.pack_path),
        Path::new(&run.receipt_path),
        mcp::Transport::Mcp,
    )
    .unwrap();
    mcp::pause(checkpoint).unwrap();
    let before = fs::read(checkpoint).unwrap();
    for dry_run in [true, false] {
        assert_eq!(
            ok(inventory.request(prune(dry_run), &[]))["action"]["marked_count"],
            0
        );
        assert_eq!(fs::read(checkpoint).unwrap(), before);
    }
    assert_eq!(mcp::resume(checkpoint).unwrap().status, "needs-probes");

    let inventory = Inventory::new();
    inventory.confirm();
    let run = inventory.prepare();
    let checkpoint = Path::new(&run.state_path);
    let mut legacy: Value = serde_json::from_slice(&fs::read(checkpoint).unwrap()).unwrap();
    legacy["schema_version"] = json!(2);
    legacy.as_object_mut().unwrap().remove("session");
    fs::write(checkpoint, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let probe = mcp::step(checkpoint, &[]).unwrap().probes[0]["probe_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let answer = inventory.root.join("legacy-result.json");
    fs::write(&answer, br#"{"result":[{"events":10}]}"#).unwrap();
    mcp::step(checkpoint, &[(probe, answer)]).unwrap();
    let preview = ok(inventory.request(prune(true), &[]));
    assert_eq!(
        preview["action"]["eligible_count"], 0,
        "unpromoted standalone activation"
    );
    let mut state = inventory.read();
    state.discovery_runs[0].status = "failed".into();
    inventory.write(&state);
    assert!(
        !inventory.request(prune(false), &[]).status.success(),
        "contradictory failure"
    );
    state.discovery_runs[0].status = "complete".into();
    inventory.write(&state);
    assert!(
        !inventory.request(prune(false), &[]).status.success(),
        "missing promotion"
    );
    state.discovery_runs[0].status = "superseded".into();
    inventory.write(&state);
    assert_eq!(
        ok(inventory.request(prune(false), &[]))["action"]["marked_count"],
        1
    );
    assert!(!checkpoint.exists());

    let inventory = Inventory::new();
    inventory.confirm();
    let run = inventory.prepare();
    let reply = inventory.settle(0, 0);
    assert_eq!(
        reply["status"]["discovery_runs"][0]["status"], "failed",
        "{reply}"
    );
    let preview = ok(inventory.request(prune(true), &[]));
    assert!(
        preview["action"]["kept"]
            .as_array()
            .unwrap()
            .iter()
            .any(|unit| unit["reason"] == "failed-diagnostics-retained")
    );
    let receipt = Path::new(&run.receipt_path);
    fs::write(receipt, b"user receipt").unwrap();
    let mut failed = prune(false);
    failed["include_failed"] = json!(true);
    let applied = ok(inventory.request(failed, &[]));
    assert_eq!(applied["action"]["partial_remove_count"], 1);
    assert_eq!(fs::read(receipt).unwrap(), b"user receipt");
    assert!(Path::new(&run.state_path).exists());
    let mut state = inventory.read();
    let boundary = InventoryCaptureBoundary {
        run: run.state_path.clone(),
        request_sha256: "a".repeat(64),
        accepted_probe_ids: vec![],
    };
    let target = state.target.as_ref().unwrap().environment.clone();
    assert!(
        state
            .record_inventory_hints_with_boundary(&target, vec![], Some(boundary))
            .is_err()
    );

    let inventory = Inventory::new();
    inventory.imports();
    ok(inventory.request(inventory.import_action(3), &[]));
    let mut state = inventory.read();
    state.packs[0].imported_at = "2026-01-02T01:00:00+01:00".into();
    state.packs[1].imported_at = NOW.into();
    inventory.write(&state);
    let mut keep = prune(true);
    keep["keep_last"] = json!(1);
    let reply = ok(inventory.request(keep.clone(), &[]));
    assert_eq!(
        reply["action"]["items"][0]["path"],
        Path::new(&state.packs[1].path)
            .parent()
            .unwrap()
            .to_str()
            .unwrap()
    );
    state.packs[0].pruned_at = Some("2026-01-03T00:00:00Z".into());
    state.packs[0].cleanup_pending = true;
    inventory.write(&state);
    let reply = ok(inventory.request(keep, &[]));
    assert_eq!(reply["action"]["eligible_count"], 0);
    assert_eq!(reply["action"]["items"][0]["cleanup_pending"], true);

    // An update can restore a cumulative digest still held in a different directory.
    let inventory = Inventory::new();
    inventory.confirm();
    let original = inventory.import_action(1);
    let input = PathBuf::from(original["pack_file"].as_str().unwrap());
    let mut first = storage::read_pack(&input).unwrap();
    first.data.sources[0]["selectors"]["event_family"] = json!("Example");
    first.data.sources[0]["description"] = json!("Original description");
    first.content_digest = first.digest().unwrap();
    fs::write(&input, serde_json::to_vec(&first).unwrap()).unwrap();
    ok(inventory.request(original, &[]));
    let mut changed = first.clone();
    changed.data.sources[0]["selectors"]
        .as_object_mut()
        .unwrap()
        .remove("event_family");
    changed.data.sources[0]["description"] = json!("Changed description");
    changed.content_digest = changed.digest().unwrap();
    let update = json!({"kind":"import-pack","pack_file":input,"mode":"update"});
    fs::write(&input, serde_json::to_vec(&changed).unwrap()).unwrap();
    ok(inventory.request(update.clone(), &[]));
    fs::write(&input, serde_json::to_vec(&first).unwrap()).unwrap();
    ok(inventory.request(update, &[]));
    let state = inventory.read();
    assert_eq!(state.packs[0].content_digest, state.packs[2].content_digest);
    let old = Path::new(&state.packs[0].path);
    let active = Path::new(&state.packs[2].path);
    ok(inventory.request(prune(false), &[]));
    assert!(!old.exists());
    assert!(active.exists());
}

#[test]
fn prune_ownership() {
    let inventory = Inventory::new();
    let old = inventory.imports();
    let copy = inventory.state.with_file_name("backup");
    fs::hard_link(&inventory.state, &copy).unwrap();
    let action = ok(inventory.request(prune(false), &[]));
    assert_eq!(action["action"]["state_updated"], false);
    assert!(old.exists());
    fs::remove_file(copy).unwrap();
    ok(inventory.request(prune(false), &[]));
    assert!(!old.exists());

    let inventory = Inventory::new();
    let old = inventory.imports();
    let parent = inventory.state.parent().unwrap();
    fs::copy(&inventory.state, parent.join("copy")).unwrap();
    assert_eq!(
        ok(inventory.request(prune(true), &[]))["action"]["eligible_count"],
        0
    );
    fs::remove_file(parent.join("copy")).unwrap();
    fs::write(
        parent.join("other"),
        b"{\"format\":\"cqlmatic-inventory-state\",",
    )
    .unwrap();
    assert_eq!(
        ok(inventory.request(prune(true), &[]))["action"]["eligible_count"],
        0
    );
    fs::write(parent.join("other"), b"{\"format\":\"different\"}").unwrap();
    let oversized = fs::File::create(parent.join("unrelated")).unwrap();
    oversized
        .set_len(cqlmatic::inventory::MAX_STATE_BYTES + 1)
        .unwrap();
    drop(oversized);
    assert_eq!(
        ok(inventory.request(prune(true), &[]))["action"]["eligible_count"],
        1
    );
    let absolute = std::path::absolute(&inventory.state).unwrap();
    let key = cqlmatic::sha256_hex(absolute.to_str().unwrap().as_bytes());
    for name in [
        format!("inventory-{}-import-42", &key[..12]),
        "inventory-000000000000-import-42".into(),
    ] {
        let orphan = parent.join(name);
        fs::create_dir(&orphan).unwrap();
        fs::copy(&old, orphan.join("pack.json")).unwrap();
    }
    ok(inventory.request(prune(false), &[]));
    assert!(
        parent
            .join(format!("inventory-{}-import-42", &key[..12]))
            .join("pack.json")
            .exists()
    );
    assert!(
        parent
            .join("inventory-000000000000-import-42")
            .join("pack.json")
            .exists()
    );

    let mut state = inventory.read();
    let reserved = parent.join(format!("inventory-{}-import-3", &key[..12]));
    state.packs[0].path = reserved.join("pack.json").to_str().unwrap().into();
    inventory.write(&state);
    ok(inventory.request(inventory.import_action(3), &[]));
    assert!(!reserved.exists());
    assert!(
        Path::new(&inventory.read().packs[2].path)
            .starts_with(parent.join(format!("inventory-{}-import-4", &key[..12])))
    );

    let inventory = Inventory::new();
    inventory.historical_run();
    ok(inventory.request(prune(false), &[]));
    let key = cqlmatic::sha256_hex(inventory.state.to_str().unwrap().as_bytes());
    let reserved = inventory
        .state
        .parent()
        .unwrap()
        .join(format!("inventory-{}-discovery-2", &key[..12]));
    let mut history = inventory.read();
    let run = &mut history.discovery_runs[0];
    run.state_path = reserved.join("relay-state.json").to_str().unwrap().into();
    run.pack_path = reserved.join("pack.json").to_str().unwrap().into();
    run.receipt_path = reserved.join("receipt.json").to_str().unwrap().into();
    history.packs[0].path = run.pack_path.clone();
    history.packs[0].discovery_run = Some(run.state_path.clone());
    inventory.write(&history);
    let next = inventory.prepare();
    assert!(!reserved.exists());
    assert!(next.state_path.contains("-discovery-3"));

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        let mut inventory = Inventory::new();
        inventory.confirm();
        let physical = inventory.state.clone();
        let alias = inventory.root.join("aliased").join("state.json");
        symlink(physical.parent().unwrap(), alias.parent().unwrap()).unwrap();
        inventory.state = alias;
        ok(inventory.request(inventory.import_action(1), &[]));
        inventory.state = physical;
        ok(inventory.request(inventory.import_action(2), &[]));
        assert_eq!(
            ok(inventory.request(prune(true), &[]))["action"]["eligible_count"],
            1
        );
        let state = inventory.read();
        let old = Path::new(&state.packs[0].path);
        let directory = old.parent().unwrap();
        let external = inventory.root.join("external");
        fs::rename(directory, &external).unwrap();
        symlink(&external, directory).unwrap();
        let blocked = ok(inventory.request(prune(false), &[]));
        assert_eq!(blocked["action"]["state_updated"], false);
        assert!(external.join("pack.json").exists());
        fs::remove_file(directory).unwrap();
        fs::rename(&external, directory).unwrap();
        ok(inventory.request(prune(false), &[]));
        assert!(!directory.exists());
    }
}

#[test]
fn prune_roundtrip() {
    let inventory = Inventory::new();
    inventory.imports();
    let initial = inventory.read();
    for version in 1..=3 {
        let mut legacy = initial.clone();
        legacy.schema_version = version;
        inventory.write(&legacy);
        let raw = fs::read(&inventory.state).unwrap();
        let mut keep = prune(false);
        keep["keep_last"] = json!(1000);
        assert_eq!(
            ok(inventory.request(keep, &[]))["action"]["state_updated"],
            false
        );
        assert_eq!(fs::read(&inventory.state).unwrap(), raw);
    }
    let mut before = inventory.read();
    before.schema_version = 3;
    inventory.write(&before);
    let digest = before.evidence_digest().unwrap();
    ok(inventory.request(prune(false), &[]));
    let after = inventory.read();
    assert_eq!(after.schema_version, 4);
    assert_eq!(after.field_evidence_schema_version, Some(3));
    assert_eq!(after.evidence_digest().unwrap(), digest);
    assert!(after.packs[0].pruned_at.is_some());
    assert!(!after.packs[0].cleanup_pending);
    let mut invalid = after.clone();
    invalid.active_pack.as_mut().unwrap().pruned_at = Some(NOW.into());
    assert!(invalid.validate().is_err());
    let mut invalid = after.clone();
    invalid.packs[0].content_digest = "bad-digest".into();
    assert!(invalid.validate().is_err());
    let mut invalid = after;
    invalid.packs[0].pruned_at = None;
    invalid.packs[0].cleanup_pending = true;
    assert!(invalid.validate().is_err());

    let inventory = Inventory::new();
    inventory.historical_run();
    ok(inventory.request(prune(false), &[]));
    let state = inventory.read();
    state.validate().unwrap();
    for change in [
        "duplicate",
        "unowned",
        "half-marked",
        "superseded",
        "failed",
        "mismatched",
    ] {
        let mut invalid = state.clone();
        match change {
            "duplicate" => invalid.packs.push(invalid.packs[0].clone()),
            "unowned" => invalid.packs[0].discovery_run = Some("unowned".into()),
            "half-marked" => invalid.packs[0].pruned_at = None,
            "mismatched" => invalid.packs[0].cleanup_pending = true,
            status => invalid.discovery_runs[0].status = status.into(),
        }
        assert!(invalid.validate().is_err(), "{change}");
    }

    let inventory = Inventory::new();
    inventory.imports();
    ok(inventory.request(inventory.import_action(3), &[]));
    let replay_before = ok(inventory.request(inventory.import_action(2), &[]));
    assert_eq!(replay_before["action"]["already_imported"], true);
    ok(inventory.request(prune(false), &[]));
    let replay_after = ok(inventory.request(inventory.import_action(2), &[]));
    assert_eq!(replay_after["action"], replay_before["action"]);
}

#[test]
fn prune_readers() {
    let inventory = Inventory::new();
    let old = inventory.imports();
    ok(inventory.request(prune(false), &[]));
    assert!(!old.exists());
    let binary = env!("CARGO_BIN_EXE_cqlmatic");
    let report = ok(Command::new(binary)
        .args(["status", "--json", "--state"])
        .arg(&inventory.state)
        .output()
        .unwrap());
    assert!(!report.is_null());

    let inventory = Inventory::new();
    let run = inventory.historical_run();
    ok(inventory.request(prune(false), &[]));
    let checkpoint = Path::new(&run.state_path);
    let directory = checkpoint.parent().unwrap();
    for action in [
        json!({"kind":"next-probe"}),
        json!({"kind":"discovery-step","answers":[]}),
        json!({"kind":"dispatch-discovery","probe_id":"p-unused"}),
        json!({"kind":"pause-discovery"}),
        json!({"kind":"resume-discovery"}),
    ] {
        {
            let mut action = action;
            let output = inventory.request(action.clone(), &[]);
            assert!(!output.status.success());
            assert!(String::from_utf8_lossy(&output.stderr).contains("pruned"));
            action["run_index"] = json!(0);
            assert!(!inventory.request(action, &[]).status.success());
            assert!(!directory.exists());
        }
    }
    let mut state = inventory.read();
    let target = state.target.as_ref().unwrap().environment.clone();
    let boundary = InventoryCaptureBoundary {
        run: run.state_path.clone(),
        request_sha256: "a".repeat(64),
        accepted_probe_ids: vec![],
    };
    assert!(
        state
            .record_inventory_hints_with_boundary(&target, vec![], Some(boundary))
            .is_err()
    );
    ok(inventory.request(
        json!({"kind":"record-inventory-hints","environment":target,
        "hints":[{"source":{"type":"discovered-source"},"fields":["later"]}]}),
        &[],
    ));
    assert!(!directory.exists());
    let new = inventory.prepare();
    assert_ne!(new.state_path, run.state_path);
    ok(inventory.request(json!({"kind":"pause-discovery","run_index":1}), &[]));
    let resumed = ok(inventory.request(json!({"kind":"resume-discovery","run_index":1}), &[]));
    assert_eq!(resumed["action"]["run_index"], 1);
    assert_eq!(resumed["action"]["relay"]["status"], "needs-probes");
    assert!(!directory.exists());
}

#[test]
fn prune_recovery() {
    let inventory = Inventory::new();
    let old = inventory.imports();
    let bytes = fs::read(&old).unwrap();
    let inventory_guard = storage::Writer::acquire(&inventory.state).unwrap();
    let refused = inventory.request(prune(false), &[]);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("INVENTORY_PRUNE_BUSY"));
    drop(inventory_guard);
    let guard = storage::Writer::acquire(&old).unwrap();
    let before = fs::read(&inventory.state).unwrap();
    let busy = inventory.request(prune(false), &[]);
    assert!(!busy.status.success());
    assert!(
        String::from_utf8_lossy(&busy.stderr)
            .to_lowercase()
            .contains("retry")
    );
    assert_eq!(before, fs::read(&inventory.state).unwrap());
    drop(guard);
    ok(inventory.request(prune(false), &[]));
    fs::create_dir_all(old.parent().unwrap()).unwrap();
    fs::write(&old, &bytes).unwrap();
    let retry = ok(inventory.request(prune(false), &["--brief"]));
    assert_eq!(retry["action"]["outcome"], "applied");
    assert_eq!(retry["action"]["reclaimed_bytes"], 0);
    assert_eq!(retry["action"]["state_updated"], false);
    assert_eq!(fs::read(old).unwrap(), bytes);

    for absent in [
        "pack.json",
        "receipt.json",
        ".pack.json.cqlmatic-prune-detached",
    ] {
        let inventory = Inventory::new();
        let run = inventory.historical_run();
        let checkpoint = Path::new(&run.state_path);
        let directory = checkpoint.parent().unwrap();
        let receipt = Path::new(&run.receipt_path);
        let receipt_bytes = fs::read(receipt).unwrap();
        fs::remove_file(receipt).unwrap();
        let missing = inventory.request(prune(false), &[]);
        let error = String::from_utf8_lossy(&missing.stderr);
        assert!(
            error.contains("receipt.json")
                && error.contains("run_index 0")
                && !error.contains("reconcile"),
            "{error}"
        );
        fs::write(receipt, receipt_bytes).unwrap();
        for file in [&run.state_path, &run.pack_path, &run.receipt_path] {
            let held = storage::Writer::acquire(Path::new(file)).unwrap();
            let refused = inventory.request(prune(false), &[]);
            assert!(String::from_utf8_lossy(&refused.stderr).contains("INVENTORY_PRUNE_BUSY"));
            assert!(inventory.read().discovery_runs[0].pruned_at.is_none());
            drop(held);
        }
        let mut marked = inventory.read();
        marked.discovery_runs[0].pruned_at = Some(NOW.into());
        marked.discovery_runs[0].cleanup_pending = true;
        marked.packs[0].pruned_at = Some(NOW.into());
        marked.packs[0].cleanup_pending = true;
        inventory.write(&marked);
        if absent.starts_with('.') {
            fs::rename(&run.pack_path, directory.join(absent)).unwrap();
        } else {
            fs::remove_file(directory.join(absent)).unwrap();
        }
        assert!(mcp::inspect_state(checkpoint).is_err());
        let temporary = directory.join(".cqlmatic-user.tmp");
        fs::write(&temporary, b"local notes").unwrap();
        let partial = ok(inventory.request(prune(false), &["--brief"]));
        assert_eq!(partial["action"]["partial_remove_count"], 1);
        assert_eq!(fs::read(&temporary).unwrap(), b"local notes");
        assert!(inventory.read().discovery_runs[0].cleanup_pending);
        fs::remove_file(temporary).unwrap();
        let done = ok(inventory.request(prune(false), &[]));
        assert_eq!(done["action"]["removed_count"], 1);
        assert!(!directory.exists());
        assert!(!inventory.read().discovery_runs[0].cleanup_pending);
    }

    let inventory = Inventory::new();
    let old = inventory.imports();
    let mut state = inventory.read();
    let key = cqlmatic::sha256_hex(inventory.state.to_str().unwrap().as_bytes());
    for index in 3..=70 {
        let dir = inventory
            .state
            .parent()
            .unwrap()
            .join(format!("inventory-{}-import-{index}", &key[..12]));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("pack.json");
        fs::copy(&old, &path).unwrap();
        let mut historical = state.packs[0].clone();
        historical.path = path.to_str().unwrap().into();
        state.packs.push(historical);
    }
    inventory.write(&state);
    let first = ok(inventory.request(prune(false), &[]));
    assert_eq!(first["action"]["marked_count"], 64);
    assert_eq!(first["action"]["remaining_eligible"], 5);
    let second = ok(inventory.request(prune(false), &[]));
    assert_eq!(second["action"]["marked_count"], 5);
    assert_eq!(second["action"]["remaining_eligible"], 0);
}
