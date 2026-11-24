//! Status words say what is done and what is still open.
use cqlmatic::{
    contracts::{Cloud, EnvironmentId, Scope},
    inventory::*,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-status-words-{}-{}",
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

fn state_path(dir: &Path) -> PathBuf {
    dir.join("workflow/state.json")
}
fn fixture(dir: &Path, name: &str, value: Value) -> String {
    let file = dir.join(name);
    fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
    file.to_string_lossy().into_owned()
}
fn inventory(dir: &Path, action: Value, flags: &[&str]) -> Output {
    let request = fixture(
        dir,
        "request.json",
        json!({"schema_version":1,"action":action}),
    );
    Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["env", "inventory", "--json", "--state"])
        .arg(state_path(dir))
        .arg("--request")
        .arg(request)
        .args(flags)
        .output()
        .unwrap()
}
fn success(output: Output) -> Vec<u8> {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}
fn call(dir: &Path, action: Value) -> Value {
    serde_json::from_slice(&success(inventory(dir, action, &[]))).unwrap()
}
fn brief_call(dir: &Path, action: Value) -> Value {
    let bytes = success(inventory(dir, action, &["--brief"]));
    assert!(bytes.len() <= 4096, "{}", bytes.len());
    serde_json::from_slice(&bytes).unwrap()
}
fn status(dir: &Path, flags: &[&str]) -> Vec<u8> {
    success(
        Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .arg("status")
            .args(flags)
            .arg("--state")
            .arg(state_path(dir))
            .output()
            .unwrap(),
    )
}
fn status_json(dir: &Path, flags: &[&str]) -> Value {
    let mut all = vec!["--json"];
    all.extend_from_slice(flags);
    serde_json::from_slice(&status(dir, &all)).unwrap()
}
fn status_text(dir: &Path) -> String {
    String::from_utf8(status(dir, &[])).unwrap()
}
fn environment() -> Value {
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"})
}
fn reconcile() -> Value {
    json!({"kind":"reconcile","reviewed_at":"2026-01-02T00:00:00Z"})
}
fn census(rows: usize) -> Value {
    let rows: Vec<_> = (0..rows)
        .map(|i| json!({"#repo":"synthetic","@sourcetype":format!("synthetic-label-{i}"),"events":1}))
        .collect();
    json!({"result":rows})
}

