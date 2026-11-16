use super::*;

const RELEASE_PHASES: [&str; 5] = [
    "activated",
    "paused",
    "cancel-requested",
    "cleanup-pending",
    "failed",
];

fn confirm_release_target(writer: &Path, dir: &Path, two_sources: bool) {
    let sources = if two_sources {
        json!([{"type":"synthetic"},{"type":"synthetic-second"}])
    } else {
        json!([{"type":"synthetic"}])
    };
    call(
        writer,
        dir,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic discovery target","expected_sources":sources}),
    );
}

fn release_answer(writer: &Path, dir: &Path, prepared: &Value, response: Value) -> Value {
    assert!(prepared["action"].get("run_index").is_none());
    let probe = prepared["action"]["relay"]["probes"][0]["probe_id"].clone();
    assert!(probe.is_string());
    call(
        writer,
        dir,
        json!({"kind":"dispatch-discovery","probe_id":probe}),
    );
    let file = dir.join("release-answer.json");
    write(&file, &response);
    call(
        writer,
        dir,
        json!({"kind":"discovery-step","answers":[{"probe_id":probe,"result_file":file}]}),
    )
}

fn release_fixture(writer: &Path, dir: &Path, phase: &str) -> usize {
    call(
        writer,
        dir,
        json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
    );
    confirm_release_target(writer, dir, false);
    // Every fixture starts with a real completed release pack, including failed histories.
    let baseline = prepare(writer, dir, 4);
    let completed = release_answer(writer, dir, &baseline, json!({"result":[{"events":10}]}));
    assert_eq!(completed["action"]["relay"]["status"], "complete");
    if phase == "failed" {
        confirm_release_target(writer, dir, true);
    }
    let prepared = prepare(writer, dir, if phase == "failed" { 1 } else { 5 });
    let index = read(&state(dir))["discovery_runs"]
        .as_array()
        .unwrap()
        .len()
        - 1;
    match phase {
        "activated" | "failed" | "cleanup-pending" => {
            let response = if phase == "cleanup-pending" {
                json!({"result":[],"error":"Synthetic timeout"})
            } else {
                json!({"result":[{"events":10}]})
            };
            let result = release_answer(writer, dir, &prepared, response);
            assert_eq!(
                result["action"]["relay"]["status"],
                if phase == "activated" {
                    "complete"
                } else {
                    phase
                }
            );
        }
        "paused" | "cancel-requested" => {
            if phase == "cancel-requested" {
                call(
                    writer,
                    dir,
                    json!({"kind":"dispatch-discovery","probe_id":prepared["action"]["relay"]["probes"][0]["probe_id"]}),
                );
            }
            let result = call(writer, dir, json!({"kind":"pause-discovery"}));
            assert_eq!(result["action"]["relay"]["status"], phase);
        }
        _ => unreachable!(),
    }
    assert_eq!(read(&state(dir))["schema_version"], 3);
    for run in 0..=index {
        let raw = read(&checkpoint(dir, run));
        assert_eq!(raw["producer_version"], PREVIOUS_PRODUCER);
        assert_eq!(raw["schema_version"], 3);
        assert!(raw["session"].get("inventory_owner").is_none());
    }
    let retained = read(&checkpoint(dir, index));
    assert_eq!(retained["status"], phase);
    if phase == "failed" {
        assert_eq!(retained["request"]["max_queries"], 1);
        assert_eq!(retained["request"]["sources"].as_array().unwrap().len(), 2);
        assert_eq!(retained["failure"]["code"], "DISCOVERY_EVIDENCE");
    }
    let inventory = read(&state(dir));
    let pack = read(Path::new(
        inventory["active_pack"]["path"].as_str().unwrap(),
    ));
    assert_eq!(pack["producer_version"], PREVIOUS_PRODUCER);
    assert_eq!(pack["profile_id"], "cqlmatic-ngsiem-core-v2");
    assert_eq!(
        pack["knowledge_revision"],
        "2158f99fc529e234d5fd6fd4d4af899d7d83a848"
    );
    assert_eq!(
        inventory["active_pack"]["content_digest"],
        pack["content_digest"]
    );
    let attempts = retained["session"]["attempts"].as_array().unwrap();
    match phase {
        "paused" => assert!(attempts.is_empty()),
        "activated" | "failed" => {
            assert_eq!(attempts.len(), 1);
            assert_eq!(attempts[0]["outcome"], "complete");
        }
        "cancel-requested" => assert_eq!(attempts[0]["outcome"], "dispatched"),
        "cleanup-pending" => assert_eq!(attempts[0]["outcome"], "cleanup-pending"),
        _ => unreachable!(),
    }
    index
}

