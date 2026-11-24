use cqlmatic::{
    contracts::{Cloud, EnvironmentId},
    environment::{self, Pack, PackData},
    mcp, sha256_hex, storage,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const SOURCE: &str = "env:repo:example";
const LEGACY: &str = "env:field:dataset";
const SECOND: &str = "env:field:dataset-second";
const DISCOVERED: &str = "env:field:dataset-discovered";
const PINNED: &str =
    "#repo=example_repo #type=example_type #event.dataset=synthetic | user=* | count()";

fn day(day: u32) -> String {
    format!("2026-01-{day:02}T00:00:00Z")
}

fn environment_id() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "search-all".into(),
    }
}

fn proof(end: u32) -> String {
    format!("prov:example-{end}")
}

fn window(end: u32) -> String {
    format!("{}/{}", day(end - 1), day(end))
}

fn tag(id: &str, path: &str, end: u32) -> Value {
    json!({"id":id,"source_id":SOURCE,"path":path,"is_tag":true,"data_type":"unknown",
        "evidence_kind":"observed","observation_window":window(end),
        "coverage":{"observed":true},"provenance_ids":[proof(end)]})
}

fn sample(id: &str, field_id: &str, end: u32) -> Value {
    json!({"id":format!("env:value:{id}"),"field_id":field_id,"value":id,"value_kind":"sample",
        "vocabulary":"sampled","evidence_kind":"observed","provenance_ids":[proof(end)]})
}

fn pack(created: u32, measured: u32, fields: Vec<Value>, values: Vec<Value>) -> Pack {
    let mut measurements = vec![measured];
    for field in &fields {
        for end in 2..=6 {
            if field["provenance_ids"] == json!([proof(end)]) && !measurements.contains(&end) {
                measurements.push(end);
            }
        }
    }
    Pack::new(
        environment_id(),
        day(created),
        PackData {
            sources: vec![json!({"id":SOURCE,"kind":"source","name":"Example source",
                "inventory_status":"observed-only",
                "selectors":{"repo":"example_repo","type":"example_type"},
                "evidence":{"observed":true,"observation_window":window(measured),
                    "measured_fields":["#event.dataset"]},"provenance_ids":[proof(measured)]})],
            fields,
            values,
            provenance: measurements
                .into_iter()
                .map(|end| {
                    json!({"id":proof(end),
                "source_type":"mcp-observation","canonical_source":"Synthetic observation",
                "retrieved_at":day(end),"observation_window":window(end),
                "redaction_status":"none","coverage_status":"observed-only"})
                })
                .collect(),
            ..PackData::default()
        },
    )
    .unwrap()
}

fn seal(pack: &mut Pack) {
    pack.content_digest = pack.digest().unwrap();
    pack.validate().unwrap();
}

