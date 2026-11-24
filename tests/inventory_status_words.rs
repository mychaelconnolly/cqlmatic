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
    assert_eq!(bytes.last(), Some(&b'\n'));
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
    json!({"cloud":"us1","tenant":"0".repeat(32),"view":"synthetic-view"})
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

fn scout_request(environment: Value, start: &str, end: &str, quick: bool) -> Value {
    let mut request =
        json!({"kind":"prepare-scout","environment":environment,"start":start,"end":end});
    if quick {
        request["max_seconds"] = json!(60);
    }
    request
}

fn scout_import(dir: &Path, plan: &Value, response: Value) -> Value {
    let scout = fixture(dir, "scout.json", response);
    json!({"kind":"import-scout","result_file":scout,"arguments":plan["action"]["arguments"],
        "retrieved_at":plan["action"]["arguments"]["end"]})
}

fn confirm(dir: &Path, scope: &str, expected: Value) -> Value {
    call(
        dir,
        json!({"kind":"confirm-target","environment":environment(),"scope":scope,
        "context":"Synthetic status words","expected_sources":expected}),
    )
}

fn discovery_request(values: bool) -> Value {
    json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z",
        "source_ids":[],"value_fields":if values { vec!["user"] } else { vec![] },
        "kind_fields":[],"max_queries":30,"max_seconds":300})
}

fn review(dir: &Path, source: &Value) {
    call(
        dir,
        json!({"kind":"review-source","source_id":source,"purpose":"Synthetic native test",
        "data_model":"Synthetic one-field model","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Explicit synthetic fixture"}),
    );
}

fn native_parser(dir: &Path, source: &Value) {
    call(
        dir,
        json!({"kind":"parser-not-applicable","source_id":source,"reason":"falcon-sensor-native",
        "reviewed_at":"2026-01-02T00:00:00Z","evidence":"Synthetic fixture represents a native source with no tenant parser"}),
    );
}

fn empty_catalog(dir: &Path) {
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

/// A confirmed Full target with one saved census and one completed discovery run. `reviewed`
/// also records the source review, a supported N/A parser classification and an empty catalog.
fn full_inventory(dir: &Path, census: Value, reviewed: bool, values: Option<Value>) -> Value {
    call(dir, json!({"kind":"init"}));
    let plan = call(
        dir,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            false,
        ),
    );
    call(dir, scout_import(dir, &plan, census));
    let confirmed = confirm(dir, "full-environment", json!([{"repo":"synthetic"}]));
    let source = confirmed["status"]["sources"][0]["source_id"].clone();
    if reviewed {
        review(dir, &source);
        native_parser(dir, &source);
        empty_catalog(dir);
    }
    let progress = call(dir, discovery_request(values.is_some()));
    complete_discovery(dir, progress, values);
    source
}

