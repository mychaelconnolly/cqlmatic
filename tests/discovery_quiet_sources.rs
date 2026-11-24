use cqlmatic::{
    app,
    contracts::{CheckRequest, Cloud, Context, EnvironmentId, Outcome, Scope},
    discovery,
    inventory::{self, DiscoveryMode, DiscoveryRun, InventoryState, QuickDepth, TargetScope},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

const START: &str = "2026-01-01T00:00:00Z";
const END: &str = "2026-01-02T00:00:00Z";
const VIEW: &str = "synthetic_view";
const SERVER: &str = "synthetic-server";
const ALL_QUIET_REASON: &str = "Every selected source had zero events in the window.";
const EMPTY_LISTING_REASON: &str = "A source with events listed no fields in the window.";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-quiet-{}-{}",
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
        self.0.join("workflow").join("state.json")
    }
    fn saved(&self) -> Value {
        read(&self.state())
    }
    fn bytes(&self) -> Vec<u8> {
        fs::read(self.state()).unwrap()
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn environment() -> Value {
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":VIEW})
}
fn environment_id() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: VIEW.into(),
    }
}
fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
}
fn cqlmatic(dir: &Temp) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    command.env("HOME", &dir.0);
    command
}
fn inventory(dir: &Temp, action: Value, brief: bool) -> Output {
    let request = dir.0.join("request.json");
    write(&request, &json!({"schema_version":1,"action":action}));
    let mut command = cqlmatic(dir);
    command.args(["env", "inventory", "--json"]);
    if brief {
        command.arg("--brief");
    }
    command
        .arg("--state")
        .arg(dir.state())
        .arg("--request")
        .arg(request)
        .output()
        .unwrap()
}
fn call(dir: &Temp, action: Value) -> Value {
    let output = inventory(dir, action.clone(), false);
    assert!(
        output.status.success() || output.status.code() == Some(3),
        "{action}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn call_brief(dir: &Temp, action: Value) -> Value {
    let output = inventory(dir, action.clone(), true);
    serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{action}: {}", String::from_utf8_lossy(&output.stderr)))
}
fn refused(dir: &Temp, action: Value) -> Value {
    let output = inventory(dir, action.clone(), false);
    assert_eq!(output.status.code(), Some(2), "{action}: {output:?}");
    serde_json::from_slice(&output.stderr).unwrap()
}
fn status_output(dir: &Temp, flags: &[&str]) -> Output {
    cqlmatic(dir)
        .arg("status")
        .args(flags)
        .arg("--state")
        .arg(dir.state())
        .output()
        .unwrap()
}
fn status_with(dir: &Temp, flags: &[&str]) -> Value {
    let output = status_output(dir, flags);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn status(dir: &Temp) -> Value {
    status_with(dir, &["--json"])
}
fn status_text(dir: &Temp) -> String {
    let output = status_output(dir, &[]);
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}
fn source_status<'a>(status: &'a Value, id: &str) -> &'a Value {
    status["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["source_id"] == id)
        .unwrap()
}

fn scope(name: &str) -> Scope {
    Scope {
        repo: Some(format!("{name}_repo")),
        type_: Some(format!("{name}_type")),
        event: None,
    }
}
fn id(name: &str) -> String {
    discovery::source_id(&scope(name))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Depth {
    SourcesOnly,
    QuickFields,
    Full,
}
impl Depth {
    fn mode(self) -> Value {
        match self {
            Self::SourcesOnly => {
                json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"})
            }
            Self::QuickFields => {
                json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-and-fields"})
            }
            Self::Full => json!({"kind":"set-discovery-mode","mode":"full"}),
        }
    }
}

fn confirm(dir: &Temp, depth: Depth, names: &[&str]) {
    call(dir, depth.mode());
    call(
        dir,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources",
            "context":"Synthetic quiet-source target","expected_sources":names.iter().map(|n| scope(n)).collect::<Vec<_>>()}),
    );
}
fn prepare(dir: &Temp, max_queries: u32, value_fields: &[&str], ids: &[String]) -> Value {
    let mut action = json!({"kind":"prepare-discovery","start":START,"end":END,
        "max_queries":max_queries,"max_seconds":600,"value_fields":value_fields});
    if !ids.is_empty() {
        action["source_ids"] = json!(ids);
    }
    call(dir, action)
}

/// Synthetic answers: each source's volume and field listing; anything else is empty or full.
#[derive(Clone, Default)]
struct World {
    volume: BTreeMap<String, u64>,
    empty_listing: Vec<String>,
}
impl World {
    fn new(volumes: &[(&str, u64)]) -> Self {
        Self {
            volume: volumes
                .iter()
                .map(|(name, events)| (name.to_string(), *events))
                .collect(),
            empty_listing: vec![],
        }
    }
    fn source(&self, query: &str) -> String {
        self.volume
            .keys()
            .find(|name| query.contains(&format!("{name}_repo")))
            .unwrap_or_else(|| panic!("unknown source in {query}"))
            .clone()
    }
    fn rows(&self, query: &str) -> (String, Value) {
        let source = self.source(query);
        let empty = self.empty_listing.contains(&source);
        let (kind, rows) = if query.ends_with("fieldset()") {
            let rows = if empty {
                json!([])
            } else {
                json!([{"field":"user"},{"field":format!("{source}_field")}])
            };
            ("fieldset", rows)
        } else if query.contains("groupBy([#event.dataset]") {
            ("dataset-facet", json!([]))
        } else if query.contains("stats([count(as=total)") {
            let mut row = json!({"total":10});
            for index in 0..query.matches(", as=c").count() {
                row[format!("c{index}")] = json!(10);
            }
            ("population", json!([row]))
        } else if query.contains("=* | count(as=events)") {
            ("tag-detect", json!([{"events":0}]))
        } else if query.contains("limit=25) | sort(events, order=desc, limit=25)") {
            let field = query
                .split("groupBy([")
                .nth(1)
                .and_then(|rest| rest.split(']').next())
                .and_then(|quoted| serde_json::from_str::<String>(quoted).ok())
                .unwrap();
            ("value-facet", json!([{field: "synthetic", "events": 3}]))
        } else if query.contains("groupBy([") {
            ("record-kind-facet", json!([]))
        } else if query.ends_with("count(as=events)") {
            ("volume", json!([{"events":self.volume[&source]}]))
        } else {
            panic!("unexpected probe {query}")
        };
        (format!("{source}:{kind}"), rows)
    }
}

