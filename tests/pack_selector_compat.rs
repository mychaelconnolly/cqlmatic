use cqlmatic::{
    contracts::{Cloud, EnvironmentId, Scope},
    discovery,
    environment::{self, Pack, PackData},
    sha256_hex,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const CREATED: &str = "2026-01-02T00:00:00Z";
const LATER: &str = "2026-01-03T00:00:00Z";
const END: &str = "2026-01-04T00:00:00Z";
const PROOF: &str = "prov:example";
const RECORD: &str = "env:repo:legacy";
const RESTRICTED: &str = "private-selector-canary";

struct Inventory(PathBuf);

impl Inventory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "cqlmatic-selector-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        Self(directory)
    }

    fn state(&self) -> PathBuf {
        self.0.join("workflow").join("inventory-canary.json")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
        for key in [
            "HOME",
            "USERPROFILE",
            "LOCALAPPDATA",
            "XDG_DATA_HOME",
            "XDG_STATE_HOME",
        ] {
            command.env(key, &self.0);
        }
        command
    }

    fn request(&self, action: Value) -> Command {
        let file = self.0.join("request.json");
        write_json(&file, &json!({"schema_version":1,"action":action}));
        let mut command = self.command();
        command
            .args(["env", "inventory", "--json", "--state"])
            .arg(self.state())
            .arg("--request")
            .arg(file);
        command
    }

    fn call(&self, action: Value) -> Value {
        success(self.request(action).output().unwrap())
    }

    fn status(&self, flags: &[&str]) -> Output {
        self.command()
            .arg("status")
            .args(flags)
            .arg("--state")
            .arg(self.state())
            .output()
            .unwrap()
    }

    fn confirm(&self, sources: Vec<Value>) {
        self.call(json!({
            "kind":"confirm-target","environment":environment_id(),"scope":"selected-sources",
            "context":"Synthetic selected sources","expected_sources":sources
        }));
    }

    fn mode(&self, mode: &str, depth: &str) {
        self.call(json!({"kind":"set-discovery-mode","mode":mode,"quick_depth":depth}));
    }

    fn import(&self, pack: &Pack) {
        let file = self.0.join("pack-canary.json");
        write_json(&file, &json!(pack));
        self.call(json!({"kind":"import-pack","pack_file":file}));
    }

    fn finish(&self, mut result: Value) -> Pack {
        for _ in 0..40 {
            let relay = &result["action"]["relay"];
            if relay["status"] == "complete" {
                let active = result["status"]["active_pack"]["path"].as_str().unwrap();
                return cqlmatic::storage::read_pack(Path::new(active)).unwrap();
            }
            assert_eq!(relay["status"], "needs-probes", "{result}");
            let index = &result["action"]["run_index"];
            let probe = &relay["probes"][0];
            let rows = match probe["kind"].as_str().unwrap() {
                "volume" => json!([{"events":10}]),
                "fieldset" => json!([{"field":"user"}]),
                "dataset-facet" => json!([]),
                "tag-detect" => json!([{"events":0}]),
                "tag-batch" => {
                    let mut row = json!({"total":10});
                    for i in 0..probe["fields"].as_array().unwrap().len() {
                        row[format!("c{i}")] = json!(0);
                    }
                    json!([row])
                }
                "population" => {
                    let mut row = json!({"total":10});
                    for i in 0..probe["fields"].as_array().unwrap().len() {
                        row[format!("c{i}")] = json!(10);
                    }
                    json!([row])
                }
                "value-facet" => json!([{"user":"synthetic","events":10}]),
                kind => panic!("unexpected probe {kind}"),
            };
            let file = self.0.join("answer.json");
            write_json(&file, &json!({"result":rows}));
            self.call(
                json!({"kind":"dispatch-discovery","run_index":index,"probe_id":probe["probe_id"]}),
            );
            result = self.call(json!({"kind":"discovery-step","run_index":index,
                "answers":[{"probe_id":probe["probe_id"],"result_file":file}]}));
        }
        panic!("Discovery did not finish");
    }

    fn snapshots(&self) -> Vec<PathBuf> {
        let mut directories = fs::read_dir(self.state().parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_dir())
            .collect::<Vec<_>>();
        directories.sort();
        directories
    }

    fn reject(&self, action: &Value, brief: bool) -> Value {
        let state = fs::read(self.state()).unwrap();
        let dirs = self.snapshots();
        let mut command = self.request(action.clone());
        if brief {
            command.arg("--brief");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty());
        if brief {
            assert!(output.stderr.len() <= 4096);
        }
        assert_eq!(fs::read(self.state()).unwrap(), state);
        assert_eq!(self.snapshots(), dirs);
        serde_json::from_slice(&output.stderr).unwrap()
    }
}