fn release_status_and_pack(reader: &Path, dir: &Path) -> Value {
    let before = snapshot(dir);
    let inventory = read(&state(dir));
    let pack_path = PathBuf::from(inventory["active_pack"]["path"].as_str().unwrap());
    let bytes = fs::read(&pack_path).unwrap();
    let pack = read(&pack_path);
    assert_eq!(pack["producer_version"], PREVIOUS_PRODUCER);
    assert_eq!(pack["profile_id"], "cqlmatic-ngsiem-core-v2");
    assert_eq!(
        pack["knowledge_revision"],
        "2158f99fc529e234d5fd6fd4d4af899d7d83a848"
    );
    assert_eq!(
        inventory["active_pack"]["content_digest"],
        pack["content_digest"]
    );
    let output = Command::new(reader)
        .args(["env", "inventory", "--json", "--state"])
        .arg(state(dir))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["active_pack"], inventory["active_pack"]);
    let output = Command::new(reader)
        .args(["env", "inspect"])
        .arg(&pack_path)
        .arg("--full")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let inspected: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(inspected, pack);
    unchanged(&before);
    assert_eq!(fs::read(&pack_path).unwrap(), bytes);
    pack
}

fn release_migrated(dir: &Path, retained: &[(PathBuf, Vec<u8>)]) {
    assert_eq!(read(&state(dir))["schema_version"], 4);
    assert_eq!(read(&state(dir))["field_evidence_schema_version"], 3);
    unchanged(&retained[1..]);
    for (path, _) in &retained[1..] {
        let raw = read(path);
        assert_eq!(raw["producer_version"], PREVIOUS_PRODUCER);
        assert_eq!(raw["schema_version"], 3);
        assert!(raw["session"].get("inventory_owner").is_none());
    }
}

fn release_allowed_route(reader: &Path, dir: &Path, kind: &str) {
    assert!(matches!(
        read(&state(dir))["schema_version"].as_u64(),
        Some(3 | 4)
    ));
    let retained = snapshot(dir);
    let action = match kind {
        "dispatch-discovery" | "resume-discovery" => {
            let prepared = prepare(reader, dir, 6);
            release_migrated(dir, &retained);
            let index = prepared["action"]["run_index"].clone();
            assert!(index.is_number());
            if kind == "resume-discovery" {
                call(
                    reader,
                    dir,
                    json!({"kind":"pause-discovery","run_index":index}),
                );
                json!({"kind":kind,"run_index":index})
            } else {
                json!({"kind":kind,"run_index":index,"probe_id":prepared["action"]["relay"]["probes"][0]["probe_id"]})
            }
        }
        "dispatch-refresh" => {
            let prepared = call(
                reader,
                dir,
                json!({"kind":"prepare-refresh","session_id":"synthetic-session","host_evidence_file":host(dir),"automatic":false}),
            );
            release_migrated(dir, &retained);
            json!({"kind":kind,"attempt_id":prepared["action"]["attempt_id"],"session_id":"synthetic-session"})
        }
        "dispatch-capability" | "resume-capabilities" => {
            let prepared = call(reader, dir, capability(dir));
            release_migrated(dir, &retained);
            let run_id = prepared["action"]["capability"]["run_id"].clone();
            assert!(run_id.is_string());
            if kind == "resume-capabilities" {
                call(
                    reader,
                    dir,
                    json!({"kind":"pause-capabilities","run_id":run_id}),
                );
                json!({"kind":kind,"run_id":run_id,"execution_context_id":"synthetic-auth"})
            } else {
                json!({"kind":kind,"run_id":run_id,"case_id":"array-union","execution_context_id":"synthetic-auth","session_id":"synthetic-session"})
            }
        }
        _ => route(reader, dir, kind),
    };
    // Dispatch/resume start from v4 only after their required current preparation above.
    call(reader, dir, action);
    release_migrated(dir, &retained);
}

