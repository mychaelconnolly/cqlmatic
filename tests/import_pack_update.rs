use cqlmatic::{
    contracts::{Cloud, EnvironmentId},
    environment::{self, ImportUpdateResult, Pack, PackData},
    inventory::InventoryState,
    storage,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const SOURCE: &str = "env:repo:example";
const FIELD: &str = "env:field:user";
const LEGACY: &str = "prov:mcp:legacy-1";
const MCP: &str = "CrowdStrike Falcon MCP falcon_search_ngsiem (agent-relayed)";

fn day(day: u32) -> String {
    format!("2026-01-{day:02}T00:00:00Z")
}

fn window(end: u32) -> String {
    format!("{}/{}", day(end - 1), day(end))
}

fn target() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "search-all".into(),
    }
}

fn source(id: &str, selectors: Value, proof: &str) -> Value {
    json!({"id":id,"kind":"source","name":"Example source","inventory_status":"observed-only",
        "selectors":selectors,"provenance_ids":[proof]})
}

fn selectors() -> Value {
    json!({"repo":"example_repo","type":"example_type"})
}

fn field(id: &str, path: &str, source: &str, proof: &str) -> Value {
    json!({"id":id,"source_id":source,"path":path,"data_type":"string","is_tag":false,
        "evidence_kind":"observed","coverage":{"observed":true},"provenance_ids":[proof]})
}

fn proof(id: &str, end: u32) -> Value {
    json!({"id":id,"source_type":"mcp-observation","canonical_source":"mcp-observation",
        "retrieved_at":day(end),"observation_window":window(end),
        "redaction_status":"none","coverage_status":"observed-only"})
}

fn sample(id: &str, field: &str, value_kind: &str, value: Value, proof: &str) -> Value {
    json!({"id":id,"field_id":field,"value":value,"value_kind":value_kind,"vocabulary":"sampled",
        "evidence_kind":"observed","provenance_ids":[proof]})
}

fn annotation(id: &str, notes: &str) -> Value {
    json!({"source_id":id,"notes":notes,"reviewed_at":day(1)})
}

fn pack(created: u32, data: PackData) -> Pack {
    Pack::new(target(), day(created), data).unwrap()
}

fn simple() -> Pack {
    pack(
        5,
        PackData {
            sources: vec![source(SOURCE, selectors(), LEGACY)],
            fields: vec![field(FIELD, "user", SOURCE, LEGACY)],
            values: vec![sample(
                "env:value:user",
                FIELD,
                "sample",
                json!("blue"),
                LEGACY,
            )],
            provenance: vec![proof(LEGACY, 4)],
            annotations: vec![annotation(SOURCE, "Example review")],
        },
    )
}

fn reseal(pack: &mut Pack) {
    pack.content_digest = pack.digest().unwrap();
    pack.validate().unwrap();
}

fn update(old: &Pack, input: &Pack) -> ImportUpdateResult {
    environment::import_update(old, input, &BTreeSet::new()).unwrap()
}