/// Every search the run asks for, answered from `world` through `next-probe`, up to `limit`.
struct Drive {
    searches: Vec<String>,
    last: Value,
    pending: Option<(String, String)>,
}
fn next_probe(dir: &Temp, answer: Option<&(String, String)>, brief: bool) -> Value {
    let mut command = cqlmatic(dir);
    command.args(["env", "inventory", "--json", "--next-probe"]);
    if brief {
        command.arg("--brief");
    }
    if let Some((probe, file)) = answer {
        command
            .arg("--probe-id")
            .arg(probe)
            .arg("--result-file")
            .arg(file);
    }
    let output = command.arg("--state").arg(dir.state()).output().unwrap();
    let value: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    value["action"].clone()
}
fn drive(dir: &Temp, world: &World, limit: Option<usize>) -> Drive {
    let mut drive = Drive {
        searches: vec![],
        last: Value::Null,
        pending: None,
    };
    loop {
        let action = next_probe(dir, drive.pending.as_ref(), false);
        drive.pending = None;
        drive.last = action.clone();
        let Some(probe) = action["probe"].as_object() else {
            return drive;
        };
        let query = probe["arguments"]["query_string"].as_str().unwrap();
        let (label, rows) = world.rows(query);
        let file = dir.0.join(format!("answer-{}.json", drive.searches.len()));
        write(&file, &json!({"result":rows}));
        drive.pending = Some((
            probe["probe_id"].as_str().unwrap().into(),
            file.to_string_lossy().into_owned(),
        ));
        // Stop with the reserved probe's answer ready but not submitted.
        if limit.is_some_and(|limit| drive.searches.len() == limit) {
            return drive;
        }
        drive.searches.push(label);
        assert!(drive.searches.len() < 400, "run did not finish");
    }
}
fn last_run(dir: &Temp) -> Value {
    dir.saved()["discovery_runs"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone()
}
fn active_pack(dir: &Temp) -> Option<Value> {
    dir.saved()["active_pack"]["path"]
        .as_str()
        .map(|path| read(Path::new(path)))
}
/// A source's records in a pack: its source record, fields, values and their provenance.
fn records(pack: &Value, source: &str) -> Value {
    let fields: Vec<_> = pack["data"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["source_id"] == source)
        .cloned()
        .collect();
    let field_ids: Vec<_> = fields.iter().map(|f| f["id"].clone()).collect();
    let values: Vec<_> = pack["data"]["values"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|v| field_ids.contains(&v["field_id"]))
        .cloned()
        .collect();
    let record = pack["data"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == source)
        .cloned();
    let mut ids: Vec<Value> = record
        .iter()
        .chain(&fields)
        .chain(&values)
        .flat_map(|r| r["provenance_ids"].as_array().cloned().unwrap_or_default())
        .collect();
    ids.sort_by_key(|v| v.to_string());
    ids.dedup();
    let provenance: Vec<_> = pack["data"]["provenance"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| ids.contains(&p["id"]))
        .cloned()
        .collect();
    json!({"source":record,"fields":fields,"values":values,"provenance":provenance})
}

#[test]
fn mixed_and_empty() {
    for depth in [Depth::SourcesOnly, Depth::QuickFields, Depth::Full] {
        for (live, quiet) in [("alpha", "bravo"), ("bravo", "alpha")] {
            let dir = Temp::new();
            confirm(&dir, depth, &["alpha", "bravo"]);
            let values: &[&str] = if depth == Depth::SourcesOnly {
                &[]
            } else {
                &["user"]
            };
            // A first run sees both sources, so the later quiet source has earlier records.
            prepare(&dir, 200, values, &[]);
            let first = drive(&dir, &World::new(&[("alpha", 7), ("bravo", 9)]), None);
            assert_eq!(
                last_run(&dir)["status"],
                "complete",
                "{depth:?} {} {:?} {}",
                first.last,
                first.searches,
                dir.saved()["failures"]
            );
            let before = active_pack(&dir).unwrap();
            let earlier = records(&before, &id(quiet));
            assert!(earlier["source"].is_object());

            prepare(&dir, 200, values, &[]);
            let world = World::new(&[(live, 5), (quiet, 0)]);
            let run = drive(&dir, &world, None);
            // The quiet source costs its volume search and nothing more.
            assert_eq!(
                run.searches
                    .iter()
                    .filter(|s| s.starts_with(quiet))
                    .collect::<Vec<_>>(),
                [&format!("{quiet}:volume")],
                "{depth:?}"
            );
            assert_eq!(run.searches[..2].len(), 2);
            assert!(run.searches[..2].iter().all(|s| s.ends_with(":volume")));
            let saved = last_run(&dir);
            assert_eq!(saved["status"], "complete", "{depth:?} {}", run.last);
            assert_eq!(saved["source_ids"].as_array().unwrap().len(), 2);
            assert_eq!(saved["quiet_source_ids"], json!([id(quiet)]));
            // The live source is merged; the quiet source keeps its earlier records unchanged.
            let after = active_pack(&dir).unwrap();
            assert_ne!(after, before);
            assert_eq!(records(&after, &id(quiet)), earlier, "{depth:?}");
            assert_ne!(records(&after, &id(live)), records(&before, &id(live)));
            // The receipt keeps every capture whole, the zero included.
            let receipt = read(Path::new(saved["receipt_path"].as_str().unwrap()));
            let captures = receipt["captures"].as_array().unwrap();
            assert_eq!(captures.len(), run.searches.len());
            assert!(captures.iter().any(|c| c["probe"]["kind"] == "volume"
                && c["probe"]["source"]["type"] == format!("{quiet}_type")
                && c["receipt"]["events"] == json!([{"events":0}])));
            let report = status(&dir);
            let quiet_status = source_status(&report, &id(quiet));
            assert_eq!(quiet_status["measurements"], "pending");
            assert_eq!(quiet_status["observation"], "pending");
            assert_ne!(quiet_status["status"], "done");
            assert_eq!(quiet_status["quiet"]["events"], 0);
            assert_eq!(quiet_status["quiet"]["start"], START);
            assert_eq!(quiet_status["quiet"]["end"], END);
            let live_status = source_status(&report, &id(live));
            assert_eq!(live_status["measurements"], "done", "{depth:?}");
            assert!(live_status.get("quiet").is_none());
            assert_eq!(report["quick_scope_complete"], false);
            assert_eq!(report["selected_scope_complete"], false);
        }
    }

    // Every source quiet, with and without an earlier pack: failed, nothing activated.
    for predecessor in [false, true] {
        let dir = Temp::new();
        confirm(&dir, Depth::SourcesOnly, &["alpha", "bravo"]);
        if predecessor {
            prepare(&dir, 10, &[], &[]);
            drive(&dir, &World::new(&[("alpha", 7), ("bravo", 9)]), None);
        }
        let before = dir.saved()["active_pack"].clone();
        prepare(&dir, 10, &[], &[]);
        let world = World::new(&[("alpha", 0), ("bravo", 0)]);
        // The reply that submits the last zero carries the fixed reason.
        let run = drive(&dir, &world, None);
        assert_eq!(run.searches, ["alpha:volume", "bravo:volume"]);
        let saved = last_run(&dir);
        assert_eq!(saved["status"], "failed");
        assert_eq!(saved["quiet_source_ids"], json!([id("alpha"), id("bravo")]));
        assert_eq!(dir.saved()["active_pack"], before);
        assert_eq!(run.last["state"], "failed", "{}", run.last);
        assert_eq!(run.last["diagnostics"][0]["message"], ALL_QUIET_REASON);
        assert_eq!(run.last["quiet_source_count"], 2);
        let poll = call(&dir, json!({"kind":"discovery-step","answers":[]}));
        assert_eq!(
            poll["action"]["relay"]["diagnostics"][0]["message"],
            discovery::ALL_QUIET_SOURCES
        );
        let brief = call_brief(&dir, json!({"kind":"discovery-step","answers":[]}));
        assert_eq!(
            brief["action"]["diagnostics"][0]["message"], ALL_QUIET_REASON,
            "{brief}"
        );
        let report = status(&dir);
        for name in ["alpha", "bravo"] {
            assert_eq!(source_status(&report, &id(name))["quiet"]["events"], 0);
        }
    }

    // A positive source with an empty final listing fails the whole field run in the base pass.
    for depth in [Depth::QuickFields, Depth::Full] {
        let dir = Temp::new();
        confirm(&dir, depth, &["alpha", "bravo"]);
        prepare(&dir, 200, &[], &[]);
        let mut world = World::new(&[("alpha", 5), ("bravo", 4)]);
        world.empty_listing = vec!["bravo".into()];
        let run = drive(&dir, &world, None);
        assert_eq!(
            run.searches,
            [
                "alpha:volume",
                "bravo:volume",
                "alpha:dataset-facet",
                "alpha:fieldset",
                "bravo:dataset-facet",
                "bravo:fieldset",
                "bravo:fieldset"
            ],
            "{depth:?}"
        );
        let saved = last_run(&dir);
        assert_eq!(saved["status"], "failed");
        assert!(saved.get("quiet_source_ids").is_none());
        assert!(dir.saved()["active_pack"].is_null());
        assert_eq!(run.last["diagnostics"][0]["message"], EMPTY_LISTING_REASON);
        let brief = call_brief(&dir, json!({"kind":"discovery-step","answers":[]}));
        assert_eq!(
            brief["action"]["diagnostics"][0]["message"],
            EMPTY_LISTING_REASON
        );
        let poll = call(&dir, json!({"kind":"discovery-step","answers":[]}));
        assert_eq!(
            poll["action"]["relay"]["diagnostics"][0]["message"],
            discovery::EMPTY_FIELD_LISTING
        );
        assert!(
            source_status(&status(&dir), &id("bravo"))
                .get("quiet")
                .is_none()
        );
    }
}

fn relay_state(dir: &Temp, index: usize) -> PathBuf {
    PathBuf::from(
        dir.saved()["discovery_runs"][index]["state_path"]
            .as_str()
            .unwrap(),
    )
}
fn pack_count(dir: &Temp) -> usize {
    dir.saved()["packs"].as_array().unwrap().len()
}
fn tampered(dir: &Temp, edit: impl FnOnce(&mut Value)) -> Vec<u8> {
    let original = dir.bytes();
    let mut saved = dir.saved();
    edit(&mut saved);
    write(&dir.state(), &saved);
    original
}

/// The session binding as a reader recomputes it for each plan.
fn binding(checkpoint: &Value) -> String {
    let session = &checkpoint["session"];
    let mut bytes = serde_json::to_vec(&(
        &checkpoint["request_sha256"],
        &checkpoint["created_at"],
        &checkpoint["pack"],
        &checkpoint["receipt"],
        &session["transport"],
        &session["predecessor_sha256"],
    ))
    .unwrap();
    if session["sparse_fieldset_fallback"] == true {
        bytes.extend_from_slice(b"\0sparse-fieldset-fallback-v1");
    }
    if let Some(owner) = session.get("inventory_owner") {
        bytes.extend_from_slice(b"\0inventory-owner-v1\0");
        bytes.extend_from_slice(&serde_json::to_vec(owner).unwrap());
    }
    if checkpoint["discovery_algorithm"] == "bounded-discovery-v3" {
        bytes.extend_from_slice(b"\0bounded-discovery-v3");
    }
    cqlmatic::sha256_hex(&bytes)
}

#[test]
fn quiet_reopen() {
    // Interrupted after the zero is settled: polls change no ID, charge, byte or pack.
    let dir = Temp::new();
    confirm(&dir, Depth::QuickFields, &["alpha", "bravo"]);
    prepare(&dir, 200, &[], &[]);
    let world = World::new(&[("alpha", 0), ("bravo", 5)]);
    let partial = drive(&dir, &world, Some(1));
    assert_eq!(partial.searches, ["alpha:volume"]);
    assert_eq!(last_run(&dir)["quiet_source_ids"], json!([id("alpha")]));
    assert_eq!(last_run(&dir)["status"], "needs-probes");
    // Active time accrues between polls; IDs, charges and accepted answer hashes do not change.
    let hashes = |dir: &Temp| {
        let relay = read(&relay_state(dir, 0));
        (
            relay["answers"].clone(),
            relay["session"]["captures"]
                .as_object()
                .map(|c| c.keys().cloned().collect::<Vec<_>>()),
            last_run(dir)["answered"].clone(),
        )
    };
    let first = call(&dir, json!({"kind":"discovery-step","answers":[]}));
    let after_first = hashes(&dir);
    let second = call(&dir, json!({"kind":"discovery-step","answers":[]}));
    assert!(hashes(&dir) == after_first);
    assert_eq!(
        first["action"]["relay"]["cost"],
        second["action"]["relay"]["cost"]
    );
    assert_eq!(
        first["action"]["quiet_sources"],
        second["action"]["quiet_sources"]
    );
    assert_eq!(second["action"]["quiet_source_count"], 1);
    assert_eq!(pack_count(&dir), 0);
    assert_eq!(last_run(&dir)["quiet_source_ids"], json!([id("alpha")]));
    // Continue: the reserved live volume and its dependent work finish and activate.
    let rest = drive(&dir, &world, None);
    let _ = rest;
    assert_eq!(last_run(&dir)["status"], "complete");
    assert_eq!(last_run(&dir)["quiet_source_ids"], json!([id("alpha")]));
    assert_eq!(pack_count(&dir), 1);

    // Promotion interrupted after the checkpoint activated: resume-discovery promotes it and
    // records the zero, and later guarded commands accept the run.
    let dir = Temp::new();
    confirm(&dir, Depth::SourcesOnly, &["alpha", "bravo"]);
    prepare(&dir, 10, &[], &[]);
    let world = World::new(&[("alpha", 5), ("bravo", 0)]);
    let partial = drive(&dir, &world, Some(1));
    let before_promotion = dir.bytes();
    assert!(last_run(&dir).get("quiet_source_ids").is_none());
    next_probe(&dir, partial.pending.as_ref(), false);
    assert_eq!(last_run(&dir)["status"], "complete");
    fs::write(dir.state(), &before_promotion).unwrap();
    let resumed = call(&dir, json!({"kind":"resume-discovery"}));
    assert_eq!(
        resumed["action"]["relay"]["status"], "complete",
        "{resumed}"
    );
    let run = last_run(&dir);
    assert_eq!(run["status"], "complete");
    assert_eq!(run["quiet_source_ids"], json!([id("bravo")]));
    assert_eq!(dir.saved()["active_pack"]["path"], run["pack_path"]);
    assert_eq!(resumed["action"]["quiet_source_count"], 1);
    call(&dir, json!({"kind":"discovery-step","answers":[]}));
    status(&dir);
    let next = prepare(&dir, 10, &[], &[]);
    assert_eq!(next["action"]["run_index"], 1, "{next}");

    // Recorded zeros must match the accepted answers, and be well formed.
    let dir = Temp::new();
    confirm(&dir, Depth::SourcesOnly, &["alpha", "bravo", "charlie"]);
    prepare(&dir, 10, &[], &[]);
    drive(
        &dir,
        &World::new(&[("alpha", 5), ("bravo", 6), ("charlie", 0)]),
        None,
    );
    assert_eq!(last_run(&dir)["status"], "complete");
    assert_eq!(last_run(&dir)["quiet_source_ids"], json!([id("charlie")]));
    for (label, quiet) in [
        ("false quiet", json!([id("bravo"), id("charlie")])),
        ("missing zero", json!([])),
    ] {
        let original = tampered(&dir, |saved| {
            saved["discovery_runs"][0]["quiet_source_ids"] = quiet;
        });
        status(&dir);
        let error = refused(
            &dir,
            json!({"kind":"prepare-discovery","start":START,"end":END,"max_queries":10,"max_seconds":600}),
        );
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("Recorded quiet discovery sources differ"),
            "{label}: {error}"
        );
        fs::write(dir.state(), original).unwrap();
    }
    for (label, quiet) in [
        ("duplicate", json!([id("charlie"), id("charlie")])),
        ("unknown", json!([id("delta")])),
        ("out of order", json!([id("charlie"), id("bravo")])),
        (
            "every source complete",
            json!([id("alpha"), id("bravo"), id("charlie")]),
        ),
    ] {
        let original = tampered(&dir, |saved| {
            saved["discovery_runs"][0]["quiet_source_ids"] = quiet;
        });
        let output = status_output(&dir, &["--json"]);
        assert_eq!(output.status.code(), Some(2), "{label}: {output:?}");
        fs::write(dir.state(), original).unwrap();
    }
    status(&dir);

    // An answer for a probe that was never reserved cannot record a zero.
    let dir = Temp::new();
    confirm(&dir, Depth::SourcesOnly, &["alpha", "bravo"]);
    let prepared = prepare(&dir, 10, &[], &[]);
    let probes = prepared["action"]["relay"]["probes"].as_array().unwrap();
    let file = dir.0.join("unreserved.json");
    write(&file, &json!({"result":[{"events":0}]}));
    for probe in probes {
        let reply = call(
            &dir,
            json!({"kind":"discovery-step","answers":[{"probe_id":probe["probe_id"],"result_file":file}]}),
        );
        assert_eq!(reply["action"]["status"], "incomplete", "{reply}");
        assert_eq!(
            reply["action"]["diagnostics"][0]["code"],
            "MCP_UNKNOWN_PROBE"
        );
        assert!(reply["action"].get("quiet_sources").is_none());
    }
    assert!(last_run(&dir).get("quiet_source_ids").is_none());

    // An earlier-plan run row never gains the field and is not checked for it.
    let dir = Temp::new();
    confirm(&dir, Depth::SourcesOnly, &["alpha"]);
    prepare(&dir, 10, &[], &[]);
    drive(&dir, &World::new(&[("alpha", 0)]), None);
    assert_eq!(last_run(&dir)["status"], "failed");
    let checkpoint = relay_state(&dir, 0);
    let mut saved = read(&checkpoint);
    saved["discovery_algorithm"] = json!("bounded-discovery-v2");
    saved["session"]["binding_sha256"] = json!(binding(&saved));
    write(&checkpoint, &saved);
    let mut state = dir.saved();
    state["discovery_runs"][0]
        .as_object_mut()
        .unwrap()
        .remove("quiet_source_ids");
    // Written as this build writes state: the earlier-plan row serializes without the field.
    let typed: InventoryState = serde_json::from_value(state).unwrap();
    let bytes = serde_json::to_vec_pretty(&typed).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("quiet_source_ids"));
    fs::write(dir.state(), &bytes).unwrap();
    let parsed: InventoryState = serde_json::from_slice(&bytes).unwrap();
    assert!(serde_json::to_vec_pretty(&parsed).unwrap() == bytes);
    let report = status(&dir);
    assert!(
        report["discovery_runs"][0]
            .get("quiet_source_ids")
            .is_none()
    );
    assert!(source_status(&report, &id("alpha")).get("quiet").is_none());
    let row = dir.saved()["discovery_runs"][0].clone();
    prepare(&dir, 10, &[], &[]);
    assert_eq!(dir.saved()["discovery_runs"][0], row);
}