fn complete_discovery(dir: &Path, mut progress: Value, values: Option<Value>) {
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
    let mut brief_detail = counts.clone();
    brief_detail["next_action"] = detail["next_action"].clone();
    assert_eq!(brief["completion_detail"], brief_detail);
    assert!(
        detail["next_action"]
            .as_str()
            .unwrap()
            .contains("review-source"),
        "{detail}"
    );

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
    assert_eq!(
        reconciled["status"]["completion_detail"]["latest_census"],
        json!({"rows":ROW_CEILING - 1,"at_row_ceiling":false})
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
    let later = json!({"kind":"reconcile","reviewed_at":"2026-01-03T00:00:00Z"});
    let again = brief_call(&dir.0, later.clone());
    assert_eq!(again["action"]["outcome"], "already-current", "{again}");
    assert!(
        again["action"]["next_action"]
            .as_str()
            .unwrap()
            .contains("review-source")
    );
    assert_eq!(
        status_json(&dir.0, &[])["completion_detail"]["evidence_reviewed_at"],
        "2026-01-03T00:00:00Z"
    );
    let full_again = call(&dir.0, later);
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
fn empty_repeat_sample_keeps_values_saved_from_the_retained_pack() {
    let dir = Temp::new();
    full_inventory(
        &dir.0,
        census(1),
        false,
        Some(json!([{"user":"synthetic-user","events":2}])),
    );
    let mut request = discovery_request(true);
    request["start"] = json!("2026-01-02T00:00:00Z");
    request["end"] = json!("2026-01-03T00:00:00Z");
    let run = call(&dir.0, request);
    complete_discovery(&dir.0, run, Some(json!([])));
    let status = status_json(&dir.0, &[]);
    let user = status["sources"][0]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["field"] == "user")
        .unwrap();
    assert_eq!(user["values_requested"], true);
    assert_eq!(user["values_saved"], true);
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
const RELEASED_STATE: &str = r#"{"format":"cqlmatic-inventory-state","schema_version":4,"created_at":"2026-11-24T04:36:19.27143Z","updated_at":"2026-11-24T04:36:19.3728Z","discovery_mode":"full","quick_depth":"sources-and-fields","unverified_leads":[],"query_observations":[],"refresh_settings":{"automatic":false,"lookback":"24h"},"refresh_attempts":[],"refresh_reminder":{"environment":{"cloud":"us1","tenant":"<synthetic-cid>","view":"search-all"},"baseline_at":"2026-11-24T04:36:19.358663Z","notified_cycle_sha256":null,"defer_until":null},"source_metadata":[],"pending_inventory":[],"scout_plan":{"environment":{"cloud":"us1","tenant":"<synthetic-cid>","view":"search-all"},"arguments":{"repository":"search-all","query_string":"groupBy([#repo, #type, @sourcetype], function=count(as=events), limit=max)","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z"},"query_sha256":"c1036bc5b30d89142a1f2d17d493bb1977f382da8f06f2f472f25264491d6f7d"},"scouting":[{"plan":{"environment":{"cloud":"us1","tenant":"<synthetic-cid>","view":"search-all"},"arguments":{"repository":"search-all","query_string":"groupBy([#repo, #type, @sourcetype], function=count(as=events), limit=max)","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z"},"query_sha256":"c1036bc5b30d89142a1f2d17d493bb1977f382da8f06f2f472f25264491d6f7d"},"snapshot":{"sha256":"a8672e2e1458639b3bd4ec05599348887fbd40938fc4af10c11f1dcb2b552bc1","retrieved_at":"2026-01-02T00:00:00Z","raw_hex":"7b22726573756c74223a205b7b22237265706f223a202273796e746865746963222c20226576656e7473223a2031307d5d7d"},"rows":1}],"target":{"environment":{"cloud":"us1","tenant":"<synthetic-cid>","view":"search-all"},"scope":"full-environment","context":"Synthetic released state","confirmed_at":"2026-11-24T04:36:19.358663Z"},"sources":[{"id":"env:repo:a81ea3223a5156e8","selectors":{"repo":"synthetic"},"scouted_events":10,"expected":true,"review":null,"binding":null}],"parser_pages":[],"parsers":[],"parser_exclusions":[],"packs":[],"active_pack":null,"discovery_runs":[],"parser_runs":[],"failures":[],"reconciled_evidence_sha256":"9e2be6b524155be7f4b9f8b4ff099b88274798878c6abd4135d6eb9f7e30837d","reconciled_at":"2026-01-02T00:00:00Z"}"#;

#[test]
fn state_written_by_the_previous_release_keeps_its_reconciliation() {
    let dir = Temp::new();
    fs::create_dir(dir.0.join("workflow")).unwrap();
    fs::write(
        state_path(&dir.0),
        RELEASED_STATE.replace("<synthetic-cid>", &"0".repeat(32)),
    )
    .unwrap();
    let full = status_json(&dir.0, &[]);
    assert_eq!(full["reconciliation"], "done", "{full}");
    assert_eq!(full["completion_detail"]["scout_awaiting_capture"], false);
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
    let dir = Temp::new();
    fs::create_dir(dir.0.join("workflow")).unwrap();
    fs::write(state_path(&dir.0), serde_json::to_vec(&state).unwrap()).unwrap();
    let reply = brief_call(&dir.0, reconcile());
    assert_eq!(reply["action"]["outcome"], "recorded");
    let text = reply.to_string();
    for private in [
        "evidence_reviewed_at",
        "reconciliation_meaning",
        "reviewed_at",
        "evidence_sha256",
        "Synthetic largest brief",
        "synthetic-session",
        "synthetic-view",
        &"0".repeat(32),
    ] {
        assert!(!text.contains(private), "{private} appeared in brief");
    }
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

    // Advice for a blocking run with the largest index is the longest step. A step of this
    // bound fits the reconcile envelope, where it appears in the status and the action.
    const STEP_BOUND: usize = 320;
    for retained in [
        RetainedRun::Settle(usize::MAX),
        RetainedRun::Unreadable(usize::MAX),
        RetainedRun::ProducerMismatch(usize::MAX),
    ] {
        let mut advised = assess_at(&state, &[], NOW).unwrap();
        advise(&state, &mut advised, Some(retained));
        let run = advised
            .next_actions
            .iter()
            .find(|action| action.contains(&format!("run_index {}", usize::MAX)))
            .unwrap();
        assert!(run.len() <= STEP_BOUND && !run.contains('/'), "{run}");
    }
    let longest = "x".repeat(STEP_BOUND);
    for capability_blocked in [false, true] {
        status.capability_blocked = capability_blocked;
        status.completion_detail.as_mut().unwrap().next_action = Some(longest.clone());
        let mut projected = brief(&status, Some("synthetic-session"));
        assert_eq!(projected["completion_detail"]["next_action"], longest);
        // The reconcile envelope keeps the action's copy only.
        projected["completion_detail"]
            .as_object_mut()
            .unwrap()
            .remove("next_action");
        let envelope = json!({
            "schema_version":1,"brief":true,"status":projected,
            "action":{"outcome":"already-current","notification_only":true,"executes_searches":false,
                "next_action":longest}
        });
        // The CLI omits trailing references, and counts them, until the envelope fits.
        let mut envelope = envelope;
        let references = envelope["status"]["references"].as_array().unwrap().len();
        let omitted = (0..=references)
            .find(|_| {
                if serde_json::to_vec(&envelope).unwrap().len() < 4096 {
                    return true;
                }
                envelope["status"]["references"]
                    .as_array_mut()
                    .unwrap()
                    .pop();
                false
            })
            .expect("the envelope fits once references are omitted");
        println!(
            "reconcile envelope fits with {omitted} of {references} references omitted: {} bytes",
            serde_json::to_vec(&envelope).unwrap().len()
        );
    }
}

fn advice(dir: &Path, command: &str) -> String {
    let full = call(dir, reconcile());
    let next = full["action"]["next_action"].as_str().unwrap();
    assert!(next.contains(command), "expected {command}: {next}");
    let summary: Value =
        serde_json::from_slice(&success(inventory(dir, reconcile(), &["--summary"]))).unwrap();
    assert_eq!(summary["action"]["next_action"], next, "{summary}");
    let brief = brief_call(dir, reconcile());
    assert_eq!(brief["action"]["next_action"], next);
    // The brief envelope carries the step once, in the action.
    assert!(
        brief["status"]["completion_detail"]
            .get("next_action")
            .is_none(),
        "{brief}"
    );
    next.into()
}

#[test]
fn reconcile_confirms_target_before_catalog_or_scout_work() {
    let dir = Temp::new();
    call(&dir.0, json!({"kind":"init"}));
    advice(&dir.0, "confirm-target");
    call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            false,
        ),
    );
    advice(&dir.0, "confirm-target");
    confirm(&dir.0, "full-environment", json!([{"repo":"synthetic"}]));
}

#[test]
fn reconcile_imports_an_already_saved_scout_before_advising_any_search() {
    let dir = Temp::new();
    confirm(&dir.0, "full-environment", json!([{"repo":"synthetic"}]));
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            false,
        ),
    );
    let response = scout_import(&dir.0, &plan, census(1));
    let next = advice(&dir.0, "import-scout");
    assert!(
        next.find("already ran")
            .is_some_and(|check| next.find("import-scout").unwrap() < check),
        "{next}"
    );
    let imported = call(&dir.0, response);
    assert_eq!(
        imported["status"]["completion_detail"]["scout_awaiting_capture"],
        false
    );
}

#[test]
fn reconcile_replaces_scouts_that_the_current_mode_cannot_import() {
    for (starts_quick, named_view) in [(false, true), (true, true), (false, false)] {
        let dir = Temp::new();
        let mut env = environment();
        if !named_view {
            env["view"] = json!("search-all");
        }
        call(
            &dir.0,
            json!({"kind":"confirm-target","environment":env,"scope":"selected-sources",
            "context":"Synthetic mode change","expected_sources":[{"repo":"synthetic"}]}),
        );
        if starts_quick {
            call(&dir.0, json!({"kind":"set-discovery-mode","mode":"quick"}));
        }
        let prepare = |quick| {
            scout_request(
                env.clone(),
                "2026-01-01T00:00:00Z",
                "2026-01-02T00:00:00Z",
                quick,
            )
        };
        call(&dir.0, prepare(starts_quick));
        call(
            &dir.0,
            json!({"kind":"set-discovery-mode","mode":if starts_quick { "full" } else { "quick" }}),
        );
        let next = advice(&dir.0, "prepare-scout");
        assert!(!next.contains("import-scout"), "{next}");
        assert!(next.starts_with("Use prepare-scout"), "{next}");
        let plan = call(&dir.0, prepare(!starts_quick));
        assert_eq!(
            plan["action"]["arguments"]["repository"],
            if starts_quick {
                json!("search-all")
            } else {
                env["view"].clone()
            }
        );
        advice(&dir.0, "import-scout");
        call(&dir.0, scout_import(&dir.0, &plan, census(1)));
    }
}