impl Drop for Inventory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn write_json(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn environment_id() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "search-all".into(),
    }
}

fn scope(repo: &str) -> Value {
    json!({"repo":repo,"type":"example"})
}

fn source_id(repo: &str) -> String {
    discovery::source_id(&serde_json::from_value::<Scope>(scope(repo)).unwrap())
}

fn record(id: &str, selectors: Value) -> Value {
    json!({"id":id,"kind":"source","name":"Synthetic source","inventory_status":"observed-only",
        "selectors":selectors,"provenance_ids":[PROOF]})
}

fn pack(created: &str, sources: Vec<Value>) -> Pack {
    Pack::new(
        environment_id(),
        created.into(),
        PackData {
            sources,
            provenance: vec![json!({"id":PROOF,"source_type":"mcp-observation",
            "canonical_source":"Synthetic pack","retrieved_at":CREATED,
            "redaction_status":"none","coverage_status":"observed-only"})],
            ..PackData::default()
        },
    )
    .unwrap()
}

fn prepare(start: &str) -> Value {
    json!({"kind":"prepare-discovery","start":start,"end":END,"max_queries":40,"max_seconds":600})
}

fn message(error: &Value) -> &str {
    error["message"].as_str().unwrap()
}

fn make_unowned(result: &Value) -> PathBuf {
    let checkpoint = PathBuf::from(
        result["status"]["discovery_runs"][0]["state_path"]
            .as_str()
            .unwrap(),
    );
    let mut state = read_json(&checkpoint);
    state["session"]
        .as_object_mut()
        .unwrap()
        .remove("inventory_owner");
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
    state["session"]["binding_sha256"] = json!(sha256_hex(&binding));
    write_json(&checkpoint, &state);
    cqlmatic::mcp::inspect_state(&checkpoint).unwrap();
    checkpoint
}

// A version 4 state written with the original field names and without newer optional keys.
const V4_STATE: &str = r#"{
  "format":"cqlmatic-inventory-state","schema_version":4,
  "created_at":"2026-01-02T00:00:00Z","updated_at":"2026-01-02T00:00:00Z",
  "discovery_mode":"quick","quick_depth":"sources-only",
  "target":{"environment":"ENV",
    "scope":"selected-sources","context":"Synthetic selected sources","confirmed_at":"2026-01-02T00:00:00Z"},
  "sources":[{"id":"SOURCE_ID","selectors":{"repo":"alpha","type":"example"},"expected":true,
    "scouted_events":null,"review":null,"binding":null}],
  "scout_plan":null,"scouting":[],"parser_pages":[],"parsers":[],"parser_exclusions":[],
  "packs":[],"active_pack":null,"discovery_runs":[],"parser_runs":[],"failures":[],
  "reconciled_evidence_sha256":null,"reconciled_at":null
}"#;

