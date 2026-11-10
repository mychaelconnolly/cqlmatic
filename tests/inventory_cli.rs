use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-inventory-{}-{}",
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
fn call(dir: &Path, action: Value) -> Value {
    let request = dir.join("request.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({"schema_version":1,"action":action})).unwrap(),
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["env", "inventory", "--json", "--state"])
        .arg(dir.join("workflow/state.json"))
        .arg("--request")
        .arg(&request)
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
    let file = dir.join(name);
    fs::write(&file, serde_json::to_vec(&value).unwrap()).unwrap();
    file.to_string_lossy().into_owned()
}
fn environment() -> Value {
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"})
}
fn relay(
    dir: &Path,
    mut progress: Value,
    fields: &[&str],
    zero: &[&str],
    expected_population: Option<&[&str]>,
) -> Value {
    let mut populated = Vec::new();
    for round in 0..8 {
        if progress["action"]["relay"]["status"] != "needs-probes" {
            assert_eq!(
                progress["action"]["relay"]["status"], "complete",
                "{progress}"
            );
            if let Some(expected) = expected_population {
                populated.sort();
                let mut expected = expected.to_vec();
                expected.sort();
                assert_eq!(populated, expected);
            }
            return progress;
        }
        let mut answers = Vec::new();
        for (index, probe) in progress["action"]["relay"]["probes"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            let rows = match probe["kind"].as_str().unwrap() {
                "volume" => json!([{"events":10}]),
                "fieldset" => json!(
                    fields
                        .iter()
                        .map(|field| json!({"field":field}))
                        .collect::<Vec<_>>()
                ),
                "dataset-facet" => json!([]),
                "population" => {
                    let mut row = json!({"total":10});
                    for (i, field) in probe["fields"].as_array().unwrap().iter().enumerate() {
                        populated.push(field.as_str().unwrap().to_string());
                        row[format!("c{i}")] = json!(if zero.contains(&field.as_str().unwrap()) {
                            0
                        } else {
                            10
                        });
                    }
                    json!([row])
                }
                "tag-detect" => json!([{"events":0}]),
                kind => panic!("unexpected probe {kind}"),
            };
            let file = fixture(
                dir,
                &format!("batch-answer-{round}-{index}.json"),
                json!({"result":rows}),
            );
            answers.push(json!({"probe_id":probe["probe_id"],"result_file":file}));
        }
        progress = call(dir, json!({"kind":"discovery-step","answers":answers}));
    }
    panic!("Relay did not finish within its expected finite rounds");
}

#[test]
fn no_state_status_has_no_side_effects_and_explicit_state_is_selected() {
    let dir = Temp::new();
    let path = dir.0.join("absent/state.json");
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["status", "--json", "--state"])
        .arg(&path)
        .output()
        .unwrap();
    assert!(output.status.success());
    let status: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["blank_canvas"], true);
    assert_eq!(status["full_map_complete"], false);
    assert!(!path.parent().unwrap().exists());
}

#[test]
fn relative_inventory_state_path_resolves_before_protection() {
    let dir = Temp::new();
    let request = fixture(
        &dir.0,
        "request.json",
        json!({"schema_version":1,"action":{"kind":"record-failure","stage":"synthetic-resume","message":"saved marker","recorded_at":"2026-01-01T00:00:00Z"}}),
    );
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .current_dir(&dir.0)
        .args([
            "env",
            "inventory",
            "--json",
            "--state",
            "workflow/state.json",
            "--request",
        ])
        .arg(request)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["status"]["failures"][0]["message"], "saved marker");
    assert!(dir.0.join("workflow/state.json").exists());
}

