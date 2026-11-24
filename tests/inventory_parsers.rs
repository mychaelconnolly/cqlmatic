//! A parser import keeps review work: bindings, their line reviews and exclusions.

use cqlmatic::{
    contracts::{Cloud, EnvironmentId, Scope},
    discovery,
    environment::{Pack, PackData},
    inventory::*,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
};

const RETRIEVED: &str = "2026-01-02T00:00:00Z";
const SCRIPT: &str = "user := \"synthetic\"\nparseJson()";
const SUSPENDED_REPLY: &str = "Some parser bindings or exclusions are suspended: the latest capture lacks the exact parser definition each was reviewed against, so they count for nothing until restored. They are kept with their line reviews and reasons. Import or fetch a capture that includes those definitions to restore them; where a parser has changed, review its new definition and bind or exclude it again.";
const SUSPENDED_SOURCE: &str = "This source's parser binding is suspended: the latest capture lacks the exact parser definition it was reviewed against, so it counts for nothing until restored. Import or fetch a capture that includes that definition to restore it with its line reviews, or review the source's current parser definition and bind it again.";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-parsers-{}-{}",
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
    fn state(&self) -> PathBuf {
        self.0.join("workflow/state.json")
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn cli(dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    for variable in [
        "HOME",
        "USERPROFILE",
        "LOCALAPPDATA",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
    ] {
        command.env(variable, dir);
    }
    command
}
fn run(dir: &Path, action: Value, flags: &[&str]) -> Output {
    let request = dir.join("request.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({"schema_version":1,"action":action})).unwrap(),
    )
    .unwrap();
    cli(dir)
        .args(["env", "inventory", "--json", "--state"])
        .arg(dir.join("workflow/state.json"))
        .args(flags)
        .arg("--request")
        .arg(request)
        .output()
        .unwrap()
}
fn call_with(dir: &Path, action: Value, flags: &[&str]) -> Value {
    let output = run(dir, action, flags);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.len() <= 4096 || !flags.contains(&"--brief"));
    serde_json::from_slice(&output.stdout).unwrap()
}
fn call(dir: &Path, action: Value) -> Value {
    call_with(dir, action, &[])
}
fn environment() -> Value {
    json!({"cloud":"us1","tenant":"0".repeat(32),"view":"search-all"})
}
/// Confirms one selected source and returns its inventory source ID.
fn confirm_source(dir: &Path) -> String {
    confirm_sources(dir, json!([{"repo":"synthetic-repo"}]))[0]["source_id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn confirm_sources(dir: &Path, expected_sources: Value) -> Value {
    call(
        dir,
        json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
    );
    call(
        dir,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic parser review","expected_sources":expected_sources}),
    )["status"]["sources"]
        .clone()
}
fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    fs::write(&path, bytes).unwrap();
    path.to_string_lossy().into_owned()
}
fn detail(dir: &Path, name: &str, resource: Value) -> String {
    write(
        dir,
        name,
        &serde_json::to_vec_pretty(&json!({"resources":[resource]})).unwrap(),
    )
}
fn resource(id: &str, script: &str) -> Value {
    json!({"id":id,"name":"Synthetic parser","script":script,"fields_to_tag":["event.kind"]})
}
/// The same parser resource as `resource`, with other key order, spacing and response metadata.
fn re_encoded(dir: &Path, name: &str, id: &str, script: &str) -> String {
    let script = serde_json::to_string(script).unwrap();
    write(
        dir,
        name,
        format!(
            r#"{{"meta":{{"capture":"second"}},"resources":[{{"script":{script},"fields_to_tag":["event.kind"],"name":"Synthetic parser","id":"{id}"}}]}}"#
        )
        .as_bytes(),
    )
}
fn import_with(dir: &Path, flags: &[&str], ids: &[&str], details: &[(&str, &str)]) -> Value {
    let page = write(
        dir,
        "page.json",
        &serde_json::to_vec(
            &json!({"resources":ids,"meta":{"pagination":{"offset":0,"total":ids.len()}}}),
        )
        .unwrap(),
    );
    let details: Vec<_> = details
        .iter()
        .map(|(id, file)| json!({"parser_id":id,"result_file":file}))
        .collect();
    call_with(
        dir,
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":page}],"details":details,"retrieved_at":RETRIEVED}),
        flags,
    )
}
fn import(dir: &Path, ids: &[&str], details: &[(&str, &str)]) -> Value {
    import_with(dir, &[], ids, details)
}
fn sha(reply: &Value, id: &str) -> String {
    reply["status"]["parsers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == id)
        .unwrap()["sha256"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn bind(dir: &Path, source: &str, parser: &str, sha256: &str) -> Value {
    call(
        dir,
        json!({"kind":"bind-parser","source_id":source,"parser_id":parser,"parser_sha256":sha256,"reviewed_at":RETRIEVED,"evidence":"Synthetic exact parser binding","semantics":[{"line":2,"interpretation":"Synthetic parse step reviewed","evidence":"Read the saved script line"}]}),
    )
}
fn counts(kept: u64, carried: u64, suspended: u64, restored: u64) -> Value {
    json!({"kept":kept,"carried":carried,"suspended":suspended,"restored":restored})
}
/// A confirmed source bound to parser `p1` with a review of its line 2.
fn bound(dir: &Path) -> (String, String, String) {
    let source = confirm_source(dir);
    let p1 = detail(dir, "p1.json", resource("p1", SCRIPT));
    let imported = import(dir, &["p1"], &[("p1", &p1)]);
    let h1 = sha(&imported, "p1");
    assert_eq!(
        bind(dir, &source, "p1", &h1)["status"]["sources"][0]["parser"],
        "done"
    );
    (source, p1, h1)
}

#[test]
fn an_import_with_no_capture_is_refused_and_saves_nothing() {
    let dir = Temp::new();
    bound(&dir.0);
    let before = fs::read(dir.state()).unwrap();
    let output = run(
        &dir.0,
        json!({"kind":"import-parsers","pages":[],"details":[],"retrieved_at":RETRIEVED}),
        &[],
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("INVENTORY_INPUT"), "{stderr}");
    assert!(
        stderr.contains(
            "An import replaces the saved parser inventory and needs at least one capture."
        ),
        "{stderr}"
    );
    assert_eq!(fs::read(dir.state()).unwrap(), before);

    // A catalog page that attests zero parsers is still a capture.
    let emptied = import(&dir.0, &[], &[]);
    assert_eq!(emptied["status"]["parser_catalog"], "done");
    assert_eq!(emptied["status"]["parser_count"], 0);
    assert_eq!(emptied["action"]["parser_reviews"], counts(0, 0, 1, 0));
}

#[test]
fn a_capture_without_the_reviewed_definition_suspends_review_work_until_the_same_bytes_return() {
    let dir = Temp::new();
    let source = confirm_source(&dir.0);
    let p1 = detail(&dir.0, "p1.json", resource("p1", SCRIPT));
    let p2 = detail(&dir.0, "p2.json", resource("p2", "user := \"other\""));
    let imported = import(&dir.0, &["p1", "p2"], &[("p1", &p1), ("p2", &p2)]);
    let (h1, h2) = (sha(&imported, "p1"), sha(&imported, "p2"));
    bind(&dir.0, &source, "p1", &h1);
    let excluded = call(
        &dir.0,
        json!({"kind":"exclude-parser","parser_id":"p2","parser_sha256":h2,"reviewed_at":RETRIEVED,"reason":"Synthetic parser for another source"}),
    );
    assert_eq!(excluded["status"]["sources"][0]["parser"], "done");
    assert_eq!(excluded["status"]["unclassified_parser_ids"], json!([]));

    // The catalog still lists both parsers, but their details are missing from this capture.
    let partial = import(&dir.0, &["p1", "p2"], &[]);
    let held = &partial["status"]["suspended_parser_reviews"];
    assert_eq!(held.as_array().map(Vec::len), Some(2), "{partial}");
    assert_eq!(held[0]["kind"], "binding");
    assert_eq!(held[0]["source_id"], source.as_str());
    assert_eq!(held[0]["binding"]["parser_sha256"], h1.as_str());
    assert_eq!(held[0]["binding"]["semantics"][0]["line"], 2);
    assert_eq!(held[1]["kind"], "exclusion");
    assert_eq!(held[1]["exclusion"]["parser_sha256"], h2.as_str());
    assert_eq!(
        held[1]["exclusion"]["reason"],
        "Synthetic parser for another source"
    );
    let suspended = &partial["status"]["sources"][0];
    assert!(suspended["binding"].is_null());
    assert_eq!(suspended["parser"], "pending");
    assert!(
        suspended["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action == SUSPENDED_SOURCE),
        "{suspended}"
    );
    assert_eq!(partial["status"]["parser_catalog"], "pending");
    assert_eq!(
        partial["status"]["unclassified_parser_ids"],
        json!(["p1", "p2"])
    );
    assert_eq!(partial["action"]["parser_reviews"], counts(0, 0, 2, 0));
    assert_eq!(partial["action"]["next_action"], SUSPENDED_REPLY);

    // The same bytes return: both records are active again with nothing re-entered.
    let restored = import(&dir.0, &["p1", "p2"], &[("p1", &p1), ("p2", &p2)]);
    let active = &restored["status"]["sources"][0];
    assert_eq!(active["parser"], "done", "{restored}");
    assert_eq!(active["binding"]["parser_sha256"], h1.as_str());
    assert_eq!(active["binding"]["semantics"][0]["line"], 2);
    assert_eq!(restored["status"]["unclassified_parser_ids"], json!([]));
    assert!(restored["status"].get("suspended_parser_reviews").is_none());
    assert_eq!(restored["action"]["parser_reviews"], counts(0, 0, 0, 2));
    assert!(restored["action"].get("next_action").is_none());
}

#[test]
fn a_re_encoded_definition_carries_review_work_and_keeps_both_hashes() {
    let dir = Temp::new();
    let (_, p1, h1) = bound(&dir.0);
    let second = re_encoded(&dir.0, "p1-second.json", "p1", SCRIPT);
    let carried = import(&dir.0, &["p1"], &[("p1", &second)]);
    let saved = sha(&carried, "p1");
    assert_ne!(saved, h1);
    let source = &carried["status"]["sources"][0];
    assert_eq!(source["parser"], "done", "{carried}");
    assert_eq!(source["binding"]["parser_sha256"], h1.as_str());
    assert_eq!(source["binding"]["semantics"][0]["line"], 2);
    assert_eq!(
        carried["status"]["carried_parser_reviews"],
        json!([{"parser_id":"p1","reviewed_sha256":h1,"saved_sha256":saved}])
    );
    assert_eq!(carried["action"]["parser_reviews"], counts(0, 1, 0, 0));

    // The reviewed bytes return: the binding stands on them again.
    let back = import(&dir.0, &["p1"], &[("p1", &p1)]);
    assert_eq!(back["status"]["sources"][0]["parser"], "done");
    assert!(back["status"].get("carried_parser_reviews").is_none());
    assert_eq!(back["action"]["parser_reviews"], counts(1, 0, 0, 0));
}

#[test]
fn a_changed_parser_suspends_review_work_and_needs_a_fresh_review() {
    let mut description = resource("p1", SCRIPT);
    description["description"] = json!("Changed outside the script");
    for changed in [
        resource("p1", "user := \"changed\"\nparseJson()"),
        description,
    ] {
        let dir = Temp::new();
        let (source, _, h1) = bound(&dir.0);
        let file = detail(&dir.0, "p1-changed.json", changed);
        let imported = import(&dir.0, &["p1"], &[("p1", &file)]);
        let held = &imported["status"]["suspended_parser_reviews"];
        assert_eq!(held.as_array().map(Vec::len), Some(1), "{imported}");
        assert_eq!(held[0]["binding"]["parser_sha256"], h1.as_str());
        let status = &imported["status"]["sources"][0];
        assert_eq!(status["parser"], "pending");
        assert!(status["binding"].is_null());
        assert_eq!(imported["status"]["unclassified_parser_ids"], json!(["p1"]));
        assert!(imported["status"].get("carried_parser_reviews").is_none());
        assert_eq!(imported["action"]["parser_reviews"], counts(0, 0, 1, 0));

        // A fresh review of the new bytes replaces the suspended binding.
        let rebound = bind(&dir.0, &source, "p1", &sha(&imported, "p1"));
        assert_eq!(rebound["status"]["sources"][0]["parser"], "done");
        assert!(rebound["status"].get("suspended_parser_reviews").is_none());
    }
}

#[test]
fn a_changed_parser_suspends_its_exclusion() {
    let dir = Temp::new();
    confirm_source(&dir.0);
    let p1 = detail(&dir.0, "p1.json", resource("p1", SCRIPT));
    let imported = import(&dir.0, &["p1"], &[("p1", &p1)]);
    let h1 = sha(&imported, "p1");
    call(
        &dir.0,
        json!({"kind":"exclude-parser","parser_id":"p1","parser_sha256":h1,"reviewed_at":RETRIEVED,"reason":"Synthetic parser for another source"}),
    );
    let changed = detail(
        &dir.0,
        "p1-changed.json",
        resource("p1", "user := \"changed\""),
    );
    let reply = import(&dir.0, &["p1"], &[("p1", &changed)]);
    let held = &reply["status"]["suspended_parser_reviews"];
    assert_eq!(held.as_array().map(Vec::len), Some(1), "{reply}");
    assert_eq!(held[0]["exclusion"]["parser_sha256"], h1.as_str());
    assert_eq!(reply["status"]["unclassified_parser_ids"], json!(["p1"]));
    assert_eq!(reply["action"]["parser_reviews"], counts(0, 0, 1, 0));
}

#[test]
fn suspended_review_work_returns_with_a_re_encoded_definition() {
    let dir = Temp::new();
    let (_, _, h1) = bound(&dir.0);
    let partial = import(&dir.0, &["p1"], &[]);
    assert_eq!(partial["status"]["sources"][0]["parser"], "pending");
    let second = re_encoded(&dir.0, "p1-second.json", "p1", SCRIPT);
    let returned = import(&dir.0, &["p1"], &[("p1", &second)]);
    assert_eq!(
        returned["status"]["sources"][0]["parser"], "done",
        "{returned}"
    );
    assert_eq!(
        returned["status"]["sources"][0]["binding"]["parser_sha256"],
        h1.as_str()
    );
    assert_eq!(
        returned["status"]["carried_parser_reviews"][0]["reviewed_sha256"],
        h1.as_str()
    );
    assert_eq!(returned["action"]["parser_reviews"], counts(0, 1, 0, 0));
}

#[test]
fn import_replies_count_review_work_in_full_summary_and_brief() {
    let dir = Temp::new();
    let (_, p1, _) = bound(&dir.0);
    let full = import(&dir.0, &["p1"], &[("p1", &p1)]);
    assert_eq!(full["action"]["parser_reviews"], counts(1, 0, 0, 0));
    assert!(full["action"].get("next_action").is_none());

    let summary = import_with(&dir.0, &["--summary"], &["p1"], &[]);
    assert_eq!(summary["action"]["parser_reviews"], counts(0, 0, 1, 0));
    assert_eq!(summary["action"]["next_action"], SUSPENDED_REPLY);

    let brief = import_with(&dir.0, &["--brief"], &["p1"], &[("p1", &p1)]);
    assert_eq!(brief["action"]["outcome"], "applied");
    assert_eq!(brief["action"]["parser_reviews"], counts(0, 0, 0, 1));
    assert!(brief["action"].get("next_action").is_none());

    let brief = import_with(&dir.0, &["--brief"], &["p1"], &[]);
    assert_eq!(brief["action"]["parser_reviews"], counts(0, 0, 1, 0));
    assert_eq!(brief["action"]["next_action"], SUSPENDED_REPLY);
}

#[test]
fn a_state_with_no_held_review_work_keeps_its_earlier_form_and_fingerprint() {
    let dir = Temp::new();
    call(&dir.0, json!({"kind":"init"}));
    let mut initial: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    initial["created_at"] = json!(RETRIEVED);
    fs::write(dir.state(), serde_json::to_vec(&initial).unwrap()).unwrap();
    let (_, p1, _) = bound(&dir.0);
    let mut confirmed: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    confirmed["target"]["confirmed_at"] = json!(RETRIEVED);
    confirmed["refresh_reminder"]["baseline_at"] = json!(RETRIEVED);
    fs::write(dir.state(), serde_json::to_vec(&confirmed).unwrap()).unwrap();
    let reconciled = call(
        &dir.0,
        json!({"kind":"reconcile","reviewed_at":"2026-01-02T00:01:00Z"}),
    );
    assert_eq!(reconciled["status"]["reconciliation"], "done");
    let before = fs::read(dir.state()).unwrap();
    let saved: Value = serde_json::from_slice(&before).unwrap();
    assert!(saved.get("suspended_parser_reviews").is_none());
    assert!(saved.get("carried_parser_reviews").is_none());
    let state: InventoryState = serde_json::from_slice(&before).unwrap();
    // Reconciled by the 0.1.13 CLI with these fixed fixture timestamps and captures.
    assert_eq!(
        state.evidence_digest().unwrap(),
        "6680997137716905a2bdf398d8f1efd9a9cad52fb1b507d0b5fe084c08f6a2ac"
    );
    let mut serialized = serde_json::to_vec_pretty(&state).unwrap();
    serialized.push(b'\n');
    assert!(
        serialized == before,
        "re-serialization changed the saved state"
    );

    // Suspend, then restore with the same bytes: the state and its fingerprint come back.
    import(&dir.0, &["p1"], &[]);
    let restored = import(&dir.0, &["p1"], &[("p1", &p1)]);
    assert_eq!(restored["status"]["reconciliation"], "done", "{restored}");
    let mut after: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    let mut expected = saved;
    after.as_object_mut().unwrap().remove("updated_at");
    expected.as_object_mut().unwrap().remove("updated_at");
    assert_eq!(after, expected);
}

const NOW: &str = "2026-01-02T00:00:00Z";
const START: &str = "2026-01-01T00:00:00Z";
fn environment_id() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "search-all".into(),
    }
}
fn selectors() -> Scope {
    Scope {
        repo: Some("synthetic".into()),
        type_: Some("synthetic-type".into()),
        event: None,
    }
}
fn full_environment() -> InventoryState {
    let mut state = InventoryState::new(NOW).unwrap();
    state
        .prepare_scout(environment_id(), START.into(), NOW.into())
        .unwrap();
    let args = state.scout_plan.as_ref().unwrap().arguments.clone();
    state
        .import_scout(
            args,
            br##"{"result":[{"#repo":"synthetic","#type":"synthetic-type","events":"10"}]}"##,
            NOW,
        )
        .unwrap();
    state
        .confirm(
            environment_id(),
            TargetScope::FullEnvironment,
            "Synthetic inventory test".into(),
            vec![],
            NOW,
        )
        .unwrap();
    state
}
fn parser(id: &str, script: &str) -> ParserSnapshot {
    let raw = serde_json::to_vec(
        &json!({"resources":[{"id":id,"name":"Synthetic parser","script":script}]}),
    )
    .unwrap();
    parse_parser(id, RawSnapshot::new(&raw, NOW).unwrap()).unwrap()
}
fn catalog_page(ids: &[&str]) -> ParserPage {
    let response = json!({"resources":ids,"meta":{"pagination":{"offset":0,"total":ids.len()}}});
    ParserPage {
        offset: 0,
        limit: 100,
        snapshot: RawSnapshot::new(&serde_json::to_vec(&response).unwrap(), NOW).unwrap(),
    }
}
fn observed_pack() -> Pack {
    let sid = discovery::source_id(&selectors());
    Pack::new(environment_id(), NOW.into(), PackData {
        sources: vec![json!({"id":sid,"kind":"source","name":"complete","inventory_status":"observed-only",
            "selectors":{"repo":"synthetic","type":"synthetic-type"},
            "evidence":{"observed":true,"observation_window":format!("{START}/{NOW}")},
            "provenance_ids":["prov:complete"]})],
        fields:vec![json!({"id":"env:field:complete","source_id":sid,"path":"user",
            "data_type":"unknown","evidence_kind":"observed","coverage":{"observed":true},"provenance_ids":["prov:complete"]})],
        provenance: vec![json!({"id":"prov:complete","source_type":"mcp-observation","canonical_source":"Synthetic saved response",
            "retrieved_at":NOW,"redaction_status":"none","coverage_status":"observed-only"})],
        ..PackData::default()
    }).unwrap()
}

fn full_with_parser(p1: &ParserSnapshot) -> (InventoryState, Pack) {
    let mut state = full_environment();
    state.sources[0].review = Some(SourceReview {
        purpose: "Synthetic source".into(),
        data_model: "Synthetic events".into(),
        reviewed_at: NOW.into(),
        evidence: "Synthetic source contract".into(),
    });
    state
        .import_parser_inventory(vec![catalog_page(&["p1"])], vec![p1.clone()])
        .unwrap();
    state.sources[0].binding = Some(ParserBinding::Parser {
        parser_id: p1.id.clone(),
        parser_sha256: p1.snapshot.sha256.clone(),
        reviewed_at: NOW.into(),
        evidence: "Synthetic exact parser binding".into(),
        semantics: vec![],
    });
    let pack = observed_pack();
    let reference = PackReference {
        pruned_at: None,
        cleanup_pending: false,
        path: "synthetic-pack.json".into(),
        content_digest: pack.content_digest.clone(),
        input_content_digest: None,
        imported_at: NOW.into(),
        discovery_run: Some("synthetic-run.json".into()),
    };
    state.packs.push(reference.clone());
    state.active_pack = Some(reference);
    state.discovery_runs.push(serde_json::from_value(json!({"state_path":"synthetic-run.json","pack_path":"synthetic-pack.json","receipt_path":"synthetic-receipt.json","start":START,"end":NOW,"source_ids":[state.sources[0].id],"value_fields":[],"max_queries":20,"max_seconds":300,"answered":5,"elapsed_seconds":1.0,"status":"complete"})).unwrap());
    state.reconciled_evidence_sha256 = Some(state.evidence_digest().unwrap());
    (state, pack)
}

#[test]
fn full_map_complete_is_false_while_a_binding_is_suspended() {
    let p1 = parser("p1", "user := \"a\"");
    let (mut state, pack) = full_with_parser(&p1);
    let status = assess(&state, std::slice::from_ref(&pack)).unwrap();
    assert!(status.full_map_complete, "positive control");

    // The next capture no longer lists p1. Its only parser is excluded, so the catalog is
    // complete and classified, and the evidence is reconciled again.
    let p2 = parser("p2", "user := \"b\"");
    state
        .import_parser_inventory(vec![catalog_page(&["p2"])], vec![p2.clone()])
        .unwrap();
    state.parser_exclusions.push(ParserExclusion {
        parser_id: p2.id.clone(),
        parser_sha256: p2.snapshot.sha256.clone(),
        reviewed_at: NOW.into(),
        reason: "Synthetic parser for another source".into(),
    });
    state.reconciled_evidence_sha256 = Some(state.evidence_digest().unwrap());
    let status = assess(&state, std::slice::from_ref(&pack)).unwrap();
    assert_eq!(status.parser_catalog, Progress::Done);
    assert!(status.unclassified_parser_ids.is_empty());
    assert_eq!(status.reconciliation, Progress::Done);
    assert_eq!(status.sources[0].parser, Progress::Pending);
    assert!(!status.full_map_complete);
    let saved = serde_json::to_value(&state).unwrap();
    assert_eq!(
        saved["suspended_parser_reviews"][0]["binding"]["parser_sha256"],
        p1.snapshot.sha256.as_str()
    );
}

fn exclude(dir: &Path, parser: &str, sha256: &str) -> Value {
    call(
        dir,
        json!({"kind":"exclude-parser","parser_id":parser,"parser_sha256":sha256,"reviewed_at":RETRIEVED,"reason":"Synthetic exclusion"}),
    )
}

fn reconcile(dir: &Temp) -> (Value, InventoryState) {
    let reply = call(&dir.0, json!({"kind":"reconcile","reviewed_at":RETRIEVED}));
    assert_eq!(reply["status"]["reconciliation"], "done");
    (
        reply,
        serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap(),
    )
}

fn assert_same_reconciliation(dir: &Temp, reply: &Value, reconciled: &InventoryState) {
    assert_eq!(reply["status"]["reconciliation"], "done");
    let saved: InventoryState = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    assert_eq!(
        saved.reconciled_evidence_sha256,
        reconciled.reconciled_evidence_sha256
    );
    assert_eq!(
        saved.evidence_digest().unwrap(),
        reconciled.evidence_digest().unwrap()
    );
    assert_eq!(
        serde_json::to_value(saved.parser_exclusions).unwrap(),
        serde_json::to_value(&reconciled.parser_exclusions).unwrap()
    );
}

fn assert_carried_review_round_trip(bindings: bool) {
    let dir = Temp::new();
    let repos = if bindings {
        json!([{"repo":"synthetic-repo"},{"repo":"other-synthetic-repo"}])
    } else {
        json!([{"repo":"synthetic-repo"}])
    };
    let sources = confirm_sources(&dir.0, repos);
    let (first, second) = ("p1", "p2");
    let ids = [first, second];
    let a = detail(&dir.0, "a.json", resource(first, SCRIPT));
    let b = detail(&dir.0, "b.json", resource(second, SCRIPT));
    let original = import(&dir.0, &ids, &[(first, &a), (second, &b)]);
    for (index, id) in ids.into_iter().enumerate() {
        let hash = sha(&original, id);
        if bindings {
            bind(
                &dir.0,
                sources[index]["source_id"].as_str().unwrap(),
                id,
                &hash,
            );
        } else {
            exclude(&dir.0, id, &hash);
        }
    }
    let a = re_encoded(&dir.0, "re-encoded-a.json", first, SCRIPT);
    let b = re_encoded(&dir.0, "re-encoded-b.json", second, SCRIPT);
    let full = [(first, a.as_str()), (second, b.as_str())];
    assert_eq!(
        import(&dir.0, &ids, &full)["action"]["parser_reviews"],
        counts(0, 2, 0, 0)
    );
    let (before, reconciled) = reconcile(&dir);

    let partial = import(&dir.0, &ids, &full[1..]);
    assert_eq!(partial["action"]["parser_reviews"], counts(0, 1, 1, 0));
    assert_eq!(partial["status"]["reconciliation"], "pending");
    for _ in 0..2 {
        let restored = import(&dir.0, &ids, &full);
        assert_eq!(restored["action"]["parser_reviews"], counts(0, 2, 0, 0));
        assert_eq!(
            restored["status"]["carried_parser_reviews"],
            before["status"]["carried_parser_reviews"],
            "the same capture must preserve carried review order"
        );
        assert_same_reconciliation(&dir, &restored, &reconciled);
    }
}

#[test]
fn carried_exclusions_preserve_reconciliation_when_the_same_capture_returns() {
    assert_carried_review_round_trip(false);
}

#[test]
fn carried_bindings_preserve_reconciliation_when_the_same_capture_returns() {
    assert_carried_review_round_trip(true);
}

#[test]
fn suspended_reviews_preserve_reconciliation_when_the_same_partial_capture_returns() {
    let dir = Temp::new();
    confirm_source(&dir.0);
    let ids = ["p1", "p2"];
    let a = detail(&dir.0, "a.json", resource("p1", SCRIPT));
    let b = detail(&dir.0, "b.json", resource("p2", SCRIPT));
    let first = import(&dir.0, &ids, &[("p1", &a), ("p2", &b)]);
    exclude(&dir.0, "p1", &sha(&first, "p1"));
    exclude(&dir.0, "p2", &sha(&first, "p2"));
    let partial = import(&dir.0, &ids, &[]);
    assert_eq!(partial["action"]["parser_reviews"], counts(0, 0, 2, 0));
    let (before, reconciled) = reconcile(&dir);

    let mixed = import(&dir.0, &ids, &[("p2", &b)]);
    assert_eq!(mixed["action"]["parser_reviews"], counts(0, 0, 1, 1));
    assert_eq!(mixed["status"]["reconciliation"], "pending");
    let restored = import(&dir.0, &ids, &[]);
    assert_eq!(
        restored["status"]["suspended_parser_reviews"],
        before["status"]["suspended_parser_reviews"],
        "the same partial capture must preserve suspended review order"
    );
    assert_same_reconciliation(&dir, &restored, &reconciled);
}

#[test]
fn status_rejects_contradictory_held_digests_for_the_same_reviewed_bytes() {
    let dir = Temp::new();
    let sources = confirm_sources(
        &dir.0,
        json!([{"repo":"synthetic-repo"},{"repo":"other-synthetic-repo"}]),
    );
    let parser = detail(&dir.0, "parser.json", resource("p1", SCRIPT));
    let first = import(&dir.0, &["p1"], &[("p1", &parser)]);
    for source in sources.as_array().unwrap() {
        bind(
            &dir.0,
            source["source_id"].as_str().unwrap(),
            "p1",
            &sha(&first, "p1"),
        );
    }
    let held = import(&dir.0, &["p1"], &[]);
    assert_eq!(held["action"]["parser_reviews"], counts(0, 0, 2, 0));
    let status = || {
        cli(&dir.0)
            .args(["status", "--json", "--state"])
            .arg(dir.state())
            .output()
            .unwrap()
    };
    assert!(
        status().status.success(),
        "matching held digests must be accepted"
    );
    let mut state: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    let other_digest = "a".repeat(64);
    assert_ne!(
        state["suspended_parser_reviews"][0]["resource_sha256"],
        other_digest
    );
    state["suspended_parser_reviews"][1]["resource_sha256"] = json!(other_digest);
    let bytes = serde_json::to_vec(&state).unwrap();
    fs::write(dir.state(), &bytes).unwrap();
    let output = status();
    assert_eq!(
        output.status.code(),
        Some(2),
        "status accepted contradictory held digests"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("INVENTORY_EVIDENCE"),
        "{output:?}"
    );
    assert_eq!(fs::read(dir.state()).unwrap(), bytes);
}

fn not_applicable(dir: &Path, source: &str) -> Value {
    call(
        dir,
        json!({"kind":"parser-not-applicable","source_id":source,"reason":"platform-internal","reviewed_at":RETRIEVED,"evidence":"Synthetic native source"}),
    )
}

fn status(dir: &Temp, flags: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["status", "--state"])
        .arg(dir.state())
        .args(flags)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn missing_captures_are_refused_before_creating_a_state_directory() {
    let dir = Temp::new();
    let output = run(
        &dir.0,
        json!({"kind":"import-parsers","pages":[],"details":[],"retrieved_at":RETRIEVED}),
        &[],
    );
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("needs at least one capture"));
    assert!(!dir.state().parent().unwrap().exists());
}

#[test]
fn new_binding_or_exclusion_supersedes_a_suspended_exclusion() {
    for bind_instead in [false, true] {
        let dir = Temp::new();
        let (source, p1, original) = {
            let source = confirm_source(&dir.0);
            let p1 = detail(&dir.0, "original.json", resource("p1", SCRIPT));
            let imported = import(&dir.0, &["p1"], &[("p1", &p1)]);
            (source, p1, sha(&imported, "p1"))
        };
        exclude(&dir.0, "p1", &original);
        let changed = detail(
            &dir.0,
            "changed.json",
            resource("p1", "other := true\nparseJson()"),
        );
        let current = import(&dir.0, &["p1"], &[("p1", &changed)]);
        assert_eq!(current["action"]["parser_reviews"], counts(0, 0, 1, 0));
        let current = sha(&current, "p1");
        let reviewed = if bind_instead {
            bind(&dir.0, &source, "p1", &current)
        } else {
            exclude(&dir.0, "p1", &current)
        };
        assert!(reviewed["status"].get("suspended_parser_reviews").is_none());
        let rollback = import(&dir.0, &["p1"], &[("p1", &p1)]);
        assert_eq!(rollback["action"]["parser_reviews"], counts(0, 0, 1, 0));
        assert_eq!(rollback["status"]["unclassified_parser_ids"], json!(["p1"]));
    }
}

#[test]
fn carried_bindings_are_pruned_only_after_the_last_review_is_replaced() {
    for replace_with_native in [false, true] {
        let dir = Temp::new();
        let (source, _, h1) = bound(&dir.0);
        // Two sources can share a carried mapping.
        let mut state: InventoryState =
            serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
        let mut other = state.sources[0].clone();
        other.selectors.repo = Some("another-synthetic-repo".into());
        other.id = discovery::source_id(&other.selectors);
        let other_id = other.id.clone();
        state.sources.push(other);
        fs::write(dir.state(), serde_json::to_vec(&state).unwrap()).unwrap();
        let second = re_encoded(&dir.0, "second.json", "p1", SCRIPT);
        let carried = import(&dir.0, &["p1"], &[("p1", &second)]);
        assert_eq!(carried["action"]["parser_reviews"], counts(0, 2, 0, 0));
        let saved = sha(&carried, "p1");
        let rebound = bind(&dir.0, &source, "p1", &saved);
        assert_eq!(
            rebound["status"]["carried_parser_reviews"][0]["reviewed_sha256"],
            h1
        );
        let replaced = if replace_with_native {
            not_applicable(&dir.0, &other_id)
        } else {
            bind(&dir.0, &other_id, "p1", &saved)
        };
        assert!(replaced["status"].get("carried_parser_reviews").is_none());
        let state: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
        assert!(state.get("carried_parser_reviews").is_none());
    }
}

#[test]
fn marking_a_source_not_applicable_clears_its_suspended_binding() {
    let dir = Temp::new();
    let (source, p1, _) = bound(&dir.0);
    let partial = import(&dir.0, &["p1"], &[]);
    assert_eq!(partial["action"]["parser_reviews"], counts(0, 0, 1, 0));
    let native = not_applicable(&dir.0, &source);
    assert!(native["status"].get("suspended_parser_reviews").is_none());
    let returned = import(&dir.0, &["p1"], &[("p1", &p1)]);
    assert_eq!(returned["action"]["parser_reviews"], counts(0, 0, 0, 0));
    assert_eq!(
        returned["status"]["sources"][0]["binding"]["kind"],
        "not-applicable"
    );
}

#[test]
fn chaining_re_encodings_preserves_both_review_kinds_then_restores_them_by_value() {
    let dir = Temp::new();
    let source = confirm_source(&dir.0);
    let first = detail(&dir.0, "first.json", resource("p1", SCRIPT));
    let excluded = detail(&dir.0, "excluded.json", resource("p2", SCRIPT));
    let ids = ["p1", "p2"];
    let original = import(&dir.0, &ids, &[("p1", &first), ("p2", &excluded)]);
    let h1 = sha(&original, "p1");
    let h2 = sha(&original, "p2");
    bind(&dir.0, &source, "p1", &h1);
    exclude(&dir.0, "p2", &h2);
    let b1 = re_encoded(&dir.0, "b1.json", "p1", SCRIPT);
    let b2 = re_encoded(&dir.0, "b2.json", "p2", SCRIPT);
    let b = import(&dir.0, &ids, &[("p1", &b1), ("p2", &b2)]);
    assert_eq!(b["action"]["parser_reviews"], counts(0, 2, 0, 0));
    let c1 = write(
        &dir.0,
        "c1.json",
        &[fs::read(&b1).unwrap(), b"\n".to_vec()].concat(),
    );
    let c2 = write(
        &dir.0,
        "c2.json",
        &[fs::read(&b2).unwrap(), b"\n".to_vec()].concat(),
    );
    let c = import(&dir.0, &ids, &[("p1", &c1), ("p2", &c2)]);
    assert_eq!(c["action"]["parser_reviews"], counts(0, 2, 0, 0));
    let saved = sha(&c, "p2");
    assert_ne!(saved, sha(&b, "p2"));
    assert_eq!(
        c["status"]["carried_parser_reviews"],
        json!([
            {"parser_id":"p1","reviewed_sha256":h1,"saved_sha256":sha(&c, "p1")},
            {"parser_id":"p2","reviewed_sha256":h2,"saved_sha256":saved}
        ])
    );

    let partial = import(&dir.0, &ids, &[]);
    assert_eq!(partial["action"]["parser_reviews"], counts(0, 0, 2, 0));
    assert!(partial["status"].get("carried_parser_reviews").is_none());
    let returned = import(&dir.0, &ids, &[("p1", &c1), ("p2", &c2)]);
    assert_eq!(returned["action"]["parser_reviews"], counts(0, 2, 0, 0));
    assert_eq!(
        returned["status"]["carried_parser_reviews"],
        c["status"]["carried_parser_reviews"]
    );
    let excluded = exclude(&dir.0, "p2", &saved);
    let remaining = excluded["status"]["carried_parser_reviews"]
        .as_array()
        .unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0]["parser_id"], "p1");
}