fn release_blocked(reader: &Path, dir: &Path, index: usize) -> Value {
    let before = snapshot(dir);
    let output = command(reader, dir, scout());
    unchanged(&before);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "MCP_STATE_PRODUCER");
    assert_eq!(diagnostic["pointer"], format!("/discovery_runs/{index}"));
    diagnostic
}

fn release_standalone(writer: &Path, path: &Path, args: &[String], code: i32) -> Value {
    let output = Command::new(writer)
        .args(["env", "discover", "--mcp", "--state"])
        .arg(path)
        .args(args)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

fn release_recover_checkpoint(writer: &Path, dir: &Path, index: usize) -> PathBuf {
    let path = checkpoint(dir, index);
    let original = read(&path);
    assert_eq!(original["producer_version"], PREVIOUS_PRODUCER);
    assert_eq!(original["schema_version"], 3);
    assert_eq!(original["status"], "paused");
    assert!(original["session"].get("inventory_owner").is_none());
    assert!(
        original["session"]["attempts"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let inventory = fs::read(state(dir)).unwrap();
    let before = read(&state(dir));
    assert_eq!(before["discovery_runs"][index]["status"], "paused");
    assert_eq!(before["discovery_runs"][index]["answered"], 0);
    let active_path = Path::new(before["active_pack"]["path"].as_str().unwrap());
    let active_bytes = fs::read(active_path).unwrap();
    let resumed = release_standalone(writer, &path, &["--resume".into()], 3);
    assert_eq!(resumed["status"], "needs-probes");
    assert_eq!(resumed["max_queries"], original["request"]["max_queries"]);
    let probe = resumed["probes"][0]["probe_id"].as_str().unwrap();
    let dispatched = release_standalone(writer, &path, &["--dispatch".into(), probe.into()], 3);
    assert_eq!(dispatched["status"], "needs-probes");
    let answer = dir.join(format!("recovery-answer-{index}.json"));
    write(&answer, &json!({"result":[{"events":20}]}));
    let completed = release_standalone(
        writer,
        &path,
        &["--answer".into(), format!("{probe}={}", answer.display())],
        0,
    );
    assert_eq!(completed["status"], "complete");
    assert_eq!(completed["answered"], 1);
    let recovered = read(&path);
    assert_eq!(recovered["status"], "activated");
    assert_eq!(recovered["producer_version"], PREVIOUS_PRODUCER);
    assert_eq!(recovered["schema_version"], 3);
    for field in ["request", "request_sha256", "created_at", "pack", "receipt"] {
        assert_eq!(recovered[field], original[field], "{field}");
    }
    assert_eq!(
        recovered["session"]["binding_sha256"],
        original["session"]["binding_sha256"]
    );
    let prior_periods = original["session"]["periods"].as_array().unwrap();
    let periods = recovered["session"]["periods"].as_array().unwrap();
    assert_eq!(&periods[..prior_periods.len()], prior_periods);
    assert_eq!(periods.len(), prior_periods.len() + 1);
    assert!(periods.iter().all(|period| period["end"].is_string()));
    let attempts = recovered["session"]["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["outcome"], "complete");
    assert!(attempts.len() < original["request"]["max_queries"].as_u64().unwrap() as usize);
    assert_eq!(recovered["answers"].as_object().unwrap().len(), 1);
    let pack_path = PathBuf::from(recovered["pack"].as_str().unwrap());
    let receipt = read(Path::new(recovered["receipt"].as_str().unwrap()));
    assert_eq!(receipt["active_pack_updated"], true);
    assert_eq!(receipt["request_sha256"], original["request_sha256"]);
    assert_eq!(receipt["captures"][0]["receipt"]["events"][0]["events"], 20);
    let pack = read(&pack_path);
    assert_eq!(pack["producer_version"], PREVIOUS_PRODUCER);
    assert_ne!(pack_path, active_path);
    assert_eq!(fs::read(state(dir)).unwrap(), inventory);
    assert_eq!(read(&state(dir))["active_pack"], before["active_pack"]);
    assert_eq!(
        read(&state(dir))["discovery_runs"],
        before["discovery_runs"]
    );
    assert_eq!(fs::read(active_path).unwrap(), active_bytes);
    let baseline_receipt = read(Path::new(
        before["discovery_runs"][0]["receipt_path"]
            .as_str()
            .unwrap(),
    ));
    assert_eq!(
        baseline_receipt["captures"][0]["receipt"]["events"][0]["events"],
        10
    );
    pack_path
}

#[test]
#[ignore = "Requires CQLMATIC_PREVIOUS_WRITER_MANIFEST and builds the preserved release offline"]
fn genuine_previous_release_recovers_after_inventory_hint_migration() {
    let writer = released_writer();
    let reader = upgraded();
    let mut blocked = Value::Null;
    for kind in ROUTES {
        let dir = Temp::new();
        let index = release_fixture(writer, &dir.0, "paused");
        // The release can resume its latest run while the inventory is still v3.
        let resumed = call(writer, &dir.0, json!({"kind":"resume-discovery"}));
        assert_eq!(resumed["action"]["relay"]["status"], "needs-probes");
        call(writer, &dir.0, json!({"kind":"pause-discovery"}));
        blocked = release_blocked(reader, &dir.0, index);
        let retained = snapshot(&dir.0);
        let checked = Command::new(reader)
            .args(["check", "--json", "--inventory-state"])
            .arg(state(&dir.0))
            .arg("#repo=search-all | user=*")
            .output()
            .unwrap();
        assert_eq!(checked.status.code(), Some(0), "{checked:?}");
        release_migrated(&dir.0, &retained);
        assert!(
            !read(&state(&dir.0))["unverified_leads"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        reject(
            writer,
            &dir.0,
            json!({"kind":"resume-discovery"}),
            "INVENTORY_INPUT",
        );
        let recovered_pack = release_recover_checkpoint(writer, &dir.0, index);
        if kind == "prepare-scout" {
            let retained = snapshot(&dir.0);
            let active = read(&state(&dir.0))["active_pack"].clone();
            let active_path = Path::new(active["path"].as_str().unwrap());
            let active_bytes = fs::read(active_path).unwrap();
            let recovered_bytes = fs::read(&recovered_pack).unwrap();
            let checked = Command::new(reader)
                .args(["check", "--json", "--pack"])
                .arg(&recovered_pack)
                .arg("--inventory-state")
                .arg(state(&dir.0))
                .arg("#type=synthetic | count()")
                .output()
                .unwrap();
            assert_eq!(checked.status.code(), Some(0), "{checked:?}");
            let result: Value = serde_json::from_slice(&checked.stdout).unwrap();
            assert_eq!(result["outcome"], "ready");
            assert_eq!(
                result["grounding"]["pack_id"],
                read(&recovered_pack)["content_digest"]
            );
            unchanged(&retained);
            reject(
                reader,
                &dir.0,
                json!({"kind":"import-pack","pack_file":recovered_pack}),
                "PACK_INTEGRITY",
            );
            assert_eq!(read(&state(&dir.0))["active_pack"], active);
            assert_eq!(fs::read(active_path).unwrap(), active_bytes);
            assert_eq!(fs::read(&recovered_pack).unwrap(), recovered_bytes);
            eprintln!(
                "PASS genuine release recovered pack: explicit-pack covered query is ready; conflicting import is rejected atomically without inventory completion credit"
            );
        }
        release_allowed_route(reader, &dir.0, kind);
        eprintln!(
            "PASS genuine release hint-migration recovery route={kind}: standalone completion, unchanged inventory history and active pack, finite original allowance"
        );
    }
    let message = blocked["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("standalone") && message.contains("checkpoint"),
        "Producer recovery diagnostic must identify standalone checkpoint recovery: {blocked}"
    );
}

#[test]
#[ignore = "Requires CQLMATIC_PREVIOUS_WRITER_MANIFEST and builds the preserved release offline"]
fn genuine_previous_release_recovers_older_and_latest_paused_checkpoints() {
    let writer = released_writer();
    let reader = upgraded();
    let mut blocked = Value::Null;
    for kind in ROUTES {
        let dir = Temp::new();
        let older = release_fixture(writer, &dir.0, "paused");
        let prepared = prepare(writer, &dir.0, 6);
        assert!(prepared["action"].get("run_index").is_none());
        let latest = older + 1;
        call(writer, &dir.0, json!({"kind":"pause-discovery"}));
        let older_bytes = fs::read(checkpoint(&dir.0, older)).unwrap();
        let resumed = call(writer, &dir.0, json!({"kind":"resume-discovery"}));
        assert_eq!(resumed["action"]["relay"]["status"], "needs-probes");
        assert_eq!(read(&checkpoint(&dir.0, latest))["status"], "needs-probes");
        assert_eq!(fs::read(checkpoint(&dir.0, older)).unwrap(), older_bytes);
        call(writer, &dir.0, json!({"kind":"pause-discovery"}));
        assert_eq!(read(&state(&dir.0))["schema_version"], 3);
        blocked = release_blocked(reader, &dir.0, older);
        let latest_bytes = fs::read(checkpoint(&dir.0, latest)).unwrap();
        release_recover_checkpoint(writer, &dir.0, older);
        assert_eq!(fs::read(checkpoint(&dir.0, latest)).unwrap(), latest_bytes);
        release_blocked(reader, &dir.0, latest);
        release_recover_checkpoint(writer, &dir.0, latest);
        release_allowed_route(reader, &dir.0, kind);
        eprintln!(
            "PASS genuine release multiple-paused recovery route={kind}: old inventory resumes latest only; each checkpoint independently recovered before reader admission"
        );
    }
    let message = blocked["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("standalone") && message.contains("checkpoint"),
        "Producer recovery diagnostic must identify standalone checkpoint recovery: {blocked}"
    );
}

#[test]
#[ignore = "Requires CQLMATIC_PREVIOUS_WRITER_MANIFEST and builds the preserved release offline"]
fn genuine_previous_release_recovery_requires_settled_inventory_searches() {
    let writer = released_writer();
    let reader = upgraded();
    let dir = Temp::new();
    let index = release_fixture(writer, &dir.0, "paused");
    let prepared = call(
        writer,
        &dir.0,
        json!({"kind":"prepare-refresh","session_id":"synthetic-session","host_evidence_file":host(&dir.0),"automatic":false}),
    );
    let attempt = prepared["action"]["attempt_id"].clone();
    assert!(attempt.is_string());
    let dispatched = call(
        writer,
        &dir.0,
        json!({"kind":"dispatch-refresh","attempt_id":attempt,"session_id":"synthetic-session"}),
    );
    assert_eq!(dispatched["action"]["phase"], "dispatched");
    let inventory = read(&state(&dir.0));
    assert_eq!(inventory["schema_version"], 3);
    assert_eq!(inventory["refresh_attempts"][0]["id"], attempt);
    assert_eq!(inventory["refresh_attempts"][0]["phase"], "dispatched");
    assert!(inventory["refresh_attempts"][0]["dispatched_at"].is_string());
    assert!(inventory["refresh_attempts"][0]["settlement"].is_null());
    assert_eq!(read(&checkpoint(&dir.0, index))["status"], "paused");
    let blocked = release_blocked(reader, &dir.0, index);
    let message = blocked["message"].as_str().unwrap().to_lowercase();
    assert!(
        message.contains("outstanding inventory searches")
            && message.contains("concurrent inventory execution"),
        "Producer recovery diagnostic must require settling searches and preventing concurrent inventory execution: {blocked}"
    );
}

#[test]
#[ignore = "Requires CQLMATIC_PREVIOUS_WRITER_MANIFEST and builds the preserved release offline"]
fn genuine_previous_release_rejects_duplicate_completed_retry() {
    let writer = released_writer();
    let reader = upgraded();
    let dir = Temp::new();
    call(
        writer,
        &dir.0,
        json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
    );
    confirm_release_target(writer, &dir.0, false);
    let prepared = prepare(writer, &dir.0, 4);
    assert!(prepared["action"].get("run_index").is_none());
    let probe = prepared["action"]["relay"]["probes"][0]["probe_id"].clone();
    call(
        writer,
        &dir.0,
        json!({"kind":"dispatch-discovery","probe_id":probe}),
    );
    let paused = call(writer, &dir.0, json!({"kind":"pause-discovery"}));
    assert_eq!(paused["action"]["relay"]["status"], "cancel-requested");
    let path = checkpoint(&dir.0, 0);
    let dispatched = read(&path);
    let attempt = &dispatched["session"]["attempts"][0];
    let proof = dir.0.join("cancel.json");
    write(
        &proof,
        &json!({
            "schema_version":1,
            "request_sha256":dispatched["request_sha256"],
            "probe_id":probe,
            "repository":dispatched["request"]["environment"]["view"],
            "query_sha256":cqlmatic::sha256_hex(attempt["probe"]["query"].as_str().unwrap().as_bytes()),
            "start":attempt["probe"]["start"],
            "end":attempt["probe"]["end"],
            "dispatched_at":attempt["dispatched_at"],
            "outcome":"cancelled",
            "cleanup":"confirmed",
            "caller_assertion":"exact-host-attempt-terminal-cleanup"
        }),
    );
    let settled = Command::new(writer)
        .args(["env", "discover", "--mcp", "--state"])
        .arg(&path)
        .arg("--settle-cancelled")
        .arg(format!("{}={}", probe.as_str().unwrap(), proof.display()))
        .output()
        .unwrap();
    assert_eq!(settled.status.code(), Some(3), "{settled:?}");
    let settled: Value = serde_json::from_slice(&settled.stdout).unwrap();
    assert_eq!(settled["status"], "paused");
    let resumed = call(writer, &dir.0, json!({"kind":"resume-discovery"}));
    assert!(resumed["action"].get("run_index").is_none());
    assert_eq!(resumed["action"]["relay"]["probes"][0]["probe_id"], probe);
    let completed = release_answer(writer, &dir.0, &resumed, json!({"result":[{"events":10}]}));
    assert_eq!(completed["action"]["relay"]["status"], "complete");

    let retained = snapshot(&dir.0);
    let original = read(&path);
    assert_eq!(original["producer_version"], PREVIOUS_PRODUCER);
    assert_eq!(original["schema_version"], 3);
    assert_eq!(original["status"], "activated");
    assert_eq!(original["request"]["max_queries"], 4);
    assert_eq!(original["session"]["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(original["session"]["attempts"][0]["outcome"], "cancelled");
    assert!(original["session"]["attempts"][0]["terminal"].is_object());
    assert_eq!(original["session"]["attempts"][1]["outcome"], "complete");
    assert_eq!(original["answers"].as_object().unwrap().len(), 1);
    let periods = original["session"]["periods"].as_array().unwrap();
    assert_eq!(periods.len(), 2);
    assert!(periods.iter().all(|period| period["end"].is_string()));
    let inventory = read(&state(&dir.0));
    assert_eq!(inventory["schema_version"], 3);
    assert_eq!(inventory["discovery_runs"][0]["max_queries"], 4);
    assert_eq!(inventory["discovery_runs"][0]["answered"], 1);
    assert!(inventory["active_pack"]["path"].is_string());
    release_status_and_pack(reader, &dir.0);
    call(reader, &dir.0, scout());
    release_migrated(&dir.0, &retained);
    assert_eq!(read(&path), original);

    // Preserve the released inventory schema for the rejected retained-history read.
    fs::write(state(&dir.0), &retained[0].1).unwrap();
    let mut altered = original;
    altered["session"]["attempts"][0]["outcome"] = json!("complete");
    altered["session"]["attempts"][0]["terminal"] = Value::Null;
    write(&path, &altered);
    let before = snapshot(&dir.0);
    let output = command(reader, &dir.0, scout());
    assert_eq!(
        output.status.code(),
        Some(2),
        "Duplicate completed attempts for one saved answer must be rejected: {output:?}"
    );
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "MCP_STATE_TAMPERED");
    unchanged(&before);
    assert_eq!(read(&state(&dir.0))["schema_version"], 3);
}

#[test]
#[ignore = "Requires CQLMATIC_PREVIOUS_WRITER_MANIFEST and builds the preserved release offline"]
fn genuine_previous_release() {
    let writer = released_writer();
    let reader = upgraded();
    for (label, binary) in [("writer", writer), ("reader", reader)] {
        let version = Command::new(binary).arg("--version").output().unwrap();
        assert!(version.status.success());
        let catalog = Command::new(binary)
            .args(["catalog", "--profile"])
            .output()
            .unwrap();
        assert!(catalog.status.success(), "{catalog:?}");
        let profile: Value = serde_json::from_slice(&catalog.stdout).unwrap();
        eprintln!(
            "genuine release {label}: {} profile={} knowledge={}",
            String::from_utf8_lossy(&version.stdout).trim(),
            profile["profile_id"],
            profile["knowledge_revision"]
        );
    }
    let mut guarded_cases = 0;
    let mut unavailable_routes = 0;
    let mut selected_cases = 0;
    let mut inspection_cases = 0;
    for phase in RELEASE_PHASES {
        let allowed = matches!(phase, "activated" | "failed");
        let phase_guards = if allowed { 9 } else { 4 };
        let phase_unavailable = if allowed { 0 } else { 5 };
        eprintln!(
            "RUN genuine release phase={phase}: {phase_guards} retained-guard routes, {phase_unavailable} unavailable dispatch/resume routes, 4 selected actions, status/pack inspection"
        );
        let dir = Temp::new();
        let index = release_fixture(writer, &dir.0, phase);
        let pack = release_status_and_pack(reader, &dir.0);
        inspection_cases += 1;
        eprintln!(
            "release provenance phase={phase}: producer={} relay_format=3 inventory_schema=3 profile={} knowledge={} digest={}",
            pack["producer_version"],
            pack["profile_id"],
            pack["knowledge_revision"],
            pack["content_digest"]
        );
        for kind in [
            "dispatch-discovery",
            "resume-discovery",
            "discovery-step",
            "pause-discovery",
        ] {
            let mut action = json!({"kind":kind,"run_index":index});
            if kind == "dispatch-discovery" {
                action["probe_id"] = json!("p-0000000000000000");
            } else if kind == "discovery-step" {
                action["answers"] = json!([]);
            }
            reject(reader, &dir.0, action, "MCP_STATE_PRODUCER");
            assert_eq!(read(&state(&dir.0))["schema_version"], 3);
            selected_cases += 1;
        }
        for kind in ROUTES {
            if !allowed && !kind.starts_with("prepare-") {
                eprintln!(
                    "SKIP genuine release phase={phase} route={kind}: blocking old history prevents the current preparation required for a valid dispatch/resume"
                );
                unavailable_routes += 1;
                continue;
            }
            let dir = Temp::new();
            release_fixture(writer, &dir.0, phase);
            let inventory = read(&state(&dir.0));
            let pack_path = PathBuf::from(inventory["active_pack"]["path"].as_str().unwrap());
            let pack_bytes = fs::read(&pack_path).unwrap();
            if allowed {
                release_allowed_route(reader, &dir.0, kind);
            } else {
                reject(
                    reader,
                    &dir.0,
                    route(reader, &dir.0, kind),
                    "MCP_STATE_PRODUCER",
                );
                assert_eq!(read(&state(&dir.0))["schema_version"], 3);
            }
            assert_eq!(fs::read(&pack_path).unwrap(), pack_bytes, "{phase}: {kind}");
            assert_eq!(
                read(&state(&dir.0))["active_pack"],
                inventory["active_pack"]
            );
            guarded_cases += 1;
        }
        eprintln!(
            "PASS genuine release phase={phase}: {phase_guards} retained-guard routes, 4 selected actions, status/pack inspection; {phase_unavailable} unavailable routes skipped"
        );
    }
    eprintln!(
        "SKIP bound foreign-owner history: released 0.1.6 cannot create inventory_owner bindings"
    );
    eprintln!(
        "SKIP current unowned-sibling association creation by old writer: released 0.1.6 cannot produce current-format siblings"
    );
    assert_eq!(guarded_cases, 30);
    assert_eq!(unavailable_routes, 15);
    assert_eq!(selected_cases, 20);
    assert_eq!(inspection_cases, 5);
    eprintln!(
        "PASS genuine release totals: {guarded_cases} retained-guard routes, {selected_cases} selected actions, {inspection_cases} status/pack inspections; SKIP {unavailable_routes} unavailable routes and 2 unsupported feature categories"
    );
}