fn host_evidence(dir: &Path) -> String {
    fixture(
        dir,
        "host.json",
        json!({
            "schema_version":1,"environment":environment(),"session_id":"synthetic-session",
            "verified_at":cqlmatic::discovery::timestamp(),"mechanism":"provider-enforced-search-lifetime",
            "max_search_seconds":60,"capability_reference":"Synthetic provider capability document",
            "verification_evidence":"Synthetic host verified a provider search lifetime"
        }),
    )
}

#[test]
fn reconcile_settles_a_dispatched_refresh_before_scouting() {
    for dispatched in [true, false] {
        let dir = Temp::new();
        confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
        let host = host_evidence(&dir.0);
        let prepared = call(
            &dir.0,
            json!({"kind":"prepare-refresh","session_id":"synthetic-session","host_evidence_file":host}),
        );
        let id = &prepared["status"]["refresh_attempts"][0]["attempt_id"];
        let report = if dispatched {
            call(
                &dir.0,
                json!({"kind":"dispatch-refresh","attempt_id":id,"session_id":"synthetic-session"}),
            )
        } else {
            prepared.clone()
        };
        let next = agreed(&dir.0, "settle-refresh");
        assert_eq!(next.contains("reserved"), !dispatched, "{next}");
        no_new_search(&dir.0);
        let attempt = &report["status"]["refresh_attempts"][0];
        let settlement = fixture(
            &dir.0,
            "settlement.json",
            json!({
                "schema_version":1,"attempt_id":id,"environment":environment(),"session_id":"synthetic-session",
                "arguments":attempt["arguments"],"termination":"not-started","recorded_at":cqlmatic::discovery::timestamp(),
                "search_started_at":null,"result_sha256":null,"evidence":"Synthetic exact-attempt host non-start"
            }),
        );
        let settled = call(
            &dir.0,
            json!({"kind":"settle-refresh","attempt_id":id,"evidence_file":settlement}),
        );
        assert_eq!(settled["status"]["refresh_blocked"], false);
        let plan = call(
            &dir.0,
            scout_request(
                environment(),
                "2026-01-01T00:00:00Z",
                "2026-01-02T00:00:00Z",
                false,
            ),
        );
        advice(&dir.0, "import-scout");
        call(&dir.0, scout_import(&dir.0, &plan, census(1)));
    }
}

#[test]
fn reconcile_settles_a_capability_attempt_before_scouting() {
    let dir = Temp::new();
    full_inventory(&dir.0, census(1), true, None);
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-02T00:00:00Z",
            "2026-01-03T00:00:00Z",
            false,
        ),
    );
    let host = host_evidence(&dir.0);
    let prepared = call(
        &dir.0,
        json!({"kind":"prepare-capabilities","execution_context_id":"synthetic-auth",
        "session_id":"synthetic-session","case_ids":["array-union"],"host_evidence_file":host,
        "max_queries":2,"max_seconds":120}),
    );
    let run_id = &prepared["action"]["capability"]["run_id"];
    call(
        &dir.0,
        json!({"kind":"dispatch-capability","run_id":run_id,"case_id":"array-union",
        "execution_context_id":"synthetic-auth","session_id":"synthetic-session"}),
    );
    agreed(&dir.0, "submit-capability");
    no_new_search(&dir.0);
    let stored: InventoryState =
        serde_json::from_slice(&fs::read(state_path(&dir.0)).unwrap()).unwrap();
    let attempt = &stored.capability_runs[0].attempts[0].reservation;
    let now = cqlmatic::discovery::timestamp();
    let settlement = fixture(
        &dir.0,
        "capability-settlement.json",
        json!({
            "schema_version":1,"attempt_id":attempt.id,"environment":environment(),
            "session_id":"synthetic-session","arguments":attempt.arguments,"termination":"not-started",
            "recorded_at":now,"result_sha256":null,"evidence":"Synthetic exact capability settlement"
        }),
    );
    let submitted = call(
        &dir.0,
        json!({"kind":"submit-capability","run_id":run_id,"attempt_id":attempt.id,
        "execution_context_id":"synthetic-auth","settlement_file":settlement,"retrieved_at":now}),
    );
    assert_eq!(submitted["status"]["capability_blocked"], false);
    advice(&dir.0, "import-scout");
    call(&dir.0, scout_import(&dir.0, &plan, census(1)));
}

#[test]
fn reconcile_settles_a_reserved_discovery_probe_before_scouting() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            false,
        ),
    );
    let started = call(&dir.0, discovery_request(false));
    let probe = &started["action"]["relay"]["probes"][0]["probe_id"];
    call(
        &dir.0,
        json!({"kind":"dispatch-discovery","probe_id":probe}),
    );
    advice(&dir.0, "discovery-step");
    let result = fixture(&dir.0, "result.json", json!({"result":[{"events":10}]}));
    let answered = call(
        &dir.0,
        json!({"kind":"discovery-step","answers":[{"probe_id":probe,"result_file":result}]}),
    );
    assert_eq!(answered["action"]["relay"]["answered"], 1);
    advice(&dir.0, "pause-discovery");
    let paused = call(&dir.0, json!({"kind":"pause-discovery"}));
    assert_eq!(paused["action"]["relay"]["status"], "paused");
    advice(&dir.0, "import-scout");
    call(&dir.0, scout_import(&dir.0, &plan, census(1)));
}

#[test]
fn selected_scope_resolves_source_work_before_optional_catalog() {
    let dir = Temp::new();
    let confirmed = confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let source = &confirmed["status"]["sources"][0]["source_id"];
    let status = status_json(&dir.0, &[]);
    assert_eq!(
        status["completion_detail"]["expected_sources_open"]["measurements"],
        1
    );
    advice(&dir.0, "next-probe");
    let progress = call(&dir.0, discovery_request(false));
    complete_discovery(&dir.0, progress, None);
    advice(&dir.0, "review-source");
    review(&dir.0, source);
    advice(&dir.0, "parser-not-applicable");
    native_parser(&dir.0, source);
}

#[test]
fn completed_selected_scope_identifies_optional_catalog_work() {
    let dir = Temp::new();
    let confirmed = confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let source = &confirmed["status"]["sources"][0]["source_id"];
    let progress = call(&dir.0, discovery_request(false));
    complete_discovery(&dir.0, progress, None);
    review(&dir.0, source);
    native_parser(&dir.0, source);
    let next = advice(&dir.0, "import-parsers");
    assert!(next.starts_with("Full-environment map: "), "{next}");
    assert_eq!(status_json(&dir.0, &[])["selected_scope_complete"], true);
    empty_catalog(&dir.0);
    advice(&dir.0, "full_map_complete");
}