#[cfg(windows)]
#[test]
fn drive_relative_inventory_state_can_resume_from_another_directory() {
    let dir = Temp::new();
    let drive = dir.0.to_string_lossy().chars().next().unwrap();
    let relative = format!("{drive}:workflow\\state.json");
    let request = fixture(
        &dir.0,
        "request.json",
        json!({"schema_version":1,"action":{"kind":"record-failure","stage":"synthetic-resume","message":"saved marker","recorded_at":"2026-01-01T00:00:00Z"}}),
    );
    let first = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .current_dir(&dir.0)
        .args(["env", "inventory", "--json", "--state"])
        .arg(&relative)
        .arg("--request")
        .arg(request)
        .output()
        .unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let result: Value = serde_json::from_slice(&first.stdout).unwrap();
    let saved = PathBuf::from(result["state"].as_str().unwrap());
    assert!(saved.is_absolute());
    assert_eq!(saved, dir.0.join("workflow/state.json"));
    let other = Temp::new();
    let resumed = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .current_dir(&other.0)
        .args(["status", "--json", "--state"])
        .arg(&saved)
        .output()
        .unwrap();
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let status: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(status["failures"][0]["message"], "saved marker");
}

#[test]
fn inventory_refuses_nonprivate_explicit_state_parent() {
    let dir = Temp::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(windows)]
    {
        let acl = Command::new("icacls")
            .arg(&dir.0)
            .args(["/grant", "*S-1-5-32-545:(OI)(CI)R"])
            .output()
            .unwrap();
        assert!(acl.status.success());
    }
    let request = fixture(
        &dir.0,
        "request.json",
        json!({"schema_version":1,"action":{"kind":"init"}}),
    );
    let state = dir.0.join("state.json");
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["env", "inventory", "--json", "--state"])
        .arg(&state)
        .arg("--request")
        .arg(request)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!state.exists());
    assert!(String::from_utf8_lossy(&output.stderr).contains("SETUP_PROTECTION"));
}

#[test]
fn complete_offline_relay_inventory_derives_completion_and_detects_pack_changes() {
    let dir = Temp::new();
    call(&dir.0, json!({"kind":"init"}));
    let scouting = call(
        &dir.0,
        json!({"kind":"prepare-scout","environment":environment(),"start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z"}),
    );
    let arguments = scouting["action"]["arguments"].clone();
    assert!(arguments["query_string"].is_string());
    let scout = fixture(
        &dir.0,
        "scout.json",
        json!({"result":[{"#repo":"synthetic","events":10}]}),
    );
    call(
        &dir.0,
        json!({"kind":"import-scout","result_file":scout,"arguments":arguments,"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let confirmed = call(
        &dir.0,
        json!({"kind":"confirm-target","environment":environment(),"scope":"full-environment","context":"Synthetic offline regression","expected_sources":[]}),
    );
    let text_status = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["status", "--state"])
        .arg(dir.0.join("workflow/state.json"))
        .output()
        .unwrap();
    assert!(text_status.status.success());
    assert!(String::from_utf8_lossy(&text_status.stdout).contains("[#repo=synthetic]"));
    let source = confirmed["status"]["sources"][0]["source_id"].clone();
    call(
        &dir.0,
        json!({"kind":"review-source","source_id":source,"purpose":"Synthetic native test","data_model":"Synthetic one-field model","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Explicit synthetic fixture"}),
    );
    call(
        &dir.0,
        json!({"kind":"parser-not-applicable","source_id":source,"reason":"falcon-sensor-native","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Synthetic fixture represents a native source with no tenant parser"}),
    );
    let empty = fixture(
        &dir.0,
        "catalog.json",
        json!({"resources":[],"meta":{"pagination":{"total":0,"offset":0}}}),
    );
    call(
        &dir.0,
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":empty}],"details":[],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let mut progress = call(
        &dir.0,
        json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","source_ids":[],"value_fields":[],"kind_fields":[],"max_queries":30,"max_seconds":300}),
    );
    let mut rounds = 0;
    while progress["action"]["relay"]["status"] == "needs-probes" {
        rounds += 1;
        assert!(rounds < 8);
        let mut answers = Vec::new();
        for (index, probe) in progress["action"]["relay"]["probes"]
            .as_array()
            .unwrap()
            .iter()
            .enumerate()
        {
            assert_eq!(probe["mcp"]["arguments"]["repository"], "search-all");
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
                kind => panic!("unexpected probe {kind}"),
            };
            let result = fixture(
                &dir.0,
                &format!("answer-{rounds}-{index}.json"),
                json!({"result":rows}),
            );
            answers.push(json!({"probe_id":probe["probe_id"],"result_file":result}));
        }
        progress = call(&dir.0, json!({"kind":"discovery-step","answers":answers}));
    }
    assert_eq!(
        progress["action"]["relay"]["status"], "complete",
        "{progress}"
    );
    assert_eq!(progress["status"]["full_map_complete"], false);
    let reconciled = call(
        &dir.0,
        json!({"kind":"reconcile","reviewed_at":"2026-01-02T00:00:00Z"}),
    );
    assert_eq!(
        reconciled["status"]["full_map_complete"], true,
        "{reconciled}"
    );
    let resumed = call(&dir.0, json!({"kind":"discovery-step","answers":[]}));
    assert_eq!(resumed["status"]["full_map_complete"], true, "{resumed}");
    let state: Value =
        serde_json::from_slice(&fs::read(dir.0.join("workflow/state.json")).unwrap()).unwrap();
    let pack = PathBuf::from(state["packs"][0]["path"].as_str().unwrap());
    let mut bytes = fs::read(&pack).unwrap();
    bytes.push(b'x');
    fs::write(&pack, bytes).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["status", "--json", "--state"])
        .arg(dir.0.join("workflow/state.json"))
        .output()
        .unwrap();
    assert!(!output.status.success());
}