fn write_json(path: &Path, value: &impl serde::Serialize) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn success(output: Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

struct Workflow(PathBuf);

impl Workflow {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-tagged-fields-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn state(&self) -> PathBuf {
        self.0.join("workflow").join("inventory.json")
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

    fn call(&self, action: Value) -> Value {
        let request = self.0.join("request.json");
        write_json(&request, &json!({"schema_version":1,"action":action}));
        success(
            self.command()
                .args(["env", "inventory", "--json", "--state"])
                .arg(self.state())
                .arg("--request")
                .arg(request)
                .output()
                .unwrap(),
        )
    }

    fn confirm(&self) {
        self.call(
            json!({"kind":"confirm-target","environment":environment_id(),
            "scope":"selected-sources","context":"Synthetic selected source",
            "expected_sources":[{"repo":"example_repo","type":"example_type"}]}),
        );
        self.call(json!({"kind":"set-discovery-mode","mode":"full"}));
    }

    fn import(&self, name: &str, pack: &Pack) -> Value {
        let path = self.0.join(name);
        write_json(&path, pack);
        self.call(json!({"kind":"import-pack","pack_file":path}))
    }

    fn prepare(&self, start: u32, end: u32) -> Value {
        self.call(
            json!({"kind":"prepare-discovery","start":day(start),"end":day(end),
            "max_queries":40,"max_seconds":600}),
        )
    }

    fn finish(&self, prepared: Value) -> (Value, Pack, PathBuf) {
        let index = &prepared["action"]["run_index"];
        let mut result = prepared.clone();
        for _ in 0..40 {
            let relay = &result["action"]["relay"];
            if relay["status"] == "complete" {
                assert_eq!(relay["active_pack_updated"], true);
                let path = PathBuf::from(result["status"]["active_pack"]["path"].as_str().unwrap());
                return (relay.clone(), storage::read_pack(&path).unwrap(), path);
            }
            assert_eq!(relay["status"], "needs-probes", "{result}");
            let probe = &relay["probes"][0];
            let rows = match probe["kind"].as_str().unwrap() {
                "volume" => json!([{"events":10}]),
                "fieldset" => json!([{"field":"user"}]),
                "dataset-facet" => json!([{"#event.dataset":"synthetic","events":10}]),
                "tag-detect" => {
                    let query = probe["mcp"]["arguments"]["query_string"].as_str().unwrap();
                    json!([{"events":u32::from(query.contains("#event.dataset")) * 10}])
                }
                "tag-batch" => {
                    let mut row = json!({"total":10});
                    for (i, field) in probe["fields"].as_array().unwrap().iter().enumerate() {
                        row[format!("c{i}")] = json!(u32::from(field == "#event.dataset") * 10);
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
                kind => panic!("Unexpected probe {kind}"),
            };
            let path = self.0.join("answer.json");
            write_json(&path, &json!({"result":rows}));
            self.call(
                json!({"kind":"dispatch-discovery","run_index":index,"probe_id":probe["probe_id"]}),
            );
            result = self.call(json!({"kind":"discovery-step","run_index":index,
                "answers":[{"probe_id":probe["probe_id"],"result_file":path}]}));
        }
        panic!("Discovery did not finish");
    }

    fn strict_ready(&self, pack: &Path) {
        let check = success(
            self.command()
                .args(["check", PINNED, "--require-observed", "--json", "--pack"])
                .arg(pack)
                .output()
                .unwrap(),
        );
        assert_eq!(check["outcome"], "ready", "{check}");
    }
}

impl Drop for Workflow {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn canonical_identity_strips_one_hash_from_tags_only() {
    use environment::{canonical_field_path, field_identity, measured_field_identity};

    let tag = field_identity(SOURCE, "#event.dataset", true);
    assert_eq!(tag, field_identity(SOURCE, "event.dataset", true));
    assert_ne!(tag, field_identity(SOURCE, "event.dataset", false));
    assert_ne!(
        field_identity(SOURCE, "#foo", false),
        field_identity(SOURCE, "foo", true)
    );
    assert_eq!(canonical_field_path("#foo", false), "#foo");
    assert_eq!(canonical_field_path("##foo", true), "#foo");
    assert_eq!(canonical_field_path("#", true), "#");
    assert_eq!(measured_field_identity("#event.dataset"), (tag.1, tag.2));
    assert_eq!(
        measured_field_identity("event.dataset"),
        ("event.dataset", false)
    );
}

#[test]
fn an_id_can_follow_a_tag_spelling_change_but_cannot_be_rebound() {
    let old = pack(2, 2, vec![tag(LEGACY, "#event.dataset", 2)], vec![]);
    let mut fresh = pack(4, 4, vec![tag(LEGACY, "event.dataset", 4)], vec![]);
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(merged.data.fields, fresh.data.fields);
    for (path, is_tag) in [("event.dataset", false), ("other", true)] {
        fresh.data.fields[0]["path"] = json!(path);
        fresh.data.fields[0]["is_tag"] = json!(is_tag);
        seal(&mut fresh);
        let error = environment::merge(&old, &fresh).unwrap_err();
        assert_eq!(error.code, "PACK_INTEGRITY");
        assert!(
            error.message.contains("field ID cannot be reused"),
            "{error:?}"
        );
    }
}

fn dataset_fields(pack: &Pack) -> Vec<&Value> {
    pack.data
        .fields
        .iter()
        .filter(|field| {
            matches!(
                field["path"].as_str(),
                Some("#event.dataset" | "event.dataset")
            )
        })
        .collect()
}

#[test]
fn discovery_refreshes_a_legacy_tag_and_strict_check_is_ready() {
    let workflow = Workflow::new();
    workflow.confirm();
    let old = pack(3, 2, vec![tag(LEGACY, "#event.dataset", 2)], vec![]);
    workflow.import("legacy.json", &old);
    let (_, refreshed, path) = workflow.finish(workflow.prepare(3, 4));
    let fields = dataset_fields(&refreshed);
    assert_eq!(fields.len(), 1, "{fields:?}");
    assert_eq!(fields[0]["id"], LEGACY);
    assert_eq!(fields[0]["path"], "event.dataset");
    assert!(
        !fields[0]["provenance_ids"]
            .as_array()
            .unwrap()
            .contains(&json!(proof(2)))
    );
    workflow.strict_ready(&path);
}

#[test]
fn a_newer_discovery_folds_duplicates_and_remaps_both_values() {
    let workflow = Workflow::new();
    workflow.confirm();
    let mut first = tag(LEGACY, "#event.dataset", 2);
    first["description"] = json!("First field description");
    first["retained_context"] = Value::Null;
    first["schema_variant"] = json!("first");
    let mut second = tag(SECOND, "event.dataset", 3);
    second["description"] = json!("Second field description");
    second["schema_variant"] = json!("second");
    second["is_array"] = json!(true);
    second["coverage"]["declared"] = json!(true);
    second["data_type"] = json!("string");
    let mut old = pack(
        4,
        2,
        vec![first, second],
        vec![sample("first", LEGACY, 2), sample("second", SECOND, 3)],
    );
    for (index, proof_id) in ["prov:curation-first", "prov:curation-second"]
        .into_iter()
        .enumerate()
    {
        old.data
            .provenance
            .push(json!({"id":proof_id,"source_type":"manual-curation",
            "canonical_source":"Synthetic field description","retrieved_at":day(2),
            "redaction_status":"none","coverage_status":"declared-only"}));
        old.data.fields[index]["provenance_ids"]
            .as_array_mut()
            .unwrap()
            .push(json!(proof_id));
    }
    seal(&mut old);
    workflow.import("legacy.json", &old);
    let (_, refreshed, path) = workflow.finish(workflow.prepare(4, 5));
    let fields = dataset_fields(&refreshed);
    assert_eq!(fields.len(), 1, "{fields:?}");
    assert_eq!(fields[0]["id"], LEGACY);
    assert_eq!(fields[0]["schema_variant"], "first");
    assert_eq!(
        fields[0]["retained_context"]["text"],
        "First field description"
    );
    assert_eq!(fields[0]["is_array"], true);
    assert_eq!(fields[0]["coverage"]["declared"], true);
    assert_eq!(fields[0]["data_type"], "string");
    let evidence = fields[0]["provenance_ids"].as_array().unwrap();
    for proof_id in ["prov:curation-first", "prov:curation-second"] {
        assert!(evidence.contains(&json!(proof_id)));
    }
    for end in [2, 3] {
        assert!(!evidence.contains(&json!(proof(end))));
    }
    let values = &refreshed.data.values;
    for id in ["env:value:first", "env:value:second"] {
        assert_eq!(
            values.iter().find(|v| v["id"] == id).unwrap()["field_id"],
            LEGACY
        );
    }
    workflow.strict_ready(&path);
}

#[test]
fn a_newer_retained_duplicate_keeps_both_records_and_receives_the_incoming_value() {
    for (first_path, second_path) in [
        ("#event.dataset", "event.dataset"),
        ("event.dataset", "#event.dataset"),
    ] {
        for second_id in [DISCOVERED, SECOND] {
            let old = pack(
                5,
                2,
                vec![tag(LEGACY, first_path, 2), tag(second_id, second_path, 5)],
                vec![],
            );
            let fresh = pack(
                6,
                4,
                vec![tag(DISCOVERED, "event.dataset", 4)],
                vec![sample("incoming", DISCOVERED, 4)],
            );
            let merged = environment::merge(&old, &fresh).unwrap();
            assert_eq!(merged.data.fields, old.data.fields);
            assert_eq!(merged.data.values[0]["field_id"], second_id);
        }
    }
}

#[test]
fn a_repeated_sample_can_attach_to_the_newer_alias_without_moving_history() {
    let old = pack(
        5,
        2,
        vec![
            tag(DISCOVERED, "event.dataset", 2),
            tag(SECOND, "#event.dataset", 5),
        ],
        vec![sample("repeated", DISCOVERED, 2)],
    );
    let mut fresh = pack(
        6,
        4,
        vec![tag(DISCOVERED, "event.dataset", 4)],
        vec![sample("repeated", DISCOVERED, 4)],
    );
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(merged.data.fields, old.data.fields);
    assert_eq!(merged.data.values.len(), 2);
    assert_eq!(merged.data.values[0], old.data.values[0]);
    assert_eq!(merged.data.values[1]["field_id"], SECOND);
    assert_eq!(merged.data.values[1]["value"], "repeated");

    fresh.created_at = day(7);
    seal(&mut fresh);
    let replay = environment::merge(&merged, &fresh).unwrap();
    assert_eq!(replay.data.values, merged.data.values);

    fresh.data.values[0]["value"] = json!("unrelated sample");
    seal(&mut fresh);
    let error = environment::merge(&merged, &fresh).unwrap_err();
    assert_eq!(error.code, "PACK_INTEGRITY");
    assert!(
        error.message.contains("A value ID cannot be reused"),
        "{error:?}"
    );
}

#[test]
fn conflicting_or_fieldset_only_input_cannot_fold_a_duplicate() {
    let mut newer = tag(SECOND, "event.dataset", 5);
    newer["coverage"]["observed"] = json!(false);
    let old = pack(5, 2, vec![tag(LEGACY, "#event.dataset", 2), newer], vec![]);
    let mut fresh = pack(6, 4, vec![tag(DISCOVERED, "#event.dataset", 4)], vec![]);
    let error = environment::merge(&old, &fresh).unwrap_err();
    assert_eq!(error.code, "PACK_INTEGRITY");
    assert!(
        error
            .message
            .contains("older or tied contradictory measurement"),
        "{error:?}"
    );
    fresh.data.fields[0]["observation_window"] = Value::Null;
    fresh.data.fields[0]["coverage"] = Value::Null;
    seal(&mut fresh);
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(merged.data.fields, old.data.fields);
}

#[test]
fn tag_and_data_namespaces_stay_distinct() {
    let mut data = tag(LEGACY, "event.dataset", 2);
    data["is_tag"] = json!(false);
    let old = pack(2, 2, vec![data], vec![]);
    let fresh = pack(4, 4, vec![tag(DISCOVERED, "#event.dataset", 4)], vec![]);
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(
        merged.data.fields,
        [old.data.fields[0].clone(), fresh.data.fields[0].clone()]
    );
}

#[test]
fn named_batch_marks_only_the_omitted_tag() {
    let mut data = tag(SECOND, "#event.dataset", 2);
    data["is_tag"] = json!(false);
    let old = pack(2, 2, vec![tag(LEGACY, "#event.dataset", 2), data], vec![]);
    let mut fresh = pack(4, 4, vec![], vec![]);
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(merged.data.fields[0]["refresh_status"], "not-reobserved");
    assert_eq!(merged.data.fields[1], old.data.fields[1]);

    fresh.data.sources[0]["evidence"]["measured_fields"] = json!(["user"]);
    seal(&mut fresh);
    let merged = environment::merge(&old, &fresh).unwrap();
    assert_eq!(merged.data.fields, old.data.fields);
}

#[test]
fn omitted_field_requires_a_newer_batch() {
    let old = pack(5, 2, vec![tag(LEGACY, "event.dataset", 5)], vec![]);
    for end in [4, 5, 6] {
        let fresh = pack(7, end, vec![], vec![]);
        let merged = environment::merge(&old, &fresh).unwrap();
        if end <= 5 {
            assert_eq!(merged.data.fields, old.data.fields, "{end}");
        } else {
            assert_eq!(merged.data.fields[0]["refresh_status"], "not-reobserved");
        }
    }
}

#[test]
fn conflicting_incoming_tag_spellings_are_refused() {
    let old = pack(2, 2, vec![], vec![]);
    let fresh = pack(
        4,
        4,
        vec![
            tag(LEGACY, "#event.dataset", 4),
            tag(SECOND, "event.dataset", 4),
        ],
        vec![],
    );
    let error = environment::merge(&old, &fresh).unwrap_err();
    assert_eq!(error.code, "PACK_INTEGRITY");
    assert!(
        error.message.contains(SOURCE) && error.message.contains("#event.dataset"),
        "{error:?}"
    );
}

#[test]
fn an_undated_negative_tag_completes_activation_with_a_warning() {
    for spelling in ["#event.dataset", "event.dataset"] {
        let workflow = Workflow::new();
        workflow.confirm();
        let mut negative = tag(LEGACY, spelling, 2);
        negative["evidence_kind"] = json!("declared");
        negative["coverage"]["observed"] = json!(false);
        negative["observation_window"] = Value::Null;
        let retained = pack(5, 2, vec![negative.clone()], vec![]);
        workflow.import("legacy.json", &retained);
        let (reply, refreshed, _) = workflow.finish(workflow.prepare(3, 5));
        assert_eq!(dataset_fields(&refreshed), [&negative]);
        let diagnostics = reply["diagnostics"].as_array().unwrap();
        let warnings: Vec<_> = diagnostics
            .iter()
            .filter(|warning| warning["code"] == "FIELD_MEASUREMENT_NOT_APPLIED")
            .collect();
        assert_eq!(warnings.len(), 1, "{diagnostics:?}");
        assert_eq!(warnings[0]["severity"], "warning");
        let state = read_json(&workflow.state());
        let receipt = read_json(Path::new(
            state["discovery_runs"][0]["receipt_path"].as_str().unwrap(),
        ));
        assert!(
            receipt["diagnostics"]
                .as_array()
                .unwrap()
                .contains(warnings[0])
        );
        let message = warnings[0]["message"].as_str().unwrap();
        for text in [SOURCE, "#event.dataset", day(5).as_str()] {
            assert!(message.contains(text), "{message}");
        }
    }
}

#[test]
fn legacy_history_stages_the_same_predecessor_and_replays_without_replacing_it() {
    let workflow = Workflow::new();
    workflow.confirm();
    let mut first = pack(2, 2, vec![tag(LEGACY, "#foo", 2)], vec![]);
    let mut second = pack(
        3,
        3,
        vec![tag(LEGACY, "#foo", 3), tag(SECOND, "foo", 3)],
        vec![],
    );
    for pack in [&mut first, &mut second] {
        pack.producer_version = "0.1.13".into();
        seal(pack);
    }
    let state_path = workflow.state();
    let directory = state_path.parent().unwrap();
    let first_path = directory.join("legacy-first.json");
    let second_path = directory.join("legacy-second.json");
    write_json(&first_path, &first);
    write_json(&second_path, &second);
    let mut state = read_json(&state_path);
    state["active_pack"] = Value::Null;
    state["packs"] = json!([
        {"path":first_path,"content_digest":first.content_digest,"imported_at":day(2)},
        {"path":second_path,"content_digest":second.content_digest,"imported_at":day(3)}
    ]);
    write_json(&state_path, &state);
    success(
        workflow
            .command()
            .args(["status", "--json", "--state"])
            .arg(&state_path)
            .output()
            .unwrap(),
    );
    workflow.prepare(4, 5);
    let saved = read_json(&state_path);
    let run = &saved["discovery_runs"][0];
    let staged = Path::new(run["pack_path"].as_str().unwrap());
    let relay = read_json(Path::new(run["state_path"].as_str().unwrap()));
    let bytes = fs::read(staged).unwrap();
    assert_eq!(
        relay["session"]["predecessor_sha256"],
        sha256_hex(&bytes),
        "{}",
        relay["session"]
    );
    let mut predecessor = storage::read_pack(staged).unwrap();
    assert_eq!(predecessor.data.fields.len(), 2);
    predecessor.producer_version = "0.1.13".into();
    seal(&mut predecessor);
    let expected = [
        serde_json::to_vec_pretty(&predecessor).unwrap(),
        b"\n".to_vec(),
    ]
    .concat();
    assert_eq!(
        sha256_hex(&expected),
        "b4fb5c0518d66e849ca427ca602add6fcdc67bedf50cceecf8b0b9cecea35a8d"
    );
    let before = fs::read(&state_path).unwrap();
    let replay = workflow.import("next.json", &predecessor);
    assert_eq!(replay["action"]["already_imported"], true);
    assert_eq!(replay["action"]["pack_updated"], false);
    assert!(replay["status"]["active_pack"].is_null());
    assert_eq!(fs::read(&state_path).unwrap(), before);
    assert_eq!(fs::read(staged).unwrap(), bytes);
}

#[test]
fn existing_tag_spellings_still_validate_with_their_original_digests() {
    for (fields, digest) in [
        (
            vec![tag(LEGACY, "#event.dataset", 2)],
            "79b5be9101210358c7c0bd0be8afdd0a864f3de6b25d0231b01c25d562794a5d",
        ),
        (
            vec![
                tag(LEGACY, "#event.dataset", 2),
                tag(SECOND, "event.dataset", 2),
            ],
            "66d03593a67edfeada37bce2226d69a05ab4ec6c0385a046c50c5525c5fd5204",
        ),
    ] {
        let mut fixture = pack(2, 2, fields, vec![]);
        fixture.producer_version = "0.1.13".into();
        fixture.content_digest = digest.into();
        fixture
            .validate()
            .unwrap_or_else(|error| panic!("{error:?}: {}", fixture.digest().unwrap()));
    }
}

#[test]
fn first_activation_refuses_duplicate_canonical_tag_spellings() {
    let duplicate = pack(
        4,
        4,
        vec![
            tag(LEGACY, "#event.dataset", 4),
            tag(SECOND, "event.dataset", 4),
        ],
        vec![],
    );
    let error = environment::validate_incoming_fields(&duplicate).unwrap_err();
    assert_eq!(error.code, "PACK_INTEGRITY");
    assert!(
        error.message.contains(SOURCE) && error.message.contains("#event.dataset"),
        "{error:?}"
    );

    // Fresh inventory with no predecessor refuses a first activation producing both spellings.
    let workflow = Workflow::new();
    workflow.confirm();
    let prepared = workflow.prepare(3, 5);
    let index = &prepared["action"]["run_index"];
    let mut result = prepared.clone();
    for _ in 0..40 {
        let relay = &result["action"]["relay"];
        if relay["status"] == "failed" {
            assert_eq!(relay["active_pack_updated"], false);
            let diagnostics = relay["diagnostics"].as_array().unwrap();
            assert_eq!(diagnostics[0]["code"], "PACK_INTEGRITY", "{diagnostics:?}");
            assert!(
                diagnostics[0]["message"]
                    .as_str()
                    .unwrap()
                    .contains("#event.dataset"),
                "{diagnostics:?}"
            );
            return;
        }
        assert_eq!(relay["status"], "needs-probes", "{result}");
        let probe = &relay["probes"][0];
        let rows = match probe["kind"].as_str().unwrap() {
            "volume" => json!([{"events":10}]),
            "fieldset" => json!([{"field":"user"},{"field":"##event.dataset"}]),
            "dataset-facet" => json!([{"#event.dataset":"synthetic","events":10}]),
            "tag-detect" => {
                let query = probe["mcp"]["arguments"]["query_string"].as_str().unwrap();
                json!([{"events":u32::from(query.contains("#event.dataset")) * 10}])
            }
            "tag-batch" => {
                let mut row = json!({"total":10});
                for (i, field) in probe["fields"].as_array().unwrap().iter().enumerate() {
                    row[format!("c{i}")] = json!(u32::from(field == "#event.dataset") * 10);
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
            kind => panic!("Unexpected probe {kind}"),
        };
        let path = workflow.0.join("answer.json");
        write_json(&path, &json!({"result":rows}));
        workflow.call(
            json!({"kind":"dispatch-discovery","run_index":index,"probe_id":probe["probe_id"]}),
        );
        result = workflow.call(json!({"kind":"discovery-step","run_index":index,
            "answers":[{"probe_id":probe["probe_id"],"result_file":path}]}));
    }
    panic!("Discovery did not fail on duplicate canonical incoming tag");
}

#[test]
fn an_undated_negative_data_field_completes_activation_with_a_warning() {
    let workflow = Workflow::new();
    workflow.confirm();
    let mut negative = tag("env:field:user", "user", 2);
    negative["is_tag"] = json!(false);
    negative["evidence_kind"] = json!("declared");
    negative["coverage"]["observed"] = json!(false);
    negative["observation_window"] = Value::Null;
    let retained = pack(5, 2, vec![negative.clone()], vec![]);
    workflow.import("legacy.json", &retained);
    let (reply, refreshed, _) = workflow.finish(workflow.prepare(3, 5));
    let user_fields: Vec<_> = refreshed
        .data
        .fields
        .iter()
        .filter(|field| field["path"] == "user" && field["is_tag"] == false)
        .collect();
    assert_eq!(user_fields, [&negative]);
    let warnings: Vec<_> = reply["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|warning| warning["code"] == "FIELD_MEASUREMENT_NOT_APPLIED")
        .collect();
    assert_eq!(warnings.len(), 1, "{reply}");
    assert_eq!(warnings[0]["severity"], "warning");
    let message = warnings[0]["message"].as_str().unwrap();
    for text in [SOURCE, "user", day(5).as_str()] {
        assert!(message.contains(text), "{message}");
    }
}

#[test]
fn completed_poll_recovery_and_next_probe_preserve_field_measurement_warning() {
    let workflow = Workflow::new();
    workflow.confirm();
    let mut negative = tag(LEGACY, "#event.dataset", 2);
    negative["evidence_kind"] = json!("declared");
    negative["coverage"]["observed"] = json!(false);
    negative["observation_window"] = Value::Null;
    let retained = pack(5, 2, vec![negative], vec![]);
    workflow.import("legacy.json", &retained);
    let prepared = workflow.prepare(3, 5);
    let index = prepared["action"]["run_index"].as_u64().unwrap();

    // Drive every probe through next-probe so the final completion reply goes through compact projection.
    let mut next = workflow.call(json!({"kind":"next-probe","run_index":index}));
    let final_next = loop {
        if next["action"]["state"] == "complete" {
            break next;
        }
        assert_eq!(next["action"]["state"], "probe", "{next}");
        let probe = &next["action"]["probe"];
        let query = probe["arguments"]["query_string"].as_str().unwrap();
        let rows = if query.contains("fieldset()") {
            json!([{"field":"user"}])
        } else if query.contains("groupBy([#event.dataset]") {
            json!([{"#event.dataset":"synthetic","events":10}])
        } else if query.contains("stats([count(as=total)") {
            let mut row = json!({"total":10});
            for (i, clause) in query.split("count(").skip(2).enumerate() {
                let present = clause.contains("#event.dataset") || clause.contains("user");
                row[format!("c{i}")] = json!(u32::from(present) * 10);
            }
            json!([row])
        } else if query.contains("groupBy([user]") {
            json!([{"user":"synthetic","events":10}])
        } else if query.contains("=* | count(as=events)") {
            json!([{"events":u32::from(query.contains("#event.dataset")) * 10}])
        } else {
            json!([{"events":10}])
        };
        let path = workflow.0.join("next-answer.json");
        write_json(&path, &json!({"result":rows}));
        next = workflow.call(json!({
            "kind":"next-probe",
            "run_index":index,
            "answer":{"probe_id":probe["probe_id"],"result_file":path}
        }));
    };
    assert_eq!(final_next["action"]["settled"], true, "{final_next}");
    let next_diags = final_next["action"]["diagnostics"].as_array().unwrap();
    let next_warning = next_diags
        .iter()
        .find(|d| d["code"] == "FIELD_MEASUREMENT_NOT_APPLIED")
        .expect("next-probe completion carries FIELD_MEASUREMENT_NOT_APPLIED");
    assert_eq!(next_warning["severity"], "warning");
    let next_msg = next_warning["message"].as_str().unwrap();
    assert!(
        next_msg.contains("Discovery completed")
            && !next_msg.contains("The answer could not be applied"),
        "{next_warning}"
    );

    // Polling the completed run with discovery-step preserves FIELD_MEASUREMENT_NOT_APPLIED.
    let poll = workflow.call(json!({"kind":"discovery-step","run_index":index,"answers":[]}));
    let poll_diags = poll["action"]["relay"]["diagnostics"].as_array().unwrap();
    assert!(
        poll_diags
            .iter()
            .any(|d| d["code"] == "FIELD_MEASUREMENT_NOT_APPLIED" && d["severity"] == "warning"),
        "{poll}"
    );

    // Recovering from an interrupted activation (status == "activating" with frozen intent) also returns it.
    let state = read_json(&workflow.state());
    let run = &state["discovery_runs"][0];
    let checkpoint = PathBuf::from(run["state_path"].as_str().unwrap());
    let receipt_path = PathBuf::from(run["receipt_path"].as_str().unwrap());
    let mut interrupted = read_json(&checkpoint);
    interrupted["status"] = json!("activating");
    interrupted["pack_sha256"] = Value::Null;
    write_json(&checkpoint, &interrupted);
    let _ = fs::remove_file(&receipt_path);
    let recovered = mcp::step_resumable(&checkpoint, &[]).unwrap();
    assert_eq!(recovered.status, "complete", "{recovered:?}");
    assert!(
        recovered
            .diagnostics
            .iter()
            .any(|d| d.code == "FIELD_MEASUREMENT_NOT_APPLIED"),
        "{:?}",
        recovered.diagnostics
    );
}