#[test]
fn quick_scope_qualifies_full_discovery_advice_and_excludes_unexpected_sources() {
    let dir = Temp::new();
    let confirmed = confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let source = &confirmed["status"]["sources"][0]["source_id"];
    call(&dir.0, json!({"kind":"set-discovery-mode","mode":"quick"}));
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            true,
        ),
    );
    call(
        &dir.0,
        scout_import(
            &dir.0,
            &plan,
            json!({"result":[{"#repo":"other","events":1}]}),
        ),
    );
    let status = status_json(&dir.0, &[]);
    assert!(
        status["sources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["expected"] == false)
    );
    assert_eq!(
        status["completion_detail"]["expected_sources_open"],
        json!({"measurements":1,"interpretation_review":1,"parser_review_or_binding":1})
    );
    advice(&dir.0, "next-probe");
    let progress = call(&dir.0, discovery_request(false));
    complete_discovery(&dir.0, progress, None);
    assert!(advice(&dir.0, "review-source").starts_with("Full discovery: "));
    review(&dir.0, source);
    assert!(advice(&dir.0, "import-parsers").starts_with("Full discovery: "));
    empty_catalog(&dir.0);
    assert!(advice(&dir.0, "parser-not-applicable").starts_with("Full discovery: "));
    native_parser(&dir.0, source);
}

#[test]
fn bound_and_reviewed_parser_is_excluded_from_open_counts() {
    let dir = Temp::new();
    let source = full_inventory(&dir.0, census(1), false, None);
    review(&dir.0, &source);
    let page = fixture(
        &dir.0,
        "parser-page.json",
        json!({"resources":["parser"],"meta":{"pagination":{"offset":0,"total":1}}}),
    );
    let detail = fixture(
        &dir.0,
        "parser.json",
        json!({"resources":[{"id":"parser","name":"Synthetic parser","script":"user := \"synthetic\""}]}),
    );
    let imported = call(
        &dir.0,
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":page}],
        "details":[{"parser_id":"parser","result_file":detail}],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    assert_eq!(
        imported["status"]["completion_detail"]["expected_sources_open"]["parser_review_or_binding"],
        1
    );
    advice(&dir.0, "bind-parser");
    let bound = call(
        &dir.0,
        json!({"kind":"bind-parser","source_id":source,"parser_id":"parser",
        "parser_sha256":imported["status"]["parsers"][0]["sha256"],"reviewed_at":"2026-01-02T00:00:00Z",
        "evidence":"Synthetic exact parser binding","semantics":[]}),
    );
    assert_eq!(bound["status"]["sources"][0]["parser"], "done");
    assert_eq!(
        bound["status"]["completion_detail"]["expected_sources_open"]["parser_review_or_binding"],
        0
    );
}

#[test]
fn malformed_census_rows_require_a_fresh_capture() {
    for row in [
        json!({"#repo":"synthetic","@sourcetype":"   ","events":1}),
        json!({"#repo":"synthetic","@sourcetype":123,"events":1}),
        json!({"#repo":"synthetic","#type":123,"events":1}),
    ] {
        let dir = Temp::new();
        full_inventory(&dir.0, json!({"result":[row]}), true, None);
        let reconciled = call(&dir.0, reconcile());
        let status = &reconciled["status"];
        assert_eq!(status["full_map_complete"], false);
        assert_eq!(
            status["completion_detail"]["latest_census"],
            json!({"rows":1,"at_row_ceiling":false})
        );
        let actions = status["next_actions"].to_string();
        assert!(
            actions.contains("did not pass census validation"),
            "{actions}"
        );
        assert!(
            !actions.contains("10,000-row ceiling") && !actions.contains("shorter window"),
            "{actions}"
        );
        let plan = call(
            &dir.0,
            scout_request(
                environment(),
                "2026-01-02T00:00:00Z",
                "2026-01-03T00:00:00Z",
                false,
            ),
        );
        call(&dir.0, scout_import(&dir.0, &plan, census(1)));
        assert_eq!(
            call(&dir.0, reconcile())["status"]["full_map_complete"],
            true
        );
    }
}

#[test]
fn newest_census_is_reported_but_any_qualifying_search_all_census_counts() {
    for (first_rows, second_rows, quick, complete) in
        [(0, ROW_CEILING, false, true), (ROW_CEILING, 1, true, false)]
    {
        let dir = Temp::new();
        full_inventory(&dir.0, census(first_rows), true, None);
        let first = call(&dir.0, reconcile());
        assert_eq!(first["status"]["full_map_complete"], first_rows == 0);
        if quick {
            confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
            call(&dir.0, json!({"kind":"set-discovery-mode","mode":"quick"}));
        }
        let plan = call(
            &dir.0,
            scout_request(
                environment(),
                "2026-01-02T00:00:00Z",
                "2026-01-03T00:00:00Z",
                quick,
            ),
        );
        call(&dir.0, scout_import(&dir.0, &plan, census(second_rows)));
        if quick {
            call(&dir.0, json!({"kind":"set-discovery-mode","mode":"full"}));
            confirm(&dir.0, "full-environment", json!([{"repo":"synthetic"}]));
        }
        let status = &call(&dir.0, reconcile())["status"];
        assert_eq!(
            status["completion_detail"]["latest_census"],
            json!({"rows":second_rows,"at_row_ceiling":second_rows == ROW_CEILING})
        );
        assert_eq!(status["full_map_complete"], complete, "{status}");
    }
}

/// Status names the same first step as reconcile, in every status projection.
fn agreed(dir: &Path, command: &str) -> String {
    let next = advice(dir, command);
    let full = status_json(dir, &[]);
    assert_eq!(full["next_actions"][0], next, "{full}");
    assert_eq!(full["completion_detail"]["next_action"], next);
    assert_eq!(
        status_json(dir, &["--summary"])["next_actions"],
        full["next_actions"]
    );
    assert_eq!(
        status_json(dir, &["--brief"])["completion_detail"]["next_action"],
        next
    );
    assert!(status_text(dir).contains(&format!("- {next}\n")));
    next
}

fn actions(dir: &Path) -> Vec<String> {
    status_json(dir, &[])["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|action| action.as_str().unwrap().to_owned())
        .collect()
}

/// No listed action prepares or relays another search.
fn no_new_search(dir: &Path) {
    no_search_after(dir, 0);
}

/// No action after the first `skip` prepares or relays another search.
fn no_search_after(dir: &Path, skip: usize) {
    for action in actions(dir).into_iter().skip(skip) {
        for search in [
            "Prepare scouting",
            "prepare-scout",
            "prepare-discovery",
            "census qualifies",
            "run the exact plan once",
        ] {
            assert!(!action.contains(search), "{search}: {action}");
        }
    }
}

#[test]
fn status_imports_a_saved_scout_response_before_any_new_search() {
    for scope in ["selected-sources", "full-environment"] {
        let dir = Temp::new();
        confirm(&dir.0, scope, json!([{"repo":"synthetic"}]));
        let plan = call(
            &dir.0,
            scout_request(
                environment(),
                "2026-01-01T00:00:00Z",
                "2026-01-02T00:00:00Z",
                false,
            ),
        );
        let next = agreed(&dir.0, "import-scout");
        assert!(next.starts_with("Import any saved response"), "{next}");
        // Only the import step itself mentions running its exact plan.
        no_search_after(&dir.0, 1);
        let imported = call(&dir.0, scout_import(&dir.0, &plan, census(1)));
        assert_eq!(
            imported["status"]["completion_detail"]["scout_awaiting_capture"],
            false
        );
    }
}

fn discovery_window(start: &str, end: &str) -> Value {
    let mut request = discovery_request(false);
    request["start"] = json!(start);
    request["end"] = json!(end);
    request
}

#[test]
fn status_and_reconcile_name_the_blocking_discovery_run() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            false,
        ),
    );
    let first = call(&dir.0, discovery_request(false));
    assert_eq!(first["action"]["run_index"], 0, "{first}");
    call(&dir.0, json!({"kind":"pause-discovery","run_index":0}));
    let second = call(
        &dir.0,
        discovery_window("2026-01-02T00:00:00Z", "2026-01-03T00:00:00Z"),
    );
    assert_eq!(second["action"]["run_index"], 1, "{second}");
    call(&dir.0, json!({"kind":"pause-discovery","run_index":1}));
    call(&dir.0, json!({"kind":"resume-discovery","run_index":0}));
    let polled = call(
        &dir.0,
        json!({"kind":"discovery-step","run_index":0,"answers":[]}),
    );
    let probe = polled["action"]["relay"]["probes"][0]["probe_id"].clone();
    assert!(probe.is_string(), "{polled}");
    call(
        &dir.0,
        json!({"kind":"dispatch-discovery","run_index":0,"probe_id":probe}),
    );

    // The older run blocks; the advice names it, not the latest run.
    let next = agreed(&dir.0, "discovery-step with run_index 0");
    assert!(next.contains("pause-discovery with run_index 0"), "{next}");
    no_new_search(&dir.0);
    let result = fixture(&dir.0, "result.json", json!({"result":[{"events":10}]}));
    let answered = call(
        &dir.0,
        json!({"kind":"discovery-step","run_index":0,"answers":[{"probe_id":probe,"result_file":result}]}),
    );
    assert_eq!(answered["action"]["relay"]["answered"], 1, "{answered}");
    agreed(&dir.0, "pause-discovery with run_index 0");
    let paused = call(&dir.0, json!({"kind":"pause-discovery","run_index":0}));
    assert_eq!(paused["action"]["relay"]["status"], "paused");
    agreed(&dir.0, "import-scout");
    call(&dir.0, scout_import(&dir.0, &plan, census(1)));
}