#[test]
fn cumulative_pack_covers_multiple_sources_and_declared_field_batches() {
    let dir = Temp::new();
    let plan = call(
        &dir.0,
        json!({"kind":"prepare-scout","environment":environment(),"start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z"}),
    );
    let scout = fixture(
        &dir.0,
        "scout.json",
        json!({"result":[{"#repo":"synthetic","events":10},{"#repo":"other","events":10}]}),
    );
    call(
        &dir.0,
        json!({"kind":"import-scout","result_file":scout,"arguments":plan["action"]["arguments"],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let confirmed = call(
        &dir.0,
        json!({"kind":"confirm-target","environment":environment(),"scope":"full-environment","context":"Synthetic batch regression","expected_sources":[]}),
    );
    let sources = confirmed["status"]["sources"].as_array().unwrap();
    let first = sources
        .iter()
        .find(|s| s["selectors"]["repo"] == "synthetic")
        .unwrap()["source_id"]
        .clone();
    let other = sources
        .iter()
        .find(|s| s["selectors"]["repo"] == "other")
        .unwrap()["source_id"]
        .clone();
    for source in [&first, &other] {
        call(
            &dir.0,
            json!({"kind":"review-source","source_id":source,"purpose":"Synthetic batch source","data_model":"Explicit synthetic model","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Offline fixture"}),
        );
    }
    call(
        &dir.0,
        json!({"kind":"parser-not-applicable","source_id":other,"reason":"falcon-sensor-native","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Synthetic native fixture"}),
    );
    let catalog = fixture(
        &dir.0,
        "catalog.json",
        json!({"resources":[{"ID":"parser-one","Name":"Synthetic"}],"meta":{"pagination":{"offset":0,"total":1}}}),
    );
    let detail = fixture(
        &dir.0,
        "parser.json",
        json!({"resources":[{"id":"parser-one","name":"Synthetic","script":"first := \"x\"\nsecond := \"x\"\nquiet := \"x\"","fields_to_tag":[]}]}),
    );
    let imported = call(
        &dir.0,
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":catalog}],"details":[{"parser_id":"parser-one","result_file":detail}],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    call(
        &dir.0,
        json!({"kind":"bind-parser","source_id":first,"parser_id":"parser-one","parser_sha256":imported["status"]["parsers"][0]["sha256"],"reviewed_at":"2026-01-02T00:00:00Z","evidence":"Explicit fixture source/parser assignment","semantics":[]}),
    );
    let mut immutable = Vec::new();
    for batch in [json!(["first"]), json!(["second", "quiet"])] {
        let prepared = call(
            &dir.0,
            json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","source_ids":[first],"population_fields":[{"source":{"repo":"synthetic"},"fields":batch}],"max_queries":30,"max_seconds":300}),
        );
        assert!(
            prepared["action"]["warning"]
                .as_str()
                .unwrap()
                .contains("significant model tokens")
        );
        let completed = relay(&dir.0, prepared, &["first", "second"], &["quiet"], None);
        assert_eq!(completed["status"]["full_map_complete"], false);
        let active = completed["status"]["active_pack"]["path"].as_str().unwrap();
        immutable.push((PathBuf::from(active), fs::read(active).unwrap()));
    }
    let prepared = call(
        &dir.0,
        json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","source_ids":[other],"max_queries":30,"max_seconds":300}),
    );
    let completed = relay(&dir.0, prepared, &["peer"], &[], None);
    let quiet = completed["status"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["source_id"] == first)
        .unwrap()["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["field"] == "quiet")
        .unwrap();
    assert_eq!(quiet["observed"], false);
    assert_eq!(quiet["measured"], true);
    let reconciled = call(
        &dir.0,
        json!({"kind":"reconcile","reviewed_at":"2026-01-02T00:00:00Z"}),
    );
    assert_eq!(
        reconciled["status"]["full_map_complete"], true,
        "{reconciled}"
    );
    let active = reconciled["status"]["active_pack"]["path"]
        .as_str()
        .unwrap();
    let pack = cqlmatic::storage::read_pack(Path::new(active)).unwrap();
    assert_eq!(pack.data.sources.len(), 2);
    for name in ["first", "second", "quiet", "peer"] {
        assert!(pack.data.fields.iter().any(|f| f["path"] == name), "{name}");
    }
    for (path, bytes) in immutable {
        assert_eq!(
            fs::read(path).unwrap(),
            bytes,
            "Prior packs must remain immutable"
        );
    }
    for (repo, field) in [
        ("synthetic", "first"),
        ("synthetic", "second"),
        ("other", "peer"),
    ] {
        let result = cqlmatic::app::check(
            cqlmatic::contracts::CheckRequest {
                schema_version: 1,
                id: None,
                query: format!("#repo={repo} {field}=*"),
                context: cqlmatic::contracts::Context {
                    require_observed: true,
                    ..Default::default()
                },
            },
            Some(&pack),
        );
        assert_eq!(
            result.outcome,
            cqlmatic::contracts::Outcome::Ready,
            "{result:?}"
        );
    }
}

#[test]
fn native_fieldset_batches_keep_unselected_fields_pending() {
    let dir = Temp::new();
    let plan = call(
        &dir.0,
        json!({"kind":"prepare-scout","environment":environment(),"start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z"}),
    );
    let scout = fixture(
        &dir.0,
        "scout.json",
        json!({"result":[{"#repo":"synthetic","events":10}]}),
    );
    call(
        &dir.0,
        json!({"kind":"import-scout","result_file":scout,"arguments":plan["action"]["arguments"],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let confirmed = call(
        &dir.0,
        json!({"kind":"confirm-target","environment":environment(),"scope":"full-environment","context":"Synthetic native field batch regression","expected_sources":[]}),
    );
    let source = confirmed["status"]["sources"][0]["source_id"].clone();
    call(
        &dir.0,
        json!({"kind":"review-source","source_id":source,"purpose":"Synthetic native source","data_model":"Two observed field names","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Offline fixture"}),
    );
    call(
        &dir.0,
        json!({"kind":"parser-not-applicable","source_id":source,"reason":"falcon-sensor-native","reviewed_at":"2026-01-02T00:00:00Z","evidence":"Synthetic native fixture without declarations"}),
    );
    let catalog = fixture(
        &dir.0,
        "catalog.json",
        json!({"resources":[],"meta":{"pagination":{"offset":0,"total":0}}}),
    );
    call(
        &dir.0,
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":catalog}],"details":[],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let mut first_field = None;
    for selected in ["a", "b"] {
        let prepared = call(
            &dir.0,
            json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","source_ids":[source],"population_fields":[{"source":{"repo":"synthetic"},"fields":[selected]}],"max_queries":30,"max_seconds":300}),
        );
        let completed = relay(&dir.0, prepared, &["a", "b"], &[], Some(&[selected]));
        let active = completed["status"]["active_pack"]["path"].as_str().unwrap();
        let pack = cqlmatic::storage::read_pack(Path::new(active)).unwrap();
        let a = pack.data.fields.iter().find(|f| f["path"] == "a").unwrap();
        if let Some(first) = &first_field {
            assert_eq!(a, first, "An untargeted prior field must remain unchanged");
        } else {
            first_field = Some(a.clone());
            let b = completed["status"]["sources"][0]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .find(|f| f["field"] == "b")
                .unwrap();
            assert_eq!(b["measured"], false);
            assert_eq!(b["observed"], false);
            assert_eq!(b["declared"], false);
        }
        for name in ["a", "b"] {
            let result = cqlmatic::app::check(
                cqlmatic::contracts::CheckRequest {
                    schema_version: 1,
                    id: None,
                    query: format!("#repo=synthetic {name}=*"),
                    context: cqlmatic::contracts::Context {
                        require_observed: true,
                        ..Default::default()
                    },
                },
                Some(&pack),
            );
            assert_eq!(
                result.outcome == cqlmatic::contracts::Outcome::Ready,
                name == "a" || selected == "b",
                "{result:?}"
            );
        }
        let reconciled = call(
            &dir.0,
            json!({"kind":"reconcile","reviewed_at":"2026-01-02T00:00:00Z"}),
        );
        assert_eq!(
            reconciled["status"]["full_map_complete"],
            selected == "b",
            "{reconciled}"
        );
    }
}

#[test]
fn compact_inventory_and_status_are_opt_in_and_keep_default_reports() {
    let dir = Temp::new();
    let initialized = call(&dir.0, json!({"kind":"init"}));
    assert!(initialized["status"].get("scouting_evidence").is_some());
    assert!(initialized.get("summary").is_none());
    let page = fixture(
        &dir.0,
        "summary-page.json",
        json!({"resources":["summary-parser"],"meta":{"pagination":{"offset":0,"total":1}}}),
    );
    let detail = fixture(
        &dir.0,
        "summary-parser.json",
        json!({"resources":[{"id":"summary-parser","name":"Synthetic summary parser","script":(0..100).map(|n|format!("field_{n} := \"synthetic\"\n")).collect::<String>()}]}),
    );
    call(
        &dir.0,
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":page}],"details":[{"parser_id":"summary-parser","result_file":detail}],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let state = dir.0.join("workflow/state.json");
    for command in [vec!["status"], vec!["env", "inventory"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args(command.clone())
            .args(["--json", "--summary", "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let value: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["summary"], true);
        assert_eq!(value["counts"]["sources"], 0);
        assert!(value.get("scouting_evidence").is_none());
        let normal = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args(command.clone())
            .args(["--json", "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(normal.status.success());
        let full: Value = serde_json::from_slice(&normal.stdout).unwrap();
        for key in [
            "schema_version",
            "blank_canvas",
            "full_map_complete",
            "message",
            "target",
            "environment",
            "target_scope",
            "context",
            "scouting",
            "parser_catalog",
            "reconciliation",
            "parser_count",
            "parser_page_count",
            "parsers",
            "scouting_evidence",
            "unclassified_parser_ids",
            "sources",
            "discovery_runs",
            "parser_runs",
            "failures",
            "next_actions",
            "covered_queries_usable",
            "active_pack",
        ] {
            assert!(full.get(key).is_some(), "Default status lost {key}");
        }
        assert!(
            full["parsers"][0]["declared_fields"]
                .as_array()
                .unwrap()
                .len()
                >= 100
        );
        assert!(out.stdout.len() < normal.stdout.len());
        let invalid = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args(command)
            .args(["--summary", "--state"])
            .arg(&state)
            .output()
            .unwrap();
        assert!(!invalid.status.success());
    }
    let mut legacy: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    legacy["schema_version"] = json!(1);
    for field in [
        "discovery_mode",
        "quick_depth",
        "unverified_leads",
        "query_observations",
    ] {
        legacy.as_object_mut().unwrap().remove(field);
    }
    fs::write(&state, serde_json::to_vec(&legacy).unwrap()).unwrap();
    call(
        &dir.0,
        json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
    );
    let migrated: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    assert_eq!(migrated["schema_version"], 2);
    assert_eq!(migrated["discovery_mode"], "quick");
}

#[test]
fn quick_type_only_sources_use_the_chosen_view_and_complete_only_the_selected_depth() {
    let dir = Temp::new();
    let mut target = environment();
    target["view"] = json!("selected-view");
    call(
        &dir.0,
        json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
    );
    let scout = call(
        &dir.0,
        json!({"kind":"prepare-scout","environment":target,"start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","max_seconds":120}),
    );
    assert_eq!(scout["action"]["arguments"]["repository"], "selected-view");
    assert_eq!(scout["action"]["search_budget"]["max_queries"], 1);
    assert_eq!(scout["action"]["search_budget"]["max_seconds"], 120);
    assert_eq!(scout["action"]["search_budget"]["enforced_by"], "mcp-host");
    assert_eq!(scout["action"]["authorization_required"], true);
    assert!(scout["action"]["arguments"].get("max_seconds").is_none());
    assert!(
        scout["action"]["next_action"]
            .as_str()
            .unwrap()
            .contains("cannot enforce host elapsed time")
    );
    let capture = fixture(
        &dir.0,
        "scout.json",
        json!({"result":[{"#type":"type-only","events":10}]}),
    );
    call(
        &dir.0,
        json!({"kind":"import-scout","result_file":capture,"arguments":scout["action"]["arguments"],"retrieved_at":"2026-01-02T00:00:00Z"}),
    );
    let confirmed = call(
        &dir.0,
        json!({"kind":"confirm-target","environment":target,"scope":"selected-sources","context":"Synthetic Quick view","expected_sources":[{"type":"type-only"}]}),
    );
    assert_eq!(confirmed["status"]["discovery_mode"], "quick");
    let progress = call(
        &dir.0,
        json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","max_queries":3,"max_seconds":120}),
    );
    assert_eq!(
        progress["action"]["relay"]["probes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(progress["action"]["relay"]["probes"][0]["kind"], "volume");
    let completed = relay(&dir.0, progress, &[], &[], None);
    assert_eq!(completed["status"]["quick_scope_complete"], false);
    let reconciled = call(
        &dir.0,
        json!({"kind":"reconcile","reviewed_at":"2026-01-02T00:00:00Z"}),
    );
    assert_eq!(
        reconciled["status"]["quick_scope_complete"], true,
        "{reconciled}"
    );
    assert_eq!(reconciled["status"]["full_map_complete"], false);
    assert_eq!(reconciled["status"]["parser_catalog"], "unknown");
    assert!(
        reconciled["status"]["sources"][0]["fields"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let upgraded = call(
        &dir.0,
        json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-and-fields"}),
    );
    assert_eq!(upgraded["status"]["quick_scope_complete"], false);
    assert_ne!(upgraded["status"]["sources"][0]["measurements"], "done");
    let full = call(&dir.0, json!({"kind":"set-discovery-mode","mode":"full"}));
    assert_eq!(full["status"]["selected_scope_complete"], false);
    assert_ne!(full["status"]["sources"][0]["measurements"], "done");
}

#[test]
fn imported_pack_selectors_remain_visible_unconfirmed_and_duplicate_imports_are_idempotent() {
    use cqlmatic::{
        contracts::{Cloud, EnvironmentId, Scope},
        discovery,
        environment::{Pack, PackData},
    };
    let dir = Temp::new();
    let target = EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "search-all".into(),
    };
    call(
        &dir.0,
        json!({"kind":"confirm-target","environment":target,"scope":"selected-sources","context":"Synthetic import target","expected_sources":[{"repo":"expected"}]}),
    );
    let selector = Scope {
        type_: Some("imported-type".into()),
        ..Scope::default()
    };
    let sid = discovery::source_id(&selector);
    let pack=Pack::new(target,"2026-01-02T00:00:00Z".into(),PackData{
        sources:vec![json!({"id":sid,"kind":"source","name":"Imported synthetic type","inventory_status":"observed-only","selectors":{"type":"imported-type"},"provenance_ids":["prov:synthetic-import"]})],
        fields:vec![json!({"id":"env:field:synthetic-import","source_id":sid,"path":"direct","data_type":"string","evidence_kind":"observed","provenance_ids":["prov:synthetic-import"]})],
        provenance:vec![json!({"id":"prov:synthetic-import","source_type":"mcp-observation","canonical_source":"Synthetic import","retrieved_at":"2026-01-02","redaction_status":"none","coverage_status":"observed-only"})],
        ..PackData::default()
    }).unwrap();
    let input = fixture(&dir.0, "import.json", serde_json::to_value(&pack).unwrap());
    let imported = call(&dir.0, json!({"kind":"import-pack","pack_file":input}));
    let source = imported["status"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["selectors"]["type"] == "imported-type")
        .unwrap();
    assert_eq!(source["expected"], false);
    assert_eq!(source["scouted_events"], Value::Null);
    assert_eq!(source["fields"].as_array().unwrap().len(), 1);
    let repeated = call(&dir.0, json!({"kind":"import-pack","pack_file":input}));
    assert_eq!(repeated["action"]["already_imported"], true);
    assert_eq!(
        repeated["status"]["active_pack"],
        imported["status"]["active_pack"]
    );
    let state_path = dir.0.join("workflow/state.json");
    let before = fs::read(&state_path).unwrap();
    let saved: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(saved["packs"].as_array().unwrap().len(), 1);
    let mut conflicting = pack;
    conflicting.data.fields[0]["data_type"] = json!("int");
    conflicting.content_digest = conflicting.digest().unwrap();
    fixture(
        &dir.0,
        "import.json",
        serde_json::to_value(conflicting).unwrap(),
    );
    let request = dir.0.join("request.json");
    fs::write(
        &request,
        serde_json::to_vec(
            &json!({"schema_version":1,"action":{"kind":"import-pack","pack_file":input}}),
        )
        .unwrap(),
    )
    .unwrap();
    let rejected = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["env", "inventory", "--json", "--state"])
        .arg(&state_path)
        .arg("--request")
        .arg(&request)
        .output()
        .unwrap();
    assert!(!rejected.status.success());
    assert_eq!(fs::read(state_path).unwrap(), before);
}

#[test]
fn saved_query_action_requires_protected_exact_files_and_retains_only_an_audit_summary() {
    let dir = Temp::new();
    call(
        &dir.0,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic saved query","expected_sources":[{"type":"synthetic"}]}),
    );
    let query = dir.0.join("workflow/query.cql");
    fs::write(
        &query,
        r##"#type="synthetic" | select([value, zero, flag, missing]) | head(10)"##,
    )
    .unwrap();
    let result = PathBuf::from(fixture(
        &dir.0.join("workflow"),
        "query-result.json",
        json!({"result":[{"value":"positive synthetic value","zero":0,"flag":false,"missing":null}]}),
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&query, fs::Permissions::from_mode(0o600)).unwrap();
        fs::set_permissions(&result, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let action = json!({"kind":"import-query-observations","query_file":query,"result_file":result,"environment":environment(),"source":{"type":"synthetic"},"start":"2026-01-01T00:00:00Z","end":"2026-01-02T00:00:00Z","retrieved_at":"2026-01-02T00:00:00Z","caller_assertion":"exact-query-result-target-window"});
    let imported = call(&dir.0, action.clone());
    assert_eq!(imported["action"]["field_count"], 3);
    assert_eq!(imported["action"]["pack_updated"], false);
    assert_eq!(imported["status"]["query_observation_count"], 1);
    assert!(imported["status"]["active_pack"].is_null());
    let repeated = call(&dir.0, action);
    assert_eq!(repeated["status"]["query_observation_count"], 1);
    let state_path = dir.0.join("workflow/state.json");
    let state = fs::read_to_string(&state_path).unwrap();
    assert!(!state.contains("positive synthetic value"));
    assert!(!state.contains("select(["));
    let audit = fs::read_to_string(imported["action"]["audit_path"].as_str().unwrap()).unwrap();
    assert!(!audit.contains("positive synthetic value"));
    assert!(!audit.contains("select(["));
    assert!(audit.contains("query_file"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(imported["action"]["audit_path"].as_str().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        fs::set_permissions(&result, fs::Permissions::from_mode(0o644)).unwrap();
        let request = dir.0.join("request.json");
        let before = fs::read(&state_path).unwrap();
        let denied = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .args(["env", "inventory", "--json", "--state"])
            .arg(&state_path)
            .arg("--request")
            .arg(&request)
            .output()
            .unwrap();
        assert!(!denied.status.success());
        assert_eq!(fs::read(state_path).unwrap(), before);
    }
}