#[test]
fn changing_tag_values_or_array_order_suspends_a_binding() {
    for tags in [["a", "c"], ["b", "a"]] {
        let dir = Temp::new();
        let source = confirm_source(&dir.0);
        let mut parser = resource("p1", SCRIPT);
        parser["fields_to_tag"] = json!(["a", "b"]);
        let original = detail(&dir.0, "original.json", parser.clone());
        let imported = import(&dir.0, &["p1"], &[("p1", &original)]);
        bind(&dir.0, &source, "p1", &sha(&imported, "p1"));
        parser["fields_to_tag"] = json!(tags);
        let changed = detail(&dir.0, "changed.json", parser);
        let reviewed = import(&dir.0, &["p1"], &[("p1", &changed)]);
        assert_eq!(
            reviewed["action"]["parser_reviews"],
            counts(0, 0, 1, 0),
            "{tags:?}"
        );
    }
}

#[test]
fn exclusions_return_to_their_original_order_and_reconciliation() {
    let dir = Temp::new();
    confirm_source(&dir.0);
    let a = detail(&dir.0, "a.json", resource("p1", SCRIPT));
    let b = detail(&dir.0, "b.json", resource("p2", SCRIPT));
    let ids = ["p1", "p2"];
    let original = import(&dir.0, &ids, &[("p1", &a), ("p2", &b)]);
    exclude(&dir.0, "p1", &sha(&original, "p1"));
    exclude(&dir.0, "p2", &sha(&original, "p2"));
    let reconciled = call(&dir.0, json!({"kind":"reconcile","reviewed_at":NOW}));
    assert_eq!(reconciled["status"]["reconciliation"], "done");
    let before: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    import(&dir.0, &ids, &[("p2", &b)]);
    let restored = import(&dir.0, &ids, &[("p1", &a), ("p2", &b)]);
    assert_eq!(restored["status"]["reconciliation"], "done");
    let after: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    assert_eq!(after["parser_exclusions"], before["parser_exclusions"]);
}