#[test]
fn exact_sourcetype_completes_discovery_from_v4_state() {
    let inventory = Inventory::new();
    let state = inventory.state();
    fs::create_dir_all(state.parent().unwrap()).unwrap();
    let fixture = V4_STATE
        .replace(
            "\"ENV\"",
            &serde_json::to_string(&environment_id()).unwrap(),
        )
        .replace("SOURCE_ID", &source_id("alpha"));
    fs::write(&state, fixture).unwrap();
    assert_eq!(
        success(inventory.status(&["--json"]))["sources"][0]["expected"],
        true
    );
    let mut selectors = scope("alpha");
    selectors["sourcetype"] = json!("example");
    inventory.import(&pack(CREATED, vec![record(RECORD, selectors)]));
    let completed = inventory.finish(inventory.call(prepare(LATER)));
    assert_eq!(completed.data.sources[0]["id"], RECORD);
    assert_eq!(
        completed.data.sources[0]["selectors"]["sourcetype"],
        "example"
    );
    assert_eq!(read_json(&state)["schema_version"], 4);
}

#[test]
fn merge_refuses_distinct_sourcetype_or_event_family() {
    for selectors in [
        json!({"repo":"alpha","type":"example","sourcetype":"EXAMPLE"}),
        json!({"repo":"alpha","sourcetype":"example"}),
        json!({"repo":"alpha","type":"example","event_family":"family"}),
    ] {
        let mut broad = selectors.clone();
        broad.as_object_mut().unwrap().remove("sourcetype");
        broad.as_object_mut().unwrap().remove("event_family");
        let retained = pack(CREATED, vec![record(RECORD, selectors)]);
        let fresh = pack(LATER, vec![record("env:repo:fresh", broad)]);
        assert!(
            environment::merge(&retained, &fresh)
                .unwrap_err()
                .message
                .contains("selector restrictions differ")
        );
    }
}

#[test]
fn legacy_fold_compares_both_event_family_restrictions() {
    for same_family in [true, false] {
        let inventory = Inventory::new();
        inventory.mode("quick", "sources-only");
        inventory.confirm(vec![scope("alpha"), scope("beta")]);
        let mut incoming = scope("alpha");
        incoming["event_family"] = json!("family");
        let old = pack(
            CREATED,
            vec![record(
                RECORD,
                if same_family {
                    incoming.clone()
                } else {
                    scope("alpha")
                },
            )],
        );
        let fresh = pack(LATER, vec![record("env:repo:fresh", incoming)]);
        let mut references = vec![];
        for (index, snapshot) in [old, fresh].iter().enumerate() {
            let path = inventory.0.join(format!("snapshot-{index}.json"));
            write_json(&path, &json!(snapshot));
            references.push(json!({"path":path,"content_digest":snapshot.content_digest,
                "imported_at":snapshot.created_at,"discovery_run":null}));
        }
        let state_path = inventory.state();
        let mut state = read_json(&state_path);
        state["packs"] = json!(references);
        write_json(&state_path, &state);
        let mut request = prepare(LATER);
        request["source_ids"] = json!([source_id("beta")]);
        if same_family {
            let completed = inventory.finish(inventory.call(request));
            let retained = completed
                .data
                .sources
                .iter()
                .find(|s| s["id"] == RECORD)
                .unwrap();
            assert_eq!(retained["selectors"]["event_family"], "family");
        } else {
            assert!(
                message(&inventory.reject(&request, false))
                    .contains("selector restrictions differ")
            );
        }
    }
}

