use super::*;

const RELEASED_WRITER_KNOWLEDGE_REVISION: &str =
    "b71fce37fea7286d253252ca224899b70ad35ba0ed84e8a2b2d986a0ad46284a";

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
    assert!(prepared["action"]["run_index"].is_number());
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
    assert_eq!(read(&state(dir))["schema_version"], 4);
    for run in 0..=index {
        let raw = read(&checkpoint(dir, run));
        assert_eq!(raw["producer_version"], PREVIOUS_PRODUCER);
        assert_eq!(raw["schema_version"], 3);
        assert!(raw["session"]["inventory_owner"].is_string());
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
    assert_eq!(pack["profile_id"], "cqlmatic-ngsiem-core-v3");
    assert_eq!(
        pack["knowledge_revision"],
        RELEASED_WRITER_KNOWLEDGE_REVISION
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
    assert_eq!(pack["profile_id"], "cqlmatic-ngsiem-core-v3");
    assert_eq!(
        pack["knowledge_revision"],
        RELEASED_WRITER_KNOWLEDGE_REVISION
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
    assert!(matches!(
        read(&state(dir))["field_evidence_schema_version"].as_u64(),
        None | Some(3)
    ));
    unchanged(&retained[1..]);
    for (path, _) in &retained[1..] {
        let raw = read(path);
        assert_eq!(raw["producer_version"], PREVIOUS_PRODUCER);
        assert_eq!(raw["schema_version"], 3);
        assert!(raw["session"]["inventory_owner"].is_string());
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

#[test]
#[ignore = "Requires CQLMATIC_PREVIOUS_WRITER_MANIFEST and builds the preserved release offline"]
fn genuine_previous_release() {
    let writer = released_writer();
    let reader = upgraded();
    let mut selected = 0;
    let mut guarded = 0;
    for phase in RELEASE_PHASES {
        let dir = Temp::new();
        let index = release_fixture(writer, &dir.0, phase);
        release_status_and_pack(reader, &dir.0);
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
            selected += 1;
        }
        for kind in ROUTES {
            let dir = Temp::new();
            release_fixture(writer, &dir.0, phase);
            let retained = snapshot(&dir.0);
            if matches!(phase, "activated" | "failed" | "paused") {
                release_allowed_route(reader, &dir.0, kind);
                unchanged(&retained[1..]);
            } else if kind.starts_with("prepare-") {
                reject(
                    reader,
                    &dir.0,
                    route(reader, &dir.0, kind),
                    "INVENTORY_INPUT",
                );
            } else {
                // An unresolved release search prevents preparing dispatch/resume work.
                continue;
            }
            guarded += 1;
        }
    }
    assert_eq!(selected, 20);
    assert_eq!(guarded, 35);
    eprintln!(
        "PASS genuine release: 20 selected actions, 35 guarded routes, 5 status/pack inspections; 10 dispatch/resume routes unavailable while termination is unresolved"
    );
}

#[test]
#[ignore = "Requires CQLMATIC_PREVIOUS_WRITER_MANIFEST and builds the preserved release offline"]
fn genuine_previous_release_resumes_with_its_inventory_owner() {
    let writer = released_writer();
    let reader = upgraded();
    let dir = Temp::new();
    let index = release_fixture(writer, &dir.0, "paused");
    let path = checkpoint(&dir.0, index);
    let before = snapshot(&dir.0);
    let output = Command::new(writer)
        .args(["env", "discover", "--mcp", "--state"])
        .arg(&path)
        .arg("--resume")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("DISCOVERY_INVENTORY_OWNER"));
    unchanged(&before);
    reject(
        reader,
        &dir.0,
        json!({"kind":"resume-discovery","run_index":index}),
        "MCP_STATE_PRODUCER",
    );
    let resumed = call(
        writer,
        &dir.0,
        json!({"kind":"resume-discovery","run_index":index}),
    );
    let completed = release_answer(writer, &dir.0, &resumed, json!({"result":[{"events":20}]}));
    assert_eq!(completed["action"]["relay"]["status"], "complete");
    release_status_and_pack(reader, &dir.0);
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
    assert!(prepared["action"]["run_index"].is_number());
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
    assert!(resumed["action"]["run_index"].is_number());
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
    assert_eq!(inventory["schema_version"], 4);
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
    assert_eq!(read(&state(&dir.0))["schema_version"], 4);
}