#[test]
fn standalone_status_reports_suspended_exclusions_without_exposing_them_in_brief() {
    let dir = Temp::new();
    confirm_source(&dir.0);
    let id = "synthetic-private-parser";
    let file = detail(&dir.0, "original.json", resource(id, SCRIPT));
    let imported = import(&dir.0, &[id], &[(id, &file)]);
    let hash = sha(&imported, id);
    exclude(&dir.0, id, &hash);
    import(&dir.0, &[id], &[]);
    for flags in [&["--json"][..], &["--json", "--summary"][..]] {
        let output = status(&dir, flags);
        let value: Value = serde_json::from_slice(&output.stdout).unwrap();
        if flags.contains(&"--summary") {
            assert_eq!(value["counts"]["suspended_parser_exclusions"], 1);
        } else {
            assert_eq!(value["suspended_parser_exclusions"], 1);
            assert_eq!(
                value["suspended_parser_reviews"].as_array().unwrap().len(),
                1
            );
        }
        assert!(
            value["next_actions"]
                .as_array()
                .unwrap()
                .contains(&json!(SUSPENDED_REPLY))
        );
    }
    let brief = status(&dir, &["--json", "--brief"]);
    assert!(brief.stdout.len() <= 4096);
    let text = String::from_utf8(brief.stdout).unwrap();
    for secret in [id, &hash, &file] {
        assert!(!text.contains(secret));
    }
    let brief: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(brief["counts"]["suspended_parser_exclusions"], 1);
    assert!(brief["notices"].as_array().unwrap().iter().any(|notice| {
        notice["code"] == "SUSPENDED_PARSER_REVIEWS"
            && notice["message"]
                .as_str()
                .unwrap_or_default()
                .contains("restore")
    }));

    let human = String::from_utf8(status(&dir, &[]).stdout).unwrap();
    assert!(human.contains("Suspended parser exclusions: 1"));
    assert!(human.contains("Import or fetch"));
}