/// A confirmed Full target with one saved census and one completed discovery run. `reviewed`
/// also records the source review, a supported N/A parser classification and an empty catalog.
fn full_inventory(dir: &Path, census: Value, reviewed: bool, values: Option<Value>) -> Value {
    call(dir, json!({"kind":"init"}));
    let plan = call(
        dir,
        json!({"kind":"prepare-scout","environment":environment(),"start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z"}),
    );
    let scout = fixture(dir, "scout.json", census);
    call(
        dir,
        json!({"kind":"import-scout","result_file":scout,"arguments":plan["action"]["arguments"],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let confirmed = call(
        dir,
        json!({"kind":"confirm-target","environment":environment(),"scope":"full-environment","context":"Synthetic status words","expected_sources":[]}),
    );
    let source = confirmed["status"]["sources"][0]["source_id"].clone();
    if reviewed {
        call(
            dir,
            json!({"kind":"review-source","source_id":source,"purpose":"Synthetic native test","data_model":"Synthetic one-field model","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Explicit synthetic fixture"}),
        );
        call(
            dir,
            json!({"kind":"parser-not-applicable","source_id":source,"reason":"falcon-sensor-native","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Synthetic fixture represents a native source with no tenant parser"}),
        );
        let catalog = fixture(
            dir,
            "catalog.json",
            json!({"resources":[],"meta":{"pagination":{"total":0,"offset":0}}}),
        );
        call(
            dir,
            json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":catalog}],"details":[],"retrieved_at":"2026-01-02T00:00:00Z"}),
        );
    }
    let value_fields = if values.is_some() {
        json!(["user"])
    } else {
        json!([])
    };
    let mut progress = call(
        dir,
        json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","source_ids":[],"value_fields":value_fields,"kind_fields":[],"max_queries":30,"max_seconds":300}),
    );
    let mut answers = 0;
    while progress["action"]["relay"]["status"] == "needs-probes" {
        let probes = progress["action"]["relay"]["probes"]
            .as_array()
            .unwrap()
            .clone();
        for probe in probes {
            answers += 1;
            assert!(answers < 40);
            let rows = match probe["kind"].as_str().unwrap() {
                "volume" => json!([{"events":10}]),
                "fieldset" => json!([{"field":"user"}]),
                "dataset-facet" => json!([]),
                "population" => {
                    let mut row = json!({"total":10});
                    for (i, _) in probe["fields"].as_array().unwrap().iter().enumerate() {
                        row[format!("c{i}")] = json!(10);
                    }
                    json!([row])
                }
                "tag-detect" => json!([{"events":0}]),
                "value-facet" => values.clone().unwrap(),
                kind => panic!("unexpected probe {kind}"),
            };
            let result = fixture(
                dir,
                &format!("answer-{answers}.json"),
                json!({"result":rows}),
            );
            call(
                dir,
                json!({"kind":"dispatch-discovery","probe_id":probe["probe_id"]}),
            );
            progress = call(
                dir,
                json!({"kind":"discovery-step","answers":[{"probe_id":probe["probe_id"],"result_file":result}]}),
            );
        }
    }
    assert_eq!(
        progress["action"]["relay"]["status"], "complete",
        "{progress}"
    );
    source
}

const ROW_CEILING: usize = 10_000;

#[test]
fn reconcile_leaves_open_work_visible_in_every_status_projection() {
    let dir = Temp::new();
    full_inventory(&dir.0, census(1), false, None);
    call(&dir.0, reconcile());

    let full = status_json(&dir.0, &[]);
    // The reproduced state: reconciliation is current while catalog, parser and review are open.
    assert_eq!(full["reconciliation"], "done");
    assert_eq!(full["parser_catalog"], "unknown");
    assert_eq!(full["sources"][0]["interpretation"], "review");
    assert_eq!(full["sources"][0]["parser"], "pending");
    assert_eq!(full["sources"][0]["measurements"], "done");
    let detail = &full["completion_detail"];
    assert_eq!(detail["evidence_reviewed_at"], "2026-01-02T00:00:00Z");
    assert!(
        detail["reconciliation_meaning"]
            .as_str()
            .unwrap()
            .contains("not a completion claim"),
        "{detail}"
    );
    let counts = json!({
        "expected_sources_open":{"interpretation_review":1,"parser_review_or_binding":1,"measurements":0},
        "parser_catalog_captured":false,"scout_awaiting_capture":false,
        "latest_census":{"rows":1,"at_row_ceiling":false}
    });
    for key in [
        "expected_sources_open",
        "parser_catalog_captured",
        "scout_awaiting_capture",
        "latest_census",
    ] {
        assert_eq!(detail[key], counts[key], "{key}: {detail}");
    }

    let summary = status_json(&dir.0, &["--summary"]);
    assert_eq!(summary["completion_detail"], *detail);

    let brief = status_json(&dir.0, &["--brief"]);
    assert_eq!(brief["completion_detail"], counts);

    let text = status_text(&dir.0);
    assert!(
        text.contains(
            "Expected sources still open: interpretation review 1, parser review or binding 1, measurements 0; prepared census awaiting capture: no; latest census rows: 1"
        ),
        "{text}"
    );

    // A newer prepared census is visible until its capture is imported.
    call(
        &dir.0,
        json!({"kind":"prepare-scout","environment":environment(),"start":"2026-01-02T00:00:00Z","end":"2026-01-03T00:00:00Z"}),
    );
    let full = status_json(&dir.0, &[]);
    assert_eq!(full["scouting"], "done");
    assert_eq!(full["completion_detail"]["scout_awaiting_capture"], true);
    assert_eq!(full["completion_detail"]["latest_census"]["rows"], 1);
    // The prepared plan is part of the evidence, so the earlier review no longer covers it.
    assert_eq!(full["reconciliation"], "pending");
    assert_eq!(
        full["completion_detail"]["evidence_reviewed_at"],
        Value::Null
    );
    let brief = status_json(&dir.0, &["--brief"]);
    assert_eq!(brief["completion_detail"]["scout_awaiting_capture"], true);
}

#[test]
fn census_at_the_row_ceiling_does_not_complete_the_full_map_and_status_says_why() {
    let dir = Temp::new();
    full_inventory(&dir.0, census(ROW_CEILING), true, None);
    let reconciled = call(&dir.0, reconcile());
    let status = &reconciled["status"];
    // Every other full-map requirement is met, so the census is the only blocker.
    assert_eq!(status["sources"][0]["status"], "done", "{status}");
    assert_eq!(status["parser_catalog"], "done");
    assert_eq!(status["reconciliation"], "done");
    assert_eq!(status["scouting_evidence"][0]["row_count"], ROW_CEILING);
    assert_eq!(status["full_map_complete"], false, "{status}");
    assert_eq!(
        status["completion_detail"]["latest_census"],
        json!({"rows":ROW_CEILING,"at_row_ceiling":true})
    );
    let reason: Vec<_> = status["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|action| action.as_str().unwrap().contains("10,000-row ceiling"))
        .collect();
    assert_eq!(reason.len(), 1, "{status}");
    let summary = status_json(&dir.0, &["--summary"]);
    assert_eq!(summary["full_map_complete"], false);
    assert!(
        summary["next_actions"]
            .as_array()
            .unwrap()
            .contains(reason[0])
    );
    let brief = status_json(&dir.0, &["--brief"]);
    assert_eq!(brief["full_map_complete"], false);
    assert_eq!(
        brief["completion_detail"]["latest_census"]["at_row_ceiling"],
        true
    );
}

#[test]
fn census_below_the_row_ceiling_still_completes_the_full_map() {
    let dir = Temp::new();
    full_inventory(&dir.0, census(ROW_CEILING - 1), true, None);
    let reconciled = call(&dir.0, reconcile());
    assert_eq!(
        reconciled["status"]["full_map_complete"], true,
        "{reconciled}"
    );
    assert!(
        reconciled["status"]["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|action| !action.as_str().unwrap().contains("row ceiling"))
    );
}

#[test]
fn reconcile_reports_what_it_recorded_and_names_the_first_open_category() {
    let dir = Temp::new();
    let source = full_inventory(&dir.0, census(1), false, None);
    let reconciled = call(&dir.0, reconcile());
    let action = &reconciled["action"];
    let saved: Value = serde_json::from_slice(&fs::read(state_path(&dir.0)).unwrap()).unwrap();
    assert_eq!(action["kind"], "reconcile", "{reconciled}");
    assert_eq!(action["status"], "recorded");
    assert_eq!(
        action["evidence_sha256"],
        saved["reconciled_evidence_sha256"]
    );
    assert_eq!(action["reviewed_at"], "2026-01-02T00:00:00Z");
    assert_eq!(
        action["completion_detail"],
        reconciled["status"]["completion_detail"]
    );
    assert_eq!(
        action["completion_detail"]["expected_sources_open"],
        json!({"interpretation_review":1,"parser_review_or_binding":1,"measurements":0})
    );
    assert!(
        action["next_action"]
            .as_str()
            .unwrap()
            .contains("review-source")
    );

    // Nothing changed: the fingerprint was already current.
    let again = brief_call(&dir.0, reconcile());
    assert_eq!(again["action"]["outcome"], "already-current", "{again}");
    assert!(
        again["action"]["next_action"]
            .as_str()
            .unwrap()
            .contains("review-source")
    );
    let full_again = call(&dir.0, reconcile());
    assert_eq!(full_again["action"]["status"], "already-current");
    assert_eq!(
        full_again["action"]["evidence_sha256"],
        action["evidence_sha256"]
    );

    call(
        &dir.0,
        json!({"kind":"review-source","source_id":source,"purpose":"Synthetic native test","data_model":"Synthetic one-field model","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Explicit synthetic fixture"}),
    );
    let recorded = brief_call(&dir.0, reconcile());
    assert_eq!(recorded["action"]["outcome"], "recorded", "{recorded}");
    assert!(
        recorded["action"]["next_action"]
            .as_str()
            .unwrap()
            .contains("import-parsers")
    );

    let catalog = fixture(
        &dir.0,
        "catalog.json",
        json!({"resources":[],"meta":{"pagination":{"total":0,"offset":0}}}),
    );
    call(
        &dir.0,
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":catalog}],"details":[],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let next = call(&dir.0, reconcile())["action"]["next_action"].clone();
    assert!(next.as_str().unwrap().contains("bind-parser"), "{next}");

    call(
        &dir.0,
        json!({"kind":"parser-not-applicable","source_id":source,"reason":"falcon-sensor-native","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Synthetic fixture represents a native source with no tenant parser"}),
    );
    let done = call(&dir.0, reconcile());
    assert_eq!(done["status"]["full_map_complete"], true, "{done}");
    assert!(
        done["action"]["next_action"]
            .as_str()
            .unwrap()
            .contains("full_map_complete")
    );
}

#[test]
fn empty_value_sample_is_distinguishable_from_saved_values() {
    for (rows, saved) in [
        (json!([]), false),
        (json!([{"user":"synthetic-user","events":2}]), true),
    ] {
        let dir = Temp::new();
        full_inventory(&dir.0, census(1), false, Some(rows));
        let full = status_json(&dir.0, &[]);
        let user = full["sources"][0]["fields"]
            .as_array()
            .unwrap()
            .iter()
            .find(|field| field["field"] == "user")
            .unwrap()
            .clone();
        assert_eq!(user["values_requested"], true);
        assert_eq!(user["values"], "done");
        assert_eq!(user["values_saved"], saved, "{user}");
    }
}

#[test]
fn text_status_qualifies_done_words() {
    let dir = Temp::new();
    full_inventory(&dir.0, census(1), true, None);
    call(&dir.0, reconcile());
    let text = status_text(&dir.0);
    assert!(
        text.contains(
            "scouting: census saved; parser catalog: captured (0); reconciliation: evidence review current"
        ),
        "{text}"
    );
    assert!(
        text.contains(": run complete, pack activated; queries "),
        "{text}"
    );
    for bare in [
        "scouting: DONE",
        "parser catalog: DONE",
        "reconciliation: DONE",
        ": complete; queries",
    ] {
        assert!(!text.contains(bare), "{bare}: {text}");
    }
}

/// Written by the 0.1.13 release with `env inventory`: init, prepare-scout, import-scout,
/// confirm-target and reconcile. No pack is referenced.
const RELEASED_STATE: &str = r#"{"format":"cqlmatic-inventory-state","schema_version":4,"created_at":"2026-11-24T04:36:19.27143Z","updated_at":"2026-11-24T04:36:19.3728Z","discovery_mode":"full","quick_depth":"sources-and-fields","unverified_leads":[],"query_observations":[],"refresh_settings":{"automatic":false,"lookback":"24h"},"refresh_attempts":[],"refresh_reminder":{"environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"},"baseline_at":"2026-11-24T04:36:19.358663Z","notified_cycle_sha256":null,"defer_until":null},"source_metadata":[],"pending_inventory":[],"scout_plan":{"environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"},"arguments":{"repository":"search-all","query_string":"groupBy([#repo, #type, @sourcetype], function=count(as=events), limit=max)","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z"},"query_sha256":"c1036bc5b30d89142a1f2d17d493bb1977f382da8f06f2f472f25264491d6f7d"},"scouting":[{"plan":{"environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"},"arguments":{"repository":"search-all","query_string":"groupBy([#repo, #type, @sourcetype], function=count(as=events), limit=max)","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z"},"query_sha256":"c1036bc5b30d89142a1f2d17d493bb1977f382da8f06f2f472f25264491d6f7d"},"snapshot":{"sha256":"a8672e2e1458639b3bd4ec05599348887fbd40938fc4af10c11f1dcb2b552bc1","retrieved_at":"2026-01-02T00:00:00Z","raw_hex":"7b22726573756c74223a205b7b22237265706f223a202273796e746865746963222c20226576656e7473223a2031307d5d7d"},"rows":1}],"target":{"environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"},"scope":"full-environment","context":"Synthetic released state","confirmed_at":"2026-11-24T04:36:19.358663Z"},"sources":[{"id":"env:repo:a81ea3223a5156e8","selectors":{"repo":"synthetic"},"scouted_events":10,"expected":true,"review":null,"binding":null}],"parser_pages":[],"parsers":[],"parser_exclusions":[],"packs":[],"active_pack":null,"discovery_runs":[],"parser_runs":[],"failures":[],"reconciled_evidence_sha256":"9e2be6b524155be7f4b9f8b4ff099b88274798878c6abd4135d6eb9f7e30837d","reconciled_at":"2026-01-02T00:00:00Z"}"#;

#[test]
fn state_written_by_the_previous_release_keeps_its_reconciliation() {
    let dir = Temp::new();
    fs::create_dir(dir.0.join("workflow")).unwrap();
    fs::write(state_path(&dir.0), RELEASED_STATE).unwrap();
    let full = status_json(&dir.0, &[]);
    assert_eq!(full["reconciliation"], "done", "{full}");
    assert_eq!(
        full["completion_detail"]["evidence_reviewed_at"],
        "2026-01-02T00:00:00Z"
    );
    let reconciled = call(&dir.0, reconcile());
    assert_eq!(reconciled["action"]["status"], "already-current");
}

fn library_environment() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "synthetic-view".into(),
    }
}

#[test]
fn brief_with_every_notice_and_the_largest_counts_stays_within_the_bound() {
    const NOW: &str = "2026-01-02T00:00:00Z";
    let host = serde_json::to_vec(&json!({
        "schema_version":1,"environment":library_environment(),"session_id":"synthetic-session",
        "verified_at":NOW,"mechanism":"provider-enforced-search-lifetime","max_search_seconds":60,
        "capability_reference":"synthetic provider capability document",
        "verification_evidence":"Synthetic host verified a provider search lifetime, independently of client wait time"
    }))
    .unwrap();
    const LATER: &str = "2026-01-02T00:00:01Z";
    let settlement = |state: &InventoryState, id: &str, termination: &str, recorded_at: &str| {
        let attempt = state.refresh_attempts.iter().find(|a| a.id == id).unwrap();
        serde_json::to_vec(&json!({
            "schema_version":1,"attempt_id":id,"environment":library_environment(),
            "session_id":attempt.session_id,"arguments":attempt.arguments,"termination":termination,
            "recorded_at":recorded_at,"search_started_at":attempt.dispatched_at,
            "result_sha256":null,"evidence":"Synthetic exact-attempt host termination evidence"
        }))
        .unwrap()
    };
    let mut state = InventoryState::new(NOW).unwrap();
    state
        .confirm(
            library_environment(),
            TargetScope::FullEnvironment,
            "Synthetic largest brief".into(),
            vec![Scope {
                repo: Some("synthetic".into()),
                type_: Some("known".into()),
                event: None,
            }],
            NOW,
        )
        .unwrap();
    for _ in 0..199 {
        let id = state
            .prepare_refresh("synthetic-session".into(), &host, false, NOW)
            .unwrap();
        let report = settlement(&state, &id, "not-started", NOW);
        state.settle_refresh(&id, &report, NOW).unwrap();
    }
    state
        .record_inventory_hints(
            &library_environment(),
            (0..1000)
                .map(|index| InventoryHint {
                    source: Scope {
                        repo: Some(format!("synthetic-{index}")),
                        ..Scope::default()
                    },
                    fields: vec![],
                })
                .collect(),
        )
        .unwrap();
    let id = state
        .prepare_refresh("synthetic-session".into(), &host, false, NOW)
        .unwrap();
    state
        .dispatch_refresh(&id, "synthetic-session", NOW)
        .unwrap();
    let report = settlement(&state, &id, "unresolved", LATER);
    state.settle_refresh(&id, &report, LATER).unwrap();
    let mut status =
        serde_json::to_value(assess_at(&state, &[], "2026-02-01T00:00:00Z").unwrap()).unwrap();
    for index in 0..7 {
        status["discovery_runs"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "state_path":format!("/synthetic/run-{index}"),"pack_path":"/synthetic/pack",
                "receipt_path":"/synthetic/receipt","start":"2026-01-01T00:00:00Z","end":NOW,
                "source_ids":[],"value_fields":[],"max_queries":40,"max_seconds":300,"answered":0,
                "elapsed_seconds":1.0,"status":"cleanup-pending"
            }));
    }
    let detail = &mut status["completion_detail"];
    let most = usize::MAX;
    detail["expected_sources_open"] = json!({
        "interpretation_review":most,"parser_review_or_binding":most,"measurements":most
    });
    detail["scout_awaiting_capture"] = json!(true);
    detail["latest_census"] = json!({"rows":most,"at_row_ceiling":true});
    let mut status: InventoryStatus = serde_json::from_value(status).unwrap();
    // An unresolved refresh shows its long settlement notice; a blocked capability replaces it.
    for (capability_blocked, notices) in [
        (
            false,
            [
                "REFRESH_SETTLEMENT_REQUIRED",
                "DISCOVERY_RECOVERY_RECORDED",
                "INVENTORY_REFRESH_DUE",
                "INVENTORY_WORK_PENDING",
            ]
            .as_slice(),
        ),
        (
            true,
            [
                "CAPABILITY_SETTLEMENT_REQUIRED",
                "REFRESH_RESERVED",
                "DISCOVERY_RECOVERY_RECORDED",
                "INVENTORY_REFRESH_DUE",
                "INVENTORY_WORK_PENDING",
            ]
            .as_slice(),
        ),
    ] {
        status.capability_blocked = capability_blocked;
        let report = brief(&status, Some("synthetic-session"));
        let codes: Vec<_> = report["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|notice| notice["code"].as_str().unwrap())
            .collect();
        assert_eq!(codes, notices, "{report}");
        assert_eq!(report["references"].as_array().unwrap().len(), 5);
        assert_eq!(
            report["completion_detail"]["expected_sources_open"]["measurements"],
            usize::MAX
        );
        let bytes = serde_json::to_vec(&report).unwrap();
        // The CLI appends one newline.
        assert!(bytes.len() < 4096, "{} bytes", bytes.len());
    }
}