#[test]
fn a_missing_checkpoint_is_restored_before_it_can_be_settled() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    call(&dir.0, discovery_request(false));
    let checkpoint = PathBuf::from(
        status_json(&dir.0, &[])["discovery_runs"][0]["state_path"]
            .as_str()
            .unwrap(),
    );
    let held = dir.0.join("held-checkpoint.json");
    fs::rename(&checkpoint, &held).unwrap();

    let next = agreed(&dir.0, "run_index 0");
    assert!(next.contains("missing or cannot be read"), "{next}");
    for command in ["discovery-step", "pause-discovery"] {
        assert!(!next.contains(command), "{next}");
    }
    let brief = String::from_utf8(status(&dir.0, &["--json", "--brief"])).unwrap();
    assert!(!brief.contains(checkpoint.to_str().unwrap()), "{brief}");
    // Separator-independent: JSON escapes a Windows path's backslashes.
    let name = checkpoint.file_name().unwrap().to_str().unwrap();
    assert!(!brief.contains(name), "{name} in {brief}");
    let failed = inventory(&dir.0, json!({"kind":"pause-discovery","run_index":0}), &[]);
    assert_eq!(failed.status.code(), Some(2));

    fs::rename(&held, &checkpoint).unwrap();
    agreed(&dir.0, "pause-discovery with run_index 0");
    let paused = call(&dir.0, json!({"kind":"pause-discovery","run_index":0}));
    assert_eq!(paused["action"]["relay"]["status"], "paused");
}

#[test]
fn quick_discovery_first_confirms_a_selected_source_target() {
    let dir = Temp::new();
    confirm(&dir.0, "full-environment", json!([{"repo":"synthetic"}]));
    call(&dir.0, json!({"kind":"set-discovery-mode","mode":"quick"}));
    let next = agreed(&dir.0, "confirm-target");
    assert!(next.contains("selected-sources"), "{next}");
    no_new_search(&dir.0);
    let refused = inventory(&dir.0, discovery_request(false), &[]);
    assert_eq!(refused.status.code(), Some(2));
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let started = call(&dir.0, discovery_request(false));
    assert_eq!(started["action"]["run_index"], 0);
}

/// Every action about the full-environment census or catalog is marked as optional.
fn optional_full_environment_work(dir: &Path) {
    for action in actions(dir) {
        if action.contains("census") || action.contains("parser catalog") {
            assert!(action.starts_with("Full-environment map: "), "{action}");
        }
    }
}

#[test]
fn completed_selected_scope_marks_full_environment_work_optional_everywhere() {
    let dir = Temp::new();
    let confirmed = confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let source = &confirmed["status"]["sources"][0]["source_id"];
    let progress = call(&dir.0, discovery_request(false));
    complete_discovery(&dir.0, progress, None);
    review(&dir.0, source);
    native_parser(&dir.0, source);
    let next = agreed(&dir.0, "import-parsers");
    assert!(next.starts_with("Full-environment map: "), "{next}");
    assert_eq!(status_json(&dir.0, &[])["selected_scope_complete"], true);
    let all = actions(&dir.0);
    assert!(
        all.iter().any(|a| a.contains("Prepare scouting")),
        "{all:?}"
    );
    optional_full_environment_work(&dir.0);

    // A search-all scout prepared before the target was narrowed is optional too.
    let dir = Temp::new();
    let confirmed = confirm(&dir.0, "full-environment", json!([{"repo":"synthetic"}]));
    let source = &confirmed["status"]["sources"][0]["source_id"];
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            false,
        ),
    );
    assert_eq!(plan["action"]["arguments"]["repository"], "search-all");
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let progress = call(&dir.0, discovery_request(false));
    complete_discovery(&dir.0, progress, None);
    review(&dir.0, source);
    native_parser(&dir.0, source);
    let next = agreed(&dir.0, "import-scout");
    assert!(next.starts_with("Full-environment map: "), "{next}");
    assert!(next.contains("optional"), "{next}");
    assert_eq!(status_json(&dir.0, &[])["selected_scope_complete"], true);
    optional_full_environment_work(&dir.0);
    call(&dir.0, scout_import(&dir.0, &plan, census(1)));
}