fn number_parser(value: &str) -> ParserSnapshot {
    let raw = format!(
        r#"{{"resources":[{{"id":"p1","script":"user := true","extra":{{"revision":[{value}]}}}}]}}"#
    );
    parse_parser("p1", RawSnapshot::new(raw.as_bytes(), NOW).unwrap()).unwrap()
}

fn review_numeric_parser(value: &str) -> InventoryState {
    let mut state = full_environment();
    let parser = number_parser(value);
    state
        .import_parser_inventory(vec![catalog_page(&["p1"])], vec![parser.clone()])
        .unwrap();
    state.sources[0].binding = Some(ParserBinding::Parser {
        parser_id: parser.id,
        parser_sha256: parser.snapshot.sha256,
        reviewed_at: NOW.into(),
        evidence: "Synthetic numeric parser review".into(),
        semantics: vec![],
    });
    state
}

#[test]
fn equivalent_decimal_spellings_carry_review() {
    for (left, right) in [
        ("1000", "1e3"),
        ("1000.0", "1e+0003"),
        ("-0", "0"),
        ("0.50", "5e-1"),
        ("-12.30", "-123e-1"),
        (
            "100e99999999999999999999999999",
            "1e100000000000000000000000001",
        ),
        (
            "100e-100000000000000000000000001",
            "1e-99999999999999999999999999",
        ),
    ] {
        let mut state = review_numeric_parser(left);
        let counts = state
            .import_parser_inventory(vec![catalog_page(&["p1"])], vec![number_parser(right)])
            .unwrap();
        assert_eq!(
            (counts.carried, counts.suspended),
            (1, 0),
            "{left} vs {right}"
        );
    }
}