fn incompatible_prepare(with_retained_run: bool) {
    for key in ["sourcetype", "event_family"] {
        let inventory = Inventory::new();
        inventory.mode("quick", "sources-only");
        inventory.confirm(vec![scope("alpha"), scope("beta")]);
        let mut selectors = scope("alpha");
        selectors[key] = json!(RESTRICTED);
        inventory.import(&pack(CREATED, vec![record(RECORD, selectors)]));
        let checkpoint = with_retained_run.then(|| {
            let mut request = prepare(LATER);
            request["source_ids"] = json!([source_id("beta")]);
            let prepared = inventory.call(request);
            inventory.call(json!({"kind":"pause-discovery"}));
            let path = make_unowned(&prepared);
            let bytes = fs::read(&path).unwrap();
            (path, bytes)
        });
        let mut request = prepare(LATER);
        request["source_ids"] = json!([source_id("alpha")]);
        let error = inventory.reject(&request, false);
        assert_eq!(error["code"], "INVENTORY_INPUT");
        for expected in [key, RESTRICTED, RECORD, &source_id("alpha")] {
            assert!(message(&error).contains(expected), "{error}");
        }
        let brief = inventory.reject(&request, true);
        assert!(message(&brief).contains("incompatible"), "{brief}");
        for hidden in [
            RESTRICTED,
            RECORD,
            &source_id("alpha"),
            "inventory-canary.json",
            "pack-canary.json",
            "relay-state.json",
        ] {
            assert!(!brief.to_string().contains(hidden), "{brief}");
        }
        if let Some((path, before)) = checkpoint {
            assert_eq!(fs::read(path).unwrap(), before);
        }
    }
}

#[test]
fn incompatible_first_run_allocates_no_snapshot() {
    incompatible_prepare(false);
}

#[test]
fn incompatible_prepare_does_not_bind_a_retained_checkpoint() {
    incompatible_prepare(true);
}