#[test]
fn quick_status_qualifies_tenant_parser_work_as_full_discovery() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    call(&dir.0, json!({"kind":"set-discovery-mode","mode":"quick"}));
    let page = fixture(
        &dir.0,
        "parser-page.json",
        json!({"resources":["parser"],"meta":{"pagination":{"offset":0,"total":1}}}),
    );
    let detail = fixture(
        &dir.0,
        "parser.json",
        json!({"resources":[{"id":"parser","name":"Synthetic parser","script":"user := \"synthetic\""}]}),
    );
    call(
        &dir.0,
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":page}],
        "details":[{"parser_id":"parser","result_file":detail}],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let all = actions(&dir.0);
    let unclassified: Vec<_> = all
        .iter()
        .filter(|a| a.contains("unclassified parser snapshot"))
        .collect();
    assert_eq!(unclassified.len(), 1, "{all:?}");
    assert!(unclassified[0].starts_with("Full discovery: "), "{all:?}");
    let text = status_text(&dir.0);
    assert!(
        text.lines()
            .filter(|line| line.contains("unclassified parser snapshot"))
            .all(|line| line.starts_with("- Full discovery: ")),
        "{text}"
    );
}

#[test]
fn a_named_view_census_still_leaves_the_search_all_census_to_do() {
    let dir = Temp::new();
    call(&dir.0, json!({"kind":"init"}));
    call(&dir.0, json!({"kind":"set-discovery-mode","mode":"quick"}));
    let confirmed = confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let source = confirmed["status"]["sources"][0]["source_id"].clone();
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            true,
        ),
    );
    assert_eq!(plan["action"]["arguments"]["repository"], "synthetic-view");
    call(&dir.0, scout_import(&dir.0, &plan, census(1)));
    call(&dir.0, json!({"kind":"set-discovery-mode","mode":"full"}));
    confirm(&dir.0, "full-environment", json!([{"repo":"synthetic"}]));
    review(&dir.0, &source);
    native_parser(&dir.0, &source);
    empty_catalog(&dir.0);
    let progress = call(&dir.0, discovery_request(false));
    complete_discovery(&dir.0, progress, None);

    let next = agreed(&dir.0, "search-all census");
    assert!(next.contains("prepare-scout"), "{next}");
    let status = status_json(&dir.0, &[]);
    assert_eq!(status["full_map_complete"], false);
    assert_eq!(
        status["completion_detail"]["expected_sources_open"],
        json!({"interpretation_review":0,"parser_review_or_binding":0,"measurements":0})
    );
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-02T00:00:00Z",
            "2026-01-03T00:00:00Z",
            false,
        ),
    );
    assert_eq!(plan["action"]["arguments"]["repository"], "search-all");
    call(&dir.0, scout_import(&dir.0, &plan, census(0)));
    assert_eq!(
        call(&dir.0, reconcile())["status"]["full_map_complete"],
        true
    );
}

#[test]
fn an_unresolved_refresh_is_still_settled_first() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let host = host_evidence(&dir.0);
    let prepared = call(
        &dir.0,
        json!({"kind":"prepare-refresh","session_id":"synthetic-session","host_evidence_file":host}),
    );
    let id = &prepared["status"]["refresh_attempts"][0]["attempt_id"];
    let dispatched = call(
        &dir.0,
        json!({"kind":"dispatch-refresh","attempt_id":id,"session_id":"synthetic-session"}),
    );
    let attempt = &dispatched["status"]["refresh_attempts"][0];
    let settlement = fixture(
        &dir.0,
        "settlement.json",
        json!({
            "schema_version":1,"attempt_id":id,"environment":environment(),"session_id":"synthetic-session",
            "arguments":attempt["arguments"],"termination":"unresolved","recorded_at":cqlmatic::discovery::timestamp(),
            "search_started_at":attempt["dispatched_at"],"result_sha256":null,"evidence":"Synthetic host lost its reply"
        }),
    );
    let settled = call(
        &dir.0,
        json!({"kind":"settle-refresh","attempt_id":id,"evidence_file":settlement}),
    );
    assert_eq!(
        settled["status"]["refresh_attempts"][0]["phase"],
        "unresolved"
    );
    assert_eq!(settled["status"]["refresh_blocked"], true);
    agreed(&dir.0, "settle-refresh");
    no_new_search(&dir.0);
    let refused = inventory(&dir.0, discovery_request(false), &[]);
    assert_eq!(refused.status.code(), Some(2));
}

fn invalid_census(valid: usize) -> Value {
    let mut census = census(valid);
    census["result"]
        .as_array_mut()
        .unwrap()
        .push(json!({"#repo":"synthetic","#type":123,"events":1}));
    census
}

fn census_warnings(status: &Value) -> Vec<String> {
    status["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|action| action.as_str().unwrap().to_owned())
        .filter(|action| action.contains("census qualifies"))
        .collect()
}

#[test]
fn the_newest_failed_search_all_census_explains_why_in_either_order() {
    let ceiling = "10,000-row ceiling";
    let invalid = "did not pass census validation";
    for (first, second, reason) in [
        (census(ROW_CEILING), invalid_census(0), invalid),
        (invalid_census(0), census(ROW_CEILING), ceiling),
    ] {
        let dir = Temp::new();
        full_inventory(&dir.0, first, true, None);
        let plan = call(
            &dir.0,
            scout_request(
                environment(),
                "2026-01-02T00:00:00Z",
                "2026-01-03T00:00:00Z",
                false,
            ),
        );
        call(&dir.0, scout_import(&dir.0, &plan, second));
        let status = &call(&dir.0, reconcile())["status"];
        assert_eq!(status["full_map_complete"], false);
        let warnings = census_warnings(status);
        assert_eq!(warnings.len(), 1, "{status}");
        assert!(warnings[0].contains(reason), "{warnings:?}");
        let other = if reason == ceiling { invalid } else { ceiling };
        assert!(!warnings[0].contains(other), "{warnings:?}");
    }
}

#[test]
fn an_earlier_qualifying_census_leaves_no_census_warning() {
    let dir = Temp::new();
    full_inventory(&dir.0, census(0), true, None);
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-02T00:00:00Z",
            "2026-01-03T00:00:00Z",
            false,
        ),
    );
    call(&dir.0, scout_import(&dir.0, &plan, invalid_census(0)));
    let status = &call(&dir.0, reconcile())["status"];
    assert_eq!(status["full_map_complete"], true, "{status}");
    assert!(census_warnings(status).is_empty(), "{status}");
    assert!(census_warnings(&status_json(&dir.0, &[])).is_empty());
}