#[test]
fn distinct_exact_numbers_suspend_reviews() {
    for (left, right) in [
        ("9007199254740992.0", "9007199254740993.0"),
        ("18446744073709551616", "18446744073709551617"),
        (
            "0.123456789123456789123456789",
            "0.123456789123456789123456788",
        ),
        (
            "1e99999999999999999999999999",
            "1e99999999999999999999999998",
        ),
    ] {
        let mut state = review_numeric_parser(left);
        let counts = state
            .import_parser_inventory(vec![catalog_page(&["p1"])], vec![number_parser(right)])
            .unwrap();
        assert_eq!(
            (counts.carried, counts.suspended),
            (0, 1),
            "{left} vs {right}"
        );
    }
}

fn assert_invalid_review(state: &Value, label: &str) {
    let state: InventoryState = serde_json::from_value(state.clone()).unwrap();
    assert!(state.validate().is_err(), "accepted {label}");
}

#[test]
fn validation_rejects_corrupt_held_bindings_exclusions_and_carries() {
    let dir = Temp::new();
    let (_, p1, _) = bound(&dir.0);
    let partial = import(&dir.0, &["p1"], &[]);
    let held = partial["status"]["suspended_parser_reviews"][0].clone();
    let suspended: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    for (path, value) in [
        ("/binding/parser_id", json!("")),
        ("/binding/parser_sha256", json!("not-a-digest")),
        ("/binding/parser_sha256", json!("A".repeat(64))),
        ("/binding/semantics/0/line", json!(0)),
        ("/resource_sha256", json!("A".repeat(64))),
    ] {
        let mut invalid = suspended.clone();
        *invalid["suspended_parser_reviews"][0]
            .pointer_mut(path)
            .unwrap() = value;
        assert_invalid_review(&invalid, path);
    }
    let mut exclusion = json!({
        "kind":"exclusion","resource_sha256": held["resource_sha256"],
        "exclusion":{"parser_id":"p1","parser_sha256":held["binding"]["parser_sha256"],
           "reviewed_at":NOW,"reason":"Synthetic exclusion"}
    });
    let mut conflict = suspended.clone();
    conflict["suspended_parser_reviews"] = json!([held, exclusion]);
    assert_invalid_review(&conflict, "binding and exclusion of the same held hash");
    let mut different_hash = conflict.clone();
    different_hash["suspended_parser_reviews"][1]["exclusion"]["parser_sha256"] =
        json!("a".repeat(64));
    assert_invalid_review(
        &different_hash,
        "binding and exclusion of the same held resource",
    );
    conflict["suspended_parser_reviews"] = json!([exclusion, exclusion]);
    assert_invalid_review(&conflict, "duplicate held exclusions");
    exclusion["exclusion"]["parser_id"] = json!("");
    conflict["suspended_parser_reviews"] = json!([exclusion]);
    assert_invalid_review(&conflict, "empty held exclusion ID");

    import(&dir.0, &["p1"], &[("p1", &p1)]);
    let active: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    let mut invalid = active.clone();
    invalid["sources"][0]
        .as_object_mut()
        .unwrap()
        .remove("binding");
    invalid["suspended_parser_reviews"] = json!([held]);
    invalid["suspended_parser_reviews"][0]["binding"]["parser_sha256"] = json!("a".repeat(64));
    assert_invalid_review(
        &invalid,
        "held resource already saved under a different hash",
    );
    let mut exclusion = json!({"parser_id":"p1","parser_sha256":held["binding"]["parser_sha256"],"reviewed_at":NOW,"reason":"Synthetic exclusion"});
    let mut invalid = active;
    invalid["parser_exclusions"] = json!([exclusion]);
    assert_invalid_review(&invalid, "active binding and exclusion");
    invalid["sources"][0]
        .as_object_mut()
        .unwrap()
        .remove("binding");
    invalid["parser_exclusions"] = json!([exclusion, exclusion]);
    assert_invalid_review(&invalid, "duplicate active exclusions");
    invalid["parser_exclusions"] = json!([exclusion]);
    exclusion["parser_sha256"] = json!("a".repeat(64));
    invalid["suspended_parser_reviews"] = json!([{
        "kind":"exclusion","resource_sha256":"b".repeat(64),"exclusion":exclusion
    }]);
    assert_invalid_review(&invalid, "duplicate active and held exclusions");

    let second = re_encoded(&dir.0, "second.json", "p1", SCRIPT);
    import(&dir.0, &["p1"], &[("p1", &second)]);
    let carried: Value = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    for (field, value) in [
        ("reviewed_sha256", json!("not-even-hex")),
        ("saved_sha256", json!("A".repeat(64))),
        ("parser_id", json!("")),
    ] {
        let mut invalid = carried.clone();
        invalid["carried_parser_reviews"][0][field] = value;
        assert_invalid_review(&invalid, field);
    }
    let mut invalid = carried;
    invalid["sources"][0]
        .as_object_mut()
        .unwrap()
        .remove("binding");
    assert_invalid_review(&invalid, "unreferenced carried review");
}