#[test]
fn identical_paused_prepare_does_not_bind_a_retained_checkpoint() {
    let inventory = Inventory::new();
    inventory.mode("quick", "sources-only");
    inventory.confirm(vec![scope("alpha")]);
    let request = prepare(LATER);
    let prepared = inventory.call(request.clone());
    inventory.call(json!({"kind":"pause-discovery"}));
    let path = make_unowned(&prepared);
    let before = fs::read(&path).unwrap();
    let error = inventory.reject(&request, false);
    assert!(
        message(&error).contains("identical discovery request"),
        "{error}"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn mixed_full_request_can_follow_selector_advice() {
    let inventory = Inventory::new();
    inventory.mode("full", "sources-and-fields");
    inventory.confirm(vec![scope("alpha"), scope("beta")]);
    let mut alpha = scope("alpha");
    alpha["event_family"] = json!(RESTRICTED);
    inventory.import(&pack(CREATED, vec![record(RECORD, alpha)]));
    let mut request = prepare(LATER);
    request["source_ids"] = json!([source_id("alpha"), source_id("beta")]);
    request["population_fields"] = json!([{"source":scope("alpha"),"fields":["user"]},
        {"source":scope("beta"),"fields":["user"]}]);
    let error = inventory.reject(&request, false);
    for expected in ["source_ids", "population_fields", &source_id("beta")] {
        assert!(message(&error).contains(expected), "{error}");
    }
    request["source_ids"] = json!([source_id("beta")]);
    request["population_fields"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let completed = inventory.finish(inventory.call(request));
    assert!(
        completed
            .data
            .sources
            .iter()
            .any(|s| s["selectors"]["repo"] == "beta")
    );
}

#[test]
fn single_incompatible_source_can_be_left_out_of_confirmed_target() {
    let inventory = Inventory::new();
    inventory.mode("quick", "sources-only");
    inventory.confirm(vec![scope("alpha")]);
    let mut alpha = scope("alpha");
    alpha["event_family"] = json!(RESTRICTED);
    inventory.import(&pack(CREATED, vec![record(RECORD, alpha)]));
    let error = inventory.reject(&prepare(LATER), false);
    for expected in ["No expected source is compatible", "confirmed target"] {
        assert!(message(&error).contains(expected), "{error}");
    }
    inventory.confirm(vec![scope("beta")]);
    let completed = inventory.finish(inventory.call(prepare(LATER)));
    assert!(
        completed
            .data
            .sources
            .iter()
            .any(|s| s["selectors"]["repo"] == "beta")
    );
}

#[test]
fn future_predecessor_is_refused_before_allocation() {
    let inventory = Inventory::new();
    inventory.confirm(vec![scope("alpha")]);
    let future = "2099-01-02T00:00:00Z";
    // Seed a snapshot that an earlier build could import.
    let retained = pack(future, vec![record(RECORD, scope("alpha"))]);
    let state = inventory.state();
    let path = state.parent().unwrap().join("retained-future.json");
    write_json(&path, &json!(retained));
    let reference =
        json!({"path":path,"content_digest":retained.content_digest,"imported_at":CREATED});
    let mut saved: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    saved["active_pack"] = reference.clone();
    saved["packs"] = json!([reference]);
    write_json(&state, &saved);
    let before = fs::read(&state).unwrap();
    inventory.import(&retained);
    assert_eq!(fs::read(&state).unwrap(), before);
    let error = inventory.reject(&prepare(LATER), false);
    assert_eq!(error["code"], "INVENTORY_INPUT");
    assert!(message(&error).contains(future), "{error}");
}

#[test]
fn stale_or_undated_negative_source_requires_a_window_after_the_snapshot() {
    for cause in ["stale-source", "stale-provenance", "undated-negative"] {
        let inventory = Inventory::new();
        inventory.mode("quick", "sources-only");
        inventory.confirm(vec![scope("alpha")]);
        let mut prior = pack(CREATED, vec![record(RECORD, scope("alpha"))]);
        match cause {
            "stale-source" => prior.data.sources[0]["inventory_status"] = json!("stale"),
            "stale-provenance" => prior.data.provenance[0]["coverage_status"] = json!("stale"),
            _ => prior.data.sources[0]["evidence"] = json!({"observed":false}),
        }
        prior.content_digest = prior.digest().unwrap();
        inventory.import(&prior);
        for start in ["2026-01-01T00:00:00Z", CREATED] {
            let error = inventory.reject(&prepare(start), false);
            assert_eq!(error["code"], "INVENTORY_INPUT");
            assert!(message(&error).contains(CREATED), "{error}");
            assert!(message(&error).contains(&source_id("alpha")), "{error}");
        }
        let completed = inventory.finish(inventory.call(prepare(LATER)));
        assert_eq!(completed.data.sources[0]["id"], RECORD);
        assert_eq!(completed.data.sources[0]["evidence"]["observed"], true);
    }
}

#[test]
fn status_only_exposes_retained_restrictions_in_full_summary_and_text() {
    for (key, value, compatible) in [
        ("", "", true),
        ("sourcetype", "example", true),
        ("sourcetype", RESTRICTED, false),
        ("event_family", RESTRICTED, false),
    ] {
        let inventory = Inventory::new();
        inventory.confirm(vec![scope("alpha")]);
        let mut selectors = scope("alpha");
        if !key.is_empty() {
            selectors[key] = json!(value);
        }
        inventory.import(&pack(CREATED, vec![record(RECORD, selectors)]));
        for flags in [&["--json"][..], &["--json", "--summary"]] {
            let report = success(inventory.status(flags));
            let row = &report["sources"][0];
            if key.is_empty() {
                assert!(row.get("retained_restrictions").is_none(), "{row}");
            } else {
                assert_eq!(
                    row["retained_restrictions"],
                    json!({(key): value,"discovery_compatible":compatible}),
                    "{row}"
                );
            }
        }
        let text = inventory.status(&[]);
        assert!(text.status.success(), "{text:?}");
        if !key.is_empty() {
            let text = String::from_utf8(text.stdout).unwrap();
            assert!(text.contains(&format!("{key}={value:?}")), "{text}");
            let expected = if compatible {
                "discovery-compatible"
            } else {
                "discovery-incompatible"
            };
            assert!(text.contains(expected), "{text}");
        }
        let brief = inventory.status(&["--json", "--brief"]);
        assert!(
            brief.status.success() && brief.stdout.len() <= 4096,
            "{brief:?}"
        );
        let brief = String::from_utf8(brief.stdout).unwrap();
        for hidden in [
            "retained_restrictions",
            "sourcetype",
            "event_family",
            "example",
            RESTRICTED,
        ] {
            assert!(!brief.contains(hidden), "{brief}");
        }
    }
}

#[test]
fn merge_rewrites_preexisting_parents_even_without_a_collision() {
    let old = pack(CREATED, vec![record(RECORD, scope("alpha"))]);
    let mut child = record("env:repo:child", scope("beta"));
    child["relationship"] = json!({"parent_repo_id":"env:repo:fresh"});
    let fresh = pack(LATER, vec![child, record("env:repo:fresh", scope("alpha"))]);
    let merged = environment::merge(&old, &fresh).unwrap();
    let child = merged
        .data
        .sources
        .iter()
        .find(|s| s["id"] == "env:repo:child")
        .unwrap();
    assert_eq!(child["relationship"]["parent_repo_id"], RECORD);
}

#[test]
fn a_retained_source_is_matched_once_when_fresh_records_lack_a_repo_and_type() {
    let old = pack(CREATED, vec![record(RECORD, json!({}))]);
    let fresh = pack(
        LATER,
        vec![
            record("env:repo:fresh", Value::Null),
            record("env:repo:separate", json!({})),
        ],
    );
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(merged.data.sources.len(), 2);
    assert_eq!(merged.data.sources[0]["id"], RECORD);
    assert_eq!(merged.data.sources[1]["id"], "env:repo:separate");
}

#[test]
fn discovery_collision_ids_are_stable_across_runs() {
    let inventory = Inventory::new();
    inventory.mode("quick", "sources-and-fields");
    inventory.confirm(vec![scope("alpha"), scope("beta")]);
    let beta = source_id("beta");
    let field = discovery::field_id(&beta, "user", false);
    let hash = sha256_hex(&serde_json::to_vec(&("user", "synthetic")).unwrap());
    let value = format!(
        "env:value:{}:{}",
        beta.trim_start_matches("env:repo:"),
        &hash[..16]
    );
    let mut prior = pack(CREATED, vec![record(&beta, scope("alpha"))]);
    prior
        .data
        .fields
        .push(json!({"id":field,"source_id":beta,"path":"user",
        "data_type":"string","is_tag":false,"evidence_kind":"observed","provenance_ids":[PROOF]}));
    prior.data.values.push(json!({"id":value,"field_id":field,"value":"synthetic",
        "value_kind":"sample","vocabulary":"sampled","evidence_kind":"observed","provenance_ids":[PROOF]}));
    prior.content_digest = prior.digest().unwrap();
    inventory.import(&prior);
    let mut request = prepare(LATER);
    request["source_ids"] = json!([beta]);
    request["value_fields"] = json!(["user"]);
    let derived = |kind: &str, digest_input: Vec<u8>| {
        format!("env:{kind}:merge:{}", &sha256_hex(&digest_input)[..16])
    };
    let final_source = derived(
        "repo",
        serde_json::to_vec(&("source", &beta, "beta", "example", 0)).unwrap(),
    );
    let final_field = derived(
        "field",
        serde_json::to_vec(&("field", &field, &final_source, 0)).unwrap(),
    );
    let final_value = derived(
        "value",
        serde_json::to_vec(&("value", &value, &final_field, 0)).unwrap(),
    );
    for _ in 0..2 {
        let completed = inventory.finish(inventory.call(request.clone()));
        assert_ne!(completed.created_at, CREATED);
        assert_eq!(completed.data.sources[0], prior.data.sources[0]);
        assert_eq!(completed.data.fields[0], prior.data.fields[0]);
        assert_eq!(completed.data.values[0], prior.data.values[0]);
        assert_eq!(completed.data.sources.len(), 2);
        let measured_fields = completed
            .data
            .fields
            .iter()
            .filter(|f| f["source_id"] == final_source && f["path"] == "user")
            .collect::<Vec<_>>();
        assert_eq!(completed.data.sources[1]["id"], final_source);
        assert_eq!(measured_fields.len(), 1);
        assert_eq!(measured_fields[0]["id"], final_field);
        assert_eq!(completed.data.values.len(), 2);
        assert_eq!(completed.data.values[1]["field_id"], final_field);
        assert_eq!(completed.data.values[1]["id"], final_value);
    }
}

#[test]
fn null_retained_parent_does_not_erase_a_mapped_incoming_parent() {
    let parent_scope = scope("parent");
    let child_scope = scope("child");
    let mut prior_child = record("env:repo:child_old", child_scope.clone());
    prior_child["relationship"] = json!({"parent_repo_id": null, "datasets": ["synthetic"]});
    let old = pack(
        CREATED,
        vec![
            record("env:repo:parent_old", parent_scope.clone()),
            prior_child,
        ],
    );
    let mut fresh_child = record("env:repo:child_new", child_scope);
    fresh_child["relationship"] = json!({"parent_repo_id": "env:repo:parent_new"});
    let fresh = pack(
        LATER,
        vec![record("env:repo:parent_new", parent_scope), fresh_child],
    );
    let merged = environment::merge(&old, &fresh).unwrap();
    let child = merged
        .data
        .sources
        .iter()
        .find(|s| s["id"] == "env:repo:child_old")
        .unwrap();
    assert_eq!(
        child["relationship"]["parent_repo_id"],
        "env:repo:parent_old"
    );
    assert_eq!(child["relationship"]["datasets"], json!(["synthetic"]));
}

#[test]
fn legacy_history_status_keeps_retained_equal_sourcetype_label() {
    let inventory = Inventory::new();
    inventory.mode("quick", "sources-only");
    inventory.confirm(vec![scope("alpha")]);
    let mut labeled = record("env:repo:alpha_labeled", scope("alpha"));
    labeled["selectors"]["sourcetype"] = json!("example");
    let first = pack(CREATED, vec![labeled]);
    let second = pack(LATER, vec![record("env:repo:alpha_plain", scope("alpha"))]);
    let state = inventory.state();
    let parent = state.parent().unwrap();
    let first_path = parent.join("legacy-first.json");
    let second_path = parent.join("legacy-second.json");
    fs::write(&first_path, serde_json::to_vec_pretty(&first).unwrap()).unwrap();
    fs::write(&second_path, serde_json::to_vec_pretty(&second).unwrap()).unwrap();
    let mut saved: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
    saved["active_pack"] = Value::Null;
    saved["packs"] = json!([
        {"path": first_path, "content_digest": first.content_digest, "imported_at": CREATED},
        {"path": second_path, "content_digest": second.content_digest, "imported_at": LATER},
    ]);
    fs::write(&state, serde_json::to_vec_pretty(&saved).unwrap()).unwrap();

    for flags in [&["--json"][..], &["--json", "--summary"][..]] {
        let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .arg("status")
            .args(flags)
            .arg("--state")
            .arg(&state)
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            report["sources"][0]["retained_restrictions"],
            json!({"sourcetype": "example", "discovery_compatible": true}),
            "{flags:?}: {report}"
        );
    }
    let text = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["status", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(text.status.success(), "{text:?}");
    let stdout = String::from_utf8(text.stdout).unwrap();
    assert!(
        stdout.contains("retained sourcetype=\"example\" (discovery-compatible)"),
        "{stdout}"
    );
    let brief = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["status", "--json", "--brief", "--state"])
        .arg(&state)
        .output()
        .unwrap();
    assert!(brief.status.success(), "{brief:?}");
    let brief_text = String::from_utf8(brief.stdout).unwrap();
    assert!(
        !brief_text.contains("retained_restrictions"),
        "{brief_text}"
    );
    assert!(!brief_text.contains("example"), "{brief_text}");
}