#[test]
fn a_census_below_the_ceiling_with_an_invalid_row_is_a_validation_failure() {
    let dir = Temp::new();
    full_inventory(&dir.0, invalid_census(ROW_CEILING - 2), true, None);
    let status = &call(&dir.0, reconcile())["status"];
    assert_eq!(
        status["completion_detail"]["latest_census"],
        json!({"rows":ROW_CEILING - 1,"at_row_ceiling":false})
    );
    assert_eq!(status["full_map_complete"], false);
    let warnings = census_warnings(status);
    assert_eq!(warnings.len(), 1, "{status}");
    assert!(warnings[0].contains("did not pass census validation"));
    assert!(!warnings[0].contains("10,000-row ceiling") && !warnings[0].contains("shorter window"));
}

#[test]
fn a_bound_parser_awaiting_line_review_stays_open() {
    let dir = Temp::new();
    let source = full_inventory(&dir.0, census(1), false, None);
    review(&dir.0, &source);
    let page = fixture(
        &dir.0,
        "parser-page.json",
        json!({"resources":["dynamic"],"meta":{"pagination":{"offset":0,"total":1}}}),
    );
    let detail = fixture(
        &dir.0,
        "parser.json",
        json!({"resources":[{"id":"dynamic","name":"Synthetic dynamic parser","script":"case { actor := other }","fields_to_tag":[]}]}),
    );
    let imported = call(
        &dir.0,
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":page}],
        "details":[{"parser_id":"dynamic","result_file":detail}],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let parser = &imported["status"]["parsers"][0];
    let lines = parser["review_lines"].as_array().unwrap().clone();
    assert!(!lines.is_empty(), "{parser}");
    let bind = |semantics: Value| {
        call(
            &dir.0,
            json!({"kind":"bind-parser","source_id":source,"parser_id":"dynamic",
            "parser_sha256":parser["sha256"],"reviewed_at":"2026-01-02T00:00:00Z",
            "evidence":"Synthetic exact parser binding","semantics":semantics}),
        )
    };
    let bound = bind(json!([]));
    assert_eq!(bound["status"]["sources"][0]["parser"], "review");
    assert_eq!(
        bound["status"]["completion_detail"]["expected_sources_open"]["parser_review_or_binding"],
        1
    );
    let semantics: Vec<_> = lines
        .iter()
        .map(|line| json!({"line":line,"interpretation":"Synthetic reviewed branch","evidence":"Synthetic line review"}))
        .collect();
    let reviewed = bind(json!(semantics));
    assert_eq!(reviewed["status"]["sources"][0]["parser"], "done");
    assert_eq!(
        reviewed["status"]["completion_detail"]["expected_sources_open"]["parser_review_or_binding"],
        0
    );
}

#[test]
fn a_compatible_scout_stays_importable_across_a_mode_switch() {
    for (first, second) in [("quick", "full"), ("full", "quick")] {
        let dir = Temp::new();
        let mut env = environment();
        env["view"] = json!("search-all");
        call(
            &dir.0,
            json!({"kind":"confirm-target","environment":env,"scope":"selected-sources",
            "context":"Synthetic compatible mode change","expected_sources":[{"repo":"synthetic"}]}),
        );
        call(&dir.0, json!({"kind":"set-discovery-mode","mode":first}));
        let plan = call(
            &dir.0,
            scout_request(
                env.clone(),
                "2026-01-01T00:00:00Z",
                "2026-01-02T00:00:00Z",
                true,
            ),
        );
        call(&dir.0, json!({"kind":"set-discovery-mode","mode":second}));
        let next = agreed(&dir.0, "import-scout");
        assert!(!next.contains("prepare-scout"), "{next}");
        let imported = call(&dir.0, scout_import(&dir.0, &plan, census(0)));
        assert_eq!(
            imported["status"]["completion_detail"]["scout_awaiting_capture"],
            false
        );
    }
}

#[test]
fn a_pending_scout_never_invites_a_search_while_a_discovery_run_blocks() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            false,
        ),
    );
    let started = call(&dir.0, discovery_request(false));
    let probe = &started["action"]["relay"]["probes"][0]["probe_id"];
    call(
        &dir.0,
        json!({"kind":"dispatch-discovery","probe_id":probe}),
    );
    // A compatible scout, then one the current mode cannot import.
    for mode in ["full", "quick"] {
        call(&dir.0, json!({"kind":"set-discovery-mode","mode":mode}));
        let next = agreed(&dir.0, "discovery-step with run_index 0");
        assert!(next.contains("pause-discovery with run_index 0"), "{next}");
        no_new_search(&dir.0);
        let status = status_json(&dir.0, &[]);
        assert_eq!(status["completion_detail"]["scout_awaiting_capture"], true);
    }
}

#[test]
fn reconcile_comes_before_optional_broader_work_in_quick_and_selected_scopes() {
    let reconcile_first = |dir: &Path| {
        let full = status_json(dir, &[]);
        let first = full["next_actions"][0].as_str().unwrap().to_owned();
        assert!(first.starts_with("Reconcile"), "{full}");
        assert_eq!(full["completion_detail"]["next_action"], first);
        assert_eq!(
            status_json(dir, &["--brief"])["completion_detail"]["next_action"],
            first
        );
        actions(dir)
    };

    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    call(&dir.0, json!({"kind":"set-discovery-mode","mode":"quick"}));
    let progress = call(&dir.0, discovery_request(false));
    complete_discovery(&dir.0, progress, None);
    let all = reconcile_first(&dir.0);
    for optional in [
        "interpretation review",
        "parser catalog",
        "parser-not-applicable",
    ] {
        let action = all.iter().find(|a| a.contains(optional)).unwrap();
        assert!(action.starts_with("Full discovery: "), "{all:?}");
    }
    assert_eq!(
        call(&dir.0, reconcile())["status"]["quick_scope_complete"],
        true
    );

    let dir = Temp::new();
    let confirmed = confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let source = &confirmed["status"]["sources"][0]["source_id"];
    let progress = call(&dir.0, discovery_request(false));
    complete_discovery(&dir.0, progress, None);
    review(&dir.0, source);
    native_parser(&dir.0, source);
    let all = reconcile_first(&dir.0);
    assert_eq!(status_json(&dir.0, &[])["selected_scope_complete"], false);
    for optional in ["parser catalog", "Prepare scouting"] {
        let action = all.iter().find(|a| a.contains(optional)).unwrap();
        assert!(action.starts_with("Full-environment map: "), "{all:?}");
    }
    assert!(
        !all.iter().any(|a| a.contains("selected scope is complete")),
        "{all:?}"
    );
    assert_eq!(
        call(&dir.0, reconcile())["status"]["selected_scope_complete"],
        true
    );
}