#[test]
fn changed_numeric_metadata_cannot_complete_the_full_map_or_resolve_pending_fields() {
    let (mut state, pack) = full_with_parser(&number_parser("9007199254740992.0"));
    let run = state.discovery_runs.pop().unwrap();
    state
        .record_inventory_hints(
            &environment_id(),
            vec![InventoryHint {
                source: selectors(),
                fields: vec!["user".into()],
            }],
        )
        .unwrap();
    state.discovery_runs.push(run);
    let evidence = [InventoryRunEvidence {
        run: "synthetic-run.json".into(),
        request_sha256: "a".repeat(64),
        captures: vec![serde_json::from_value(json!({
            "probe":{"kind":"population","source":selectors(),"query":"synthetic count","start":START,"end":NOW,"fields":["user"]},
            "retrieved_at":NOW,
            "receipt":{"query":"synthetic count","start":START,"end":NOW,"job_id":"job","completion":"complete","cleanup":"delegated","polls":1,"events":[{"total":10,"c0":10}],"metadata":null,"diagnostic":null}
        })).unwrap()],
    }];
    state.reconciled_evidence_sha256 = Some(state.evidence_digest().unwrap());
    let status = assess(&state, std::slice::from_ref(&pack)).unwrap();
    assert!(status.full_map_complete);
    let mut control = state.clone();
    control
        .resolve_inventoried_work_with_evidence(&status, &evidence)
        .unwrap();
    assert!(control.pending_inventory[0].resolution.is_some());

    let counts = state
        .import_parser_inventory(
            vec![catalog_page(&["p1"])],
            vec![number_parser("9007199254740993.0")],
        )
        .unwrap();
    state.reconciled_evidence_sha256 = Some(state.evidence_digest().unwrap());
    let status = assess(&state, std::slice::from_ref(&pack)).unwrap();
    state
        .resolve_inventoried_work_with_evidence(&status, &evidence)
        .unwrap();
    assert_eq!(
        (
            counts.suspended,
            status.sources[0].parser,
            status.full_map_complete,
            state.pending_inventory[0].resolution.is_some()
        ),
        (1, Progress::Pending, false, false),
    );
}