fn work(dir: &Temp, name: &str) -> Value {
    dir.saved()["pending_inventory"]
        .as_array()
        .unwrap()
        .iter()
        .find(|work| work["id"] == id(name))
        .cloned()
        .unwrap()
}
fn field<'a>(source: &'a Value, name: &str) -> &'a Value {
    source["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["field"] == name)
        .unwrap_or_else(|| panic!("{name} in {source}"))
}

#[test]
fn quiet_then_live() {
    // An imported pack in which B has events.
    let imported = Temp::new();
    confirm(&imported, Depth::QuickFields, &["bravo"]);
    prepare(&imported, 200, &[], &[]);
    drive(&imported, &World::new(&[("bravo", 9)]), None);
    let pack_file = imported.0.join("bravo-pack.json");
    fs::copy(
        imported.saved()["active_pack"]["path"].as_str().unwrap(),
        &pack_file,
    )
    .unwrap();

    let dir = Temp::new();
    confirm(&dir, Depth::QuickFields, &["alpha", "bravo"]);
    call(&dir, json!({"kind":"import-pack","pack_file":pack_file}));
    call(
        &dir,
        json!({"kind":"record-inventory-hints","environment":environment(),"hints":[
            {"source":scope("alpha"),"fields":["alpha_field"]},
            {"source":scope("bravo"),"fields":["bravo_field"]}]}),
    );
    prepare(&dir, 200, &["user"], &[]);
    let run = drive(&dir, &World::new(&[("alpha", 5), ("bravo", 0)]), None);
    assert!(run.searches.contains(&"alpha:value-facet".to_string()));
    assert_eq!(last_run(&dir)["status"], "complete");
    call(&dir, json!({"kind":"discovery-step","answers":[]}));
    call(&dir, json!({"kind":"reconcile","reviewed_at":END}));
    // Only A's work resolves; B keeps its imported positive but earns no credit from the zero.
    assert_eq!(work(&dir, "alpha")["resolution"]["kind"], "inventoried");
    assert!(work(&dir, "bravo")["resolution"].is_null());
    let report = status(&dir);
    let alpha = source_status(&report, &id("alpha"));
    let bravo = source_status(&report, &id("bravo"));
    assert_eq!(alpha["status"], "done", "{alpha}");
    assert_eq!(field(alpha, "user")["values"], "done");
    assert_eq!(field(bravo, "user")["values_requested"], true);
    assert_eq!(field(bravo, "user")["values"], "pending");
    assert_eq!(bravo["measurements"], "pending");
    assert_eq!(bravo["quiet"]["run_index"], 0);
    assert_eq!(field(bravo, "bravo_field")["observed"], true);
    assert!(
        !field(bravo, "bravo_field")["evidence_dates"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    for flag in [
        "quick_scope_complete",
        "selected_scope_complete",
        "full_map_complete",
    ] {
        assert_eq!(report[flag], false, "{flag}");
    }

    // A recorded resolution that names the quiet run as B's proof is refused.
    let original = tampered(&dir, |saved| {
        let run = saved["discovery_runs"][0]["state_path"].clone();
        for work in saved["pending_inventory"].as_array_mut().unwrap() {
            if work["id"] == id("bravo") {
                work["resolution"] = json!({"kind":"inventoried","run":run});
            }
        }
    });
    assert_eq!(status_output(&dir, &["--json"]).status.code(), Some(2));
    fs::write(dir.state(), original).unwrap();

    // Work resolution loads no evidence from a run in which the work's source was quiet.
    let receipt = PathBuf::from(last_run(&dir)["receipt_path"].as_str().unwrap());
    let aside = dir.0.join("receipt-aside.json");
    fs::rename(&receipt, &aside).unwrap();
    call(&dir, json!({"kind":"reconcile","reviewed_at":END}));
    fs::rename(&aside, &receipt).unwrap();

    // A run of A alone leaves B pending; a later run in which B has events completes it.
    prepare(&dir, 200, &[], &[id("alpha")]);
    drive(&dir, &World::new(&[("alpha", 5)]), None);
    let report = status(&dir);
    assert_eq!(
        source_status(&report, &id("bravo"))["measurements"],
        "pending"
    );
    assert_eq!(
        source_status(&report, &id("bravo"))["quiet"]["run_index"],
        0
    );
    prepare(&dir, 200, &[], &[id("bravo")]);
    drive(&dir, &World::new(&[("bravo", 4)]), None);
    call(&dir, json!({"kind":"reconcile","reviewed_at":END}));
    assert_eq!(work(&dir, "bravo")["resolution"]["kind"], "inventoried");
    let report = status(&dir);
    let bravo = source_status(&report, &id("bravo"));
    assert_eq!(bravo["status"], "done", "{bravo}");
    assert!(bravo.get("quiet").is_none());
    assert_eq!(bravo["observation"], "done");
    assert_eq!(report["quick_scope_complete"], true);
}

const NOW: &str = "2026-01-03T00:00:00Z";
const LATER: &str = "2026-01-03T00:00:01Z";

/// An inventory with one expected source whose latest discovery run measured zero.
fn census_state(source: &Scope) -> InventoryState {
    let mut state = InventoryState::new(NOW).unwrap();
    state.discovery_mode = DiscoveryMode::Quick;
    state.quick_depth = QuickDepth::SourcesOnly;
    state
        .confirm(
            environment_id(),
            TargetScope::SelectedSources,
            "Synthetic census".into(),
            vec![source.clone()],
            NOW,
        )
        .unwrap();
    state
}
fn quiet_run(state: &mut InventoryState, source: &Scope) {
    let source_id = discovery::source_id(source);
    let run: DiscoveryRun = serde_json::from_value(json!({
        "state_path":"/synthetic/relay-state.json","pack_path":"/synthetic/pack.json",
        "receipt_path":"/synthetic/receipt.json","start":START,"end":END,
        "source_ids":[source_id],"value_fields":[],"max_queries":1,"max_seconds":60,
        "answered":1,"elapsed_seconds":1.0,"status":"failed","mode":"quick",
        "quick_depth":"sources-only","quiet_source_ids":[source_id]
    }))
    .unwrap();
    state.discovery_runs.push(run);
}
fn census_rows(rows: &[(&str, &str, u64)]) -> Vec<u8> {
    serde_json::to_vec(&json!({"result":rows.iter().map(|(repo, type_, events)| {
        json!({"#repo":repo,"#type":type_,"@sourcetype":"synthetic","events":events})
    }).collect::<Vec<_>>()}))
    .unwrap()
}
fn scout(state: &mut InventoryState, raw: &[u8], start: &str, end: &str) {
    let plan = state
        .prepare_scout_with_budget(environment_id(), start.into(), end.into(), Some(60))
        .unwrap();
    state.import_scout(plan.arguments, raw, end).unwrap();
}
fn refresh(state: &mut InventoryState, raw: &[u8]) {
    let host = serde_json::to_vec(&json!({
        "schema_version":1,"environment":environment_id(),"session_id":"synthetic-session",
        "verified_at":NOW,"mechanism":"provider-enforced-search-lifetime","max_search_seconds":60,
        "capability_reference":"synthetic provider capability document",
        "verification_evidence":"Synthetic host verified a provider search lifetime, independently of client wait time"
    }))
    .unwrap();
    let id = state
        .prepare_refresh("synthetic-session".into(), &host, false, NOW)
        .unwrap();
    state
        .dispatch_refresh(&id, "synthetic-session", NOW)
        .unwrap();
    let attempt = state.refresh_attempts.iter().find(|a| a.id == id).unwrap();
    let report = serde_json::to_vec(&json!({
        "schema_version":1,"attempt_id":id,"environment":environment_id(),"session_id":"synthetic-session",
        "arguments":attempt.arguments,"termination":"completed","recorded_at":LATER,
        "search_started_at":attempt.dispatched_at,"result_sha256":cqlmatic::sha256_hex(raw),
        "evidence":"Synthetic exact-attempt host termination evidence"
    }))
    .unwrap();
    state.settle_refresh(&id, &report, LATER).unwrap();
    state.import_refresh(&id, raw, LATER).unwrap();
}
fn quiet_of(state: &InventoryState, source: &Scope) -> inventory::QuietSource {
    let report = inventory::assess(state, &[]).unwrap();
    report
        .sources
        .iter()
        .find(|s| s.source_id == discovery::source_id(source))
        .unwrap()
        .quiet
        .clone()
        .expect("latest run is quiet")
}
fn types(quiet: &inventory::QuietSource) -> Vec<(String, u64)> {
    quiet
        .other_types
        .iter()
        .map(|t| (t.type_.clone(), t.events))
        .collect()
}
fn ready(query: &str) {
    let result = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context::default(),
        },
        None,
    );
    assert_eq!(
        result.outcome,
        Outcome::Ready,
        "{query}: {:?}",
        result.diagnostics
    );
}
fn save(dir: &Temp, state: &InventoryState) {
    let workflow = dir.state().parent().unwrap().to_path_buf();
    fs::create_dir_all(&workflow).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&workflow, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::write(dir.state(), serde_json::to_vec_pretty(state).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir.state(), fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn quiet_census_detail() {
    let source = scope("alpha");
    let tail = r##" | groupBy(["#type"], function=count(as=events), limit=max) | sort(events, order=desc, limit=25)"##;

    // No saved census at all.
    let mut state = census_state(&source);
    quiet_run(&mut state, &source);
    let quiet = quiet_of(&state, &source);
    assert_eq!(quiet.census, inventory::NO_CENSUS_CAPTURE);
    assert!(quiet.other_types.is_empty());
    assert_eq!(
        (quiet.events, quiet.start.as_str(), quiet.end.as_str()),
        (0, START, END)
    );
    assert_eq!(quiet.view, VIEW);
    let query = quiet.query.clone().unwrap();
    assert!(query.starts_with("#repo = "), "{query}");
    assert!(
        query.contains("alpha_repo") && !query.contains("alpha_type"),
        "{query}"
    );
    assert!(query.ends_with(tail), "{query}");
    ready(&query);

    // Scout only: rows split by source type are summed; other repositories and the quiet type
    // itself are left out; ties sort by type.
    let mut state = census_state(&source);
    scout(
        &mut state,
        &census_rows(&[
            ("alpha_repo", "alpha_type", 3),
            ("alpha_repo", "beta", 5),
            ("alpha_repo", "aardvark", 5),
            ("alpha_repo", "small", 4),
            ("alpha_repo", "small", 3),
            ("alpha_repo", "big", 100),
            ("other_repo", "elsewhere", 50),
        ]),
        "2025-12-01T00:00:00Z",
        "2025-12-02T00:00:00Z",
    );
    quiet_run(&mut state, &source);
    let quiet = quiet_of(&state, &source);
    assert_eq!(quiet.census, inventory::OTHER_CENSUS_TYPES);
    assert_eq!(
        types(&quiet),
        [
            ("big".into(), 100),
            ("small".into(), 7),
            ("aardvark".into(), 5),
            ("beta".into(), 5)
        ]
    );
    assert_eq!(quiet.other_types[0].view, VIEW);
    assert_eq!(quiet.other_types[0].start, "2025-12-01T00:00:00Z");
    assert_eq!(quiet.other_types[0].retrieved_at, "2025-12-02T00:00:00Z");
    assert!(!quiet.ceiling_capture);

    // Overlapping windows: each type keeps its latest positive capture; a later zero does not
    // replace an earlier positive count.
    scout(
        &mut state,
        &census_rows(&[
            ("alpha_repo", "big", 20),
            ("alpha_repo", "small", 0),
            ("alpha_repo", "fresh", 2),
        ]),
        "2025-12-01T12:00:00Z",
        "2025-12-02T12:00:00Z",
    );
    let quiet = quiet_of(&state, &source);
    assert_eq!(
        types(&quiet),
        [
            ("big".into(), 20),
            ("small".into(), 7),
            ("aardvark".into(), 5),
            ("beta".into(), 5),
            ("fresh".into(), 2)
        ]
    );
    assert_eq!(quiet.other_types[0].end, "2025-12-02T12:00:00Z");
    assert_eq!(quiet.other_types[1].end, "2025-12-02T00:00:00Z");

    // Refresh only.
    let mut state = census_state(&source);
    refresh(
        &mut state,
        &census_rows(&[("alpha_repo", "renewed", 8), ("alpha_repo", "renewed", 1)]),
    );
    quiet_run(&mut state, &source);
    let quiet = quiet_of(&state, &source);
    assert_eq!(types(&quiet), [("renewed".into(), 9)]);
    assert_eq!(quiet.other_types[0].retrieved_at, LATER);

    // A capture holding only other repositories is no census for this one; a matching capture
    // with no other positive type says so.
    for (rows, census) in [
        (
            vec![("other_repo", "elsewhere", 50)],
            inventory::NO_CENSUS_CAPTURE,
        ),
        (
            vec![("alpha_repo", "alpha_type", 4), ("alpha_repo", "silent", 0)],
            inventory::NO_OTHER_CENSUS_TYPE,
        ),
    ] {
        let mut state = census_state(&source);
        scout(
            &mut state,
            &census_rows(&rows),
            "2025-12-01T00:00:00Z",
            "2025-12-02T00:00:00Z",
        );
        quiet_run(&mut state, &source);
        let quiet = quiet_of(&state, &source);
        assert_eq!(quiet.census, census);
        assert!(quiet.other_types.is_empty());
    }

    // A ceiling-sized scout: the top 25, how many were left out, and the ceiling flag.
    let names: Vec<String> = (0..10_000).map(|i| format!("kind{i:05}")).collect();
    let rows: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(i, name)| ("alpha_repo", name.as_str(), i as u64 + 1))
        .collect();
    // Scouted before the target is confirmed, so the census queues no work for its sources.
    let mut state = InventoryState::new(NOW).unwrap();
    state.discovery_mode = DiscoveryMode::Quick;
    scout(
        &mut state,
        &census_rows(&rows),
        "2025-12-01T00:00:00Z",
        "2025-12-02T00:00:00Z",
    );
    state
        .confirm(
            environment_id(),
            TargetScope::SelectedSources,
            "Synthetic census".into(),
            vec![source.clone()],
            NOW,
        )
        .unwrap();
    quiet_run(&mut state, &source);
    let quiet = quiet_of(&state, &source);
    assert!(quiet.ceiling_capture);
    assert_eq!(quiet.other_types.len(), 25);
    assert_eq!(quiet.omitted_types, 9_975);
    assert_eq!(types(&quiet)[0], ("kind09999".into(), 10_000));

    // Repository only: the same repository and window would repeat the zero, so no query.
    let repo_only = Scope {
        repo: Some("alpha_repo".into()),
        type_: None,
        event: None,
    };
    let mut state = census_state(&repo_only);
    scout(
        &mut state,
        &census_rows(&[("alpha_repo", "listed", 6)]),
        "2025-12-01T00:00:00Z",
        "2025-12-02T00:00:00Z",
    );
    quiet_run(&mut state, &repo_only);
    let quiet = quiet_of(&state, &repo_only);
    assert!(quiet.query.is_none());
    assert_eq!(types(&quiet), [("listed".into(), 6)]);

    // Type only: no repository-keyed lookup and no query.
    let type_only = Scope {
        repo: None,
        type_: Some("alpha_type".into()),
        event: None,
    };
    let mut state = census_state(&type_only);
    quiet_run(&mut state, &type_only);
    let quiet = quiet_of(&state, &type_only);
    assert!(quiet.query.is_none());
    assert_eq!(quiet.census, inventory::NO_REPOSITORY_CENSUS);

    // A repository name that needs escaping stays one literal selector.
    let odd = Scope {
        repo: Some(r#"odd"repo\*name/x"#.to_string()),
        type_: Some("odd_type".into()),
        event: None,
    };
    let query = discovery::quiet_census_query(&odd).unwrap().unwrap();
    assert!(query.ends_with(tail), "{query}");
    ready(&query);
    // Read back, it names at most the repository alone, never a typed source.
    let hints = app::inventory_hints(&query).unwrap();
    assert!(
        hints
            .iter()
            .all(|hint| hint.source.repo == odd.repo && hint.source.type_.is_none()),
        "{query}"
    );
    let mut state = census_state(&odd);
    scout(
        &mut state,
        &census_rows(&[
            (odd.repo.as_deref().unwrap(), "neighbor", 3),
            ("odd", "neighbor", 9),
        ]),
        "2025-12-01T00:00:00Z",
        "2025-12-02T00:00:00Z",
    );
    quiet_run(&mut state, &odd);
    assert_eq!(types(&quiet_of(&state, &odd)), [("neighbor".into(), 3)]);

    // Full status, summary, text and brief all show it; the brief points to full status.
    let dir = Temp::new();
    let mut state = census_state(&source);
    scout(
        &mut state,
        &census_rows(&[("alpha_repo", "big", 100), ("alpha_repo", "small", 7)]),
        "2025-12-01T00:00:00Z",
        "2025-12-02T00:00:00Z",
    );
    quiet_run(&mut state, &source);
    save(&dir, &state);
    let brief = status_with(&dir, &["--json", "--brief"]);
    assert_eq!(brief["counts"]["quiet_sources"], 1);
    let notice = brief["notices"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["code"] == "DISCOVERY_QUIET_SOURCES")
        .unwrap();
    assert_eq!(notice["message"], inventory::QUIET_BRIEF_POINTER);
    assert!(
        notice["message"]
            .as_str()
            .unwrap()
            .contains("status --json")
    );
    let full = status(&dir);
    let detail = &source_status(&full, &id("alpha"))["quiet"];
    assert_eq!(detail["other_types"][0]["type"], "big");
    assert_eq!(
        detail["query"],
        json!(discovery::quiet_census_query(&source).unwrap())
    );
    assert_eq!(detail["note"], inventory::QUIET_NOTE);
    let summary = status_with(&dir, &["--json", "--summary"]);
    assert_eq!(summary["sources"][0]["quiet"], *detail);
    let text = status_text(&dir);
    assert!(text.contains("Zero events for [#repo=\"alpha_repo\" #type=\"alpha_type\"] in 2026-01-01T00:00:00Z/2026-01-02T00:00:00Z"), "{text}");
    assert!(text.contains(inventory::OTHER_CENSUS_TYPES), "{text}");
    assert!(text.contains("#type=\"big\": 100 events"), "{text}");
    assert!(
        text.contains(&format!("Optional query over {START}/{END}")),
        "{text}"
    );
    assert!(text.contains(&query_text(&source)), "{text}");
    assert!(!text.contains("gone") || text.contains("does not show that the source is gone"));

    capture_hook_leaves_the_inventory_unchanged();
    quiet_sources_cost_one_search_each();
}

fn query_text(source: &Scope) -> String {
    discovery::quiet_census_query(source).unwrap().unwrap()
}

/// Runs the capture hook on one generic payload.
fn capture(dir: &Temp, payload: &Value) {
    let mut child = cqlmatic(dir)
        .args([
            "env", "capture", "--client", "generic", "--server", SERVER, "--state",
        ])
        .arg(dir.state())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(0), "{output:?}");
}

/// The suggested query, run under the capture hook, is not a new source.
fn capture_hook_leaves_the_inventory_unchanged() {
    let dir = Temp::new();
    confirm(&dir, Depth::SourcesOnly, &["alpha"]);
    call(&dir, json!({"kind":"reconcile","reviewed_at":END}));
    assert_eq!(status(&dir)["reconciliation"], "done");
    let before = dir.bytes();
    let response = json!({"result":[{"#type":"big","events":100},{"#type":"small","events":7}]});
    capture(
        &dir,
        &json!({
            "tool_name": format!("mcp__{SERVER}__falcon_search_ngsiem"),
            "tool_input": {"repository": VIEW, "query_string": query_text(&scope("alpha")),
                "start": START, "end": END},
            "tool_response": response.to_string(),
        }),
    );
    assert!(dir.bytes() == before, "state changed");
    let tally = read(
        &dir.0
            .join("workflow")
            .join("state.json.captures")
            .join("tally.json"),
    );
    assert_eq!(tally, json!({"not-creditable":1}));
    assert_eq!(status(&dir)["reconciliation"], "done");
}

/// A quiet source costs one search across limits, pause and exhaustion.
fn quiet_sources_cost_one_search_each() {
    // Two quiet types and one live type activate at limit 3; one quiet then one live at 2.
    for (names, volumes, limit) in [
        (
            vec!["alpha", "bravo", "charlie"],
            vec![("alpha", 0), ("bravo", 0), ("charlie", 4)],
            3,
        ),
        (vec!["alpha", "bravo"], vec![("alpha", 0), ("bravo", 4)], 2),
    ] {
        let dir = Temp::new();
        confirm(&dir, Depth::SourcesOnly, &names);
        prepare(&dir, limit, &[], &[]);
        let run = drive(&dir, &World::new(&volumes), None);
        assert_eq!(run.searches.len(), limit as usize);
        let saved = last_run(&dir);
        assert_eq!(saved["status"], "complete", "{}", run.last);
        assert_eq!(
            saved["quiet_source_ids"].as_array().unwrap().len(),
            names.len() - 1
        );
        assert_eq!(dir.saved()["active_pack"]["path"], saved["pack_path"]);
    }

    // Uninterrupted, then paused after the zero settles and resumed: the same searches, zeros
    // and activated sources.
    let world = World::new(&[("alpha", 0), ("bravo", 4)]);
    let straight = Temp::new();
    confirm(&straight, Depth::QuickFields, &["alpha", "bravo"]);
    prepare(&straight, 200, &[], &[]);
    let expected = drive(&straight, &world, None);
    let paused = Temp::new();
    confirm(&paused, Depth::QuickFields, &["alpha", "bravo"]);
    let prepared = prepare(&paused, 200, &[], &[]);
    let first = prepared["action"]["relay"]["probes"][0].clone();
    call(
        &paused,
        json!({"kind":"dispatch-discovery","probe_id":first["probe_id"]}),
    );
    let file = paused.0.join("zero.json");
    write(&file, &json!({"result":[{"events":0}]}));
    call(
        &paused,
        json!({"kind":"discovery-step","answers":[{"probe_id":first["probe_id"],"result_file":file}]}),
    );
    let pause = call(&paused, json!({"kind":"pause-discovery"}));
    assert_eq!(pause["action"]["relay"]["status"], "paused", "{pause}");
    assert_eq!(last_run(&paused)["quiet_source_ids"], json!([id("alpha")]));
    assert_eq!(pause["action"]["quiet_source_count"], 1);
    call(&paused, json!({"kind":"resume-discovery"}));
    let rest = drive(&paused, &world, None);
    let mut searches = vec!["alpha:volume".to_string()];
    searches.extend(rest.searches);
    assert_eq!(searches, expected.searches);
    assert_eq!(
        last_run(&paused)["quiet_source_ids"],
        last_run(&straight)["quiet_source_ids"]
    );
    let sources = |dir: &Temp| {
        let mut ids: Vec<_> = active_pack(dir).unwrap()["data"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].clone())
            .collect();
        ids.sort_by_key(|v| v.to_string());
        ids
    };
    assert_eq!(sources(&paused), sources(&straight));
    assert_eq!(sources(&paused), [json!(id("bravo"))]);

    // A budget spent on the zero activates nothing.
    let dir = Temp::new();
    confirm(&dir, Depth::SourcesOnly, &["alpha", "bravo"]);
    prepare(&dir, 1, &[], &[]);
    let run = drive(&dir, &World::new(&[("alpha", 0), ("bravo", 4)]), None);
    assert_eq!(run.searches, ["alpha:volume"]);
    let saved = last_run(&dir);
    assert_eq!(saved["status"], "failed");
    assert_eq!(saved["quiet_source_ids"], json!([id("alpha")]));
    assert!(dir.saved()["active_pack"].is_null());
    assert_eq!(
        source_status(&status(&dir), &id("alpha"))["quiet"]["events"],
        0
    );
}
