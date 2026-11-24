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

fn run(dir: &Path, action: Value, flags: &[&str]) -> Output {
    let request = dir.join("request.json");
    fs::write(
        &request,
        serde_json::to_vec(&json!({"schema_version":1,"action":action})).unwrap(),
    )
    .unwrap();
    Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
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
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"})
}
/// Confirms one selected source and returns its inventory source ID.
fn confirm_source(dir: &Path) -> String {
    call(
        dir,
        json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
    );
    let confirmed = call(
        dir,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources","context":"Synthetic parser review","expected_sources":[{"repo":"synthetic-repo"}]}),
    );
    confirmed["status"]["sources"][0]["source_id"]
        .as_str()
        .unwrap()
        .to_owned()
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
            r#"{{"meta":{{"trace_id":"second-capture"}},"resources":[{{"script":{script},"fields_to_tag":["event.kind"],"name":"Synthetic parser","id":"{id}"}}]}}"#
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
    let (_, p1, _) = bound(&dir.0);
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
        tenant: "00000000000000000000000000000000".into(),
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

#[test]
fn full_map_complete_is_false_while_a_binding_is_suspended() {
    let mut state = full_environment();
    state.sources[0].review = Some(SourceReview {
        purpose: "Synthetic source".into(),
        data_model: "Synthetic events".into(),
        reviewed_at: NOW.into(),
        evidence: "Synthetic source contract".into(),
    });
    let p1 = parser("p1", "user := \"a\"");
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
        path: "synthetic-pack.json".into(),
        content_digest: pack.content_digest.clone(),
        imported_at: NOW.into(),
        discovery_run: Some("synthetic-run.json".into()),
    };
    state.packs.push(reference.clone());
    state.active_pack = Some(reference);
    state.discovery_runs.push(serde_json::from_value(json!({"state_path":"synthetic-run.json","pack_path":"synthetic-pack.json","receipt_path":"synthetic-receipt.json","start":START,"end":NOW,"source_ids":[state.sources[0].id],"value_fields":[],"max_queries":20,"max_seconds":300,"answered":5,"elapsed_seconds":1.0,"status":"complete"})).unwrap());
    state.reconciled_evidence_sha256 = Some(state.evidence_digest().unwrap());
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