fn by_id<'a>(records: &'a [Value], id: &str) -> &'a Value {
    records.iter().find(|v| v["id"] == id).unwrap()
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
            "cqlmatic-import-update-{}-{}",
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
        let binary = std::env::var_os("CQLMATIC_TEST_BINARY")
            .unwrap_or_else(|| env!("CARGO_BIN_EXE_cqlmatic").into());
        let mut command = Command::new(binary);
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

    fn run(&self, action: Value, projection: &[&str]) -> Output {
        let request = self.0.join("request.json");
        write_json(&request, &json!({"schema_version":1,"action":action}));
        self.command()
            .args(["env", "inventory", "--json", "--state"])
            .arg(self.state())
            .arg("--request")
            .arg(request)
            .args(projection)
            .output()
            .unwrap()
    }

    fn call(&self, action: Value) -> Value {
        success(self.run(action, &[]))
    }

    fn confirm(&self) {
        self.call(json!({"kind":"confirm-target","environment":target(),
            "scope":"selected-sources","context":"Synthetic selected source",
            "expected_sources":[selectors()]}));
    }

    fn import_action(&self, pack: &Pack, mode: Option<&str>) -> Value {
        let path = self.0.join("input.json");
        write_json(&path, pack);
        let mut action = json!({"kind":"import-pack","pack_file":path});
        if let Some(mode) = mode {
            action["mode"] = json!(mode);
        }
        action
    }

    fn import(&self, pack: &Pack, mode: Option<&str>) -> Value {
        self.call(self.import_action(pack, mode))
    }

    fn prepare(&self, start: u32, end: u32) -> Value {
        json!({"kind":"prepare-discovery","start":day(start),"end":day(end),
            "max_queries":40,"max_seconds":600})
    }

    fn finish(&self, prepared: Value) -> Value {
        let index = &prepared["action"]["run_index"];
        let mut result = prepared.clone();
        for _ in 0..40 {
            let relay = &result["action"]["relay"];
            if relay["status"] == "complete" {
                assert_eq!(relay["active_pack_updated"], true, "{result}");
                assert_eq!(
                    result["status"]["discovery_runs"][index.as_u64().unwrap() as usize]["status"],
                    "complete"
                );
                return result;
            }
            assert_eq!(relay["status"], "needs-probes", "{result}");
            let probe = &relay["probes"][0];
            let rows = match probe["kind"].as_str().unwrap() {
                "volume" => json!([{"events":10}]),
                "fieldset" => json!([{"field":"user"}]),
                "dataset-facet" => json!([{"#event.dataset":"synthetic","events":10}]),
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

    fn active(reply: &Value) -> Pack {
        storage::read_pack(Path::new(
            reply["status"]["active_pack"]["path"].as_str().unwrap(),
        ))
        .unwrap()
    }
}

impl Drop for Workflow {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn refusal(output: Output, code: &str) -> String {
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], code, "{diagnostic}");
    diagnostic["message"].as_str().unwrap().into()
}

#[test]
fn append_conflicts_name_each_collection_and_leave_state_unchanged() {
    let workflow = Workflow::new();
    workflow.confirm();
    let retained = simple();
    assert_eq!(workflow.import(&retained, None)["action"]["mode"], "append");
    let before = fs::read(workflow.state()).unwrap();
    for (collection, key, property, changed) in [
        ("sources", SOURCE, "name", json!("Renamed")),
        ("fields", FIELD, "description", json!("A field")),
        ("values", "env:value:user", "meaning", json!("A value")),
        ("provenance", LEGACY, "notes", json!("An observation")),
        ("annotations", SOURCE, "notes", json!("New review")),
    ] {
        let mut input = serde_json::to_value(&retained).unwrap();
        input["data"][collection][0][property] = changed;
        let mut input: Pack = serde_json::from_value(input).unwrap();
        reseal(&mut input);
        let message = refusal(
            workflow.run(workflow.import_action(&input, None), &[]),
            "PACK_INTEGRITY",
        );
        for text in [collection, key, "mode: \"update\""] {
            assert!(message.contains(text), "{message}");
        }
        assert_eq!(fs::read(workflow.state()).unwrap(), before);
    }
    let input = pack(
        5,
        PackData {
            sources: vec![source("env:repo:reslugged", selectors(), LEGACY)],
            provenance: retained.data.provenance,
            ..PackData::default()
        },
    );
    let message = refusal(
        workflow.run(workflow.import_action(&input, None), &[]),
        "PACK_INTEGRITY",
    );
    for text in [
        "sources",
        "example_repo",
        "example_type",
        "mode: \"update\"",
    ] {
        assert!(message.contains(text), "{message}");
    }
    assert_eq!(fs::read(workflow.state()).unwrap(), before);
}

#[test]
fn simultaneous_source_maps_keep_retained_references_and_match_selectorless_shapes() {
    let (p, q, c, d) = (
        "env:repo:p",
        "env:repo:q",
        "env:source:child",
        "env:source:retained",
    );
    let (view, other_view, empty) = ("env:view:local", "env:view:second", "env:source:empty");
    let mut retained = pack(
        5,
        PackData {
            sources: vec![
                source(p, json!({"repo":"p"}), LEGACY),
                source(q, json!({"repo":"q"}), LEGACY),
                source(d, json!({"type":"retained"}), LEGACY),
                source(view, Value::Null, LEGACY),
                source(empty, json!({}), LEGACY),
            ],
            provenance: vec![proof(LEGACY, 4)],
            ..PackData::default()
        },
    );
    retained.data.sources[2]["relationship"] = json!({"parent_repo_id":q});
    reseal(&mut retained);
    let mut input = retained.clone();
    input.data.sources = vec![
        source(p, json!({"repo":"q"}), LEGACY),
        source(q, json!({"repo":"p"}), LEGACY),
        source(c, json!({"type":"child"}), LEGACY),
        source(view, Value::Null, LEGACY),
        source(other_view, Value::Null, LEGACY),
        source("env:source:new-empty", json!({}), LEGACY),
    ];
    input.data.sources[0]["name"] = json!("Second slug");
    input.data.sources[2]["relationship"] = json!({"parent_repo_id":q});
    input.data.sources[5]["name"] = json!("Unscoped");
    reseal(&mut input);
    let result = update(&retained, &input);
    let sources = &result.pack.data.sources;
    assert_eq!(sources.len(), 7);
    assert_eq!(by_id(sources, q)["name"], "Second slug");
    assert_eq!(by_id(sources, c)["relationship"]["parent_repo_id"], p);
    assert_eq!(by_id(sources, d)["relationship"]["parent_repo_id"], q);
    assert_eq!(by_id(sources, empty)["name"], "Unscoped");
    assert!(by_id(sources, view)["selectors"].is_null());
    assert!(by_id(sources, other_view)["selectors"].is_null());

    input.data.sources = vec![source(view, json!({"repo":"p"}), LEGACY)];
    reseal(&mut input);
    let error = environment::import_update(&retained, &input, &BTreeSet::new()).unwrap_err();
    assert!(
        error.message.contains(view) && error.message.contains(p),
        "{error:?}"
    );
    input.data.sources = vec![source(p, Value::Null, LEGACY)];
    reseal(&mut input);
    assert!(environment::import_update(&retained, &input, &BTreeSet::new()).is_err());
}

#[test]
fn collisions_reidentify_sources_fields_values_and_discovery_proofs() {
    let discovery = "prov:mcp:20260104T000000Z:2a0f21ac34ae9baa:population:0";
    let mut old = simple();
    old.data.provenance.push(proof(discovery, 4));
    old.data.sources[0]["evidence"] = json!({"observed":true,"observation_window":window(4)});
    reseal(&mut old);
    let mut input = simple();
    input.data.sources[0] = source(SOURCE, json!({"repo":"other"}), discovery);
    input.data.fields[0]["provenance_ids"] = json!([discovery]);
    input.data.values[0]["provenance_ids"] = json!([discovery]);
    input.data.provenance[0] = proof(discovery, 4);
    input.data.provenance[0]["canonical_source"] = json!(MCP);
    reseal(&mut input);
    let result = update(&old, &input);
    let data = &result.pack.data;
    let added = [
        &data.sources[1],
        &data.fields[1],
        &data.values[1],
        &data.provenance[2],
    ];
    for (record, prefix) in added.into_iter().zip([
        "env:repo:import:",
        "env:field:import:",
        "env:value:import:",
        "prov:mcp:import:",
    ]) {
        let suffix = record["id"].as_str().unwrap().strip_prefix(prefix).unwrap();
        assert_eq!(suffix.len(), 16);
        assert!(suffix.bytes().all(|b| b.is_ascii_hexdigit()));
    }
    assert_eq!(data.fields[1]["source_id"], data.sources[1]["id"]);
    assert_eq!(data.values[1]["field_id"], data.fields[1]["id"]);
    assert_eq!(data.annotations[1]["source_id"], data.sources[1]["id"]);
    for record in [&data.sources[1], &data.fields[1], &data.values[1]] {
        assert_eq!(record["provenance_ids"], json!([data.provenance[2]["id"]]));
    }
    assert_eq!(data.sources[0], old.data.sources[0]);
    assert_eq!(data, &update(&old, &input).pack.data);
    assert_eq!(data, &update(&result.pack, &input).pack.data);
    assert_eq!(
        serde_json::to_value(&result.counts.reidentified).unwrap(),
        json!({"sources":1,"fields":1,"values":1,"provenance":1})
    );
}

#[test]
fn incoming_dual_spelling_is_refused_on_the_first_update() {
    let workflow = Workflow::new();
    workflow.confirm();
    let mut input = simple();
    input.data.fields[0]["is_tag"] = json!(true);
    input.data.fields[0]["path"] = json!("#user");
    input
        .data
        .fields
        .push(field("env:field:plain-spelling", "user", SOURCE, LEGACY));
    input.data.fields[1]["is_tag"] = json!(true);
    reseal(&mut input);
    let before = fs::read(workflow.state()).unwrap();
    let msg = refusal(
        workflow.run(workflow.import_action(&input, Some("update")), &[]),
        "PACK_INTEGRITY",
    );
    assert!(
        msg.contains(FIELD) && msg.contains("env:field:plain-spelling") && msg.contains(SOURCE)
    );
    assert_eq!(fs::read(workflow.state()).unwrap(), before);
}

#[test]
fn duplicate_values_use_exact_ids_and_missing_and_null_do_not_match() {
    let workflow = Workflow::new();
    workflow.confirm();
    let mut input = simple();
    input.data.values.push(sample(
        "env:value:second-blue",
        FIELD,
        "sample",
        json!("blue"),
        LEGACY,
    ));
    input
        .data
        .values
        .push(sample("env:value:null", FIELD, "null", Value::Null, LEGACY));
    reseal(&mut input);
    let reply = workflow.import(&input, Some("update"));
    assert_eq!(reply["action"]["counts"]["values_added"], 3);
    let old_values = input.data.values.clone();
    input.data.sources[0]["description"] = json!("New description");
    reseal(&mut input);
    let reply = workflow.import(&input, Some("update"));
    assert_eq!(reply["action"]["not_applied"], json!([]));
    assert_eq!(Workflow::active(&reply).data.values, old_values);

    input.data.values = vec![sample(
        "env:value:null",
        FIELD,
        "missing",
        Value::Null,
        LEGACY,
    )];
    reseal(&mut input);
    let reply = workflow.import(&input, Some("update"));
    let updated = Workflow::active(&reply);
    assert_eq!(reply["action"]["counts"]["reidentified"]["values"], 1);
    assert_eq!(updated.data.values.len(), 4);
    assert_eq!(
        by_id(&updated.data.values, "env:value:null")["value_kind"],
        "null"
    );
    assert!(
        updated
            .data
            .values
            .iter()
            .any(|v| v["value_kind"] == "missing")
    );
}

#[test]
fn tagged_fold_rewrites_retained_value_references_only_after_all_matches_lose() {
    let mut old = measured(5, 4, Some(true), Some(true), "prov:previous");
    old.data.fields[0]["is_tag"] = json!(true);
    old.data.fields[0]["path"] = json!("#user");
    let mut duplicate = old.data.fields[0].clone();
    duplicate["id"] = json!("env:field:duplicate");
    duplicate["path"] = json!("user");
    duplicate["observation_window"] = json!(window(5));
    duplicate["provenance_ids"] = json!(["prov:newer-duplicate"]);
    old.data.provenance.push(proof("prov:newer-duplicate", 5));
    old.data.fields.push(duplicate);
    old.data.values.push(sample(
        "env:value:duplicate",
        "env:field:duplicate",
        "sample",
        json!("green"),
        "prov:previous",
    ));
    reseal(&mut old);
    let mut input = measured(5, 5, Some(true), Some(true), "prov:incoming");
    input.data.fields[0]["is_tag"] = json!(true);
    reseal(&mut input);
    assert_eq!(update(&old, &input).pack.data.fields.len(), 2);

    let mut input = measured(8, 7, Some(true), Some(true), "prov:fresh");
    input.data.fields[0]["is_tag"] = json!(true);
    reseal(&mut input);
    let result = update(&old, &input);
    assert_eq!(result.pack.data.fields.len(), 1);
    assert_eq!(result.pack.data.fields[0]["id"], FIELD);
    assert_eq!(result.pack.data.values[0]["field_id"], FIELD);
    result.pack.validate().unwrap();
}

fn measured(
    created: u32,
    end: u32,
    source_observed: Option<bool>,
    field_observed: Option<bool>,
    id: &str,
) -> Pack {
    let mut src = source(SOURCE, selectors(), id);
    src["evidence"] = json!({"observed":source_observed,"observation_window":window(end)});
    let mut fld = field(FIELD, "user", SOURCE, id);
    fld["observation_window"] = json!(window(end));
    fld["coverage"] = json!({"observed":field_observed});
    pack(
        created,
        PackData {
            sources: vec![src],
            fields: vec![fld],
            provenance: vec![proof(id, end)],
            ..PackData::default()
        },
    )
}

#[test]
fn observation_order_table_covers_sources_and_fields() {
    for (row, old_end, old_observed, new_end, new_observed, expected) in [
        (
            1,
            4,
            Some(true),
            7,
            Some(true),
            Some("unqualified-incoming"),
        ),
        (2, 4, Some(true), 7, Some(false), None),
        (3, 4, Some(true), 3, Some(false), Some("older-observation")),
        (4, 4, Some(true), 4, Some(false), Some("tied-observation")),
        (5, 6, Some(false), 7, Some(true), None),
        (
            6,
            6,
            Some(false),
            5,
            Some(true),
            Some("unorderable-against-negative"),
        ),
        (7, 4, None, 7, Some(false), None),
        (
            8,
            6,
            Some(true),
            7,
            Some(false),
            Some("unorderable-contradiction"),
        ),
        (9, 6, Some(true), 7, Some(true), None),
    ] {
        let old = measured(5, old_end, old_observed, old_observed, "prov:retained");
        let mut input = measured(8, new_end, new_observed, new_observed, "prov:incoming");
        if row == 1 {
            let mut stale = proof("prov:stale", new_end);
            stale["coverage_status"] = json!("stale");
            input.data.provenance.push(stale);
            input.data.sources[0]["provenance_ids"] = json!(["prov:incoming", "prov:stale"]);
            input.data.fields[0]["provenance_ids"] = json!(["prov:incoming", "prov:stale"]);
            reseal(&mut input);
        }
        let result = update(&old, &input);
        assert_eq!(
            result.not_applied.len(),
            if expected.is_some() { 2 } else { 0 },
            "row {row}: {result:?}"
        );
        for entry in &result.not_applied {
            assert_eq!(Some(entry.reason), expected, "row {row}, {entry:?}");
        }
        if expected.is_none() {
            assert_eq!(result.pack.data.sources, input.data.sources, "row {row}");
            assert_eq!(result.pack.data.fields, input.data.fields, "row {row}");
            assert_eq!(result.pack.created_at, day(new_end), "row {row}");
        } else {
            assert_eq!(result.pack.data, old.data, "row {row}");
            assert_eq!(result.pack.created_at, old.created_at);
        }
    }
}

#[test]
fn applied_measurements_keep_only_new_deployment_and_old_non_deployment_proofs() {
    let mut old = measured(5, 4, Some(false), Some(false), "prov:stale");
    old.data.provenance[0]["coverage_status"] = json!("stale");
    let mut manual = proof("prov:manual", 4);
    manual["source_type"] = json!("manual-curation");
    old.data.provenance.push(manual);
    old.data.sources[0]["provenance_ids"] = json!(["prov:stale", "prov:manual"]);
    old.data.fields[0]["provenance_ids"] = json!(["prov:stale", "prov:manual"]);
    old.data.fields[0]["refresh_status"] = json!("not-reobserved");
    reseal(&mut old);
    let input = measured(8, 7, Some(true), Some(true), "prov:fresh");
    let result = update(&old, &input);
    assert_eq!(result.pack.data.fields[0]["refresh_status"], Value::Null);
    for rec in [&result.pack.data.sources[0], &result.pack.data.fields[0]] {
        assert_eq!(rec["provenance_ids"], json!(["prov:fresh", "prov:manual"]));
    }
    let older = measured(8, 6, Some(false), Some(false), "prov:older");
    let third = update(&result.pack, &older);
    assert_eq!(third.not_applied.len(), 2);
    assert!(
        third
            .not_applied
            .iter()
            .all(|v| v.reason == "older-observation")
    );
    assert_eq!(third.pack.content_digest, result.pack.content_digest);
}

#[test]
fn own_clock_cannot_qualify_an_unmatched_or_retained_field() {
    let workflow = Workflow::new();
    workflow.confirm();
    let old = measured(5, 4, Some(true), Some(true), "prov:retained");
    let mut input = measured(8, 7, Some(true), Some(true), "prov:fresh");
    let mut out_of_clock = field("env:field:later", "later", SOURCE, "prov:later");
    out_of_clock["observation_window"] = json!(window(6));
    let later = proof("prov:later", 9);
    input.data.provenance.push(later);
    input.data.provenance[1]["observation_window"] = json!(window(6));
    // The retained clock is later than the incoming pack's clock.
    let mut ahead = old.clone();
    ahead.created_at = day(10);
    reseal(&mut ahead);
    input.data.fields.push(out_of_clock.clone());
    reseal(&mut input);
    let applied = update(&ahead, &input);
    assert!(
        !applied
            .pack
            .data
            .fields
            .iter()
            .any(|v| v["path"] == "later")
    );
    assert!(
        applied
            .not_applied
            .iter()
            .any(|v| v.id == "env:field:later" && v.reason == "date-inconsistent")
    );

    let mut old = old;
    old.data.fields.push(out_of_clock);
    old.data
        .provenance
        .push(input.data.provenance.pop().unwrap());
    reseal(&mut old);
    workflow.import(&old, None);
    let before = fs::read(workflow.state()).unwrap();
    let mut later = measured(11, 10, Some(true), Some(true), "prov:later-update");
    let message = refusal(
        workflow.run(workflow.import_action(&later, Some("update")), &[]),
        "PACK_INTEGRITY",
    );
    assert!(
        message.contains("env:field:later")
            && message.contains("retrieved_at")
            && message.contains("2026-01-09")
            && message.contains(&old.created_at),
        "{message}"
    );
    assert_eq!(fs::read(workflow.state()).unwrap(), before);

    later.data.sources[0] = old.data.sources[0].clone();
    later.data.sources[0]["description"] = json!("Metadata applies without moving the clock");
    later.data.fields.clear();
    later.data.provenance = old.data.provenance.clone();
    reseal(&mut later);
    let reply = workflow.import(&later, Some("update"));
    assert_eq!(reply["action"]["pack_updated"], true);
    let active = Workflow::active(&reply);
    assert_eq!(active.created_at, old.created_at);
    assert_eq!(active.data.fields, old.data.fields);
}

fn strict(workflow: &Workflow, pack: &Pack, query: &str, code: i32, expected: &str) {
    let path = workflow.0.join("strict.json");
    write_json(&path, pack);
    let output = workflow
        .command()
        .args(["check", query, "--require-observed", "--json", "--pack"])
        .arg(path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["outcome"], expected, "{result}");
}

#[test]
fn unqualified_array_metadata_cannot_grant_strict_grounding() {
    let workflow = Workflow::new();
    let mut old = simple();
    old.data.values.clear();
    old.data.fields[0]["path"] = json!("items[]");
    old.data.fields[0]["data_type"] = json!("unknown");
    old.data.fields[0]["is_array"] = json!(false);
    old.data
        .fields
        .push(field("env:field:user-string", "user", SOURCE, LEGACY));
    reseal(&mut old);
    let mut input = old.clone();
    input.data.fields[0]["evidence_kind"] = json!("declared");
    input.data.fields[0]["coverage"] = json!({"declared":true});
    input.data.fields[0]["data_type"] = json!("array");
    input.data.fields[0]["description"] = json!("A description");
    input.data.fields[0]["schema_variant"] = json!("Example schema");
    input.data.fields[1]["data_type"] = json!("unknown");
    reseal(&mut input);
    let result = update(&old, &input);
    let kept = &result.pack.data.fields[0];
    assert_eq!(kept["data_type"], "unknown");
    assert_eq!(kept["is_array"], false);
    assert_eq!(kept["description"], "A description");
    assert_eq!(kept["schema_variant"], "Example schema");
    assert_eq!(result.pack.data.fields[1]["data_type"], "string");
    let query = "#repo=example_repo #type=example_type | array:length(items)";
    for pack in [&old, &input, &result.pack] {
        strict(&workflow, pack, query, 1, "needs-context");
    }
}

#[test]
fn discovery_field_reidentification_preserves_scope_across_updates() {
    let workflow = Workflow::new();
    let discovery = "prov:mcp:20260107T000000Z:2a0f21ac34ae9baa:population:0";
    let mut old = measured(5, 4, Some(true), Some(true), "prov:retained");
    old.data.provenance.push(proof(discovery, 7));
    reseal(&mut old);
    let mut input = measured(8, 7, Some(true), Some(true), discovery);
    input.data.provenance[0]["canonical_source"] = json!(MCP);
    input.data.provenance.push(proof("prov:older-source", 3));
    input.data.sources[0]["evidence"] = json!({"observed":false,"observation_window":window(3)});
    input.data.sources[0]["provenance_ids"] = json!(["prov:older-source"]);
    reseal(&mut input);
    let first = update(&old, &input);
    assert_eq!(first.pack.data.sources[0], old.data.sources[0]);
    assert_eq!(first.not_applied[0].reason, "older-observation");

    let mut narrower = measured(10, 9, Some(false), Some(false), "prov:narrower");
    narrower.data.sources[0]["name"] = json!("Renamed");
    narrower.data.sources[0]["selectors"]["event_family"] = json!("Example");
    let mut family = narrower.data.fields[0].clone();
    family["id"] = json!("env:field:family-only");
    family["path"] = json!("family_only");
    family["coverage"]["observed"] = json!(true);
    narrower.data.fields.push(family);
    narrower.data.values.push(sample(
        "env:value:family-only",
        "env:field:family-only",
        "sample",
        json!("blue"),
        "prov:narrower",
    ));
    reseal(&mut narrower);
    let second = update(&first.pack, &narrower);
    assert_eq!(second.not_applied.len(), 4);
    assert!(
        second
            .not_applied
            .iter()
            .all(|v| v.reason == "discovery-evidence-selector-kept")
    );
    assert_eq!(second.pack.data.sources[0]["name"], "Renamed");
    assert_eq!(second.pack.data.sources[0]["selectors"], selectors());
    assert_eq!(second.pack.data.fields, first.pack.data.fields);
    assert_eq!(second.counts.fields_added, 0);
    assert_eq!(second.pack.data.values, first.pack.data.values);
    assert!(
        !second
            .pack
            .data
            .provenance
            .iter()
            .any(|v| v["id"] == "prov:narrower")
    );
    strict(
        &workflow,
        &second.pack,
        "#repo=example_repo #type=example_type | #type=* | family_only=* | count()",
        1,
        "needs-context",
    );
    strict(
        &workflow,
        &second.pack,
        "#repo=example_repo #type=example_type | user=* | count()",
        0,
        "ready",
    );
}

#[test]
fn selector_scope_checks_measurement_metadata_incoming_proof_and_promoted_ids() {
    let old = simple();
    let mut input = old.clone();
    input.data.sources[0]["selectors"]["event_family"] = json!("Example");
    reseal(&mut input);
    assert_eq!(
        update(&old, &input).pack.data.sources[0]["selectors"]["event_family"],
        "Example"
    );
    let scope = serde_json::from_value(selectors()).unwrap();
    let promoted = BTreeSet::from([cqlmatic::discovery::source_id(&scope)]);
    let result = environment::import_update(&old, &input, &promoted).unwrap();
    assert_eq!(
        result.not_applied[0].reason,
        "discovery-evidence-selector-kept"
    );

    let mut sources_only = old.clone();
    sources_only.data.sources[0]["evidence"] = json!({"discovery_depth":"sources-only"});
    reseal(&mut sources_only);
    assert_eq!(
        update(&sources_only, &input).not_applied[0].reason,
        "discovery-evidence-selector-kept"
    );

    let mut measured_fields = measured(5, 4, Some(true), Some(true), LEGACY);
    measured_fields.data.sources[0]["evidence"]["measured_fields"] = json!(["user"]);
    reseal(&mut measured_fields);
    assert_eq!(
        update(&measured_fields, &input).not_applied[0].reason,
        "discovery-evidence-selector-kept"
    );

    let discovery = "prov:api:20260104T000000Z:2a0f21ac34ae9baa:population:0";
    let mut api_proof = proof(discovery, 4);
    api_proof["source_type"] = json!("ngsiem-api");
    api_proof["canonical_source"] = json!("Falcon NG-SIEM search API");
    input.data.provenance.push(api_proof);
    input.data.values[0]["provenance_ids"] = json!([discovery]);
    reseal(&mut input);
    assert_eq!(
        update(&old, &input).not_applied[0].reason,
        "discovery-evidence-selector-kept"
    );
}

#[test]
fn value_updates_union_proof_and_conflicting_annotations_remain_retained() {
    let old = simple();
    let mut input = simple();
    input.data.values[0]["id"] = json!("env:value:alternate");
    input.data.values[0]["meaning"] = json!("The blue category");
    input.data.values[0]["sanitized"] = json!(false);
    input.data.values[0]["provenance_ids"] = json!(["prov:added"]);
    input.data.provenance.push(proof("prov:added", 5));
    input.data.annotations[0]["notes"] = json!("Losing review");
    reseal(&mut input);
    let result = update(&old, &input);
    assert_eq!(result.counts.values_updated, 1);
    assert_eq!(result.pack.data.values.len(), 1);
    let value = &result.pack.data.values[0];
    assert_eq!(value["id"], old.data.values[0]["id"]);
    assert_eq!(value["meaning"], "The blue category");
    assert_eq!(value["sanitized"], false);
    assert_eq!(value["provenance_ids"], json!([LEGACY, "prov:added"]));
    assert_eq!(result.pack.data.annotations, old.data.annotations);
    assert_eq!(result.not_applied[0].reason, "retained-annotation-kept");
}

#[test]
fn append_uses_input_digest_after_promotion_without_masking_cumulative_conflicts() {
    let workflow = Workflow::new();
    workflow.confirm();
    let extra = pack(
        5,
        PackData {
            sources: vec![source(
                "env:repo:other",
                json!({"repo":"other"}),
                "prov:other",
            )],
            provenance: vec![proof("prov:other", 4)],
            ..PackData::default()
        },
    );
    let base = simple();
    let first = workflow.import(&extra, None);
    assert!(
        first["status"]["active_pack"]
            .get("input_content_digest")
            .is_none()
    );
    let second = workflow.import(&base, None);
    assert_eq!(
        second["status"]["active_pack"]["input_content_digest"],
        base.content_digest
    );
    let cumulative = Workflow::active(&second);
    let done = workflow.finish(workflow.call(workflow.prepare(6, 7)));
    assert!(
        done["status"]["active_pack"]
            .get("input_content_digest")
            .is_none()
    );
    let before = fs::read(workflow.state()).unwrap();
    assert_eq!(
        workflow.import(&base, None)["action"]["already_imported"],
        true
    );
    assert_eq!(fs::read(workflow.state()).unwrap(), before);
    let message = refusal(
        workflow.run(workflow.import_action(&cumulative, None), &[]),
        "PACK_INTEGRITY",
    );
    assert!(message.contains(SOURCE), "{message}");
    assert_eq!(fs::read(workflow.state()).unwrap(), before);

    // Use the earlier snapshot so the promoted run is the only discovery marker.
    let mut without_markers = read_json(&workflow.state());
    without_markers["active_pack"] = second["status"]["active_pack"].clone();
    write_json(&workflow.state(), &without_markers);
    let mut narrower = base;
    narrower.data.sources[0]["selectors"]["event_family"] = json!("Restricted");
    reseal(&mut narrower);
    let reply = workflow.import(&narrower, Some("update"));
    assert_eq!(reply["action"]["pack_updated"], false);
    assert_eq!(
        reply["action"]["not_applied"][0]["reason"],
        "discovery-evidence-selector-kept"
    );

    let discovery_only = Workflow::new();
    discovery_only.confirm();
    let done = discovery_only.finish(discovery_only.call(discovery_only.prepare(6, 7)));
    assert!(
        done["status"]["active_pack"]
            .get("input_content_digest")
            .is_none()
    );
    let reply = discovery_only.import(&extra, None);
    assert_eq!(
        reply["status"]["active_pack"]["input_content_digest"],
        extra.content_digest
    );
}

#[test]
fn update_reevaluates_replayed_inputs_and_unchanged_evidence() {
    let workflow = Workflow::new();
    workflow.confirm();
    let mut original = simple();
    original.data.sources[0]["selectors"]["event_family"] = json!("Example");
    original.data.sources[0]["description"] = json!("Original description");
    reseal(&mut original);
    workflow.import(&original, None);
    let mut changed = original.clone();
    changed.data.sources[0]["selectors"] = selectors();
    changed.data.sources[0]["description"] = json!("Revised description");
    reseal(&mut changed);
    let reply = workflow.import(&changed, Some("update"));
    assert_eq!(reply["action"]["not_applied"], json!([]));
    assert_eq!(reply["action"]["counts"]["fields_unchanged"], 1);
    assert_eq!(reply["action"]["counts"]["sources_updated"], 1);
    let restored = workflow.import(&original, Some("update"));
    assert_eq!(restored["action"]["pack_updated"], true);
    assert_eq!(Workflow::active(&restored).data, original.data);
    let before = fs::read(workflow.state()).unwrap();
    let repeat = workflow.import(&original, Some("update"));
    assert_eq!(repeat["action"]["pack_updated"], false);
    assert_eq!(repeat["action"]["counts"]["sources_unchanged"], 1);
    assert_eq!(repeat["action"]["not_applied"], json!([]));
    assert_eq!(fs::read(workflow.state()).unwrap(), before);
}

#[test]
fn imported_digests_are_validated_without_changing_reconciliation() {
    let workflow = Workflow::new();
    workflow.confirm();
    workflow.import(&simple(), None);
    workflow.call(json!({"kind":"reconcile","reviewed_at":day(5)}));
    let stored = read_json(&workflow.state());
    let before: InventoryState = serde_json::from_value(stored.clone()).unwrap();
    let evidence = before.evidence_digest().unwrap();
    assert_eq!(
        before.reconciled_evidence_sha256.as_deref(),
        Some(evidence.as_str())
    );
    let mut after = before;
    let digest = "a".repeat(64);
    after.active_pack.as_mut().unwrap().input_content_digest = Some(digest.clone());
    after.packs[0].input_content_digest = Some(digest.clone());
    after.validate().unwrap();
    assert_eq!(after.evidence_digest().unwrap(), evidence);
    assert_eq!(
        after.reconciled_evidence_sha256.as_deref(),
        Some(evidence.as_str())
    );

    for invalid_digest in ["A".repeat(64), "a".repeat(63), "g".repeat(64)] {
        after.packs[0].input_content_digest = Some(invalid_digest);
        assert!(after.validate().is_err());
    }
    after.packs[0].input_content_digest = Some(digest);
    after.packs[0].discovery_run = Some("example-run".into());
    assert!(after.validate().is_err());
}

#[test]
fn projections_cap_summary_and_keep_brief_to_counts() {
    let workflow = Workflow::new();
    workflow.confirm();
    let mut old = measured(5, 4, Some(true), Some(true), LEGACY);
    let fields: Vec<_> = (0..25)
        .map(|i| {
            let mut f = old.data.fields[0].clone();
            f["id"] = json!(format!("env:field:private-example-{i}"));
            f["path"] = json!(format!("private_example_{i}"));
            f
        })
        .collect();
    old.data.fields = fields;
    reseal(&mut old);
    workflow.import(&old, None);
    let mut input = old.clone();
    for field in &mut input.data.fields {
        field["coverage"]["observed"] = json!(false);
    }
    reseal(&mut input);
    let request = workflow.import_action(&input, Some("update"));
    let full = workflow.call(request.clone());
    assert_eq!(full["action"]["mode"], "update");
    assert_eq!(full["action"]["pack_updated"], false);
    assert_eq!(full["action"]["counts"]["fields_unchanged"], 25);
    let skipped = full["action"]["not_applied"].as_array().unwrap();
    assert_eq!(skipped.len(), 25);
    let summary = success(workflow.run(request.clone(), &["--summary"]));
    assert_eq!(summary["action"]["not_applied_count"], 25);
    assert_eq!(summary["action"]["not_applied"], json!(skipped[..20]));
    let brief = workflow.run(request, &["--brief"]);
    assert!(brief.stdout.len() <= 4096);
    let text = String::from_utf8(brief.stdout.clone()).unwrap();
    for sensitive in [
        "private-example",
        "example_repo",
        "input.json",
        "inventory.json",
        "selectors",
    ] {
        assert!(!text.contains(sensitive), "{text}");
    }
    let brief = success(brief);
    assert_eq!(
        brief["action"],
        json!({"counts":full["action"]["counts"],"not_applied_count":25})
    );
}

#[test]
fn future_dates_tampered_packs_and_environment_mismatch_are_refused_atomically() {
    let workflow = Workflow::new();
    workflow.confirm();
    let before = fs::read(workflow.state()).unwrap();
    let mut input = simple();
    input.created_at = "2099-01-01T00:00:00Z".into();
    reseal(&mut input);
    for mode in ["append", "update"] {
        let request = workflow.import_action(&input, Some(mode));
        let message = refusal(workflow.run(request, &[]), "INVENTORY_INPUT");
        assert!(message.contains(&input.created_at) && message.contains("created_at"));
        assert_eq!(fs::read(workflow.state()).unwrap(), before);
    }
    let mut input = simple();
    input.data.sources[0]["name"] = json!("Unsealed edit");
    refusal(
        workflow.run(workflow.import_action(&input, Some("update")), &[]),
        "PACK_DIGEST",
    );
    input.environment.view = "different-view".into();
    reseal(&mut input);
    refusal(
        workflow.run(workflow.import_action(&input, Some("update")), &[]),
        "PACK_ENVIRONMENT_MISMATCH",
    );
    assert_eq!(fs::read(workflow.state()).unwrap(), before);
}

#[test]
fn remediation_removes_legacy_restrictions_and_discovery_promotes() {
    let workflow = Workflow::new();
    workflow.confirm();
    let mut old = simple();
    old.data.sources[0]["name"] = json!("Example legacy name");
    old.data.sources[0]["inventory_status"] = json!("stale");
    old.data.sources[0]["evidence"] = json!({"observed":false});
    old.data.sources[0]["selectors"]["event_family"] = json!("Example");
    old.data.sources[0]["selectors"]["sourcetype"] = json!("legacy_type");
    reseal(&mut old);
    workflow.import(&old, None);
    let saved = read_json(&workflow.state());
    let source_id = &saved["sources"][0]["id"];
    workflow.call(
        json!({"kind":"review-source","source_id":source_id,"purpose":"Local examples",
        "data_model":"Synthetic observations","reviewed_at":day(5),"evidence":"Local fixture"}),
    );
    workflow.call(
        json!({"kind":"parser-not-applicable","source_id":source_id,"reason":"falcon-sensor-native",
        "reviewed_at":day(5),"evidence":"Local fixture"}),
    );
    let saved = read_json(&workflow.state());
    let prepare = workflow.prepare(6, 7);
    let message = refusal(workflow.run(prepare.clone(), &[]), "INVENTORY_INPUT");
    assert!(message.contains("To remove or update an imported restriction on a source without discovery measurements, build a valid updated pack with `cqlmatic env import --from JSONL_DIR --environment ENV_JSON --out PACK_JSON` (keep the source's current name, omit the restriction from selectors; fields.jsonl may be empty) and submit import-pack with mode \"update\"."), "{message}");

    let dir = workflow.0.join("jsonl");
    fs::create_dir(&dir).unwrap();
    let mut minimal = source(SOURCE, selectors(), "prov:manual");
    minimal["name"] = old.data.sources[0]["name"].clone();
    minimal["description"] = json!("Updated source description");
    minimal["inventory_status"] = json!("inventoried");
    let mut manual = proof("prov:manual", 5);
    manual["source_type"] = json!("manual-curation");
    manual["canonical_source"] = json!("Manual entry");
    manual["coverage_status"] = json!("declared-only");
    fs::write(
        dir.join("sources.jsonl"),
        serde_json::to_vec(&minimal).unwrap(),
    )
    .unwrap();
    fs::write(
        dir.join("provenance.jsonl"),
        serde_json::to_vec(&manual).unwrap(),
    )
    .unwrap();
    fs::write(dir.join("fields.jsonl"), []).unwrap();
    let environment = workflow.0.join("environment.json");
    write_json(&environment, &target());
    let pack = workflow.0.join("converted.json");
    let output = workflow
        .command()
        .args(["env", "import", "--from"])
        .arg(dir)
        .arg("--environment")
        .arg(environment)
        .arg("--out")
        .arg(&pack)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let request = json!({"kind":"import-pack","mode":"update","pack_file":pack});
    let reply = workflow.call(request.clone());
    assert_eq!(reply["action"]["pack_updated"], true);
    let after = read_json(&workflow.state());
    assert_eq!(after["sources"][0]["review"], saved["sources"][0]["review"]);
    assert_eq!(
        after["sources"][0]["binding"],
        saved["sources"][0]["binding"]
    );
    let active = Workflow::active(&reply);
    assert_eq!(active.data.fields, old.data.fields);
    assert_eq!(active.data.sources[0]["name"], minimal["name"]);
    assert_eq!(
        active.data.sources[0]["description"],
        minimal["description"]
    );
    assert_eq!(active.data.sources[0]["selectors"], selectors());
    assert_eq!(
        active.data.sources[0]["evidence"],
        old.data.sources[0]["evidence"]
    );
    assert_eq!(active.data.sources[0]["inventory_status"], "stale");
    let brief = workflow.run(request, &["--brief"]);
    let brief_bytes = brief.stdout.len();
    assert!(brief_bytes <= 4096);
    let brief = success(brief);
    let done = workflow.finish(workflow.call(prepare));
    assert_eq!(active.created_at, old.created_at);
    assert_eq!(
        Workflow::active(&done).data.sources[0]["name"],
        minimal["name"]
    );
    assert!(done["status"]["active_pack"]["discovery_run"].is_string());
    let run_dir = Path::new(
        done["status"]["active_pack"]["discovery_run"]
            .as_str()
            .unwrap(),
    );
    let promoted_pack = run_dir.parent().unwrap().join("pack.json");
    assert_eq!(
        done["status"]["active_pack"]["path"],
        json!(std::path::absolute(promoted_pack).unwrap()),
    );
    println!(
        "env import succeeded with empty fields.jsonl; update cleared both restrictions and preserved the source name and snapshot clock; discovery completed after {} answers and promoted the active pack; brief {brief_bytes} bytes ({} skipped).",
        done["action"]["relay"]["answered"], brief["action"]["not_applied_count"]
    );
}

#[test]
fn own_clock_prevents_strengthening_an_applied_or_retained_key() {
    let wf = Workflow::new();
    wf.confirm();
    let mut old = simple();
    old.created_at = day(12);
    reseal(&mut old);
    wf.import(&old, None);

    // Incoming field links an on-time proof (day 4) and a post-clock proof (day 10) in a day-6 pack.
    let mut first = measured(6, 4, Some(true), Some(true), "prov:on-time");
    let mut later = proof("prov:too-late", 4);
    later["retrieved_at"] = json!(day(10));
    first.data.provenance.push(later);
    first.data.fields[0]["provenance_ids"] = json!(["prov:on-time", "prov:too-late"]);
    reseal(&mut first);
    let r = wf.import(&first, Some("update"));
    assert_eq!(
        Workflow::active(&r).data.fields[0]["provenance_ids"],
        json!(["prov:on-time"])
    );

    // A day-8 negative correction for the same window must beat the day-4 key.
    let mut correcting = measured(9, 4, Some(true), Some(false), "prov:corrected");
    correcting.data.provenance[0]["retrieved_at"] = json!(day(8));
    reseal(&mut correcting);
    let r = wf.import(&correcting, Some("update"));
    assert_eq!(
        Workflow::active(&r).data.fields[0]["coverage"]["observed"],
        false
    );
    let q = "#repo=example_repo #type=example_type | user=* | count()";
    strict(&wf, &Workflow::active(&r), q, 1, "needs-context");

    // A retained field that already has an on-time key (day 4) plus a post-clock proof (day 10)
    // must refuse an unrelated update that advances the cumulative clock past day 10.
    let mut retained_two = measured(6, 4, Some(true), Some(true), "prov:on-time");
    let mut later_ret = proof("prov:too-late", 4);
    later_ret["retrieved_at"] = json!(day(10));
    retained_two.data.provenance.push(later_ret);
    retained_two.data.fields[0]["provenance_ids"] = json!(["prov:on-time", "prov:too-late"]);
    reseal(&mut retained_two);
    let mut unrelated = measured(12, 11, Some(true), Some(true), "prov:unrelated");
    unrelated.data.fields[0]["id"] = json!("env:field:other");
    unrelated.data.fields[0]["path"] = json!("other");
    reseal(&mut unrelated);
    let err = environment::import_update(&retained_two, &unrelated, &BTreeSet::new())
        .expect_err("strengthening a retained field key must refuse");
    assert_eq!(err.code, "PACK_INTEGRITY");
    assert!(err.message.contains("retrieved_at"), "{}", err.message);
}

#[test]
fn retained_and_unmatched_inventoried_sources_never_gain_measurement_authority() {
    let wf = Workflow::new();
    wf.confirm();
    let mut old = measured(8, 4, Some(false), Some(true), "prov:old");
    old.data.sources[0]["inventory_status"] = json!("inventoried");
    reseal(&mut old);
    wf.import(&old, None);

    // Retained inventoried negative (day 8) has no ordering key; incoming window starting day 5
    // is <= day 8 so row 6 keeps the negative.
    let mut inc = measured(8, 6, Some(true), Some(true), "prov:new");
    inc.data.fields.clear();
    reseal(&mut inc);
    let reply = wf.import(&inc, Some("update"));
    assert_eq!(
        reply["action"]["not_applied"],
        json!([{"collection":"sources","id":SOURCE,"reason":"unorderable-against-negative"}])
    );
    let q = "#repo=example_repo #type=example_type | user=* | count()";
    strict(&wf, &Workflow::active(&reply), q, 1, "needs-context");

    // Unmatched inventoried source with post-clock retrieval (day 10 in day 5 pack) imported into
    // day 12 inventory must be blocked as date-inconsistent rather than gaining a reader key.
    let wf2 = Workflow::new();
    wf2.confirm();
    let mut base = simple();
    base.created_at = day(12);
    reseal(&mut base);
    wf2.import(&base, None);
    let mut post_clock_inv = measured(5, 4, Some(false), None, "prov:new");
    post_clock_inv.data.sources[0]["selectors"] = json!({"repo":"other"});
    post_clock_inv.data.sources[0]["inventory_status"] = json!("inventoried");
    post_clock_inv.data.provenance[0]["retrieved_at"] = json!(day(10));
    post_clock_inv.data.fields.clear();
    reseal(&mut post_clock_inv);
    let r = wf2.import(&post_clock_inv, Some("update"));
    assert_eq!(r["action"]["pack_updated"], false);
    assert_eq!(
        r["action"]["not_applied"],
        json!([{"collection":"sources","id":SOURCE,"reason":"date-inconsistent"}])
    );
    assert_eq!(Workflow::active(&r).data.sources.len(), 1);
}

#[test]
fn applied_value_observations_advance_cumulative_clock_without_display_only_bumps() {
    let wf = Workflow::new();
    wf.confirm();
    let mut old = simple();
    let mut absent = field("env:field:absent", "absent", SOURCE, LEGACY);
    absent["coverage"]["observed"] = json!(false);
    old.data.fields.push(absent);
    reseal(&mut old);
    wf.import(&old, None);

    // Display-only value edit (same proof) must not advance the clock.
    let mut display_only = old.clone();
    display_only.created_at = day(12);
    display_only.data.values[0]["meaning"] = json!("Clarified sample");
    reseal(&mut display_only);
    let d = wf.import(&display_only, Some("update"));
    assert_eq!(Workflow::active(&d).created_at, day(5));

    // Adding a qualifying own-clock deployment proof to a value must advance the clock to day 10.
    let mut inc = Workflow::active(&d);
    inc.created_at = day(12);
    inc.data.values[0]["provenance_ids"] = json!(["prov:new"]);
    inc.data.values[0]["meaning"] = json!("Latest sample");
    inc.data.provenance.push(proof("prov:new", 10));
    reseal(&mut inc);
    let r = wf.import(&inc, Some("update"));
    assert_eq!(Workflow::active(&r).created_at, day(10));

    // A day 6-7 positive observation cannot clear the undated negative once the clock is day 10.
    let mut earlier = measured(8, 7, Some(true), Some(true), "prov:day7");
    earlier.data.fields[0]["path"] = json!("absent");
    earlier.data.fields[0]["id"] = json!("env:field:absent");
    reseal(&mut earlier);
    let second = wf.import(&earlier, Some("update"));
    let q = "#repo=example_repo #type=example_type | absent=* | count()";
    strict(&wf, &Workflow::active(&second), q, 1, "needs-context");
}

#[test]
fn collision_reidentified_selectorless_sources_and_values_replay_idempotently() {
    // Selectorless source colliding with an empty-object selector source.
    let wf = Workflow::new();
    wf.confirm();
    let mut old = simple();
    old.data.sources[0]["selectors"] = json!({});
    reseal(&mut old);
    wf.import(&old, None);
    let mut input = old;
    input.data.sources[0]["selectors"] = Value::Null;
    reseal(&mut input);
    let first = wf.import(&input, Some("update"));
    assert_eq!(first["action"]["pack_updated"], true);
    assert_eq!(Workflow::active(&first).data.sources.len(), 2);
    for _ in 0..2 {
        let replay = wf.import(&input, Some("update"));
        assert_eq!(replay["action"]["pack_updated"], false);
        let p = Workflow::active(&replay);
        assert_eq!(p.data.sources.len(), 2);
        assert_eq!(p.data.fields.len(), 2);
        assert_eq!(p.data.values.len(), 2);
        assert_eq!(p.data.annotations.len(), 2);
    }

    // Colliding value under an ambiguous key replays idempotently without growing snapshots.
    let wf2 = Workflow::new();
    wf2.confirm();
    let mut base = simple();
    for name in ["env:value:other-green", "env:value:third-green"] {
        base.data
            .values
            .push(sample(name, FIELD, "sample", json!("green"), LEGACY));
    }
    reseal(&mut base);
    wf2.import(&base, None);
    let mut incoming = simple();
    incoming.data.values[0]["value"] = json!("green");
    incoming.data.values[0]["meaning"] = json!("Imported green");
    reseal(&mut incoming);
    let first = wf2.import(&incoming, Some("update"));
    assert_eq!(first["action"]["pack_updated"], true);
    assert_eq!(Workflow::active(&first).data.values.len(), 4);
    let snapshots_after_first = read_json(&wf2.state())["packs"].as_array().unwrap().len();
    for _ in 0..2 {
        let replay = wf2.import(&incoming, Some("update"));
        assert_eq!(replay["action"]["pack_updated"], false);
        assert_eq!(Workflow::active(&replay).data.values.len(), 4);
        assert_eq!(
            read_json(&wf2.state())["packs"].as_array().unwrap().len(),
            snapshots_after_first
        );
    }
}

#[test]
fn multiple_incoming_values_preserve_both_meanings_independent_of_order() {
    let old = simple();
    let mut inc = old.clone();
    inc.data.values[0]["meaning"] = json!("Analyst A's meaning");
    let mut peer = inc.data.values[0].clone();
    peer["id"] = json!("env:value:independent");
    peer["meaning"] = json!("Analyst B's meaning");
    inc.data.values.push(peer);
    reseal(&mut inc);

    for _ in 0..2 {
        let wf = Workflow::new();
        wf.confirm();
        wf.import(&old, None);
        let r = wf.import(&inc, Some("update"));
        let p = Workflow::active(&r);
        assert_eq!(p.data.values.len(), 2);
        let original_id = old.data.values[0]["id"].as_str().unwrap();
        assert_eq!(
            by_id(&p.data.values, original_id)["meaning"],
            "Analyst A's meaning"
        );
        assert_eq!(
            by_id(&p.data.values, "env:value:independent")["meaning"],
            "Analyst B's meaning"
        );
        inc.data.values.reverse();
        reseal(&mut inc);
    }
}

#[test]
fn folded_field_matches_exact_id_value_correction_in_place() {
    let wf = Workflow::new();
    wf.confirm();
    let mut old = measured(5, 4, Some(true), Some(true), "prov:prior");
    old.data.fields[0]["is_tag"] = json!(true);
    old.data.fields[0]["path"] = json!("#user");
    let mut alias = old.data.fields[0].clone();
    alias["id"] = json!("env:field:alias");
    alias["path"] = json!("user");
    old.data.fields.push(alias);
    let mut v = sample(
        "env:value:green",
        "env:field:alias",
        "sample",
        json!("green"),
        "prov:prior",
    );
    v["meaning"] = json!("Old meaning");
    old.data.values.push(v.clone());
    reseal(&mut old);
    wf.import(&old, None);

    let mut inc = measured(8, 7, Some(true), Some(true), "prov:fresh");
    inc.data.fields[0]["is_tag"] = json!(true);
    v["field_id"] = json!(FIELD);
    v["meaning"] = json!("Corrected meaning");
    v["provenance_ids"] = json!(["prov:fresh"]);
    inc.data.values.push(v);
    reseal(&mut inc);

    let r = wf.import(&inc, Some("update"));
    let p = Workflow::active(&r);
    assert_eq!(p.data.fields.len(), 1);
    assert_eq!(p.data.values.len(), 1);
    assert_eq!(p.data.values[0]["id"], "env:value:green");
    assert_eq!(p.data.values[0]["field_id"], FIELD);
    assert_eq!(p.data.values[0]["meaning"], "Corrected meaning");
    assert_eq!(
        p.data.values[0]["provenance_ids"],
        json!(["prov:prior", "prov:fresh"])
    );
    assert_eq!(r["action"]["counts"]["values_added"], 0);
}