#[test]
fn preparing_the_same_scout_again_awaits_its_fresh_reply() {
    let dir = Temp::new();
    full_inventory(&dir.0, invalid_census(0), true, None);
    let status = &call(&dir.0, reconcile())["status"];
    assert_eq!(status["completion_detail"]["scout_awaiting_capture"], false);
    assert!(
        census_warnings(status)[0].contains("fresh capture"),
        "{status}"
    );
    // The exact arguments of the saved, invalid capture.
    let plan_request = || {
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            false,
        )
    };
    let plan = call(&dir.0, plan_request());
    assert_eq!(
        plan["status"]["completion_detail"]["scout_awaiting_capture"],
        true
    );
    let next = agreed(&dir.0, "import-scout");
    assert!(next.starts_with("Import any saved response"), "{next}");
    let fresh = scout_import(&dir.0, &plan, census(1));
    let imported = call(&dir.0, fresh.clone());
    assert_eq!(
        imported["status"]["completion_detail"]["scout_awaiting_capture"],
        false
    );
    assert_eq!(
        call(&dir.0, reconcile())["status"]["full_map_complete"],
        true
    );
    // Importing the same reply again changes nothing.
    let replayed = call(&dir.0, fresh.clone());
    assert_eq!(
        replayed["status"]["completion_detail"]["scout_awaiting_capture"],
        false
    );
    assert_eq!(replayed["status"]["full_map_complete"], true);
    // Preparing the plan once more awaits a reply; importing that same reply settles it.
    let again = call(&dir.0, plan_request());
    assert_eq!(
        again["status"]["completion_detail"]["scout_awaiting_capture"],
        true
    );
    let replayed = call(&dir.0, fresh);
    assert_eq!(
        replayed["status"]["completion_detail"]["scout_awaiting_capture"],
        false
    );
}

#[test]
fn a_run_written_by_another_release_is_settled_with_that_release() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    call(&dir.0, discovery_request(false));
    let checkpoint = PathBuf::from(
        status_json(&dir.0, &[])["discovery_runs"][0]["state_path"]
            .as_str()
            .unwrap(),
    );
    let current = fs::read(&checkpoint).unwrap();
    let mut earlier: Value = serde_json::from_slice(&current).unwrap();
    earlier["producer_version"] = json!("0.1.12");
    fs::write(&checkpoint, serde_json::to_vec(&earlier).unwrap()).unwrap();

    let next = agreed(&dir.0, "run_index 0");
    assert!(next.contains("producing release"), "{next}");
    for command in ["discovery-step", "pause-discovery"] {
        assert!(!next.contains(command), "{next}");
    }
    no_new_search(&dir.0);
    let brief = status_json(&dir.0, &["--brief"]);
    assert_eq!(
        brief["refresh"]["blocker"],
        "recorded-discovery-recovery-required"
    );
    let refused = inventory(&dir.0, json!({"kind":"pause-discovery","run_index":0}), &[]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("MCP_STATE_PRODUCER"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    fs::write(&checkpoint, current).unwrap();
    agreed(&dir.0, "pause-discovery with run_index 0");
}

#[test]
fn a_later_run_that_blocks_is_named_by_its_own_index() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    call(&dir.0, discovery_request(false));
    call(&dir.0, json!({"kind":"pause-discovery","run_index":0}));
    let second = call(
        &dir.0,
        discovery_window("2026-01-02T00:00:00Z", "2026-01-03T00:00:00Z"),
    );
    assert_eq!(second["action"]["run_index"], 1, "{second}");
    let probe = &second["action"]["relay"]["probes"][0]["probe_id"];
    call(
        &dir.0,
        json!({"kind":"dispatch-discovery","run_index":1,"probe_id":probe}),
    );
    let next = agreed(&dir.0, "discovery-step with run_index 1");
    assert!(!next.contains("run_index 0"), "{next}");
}

#[test]
fn a_corrupt_or_swapped_checkpoint_is_restored_not_settled() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    call(&dir.0, discovery_request(false));
    call(&dir.0, json!({"kind":"pause-discovery","run_index":0}));
    call(
        &dir.0,
        discovery_window("2026-01-02T00:00:00Z", "2026-01-03T00:00:00Z"),
    );
    let runs = status_json(&dir.0, &[])["discovery_runs"].clone();
    let first = PathBuf::from(runs[0]["state_path"].as_str().unwrap());
    let second = PathBuf::from(runs[1]["state_path"].as_str().unwrap());
    let original = fs::read(&first).unwrap();
    for replacement in [b"{\"format\":".to_vec(), fs::read(&second).unwrap()] {
        fs::write(&first, replacement).unwrap();
        let next = agreed(&dir.0, "run_index 0");
        assert!(next.contains("missing or cannot be read"), "{next}");
        assert!(!next.contains("discovery-step"), "{next}");
    }
    fs::write(&first, original).unwrap();
    agreed(&dir.0, "discovery-step with run_index 1");
}

#[test]
fn brief_refresh_availability_follows_a_missing_paused_checkpoint() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    call(&dir.0, discovery_request(false));
    call(&dir.0, json!({"kind":"pause-discovery","run_index":0}));
    let checkpoint = PathBuf::from(
        status_json(&dir.0, &[])["discovery_runs"][0]["state_path"]
            .as_str()
            .unwrap(),
    );
    let blocker = |dir: &Path| status_json(dir, &["--brief"])["refresh"]["blocker"].clone();
    assert_eq!(blocker(&dir.0), "manual-refresh-available");
    let held = dir.0.join("held-checkpoint.json");
    fs::rename(&checkpoint, &held).unwrap();
    assert_eq!(blocker(&dir.0), "recorded-discovery-recovery-required");
    agreed(&dir.0, "run_index 0");
    fs::rename(&held, &checkpoint).unwrap();
    assert_eq!(blocker(&dir.0), "manual-refresh-available");
}

#[test]
fn an_empty_full_environment_census_says_a_source_is_needed() {
    let dir = Temp::new();
    confirm(&dir.0, "full-environment", json!([]));
    let plan = call(
        &dir.0,
        scout_request(
            environment(),
            "2026-01-01T00:00:00Z",
            "2026-01-02T00:00:00Z",
            false,
        ),
    );
    call(&dir.0, scout_import(&dir.0, &plan, census(0)));
    empty_catalog(&dir.0);
    let status = &call(&dir.0, reconcile())["status"];
    assert_eq!(status["full_map_complete"], false, "{status}");
    assert_eq!(status["reconciliation"], "done");
    let next = agreed(&dir.0, "at least one source");
    assert!(next.contains("confirm-target"), "{next}");
}

#[test]
fn brief_discovery_actions_keep_the_status_next_step() {
    let dir = Temp::new();
    confirm(&dir.0, "selected-sources", json!([{"repo":"synthetic"}]));
    let started = brief_call(&dir.0, discovery_request(false));
    let step = started["status"]["completion_detail"]["next_action"]
        .as_str()
        .unwrap();
    assert!(step.contains("run_index 0"), "{started}");
    assert_eq!(
        status_json(&dir.0, &["--brief"])["completion_detail"]["next_action"],
        step
    );
    assert!(started["action"]["next_action"].is_string(), "{started}");
}
