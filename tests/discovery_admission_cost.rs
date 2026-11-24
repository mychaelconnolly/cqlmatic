use cqlmatic::{
    contracts::*,
    discovery::{self, DiscoverRequest, ExpectedSearches, Probe},
    environment::{self, Pack},
    inventory::InventoryState,
    mcp,
    network::SearchBackend,
    sha256_hex, storage,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

const VIEW: &str = "synthetic_view";
const START: &str = "2026-01-01T00:00:00Z";
const END: &str = "2026-01-02T00:00:00Z";
const SHORT_START: &str = "2026-01-01T23:45:00Z";
const TAGS: [&str; 9] = [
    "#event_simpleName",
    "#event.dataset",
    "#event.module",
    "#event.outcome",
    "#ecs.version",
    "#Cps.version",
    "#Vendor",
    "#type",
    "#event.kind",
];

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-admission-{}-{}",
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
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
    fn state(&self) -> PathBuf {
        self.0.join("workflow").join("state.json")
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}
fn write(path: &Path, value: &Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| value.to_string()).collect()
}
fn environment_value() -> Value {
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":VIEW})
}

// ---------------------------------------------------------------------------------------------
// Inventory command line
// ---------------------------------------------------------------------------------------------

fn inventory(dir: &Temp, action: &Value, flags: &[&str]) -> Output {
    let request = dir.path("request.json");
    write(&request, &json!({"schema_version":1,"action":action}));
    Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .env("HOME", &dir.0)
        .args(["env", "inventory", "--json"])
        .args(flags)
        .arg("--state")
        .arg(dir.state())
        .arg("--request")
        .arg(request)
        .output()
        .unwrap()
}
fn call_with(dir: &Temp, action: Value, flags: &[&str]) -> Value {
    let output = inventory(dir, &action, flags);
    assert!(
        output.status.success() || output.status.code() == Some(3),
        "{action}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn call(dir: &Temp, action: Value) -> Value {
    call_with(dir, action, &[])
}
fn refused_with(dir: &Temp, action: &Value, flags: &[&str]) -> Value {
    let output = inventory(dir, action, flags);
    assert_eq!(output.status.code(), Some(2), "{action}: {output:?}");
    serde_json::from_slice(&output.stderr).unwrap()
}
fn status(dir: &Temp) {
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .env("HOME", &dir.0)
        .args(["status", "--json", "--state"])
        .arg(dir.state())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn next_probe(dir: &Temp, answer: Option<(&Value, &Path)>, brief: bool) -> Value {
    let action = match answer {
        Some((probe_id, file)) => {
            json!({"kind":"next-probe","answer":{"probe_id":probe_id,"result_file":file}})
        }
        None => json!({"kind":"next-probe"}),
    };
    call_with(dir, action, if brief { &["--brief"] } else { &[] })["action"].clone()
}
fn confirm(dir: &Temp, depth: &str, sources: &[Value]) {
    call(
        dir,
        if depth == "full" {
            json!({"kind":"set-discovery-mode","mode":"full"})
        } else {
            json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":depth})
        },
    );
    call(
        dir,
        json!({"kind":"confirm-target","environment":environment_value(),"scope":"selected-sources",
            "context":"Synthetic admission target","expected_sources":sources}),
    );
}
fn source_ids(dir: &Temp) -> Vec<String> {
    read(&dir.state())["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["id"].as_str().unwrap().to_string())
        .collect()
}
fn prepare_action(ids: &[&str], start: &str, end: &str, max_queries: u32) -> Value {
    json!({"kind":"prepare-discovery","start":start,"end":end,"source_ids":ids,
        "max_queries":max_queries,"max_seconds":600})
}
fn checkpoint(dir: &Temp, index: usize) -> PathBuf {
    PathBuf::from(
        read(&dir.state())["discovery_runs"][index]["state_path"]
            .as_str()
            .unwrap(),
    )
}
/// Every entry under the inventory folder, so a leaked run folder or staged file shows.
fn entries(dir: &Temp) -> BTreeSet<PathBuf> {
    fn walk(path: &Path, into: &mut BTreeSet<PathBuf>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, into);
            }
            into.insert(path);
        }
    }
    let mut found = BTreeSet::new();
    walk(&dir.0.join("workflow"), &mut found);
    found
}
/// The session binding as a reader recomputes it.
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
    sha256_hex(&bytes)
}
fn save_checkpoint(path: &Path, checkpoint: &mut Value) {
    checkpoint["session"]["binding_sha256"] = json!(binding(checkpoint));
    write(path, checkpoint);
    mcp::inspect_state(path).unwrap();
}
/// Leave a paused run for `ids` with no inventory owner, as a checkpoint written before the
/// inventory associated it.
fn unowned_paused_run(dir: &Temp, ids: &[&str], max_queries: u32) -> (Value, PathBuf) {
    let action = prepare_action(ids, START, END, max_queries);
    call(dir, action.clone());
    let index = read(&dir.state())["discovery_runs"]
        .as_array()
        .unwrap()
        .len()
        - 1;
    let reserved = next_probe(dir, None, false);
    let file = dir.path("unusable.json");
    write(&file, &json!({"result":[{"field":"shape-mismatch"}]}));
    let paused = next_probe(dir, Some((&reserved["probe"]["probe_id"], &file)), false);
    assert_eq!(paused["state"], "paused", "{paused}");
    let path = checkpoint(dir, index);
    let mut saved = read(&path);
    saved["session"]
        .as_object_mut()
        .unwrap()
        .remove("inventory_owner");
    save_checkpoint(&path, &mut saved);
    (action, path)
}

/// The files and folders a refused preparation must leave as they were.
struct Untouched {
    state: Vec<u8>,
    sibling: Vec<u8>,
    entries: BTreeSet<PathBuf>,
}
impl Untouched {
    fn new(dir: &Temp, sibling: &Path) -> Self {
        Self {
            state: fs::read(dir.state()).unwrap(),
            sibling: fs::read(sibling).unwrap(),
            entries: entries(dir),
        }
    }
    fn check(&self, dir: &Temp, sibling: &Path, label: &str) {
        assert!(
            fs::read(dir.state()).unwrap() == self.state,
            "{label}: state"
        );
        assert!(
            fs::read(sibling).unwrap() == self.sibling,
            "{label}: sibling"
        );
        assert_eq!(entries(dir), self.entries, "{label}: entries");
    }
}
/// A refused preparation names its reason in full output, keeps fixed brief text and writes
/// nothing, beside an unowned paused sibling.
fn assert_refused(dir: &Temp, sibling: &Path, action: &Value, full: &str, brief: &str) -> Value {
    let before = Untouched::new(dir, sibling);
    let refusal = refused_with(dir, action, &[]);
    assert!(
        refusal["message"].as_str().unwrap().contains(full),
        "{action}: {refusal}"
    );
    before.check(dir, sibling, full);
    let short = refused_with(dir, action, &["--brief"]);
    assert_eq!(short["code"], refusal["code"], "{short}");
    assert_eq!(short["message"], brief, "{action}: {short}");
    before.check(dir, sibling, brief);
    refusal
}
fn answer_file(dir: &Temp, rows: Value) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let file = dir.path(&format!(
        "answer-{}.json",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    write(&file, &json!({"result":rows}));
    file
}

const FULL_OUTPUT: &str = "Read the full command output.";

#[test]
fn admission_is_read_only() {
    let dir = Temp::new();
    let sources: Vec<Value> = (0..101)
        .map(|index| json!({"type":format!("synthetic-{index:03}")}))
        .collect();
    confirm(&dir, "sources-only", &sources);
    let ids = source_ids(&dir);
    let (paused, sibling) = unowned_paused_run(&dir, &[&ids[0]], 2);
    status(&dir);

    // A window that ends after the current time.
    let future = prepare_action(
        &[&ids[1]],
        "2098-01-01T00:00:00Z",
        "2098-01-02T00:00:00Z",
        1,
    );
    let refusal = assert_refused(
        &dir,
        &sibling,
        &future,
        "The discovery window ends at 2098-01-02T00:00:00Z, after the current time ",
        &format!("The discovery window ends after the current time. {FULL_OUTPUT}"),
    );
    assert_eq!(refusal["code"], "DISCOVERY_EVIDENCE");
    // A query limit below one volume search for each selected source.
    assert_refused(
        &dir,
        &sibling,
        &prepare_action(&[&ids[1], &ids[2]], START, END, 1),
        "The query limit 1 is below the 2 searches needed when every selected source has events. Set max_queries to at least 2.",
        &format!(
            "The query limit is below the searches needed when every selected source has events. {FULL_OUTPUT}"
        ),
    );
    // More than 100 sources.
    assert_refused(
        &dir,
        &sibling,
        &json!({"kind":"prepare-discovery","start":START,"end":END,"max_queries":1000,"max_seconds":600}),
        "More than 100 sources require an explicitly narrowed discovery run.",
        &format!("More than 100 sources are selected. {FULL_OUTPUT}"),
    );
    // The same request as the paused run.
    let before = Untouched::new(&dir, &sibling);
    let refusal = refused_with(&dir, &paused, &[]);
    assert!(
        refusal["message"]
            .as_str()
            .unwrap()
            .contains("An identical discovery request is safely paused."),
        "{refusal}"
    );
    before.check(&dir, &sibling, "identical");
    refused_with(&dir, &paused, &["--brief"]);
    before.check(&dir, &sibling, "identical brief");
    // Planning alone refuses 101 sources as well.
    let saved: InventoryState = serde_json::from_slice(&fs::read(dir.state()).unwrap()).unwrap();
    let all = saved
        .discovery_request(
            START.into(),
            END.into(),
            &[],
            vec![],
            vec![],
            vec![],
            1000,
            600,
        )
        .unwrap();
    assert_eq!(all.sources.len(), 101);
    assert_eq!(
        discovery::initial_plan(&all).unwrap_err().message,
        "More than 100 sources require an explicitly narrowed discovery run."
    );

    // Granting the stated minimum prepares and activates.
    let granted = call(&dir, prepare_action(&[&ids[1], &ids[2]], START, END, 2));
    assert_eq!(granted["action"]["relay"]["status"], "needs-probes");
    let mut step = next_probe(&dir, None, false);
    for _ in 0..2 {
        let file = answer_file(&dir, json!([{"events":10}]));
        step = next_probe(&dir, Some((&step["probe"]["probe_id"], &file)), false);
    }
    assert_eq!(step["state"], "complete", "{step}");
    assert!(read(&dir.state())["active_pack"].is_object());

    // One reading of an injected clock decides the window end and the predecessor date.
    let request = |end: &str| -> DiscoverRequest {
        serde_json::from_value(json!({"schema_version":1,"environment":environment_value(),
            "start":"2026-01-01T00:00:00Z","end":end,"sources":[{"type":"synthetic"}],
            "depth":"sources-only","max_queries":1,"max_seconds":60}))
        .unwrap()
    };
    let now = "2026-01-02T00:00:00Z";
    discovery::admit(&request(now), None, now).unwrap();
    let later = discovery::admit(&request("2026-01-02T00:00:01Z"), None, now).unwrap_err();
    assert!(later.message.contains("2026-01-02T00:00:01Z"), "{later:?}");
    discovery::admit(
        &request("2026-01-02T00:00:01Z"),
        None,
        "2026-01-02T00:00:01Z",
    )
    .unwrap();
    discovery::admit(&request(now), Some(now), now).unwrap();
    let newer = discovery::admit(&request(now), Some("2026-01-02T00:00:01Z"), now).unwrap_err();
    assert!(newer.message.contains("2026-01-02T00:00:01Z"), "{newer:?}");
    discovery::admit(
        &request(now),
        Some("2026-01-02T00:00:01Z"),
        "2026-01-02T00:00:01Z",
    )
    .unwrap();
    let mut wider = request(now);
    wider.sources.push(Scope {
        type_: Some("second".into()),
        ..Scope::default()
    });
    assert!(discovery::admit(&wider, None, now).is_err());
    wider.max_queries = 2;
    discovery::admit(&wider, None, now).unwrap();
    // Above the 1,000-query ceiling no limit helps: fewer sources do.
    let fields = plain("p", 4000);
    let names: Vec<String> = (0..5).map(|index| format!("explicit-{index}")).collect();
    let selections: Vec<Value> = names
        .iter()
        .map(|name| json!({"source":{"repo":name},"fields":fields}))
        .collect();
    let mut explicit: DiscoverRequest = serde_json::from_value(json!({"schema_version":1,
        "environment":environment_value(),"start":START,"end":now,
        "sources":names.iter().map(|name| json!({"repo":name})).collect::<Vec<_>>(),
        "population_fields":selections,"max_queries":1000,"max_seconds":60}))
    .unwrap();
    assert_eq!(estimate(&explicit).1, 5 * 204);
    let ceiling = discovery::admit(&explicit, None, now).unwrap_err();
    assert_eq!(
        ceiling.message,
        "The 1020 searches needed when every selected source has events exceed the 1000-query ceiling. Select fewer sources or population fields."
    );
    explicit.sources.pop();
    explicit.population_fields.pop();
    assert_eq!(estimate(&explicit).1, 4 * 204);
    discovery::admit(&explicit, None, now).unwrap();
    // A predecessor created at the same instant admits the run, but activation still needs a
    // candidate created after it.
    let pack = read_active_pack(&dir);
    let same = Pack::new(
        pack.environment.clone(),
        pack.created_at.clone(),
        pack.data.clone(),
    )
    .unwrap();
    assert!(environment::merge(&pack, &same).is_err());
}

fn read_active_pack(dir: &Temp) -> Pack {
    storage::read_pack(Path::new(
        read(&dir.state())["active_pack"]["path"].as_str().unwrap(),
    ))
    .unwrap()
}

#[test]
fn admission_refuses_a_pack_created_after_now() {
    // A pack with a creation time after now, imported as the active pack.
    let source = Temp::new();
    confirm(&source, "sources-only", &[json!({"type":"synthetic-000"})]);
    let id = source_ids(&source)[0].clone();
    call(&source, prepare_action(&[&id], START, END, 1));
    let step = next_probe(&source, None, false);
    let file = answer_file(&source, json!([{"events":10}]));
    assert_eq!(
        next_probe(&source, Some((&step["probe"]["probe_id"], &file)), false)["state"],
        "complete"
    );
    let pack = read_active_pack(&source);
    let future = Pack::new(
        pack.environment.clone(),
        "2098-01-01T00:00:00Z".into(),
        pack.data.clone(),
    )
    .unwrap();
    let dir = Temp::new();
    let pack_file = dir.path("future-pack.json");
    fs::write(&pack_file, serde_json::to_vec_pretty(&future).unwrap()).unwrap();
    confirm(
        &dir,
        "sources-only",
        &[
            json!({"type":"synthetic-000"}),
            json!({"type":"synthetic-001"}),
        ],
    );
    let ids = source_ids(&dir);
    let (_, sibling) = unowned_paused_run(&dir, &[&ids[0]], 1);
    call(&dir, json!({"kind":"import-pack","pack_file":pack_file}));
    status(&dir);
    assert_refused(
        &dir,
        &sibling,
        &prepare_action(&[&ids[1]], START, END, 1),
        "The active pack was created at 2098-01-01T00:00:00Z, after the current time ",
        &format!("The active pack was created after the current time. {FULL_OUTPUT}"),
    );
}

/// A checkpoint rewritten as the earlier plan wrote it, with another request.
fn as_earlier_plan_request(dir: &Temp, index: usize, change: impl Fn(&mut Value)) -> PathBuf {
    let path = checkpoint(dir, index);
    let mut saved = read(&path);
    let mut request = saved["request"].clone();
    change(&mut request);
    let typed: DiscoverRequest = serde_json::from_value(request).unwrap();
    saved["request_sha256"] = json!(sha256_hex(&serde_json::to_vec(&typed).unwrap()));
    saved["request"] = serde_json::to_value(&typed).unwrap();
    saved["discovery_algorithm"] = json!("bounded-discovery-v2");
    save_checkpoint(&path, &mut saved);
    let mut state = read(&dir.state());
    let run = &mut state["discovery_runs"][index];
    run["start"] = json!(typed.start);
    run["end"] = json!(typed.end);
    run["max_queries"] = json!(typed.max_queries);
    write(&dir.state(), &state);
    path
}

#[test]
fn earlier_plan_checkpoints_outside_admission_stay_inspectable() {
    // A prepared earlier-plan run whose window ends after now, and a settled one whose limit is
    // below its sources' minimum: status reads both, lifecycle actions keep their refusal.
    for case in ["future", "under-budget"] {
        let dir = Temp::new();
        if case == "future" {
            confirm(
                &dir,
                "sources-only",
                &[
                    json!({"type":"synthetic-000"}),
                    json!({"type":"synthetic-001"}),
                ],
            );
            call(&dir, prepare_action(&[], START, END, 2));
        } else {
            // A field run settled after one quiet volume search, its limit then below the four
            // searches one live source needs.
            confirm(
                &dir,
                "sources-and-fields",
                &[json!({"type":"synthetic-000"})],
            );
            call(&dir, prepare_action(&[], START, END, 4));
            let step = next_probe(&dir, None, false);
            let file = answer_file(&dir, json!([{"events":0}]));
            let step = next_probe(&dir, Some((&step["probe"]["probe_id"], &file)), false);
            assert_eq!(step["state"], "failed", "{step}");
        }
        let path = as_earlier_plan_request(&dir, 0, |request| {
            if case == "future" {
                request["start"] = json!("2098-01-01T00:00:00Z");
                request["end"] = json!("2098-01-02T00:00:00Z");
            } else {
                request["max_queries"] = json!(2);
            }
        });
        let bytes = (fs::read(&path).unwrap(), fs::read(dir.state()).unwrap());
        let inspected = mcp::inspect_state(&path).unwrap();
        let cost = mcp::cost(&inspected, 0).searches;
        assert_eq!(
            (cost.minimum_all_live, cost.below_expected_high),
            (None, None)
        );
        status(&dir);
        for action in [
            json!({"kind":"next-probe","run_index":0}),
            json!({"kind":"resume-discovery","run_index":0}),
            json!({"kind":"pause-discovery","run_index":0}),
        ] {
            let refusal = refused_with(&dir, &action, &[]);
            assert_eq!(refusal["code"], "MCP_STATE_PRODUCER", "{case} {action}");
        }
        assert!(
            (fs::read(&path).unwrap(), fs::read(dir.state()).unwrap()) == bytes,
            "{case}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Synthetic answers for planning loops and native execution
// ---------------------------------------------------------------------------------------------

/// One source's answers. The full window is the request window; the sample window is its
/// final 15 minutes. Counts are per window; a window of 15 minutes or less is the full window.
#[derive(Clone)]
struct Source {
    volume: u64,
    sample: Vec<String>,
    full: Vec<String>,
    a: BTreeMap<String, u64>,
    w: BTreeMap<String, u64>,
    datasets: Vec<Value>,
    facets: BTreeMap<String, Vec<Value>>,
    rare: Vec<String>,
}
impl Source {
    fn live(sample: &[&str]) -> Self {
        Self {
            volume: 1000,
            sample: strings(sample),
            full: vec![],
            a: BTreeMap::new(),
            w: BTreeMap::new(),
            datasets: vec![],
            facets: BTreeMap::new(),
            rare: vec![],
        }
    }
    fn quiet() -> Self {
        Self {
            volume: 0,
            ..Self::live(&[])
        }
    }
    fn a(mut self, counts: &[(&str, u64)]) -> Self {
        for (field, count) in counts {
            self.a.insert(field.to_string(), *count);
        }
        self
    }
    fn w(mut self, counts: &[(&str, u64)]) -> Self {
        for (field, count) in counts {
            self.w.insert(field.to_string(), *count);
        }
        self
    }
    fn full(mut self, fields: Vec<String>) -> Self {
        self.full = fields;
        self
    }
    fn facet(mut self, field: &str, values: &[(&str, u64)]) -> Self {
        self.facets.insert(
            field.into(),
            values
                .iter()
                .map(|(value, events)| json!({field:value,"events":events}))
                .collect(),
        );
        self
    }
    fn datasets(mut self, values: &[(&str, u64)]) -> Self {
        self.datasets = values
            .iter()
            .map(|(value, events)| json!({"#event.dataset":value,"events":events}))
            .collect();
        self
    }
    fn rows(&self, r: &DiscoverRequest, p: &Probe) -> Vec<Value> {
        let full = p.start == r.start;
        let (total, counts) = if full {
            (self.volume, &self.w)
        } else {
            (self.volume / 10, &self.a)
        };
        let listing = |fields: &[String]| {
            fields
                .iter()
                .map(|field| json!({"field":field}))
                .collect::<Vec<_>>()
        };
        match p.kind.as_str() {
            "volume" => vec![json!({"events":self.volume})],
            "dataset-facet" => self.datasets.clone(),
            "fieldset" if full && r.start != SHORT_START => listing(&self.full),
            "fieldset" => listing(&self.sample),
            "tag-batch" | "population" => {
                let mut row = json!({"total":total});
                for (index, field) in p.fields.iter().enumerate() {
                    row[format!("c{index}")] = json!(counts.get(field).copied().unwrap_or(0));
                }
                vec![row]
            }
            "record-kind-facet" => {
                let field = p.field.as_deref().unwrap();
                self.facets.get(field).cloned().unwrap_or_else(|| {
                    if field == "#event.dataset" {
                        let mut rows = self.datasets.clone();
                        rows.sort_by_key(|row| std::cmp::Reverse(row["events"].as_u64()));
                        rows.truncate(50);
                        rows
                    } else {
                        vec![]
                    }
                })
            }
            "kind-fieldset" => listing(&self.rare),
            "value-facet" => vec![],
            other => panic!("unexpected probe {other}"),
        }
    }
}

/// Answers by source name (repository or type).
#[derive(Clone, Default)]
struct World(BTreeMap<String, Source>);
impl World {
    fn of(sources: &[(&str, Source)]) -> Self {
        Self(
            sources
                .iter()
                .map(|(name, source)| (name.to_string(), source.clone()))
                .collect(),
        )
    }
    fn rows(&self, r: &DiscoverRequest, p: &Probe) -> Vec<Value> {
        let name = p.source.repo.as_ref().or(p.source.type_.as_ref()).unwrap();
        self.0[name].rows(r, p)
    }
}

fn request(sources: &[&str], extra: Value) -> DiscoverRequest {
    let mut value = json!({
        "schema_version":1,
        "environment":environment_value(),
        "start":START,"end":END,
        "sources":sources.iter().map(|name| json!({"repo":name})).collect::<Vec<_>>(),
        "max_queries":1000,"max_seconds":3600
    });
    for (key, field) in extra.as_object().unwrap() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}
fn quick(sources: &[&str], extra: Value) -> DiscoverRequest {
    let mut extra = extra;
    extra["max_population_fields_per_source"] = json!(20);
    request(sources, extra)
}
fn short(mut r: DiscoverRequest) -> DiscoverRequest {
    r.start = SHORT_START.into();
    r
}

/// The planning loop: answer every reachable search until none remains. Returns the searches in
/// order, or the refusal that ends the run.
fn plan(r: &DiscoverRequest, world: &World) -> Result<Vec<Probe>, Diagnostic> {
    let mut answers = BTreeMap::new();
    let mut probes = Vec::new();
    loop {
        let pending = discovery::frontier(r, &answers)?;
        if pending.is_empty() {
            return Ok(probes);
        }
        assert!(probes.len() <= 5000, "runaway plan");
        for probe in pending {
            answers.insert(discovery::probe_id(VIEW, &probe), world.rows(r, &probe));
            probes.push(probe);
        }
    }
}

struct Backend<'a> {
    request: &'a DiscoverRequest,
    world: &'a World,
    probes: BTreeMap<String, Probe>,
    emitted: Vec<String>,
}
impl SearchBackend for Backend<'_> {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        _: Instant,
    ) -> Result<String, Diagnostic> {
        let id = discovery::probe_id_for(view, query, start, end);
        assert!(self.probes.contains_key(&id), "unplanned search {query}");
        self.emitted.push(id.clone());
        Ok(id)
    }
    fn poll(&mut self, _: &str, job: &str, _: Instant) -> Result<Value, Diagnostic> {
        let rows = self.world.rows(self.request, &self.probes[job]);
        Ok(json!({"done":true,"cancelled":false,"metaData":{},"events":rows}))
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        Ok(())
    }
    fn observed_at(&self, _: &str) -> Option<String> {
        Some("2026-02-01T00:00:00Z".into())
    }
}

/// Native execution of the planned searches: the searches it ran and the pack or refusal.
fn native(
    r: &DiscoverRequest,
    world: &World,
    planned: &[Probe],
) -> (
    Vec<String>,
    Result<Pack, Diagnostic>,
    discovery::DiscoverReport,
) {
    let mut backend = Backend {
        request: r,
        world,
        probes: planned
            .iter()
            .map(|probe| (discovery::probe_id(VIEW, probe), probe.clone()))
            .collect(),
        emitted: vec![],
    };
    let (pack, report) = discovery::execute(r, &mut backend);
    let result = pack.ok_or_else(|| report.diagnostics[0].clone());
    (backend.emitted, result, report)
}

/// A run that completes: planning and native execution agree search for search.
fn witness(r: &DiscoverRequest, world: &World) -> (Vec<Probe>, Pack, discovery::DiscoverReport) {
    let planned = plan(r, world).unwrap();
    let (emitted, pack, report) = native(r, world, &planned);
    let ids: Vec<_> = planned
        .iter()
        .map(|probe| discovery::probe_id(VIEW, probe))
        .collect();
    if r.sources.len() == 1 {
        assert_eq!(emitted, ids, "native order differs from planning");
    } else {
        // Native execution measures one source after another; planning answers by stage.
        let (mut emitted, mut ids) = (emitted, ids);
        emitted.sort();
        ids.sort();
        assert_eq!(
            emitted, ids,
            "native searches differ from planning: {:?}",
            report.diagnostics
        );
    }
    (planned, pack.unwrap(), report)
}
fn searches(r: &DiscoverRequest, world: &World) -> usize {
    witness(r, world).0.len()
}
fn estimate(r: &DiscoverRequest) -> (u32, u32, u32) {
    let e: ExpectedSearches = discovery::expected_searches(r).unwrap();
    (e.low, e.minimum_all_live, e.high)
}
fn kinds(probes: &[Probe]) -> Vec<&str> {
    probes.iter().map(|probe| probe.kind.as_str()).collect()
}
fn count(probes: &[Probe], kind: &str) -> usize {
    probes.iter().filter(|probe| probe.kind == kind).count()
}
fn plain(prefix: &str, n: usize) -> Vec<String> {
    (1..=n).map(|index| format!("{prefix}{index:04}")).collect()
}

// ---------------------------------------------------------------------------------------------
// Exact facet reuse
// ---------------------------------------------------------------------------------------------

fn field<'a>(pack: &'a Pack, name: &str) -> &'a Value {
    let (path, tag) = name
        .strip_prefix('#')
        .map_or((name, false), |path| (path, true));
    pack.data
        .fields
        .iter()
        .find(|field| field["path"] == path && field["is_tag"] == tag)
        .unwrap_or_else(|| panic!("no field {name}"))
}
fn proof<'a>(report: &'a discovery::DiscoverReport, record: &Value) -> &'a discovery::Capture {
    let id = record["provenance_ids"][0].as_str().unwrap();
    let index: usize = id.rsplit(':').next().unwrap().parse().unwrap();
    &report.captures[index]
}
fn record_kinds(pack: &Pack) -> &Value {
    &pack.data.sources[0]["relationship"]["record_kinds"]
}
fn record_kind<'a>(pack: &'a Pack, field: &str) -> &'a Value {
    record_kinds(pack)
        .as_array()
        .unwrap()
        .iter()
        .find(|kind| kind["field"] == field)
        .unwrap_or_else(|| panic!("no record kind {field}"))
}

#[test]
fn exact_facets() {
    // Full with the dataset requested as a kind: a complete dataset listing below 50 rows is
    // the facet, ordered by count then value, while the source keeps the listing order.
    let r = request(&["synthetic"], json!({"kind_fields":["#event.dataset"]}));
    let world = |datasets: &[(&str, u64)]| {
        World::of(&[(
            "synthetic",
            Source::live(&["#event.dataset", "user"])
                .a(&[("#event.dataset", 100), ("user", 100)])
                .datasets(datasets),
        )])
    };
    let unsorted = [("c", 5), ("a", 9), ("b", 9), ("d", 0)];
    let (probes, pack, report) = witness(&r, &world(&unsorted));
    assert_eq!(
        count(&probes, "record-kind-facet"),
        0,
        "{:?}",
        kinds(&probes)
    );
    assert_eq!(probes.len(), 5);
    assert_eq!(
        record_kind(&pack, "#event.dataset"),
        &json!({"field":"#event.dataset","values":["a","b","c","d"],"truncated":false})
    );
    assert_eq!(
        pack.data.sources[0]["relationship"]["datasets"],
        json!(["c", "a", "b", "d"])
    );
    // The dataset field keeps its recent tag count; its wider presence is the base listing.
    assert_eq!(
        proof(&report, field(&pack, "#event.dataset")).probe.kind,
        "tag-batch"
    );
    for rows in [0, 49, 50, 51] {
        let values: Vec<(String, u64)> = (0..rows)
            .map(|index| (format!("dataset-{index:02}"), 1 + (index as u64 % 7)))
            .collect();
        let named: Vec<(&str, u64)> = values.iter().map(|(v, n)| (v.as_str(), *n)).collect();
        let (probes, pack, _) = witness(&r, &world(&named));
        let separate = count(&probes, "record-kind-facet");
        assert_eq!(separate, usize::from(rows >= 50), "{rows} rows");
        let kind = record_kind(&pack, "#event.dataset");
        assert_eq!(kind["truncated"], rows >= 50, "{rows} rows");
        let listed: Vec<u64> = kind["values"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| {
                named
                    .iter()
                    .find(|(name, _)| value == name)
                    .map(|(_, events)| *events)
                    .unwrap()
            })
            .collect();
        assert!(listed.windows(2).all(|pair| pair[0] >= pair[1]), "{rows}");
        assert_eq!(listed.len(), rows.min(50));
    }
    // Without the dataset among the kinds, Full measures no dataset facet at all.
    let plain_full = request(&["synthetic"], json!({}));
    let (probes, pack, _) = witness(&plain_full, &world(&unsorted));
    assert_eq!(count(&probes, "record-kind-facet"), 0);
    assert!(
        pack.data.sources[0]["relationship"]
            .get("record_kinds")
            .is_none()
    );

    // A literal type selector is its own #type facet, counted by the volume over the full
    // window; a selector lowered to an anchored regular expression keeps the facet search.
    let pinned = |name: &str| -> DiscoverRequest {
        let mut r = quick(&[], json!({}));
        r.sources = vec![Scope {
            type_: Some(name.into()),
            ..Scope::default()
        }];
        r
    };
    let typed = |name: &str| {
        World::of(&[(
            name,
            Source::live(&["#type", "user"])
                .a(&[("#type", 0), ("user", 100)])
                .facet("#type", &[(name, 1000)]),
        )])
    };
    let r = pinned("pinned");
    let (probes, pack, report) = witness(&r, &typed("pinned"));
    assert_eq!(
        count(&probes, "record-kind-facet"),
        0,
        "{:?}",
        kinds(&probes)
    );
    assert_eq!(probes.len(), 5);
    assert_eq!(
        record_kind(&pack, "#type"),
        &json!({"field":"#type","values":["pinned"],"truncated":false})
    );
    // The type is observed over the full window by the volume search, not by the recent zero.
    let record = field(&pack, "#type");
    assert_eq!(record["coverage"]["observed"], true);
    let capture = proof(&report, record);
    assert_eq!(capture.probe.kind, "volume");
    assert_eq!(record["observation_window"], format!("{START}/{END}"));
    let r = pinned("pin*ned");
    let (probes, pack, _) = witness(&r, &typed("pin*ned"));
    assert_eq!(count(&probes, "record-kind-facet"), 1);
    assert_eq!(probes.len(), 6);
    assert_eq!(record_kind(&pack, "#type")["values"], json!(["pin*ned"]));
    // A literal type the source never lists or counts takes no facet and no volume proof: its
    // tag batch zero stands.
    let r = pinned("pinned");
    let unlisted = World::of(&[("pinned", Source::live(&["user"]).a(&[("user", 100)]))]);
    let (_, pack, report) = witness(&r, &unlisted);
    let record = field(&pack, "#type");
    assert_eq!(record["coverage"]["observed"], false);
    assert_eq!(proof(&report, record).probe.kind, "tag-batch");
    assert!(record_kinds(&pack).is_null());

    // Its proof is the volume answer: a #type hint queued before the run resolves with it; one
    // queued after the last search that measures #type stays open, with no later facet search.
    let (work, volume) = hinted_type_run(false);
    assert_eq!(work["resolution"]["kind"], "inventoried", "{work}");
    assert_eq!(work["resolution"]["evidence"][0]["fields"]["#type"], volume);
    let (work, _) = hinted_type_run(true);
    assert!(work["resolution"].is_null(), "{work}");

    // Repository only, one dataset and several types: the type facet is searched and drives the
    // rare-kind listings.
    let r = quick(&["synthetic"], json!({}));
    let world = World::of(&[(
        "synthetic",
        Source::live(&["#type", "user"])
            .a(&[("#type", 100), ("user", 100)])
            .datasets(&[("only", 1000)])
            .facet("#type", &[("t2", 600), ("t1", 300), ("t3", 100)]),
    )]);
    let (probes, pack, _) = witness(&r, &world);
    let facets: Vec<_> = probes
        .iter()
        .filter(|probe| probe.kind == "record-kind-facet")
        .map(|probe| probe.field.as_deref().unwrap())
        .collect();
    assert_eq!(facets, ["#type"]);
    assert_eq!(count(&probes, "kind-fieldset"), 3);
    assert!(
        probes
            .iter()
            .filter(|probe| probe.kind == "kind-fieldset")
            .all(|probe| probe.field.as_deref() == Some("#type"))
    );
    assert_eq!(
        record_kind(&pack, "#type"),
        &json!({"field":"#type","values":["t2","t1","t3"],"truncated":false})
    );
    assert_eq!(
        record_kind(&pack, "#event.dataset"),
        &json!({"field":"#event.dataset","values":["only"],"truncated":false})
    );

    // Partial answers: until the base listings answer, no facet is planned, and once they do the
    // reused facets never appear.
    let r = request(&["synthetic"], json!({"kind_fields":["#event.dataset"]}));
    let world = world_with_tags();
    let mut answers = BTreeMap::new();
    let first = discovery::frontier(&r, &answers).unwrap();
    assert_eq!(kinds(&first), ["volume"]);
    for probe in &first {
        answers.insert(discovery::probe_id(VIEW, probe), world.rows(&r, probe));
    }
    let second = discovery::frontier(&r, &answers).unwrap();
    assert_eq!(kinds(&second), ["dataset-facet", "fieldset"]);
    answers.insert(
        discovery::probe_id(VIEW, &second[0]),
        world.rows(&r, &second[0]),
    );
    // The dataset listing alone does not let the run skip the fieldset.
    assert_eq!(
        kinds(&discovery::frontier(&r, &answers).unwrap()),
        ["fieldset"]
    );
    let (probes, _, _) = witness(&r, &world);
    assert!(
        probes
            .iter()
            .all(|probe| probe.field.as_deref() != Some("#event.dataset"))
    );

    // Each reuse saves one search.
    let r = request(&["synthetic"], json!({"kind_fields":["#event.dataset"]}));
    let many: Vec<(String, u64)> = (0..50).map(|i| (format!("d{i:02}"), 1)).collect();
    let many: Vec<(&str, u64)> = many.iter().map(|(v, n)| (v.as_str(), *n)).collect();
    let full_reused = searches(&r, &world_datasets(&[("a", 1), ("b", 2)]));
    let full_searched = searches(&r, &world_datasets(&many));
    assert_eq!(full_searched, full_reused + 1);
    let pinned_saved = searches(&pinned("pinned"), &typed("pinned"));
    let pinned_searched = searches(&pinned("pin*ned"), &typed("pin*ned"));
    assert_eq!(pinned_searched, pinned_saved + 1);
}
fn wire_probe(wire: &Value) -> Probe {
    let args = &wire["mcp"]["arguments"];
    Probe {
        kind: wire["kind"].as_str().unwrap().into(),
        source: serde_json::from_value(wire["source"].clone()).unwrap(),
        query: args["query_string"].as_str().unwrap().into(),
        start: args["start"].as_str().unwrap().into(),
        end: args["end"].as_str().unwrap().into(),
        fields: serde_json::from_value(wire["fields"].clone()).unwrap(),
        field: serde_json::from_value(wire["field"].clone()).unwrap(),
    }
}
/// A Quick inventory run for a literal type whose `#type` hint is queued before the run or
/// after its tag batch answer, the last search that measures `#type`; the pending work item
/// afterwards and the volume probe ID.
fn hinted_type_run(after_batch: bool) -> (Value, String) {
    let dir = Temp::new();
    confirm(&dir, "sources-and-fields", &[json!({"type":"pinned"})]);
    let hint = json!({"kind":"record-inventory-hints","environment":environment_value(),
        "hints":[{"source":{"type":"pinned"},"fields":["#type"]}]});
    if !after_batch {
        call(&dir, hint.clone());
    }
    let world = World::of(&[(
        "pinned",
        Source::live(&["#type", "user"]).a(&[("#type", 0), ("user", 100)]),
    )]);
    let mut progress = call(&dir, prepare_action(&[], START, END, 80));
    let request: DiscoverRequest =
        serde_json::from_value(read(&checkpoint(&dir, 0))["request"].clone()).unwrap();
    let mut volume = String::new();
    while progress["action"]["relay"]["status"] == "needs-probes" {
        let wire = progress["action"]["relay"]["probes"][0].clone();
        let probe = wire_probe(&wire);
        let id = wire["probe_id"].clone();
        call(&dir, json!({"kind":"dispatch-discovery","probe_id":id}));
        let file = answer_file(&dir, json!(world.rows(&request, &probe)));
        progress = call(
            &dir,
            json!({"kind":"discovery-step","answers":[{"probe_id":id,"result_file":file}]}),
        );
        if probe.kind == "volume" {
            volume = id.as_str().unwrap().to_string();
        }
        if probe.kind == "tag-batch" && after_batch {
            call(&dir, hint.clone());
        }
    }
    assert_eq!(
        progress["action"]["relay"]["status"], "complete",
        "{progress}"
    );
    (read(&dir.state())["pending_inventory"][0].clone(), volume)
}
fn world_with_tags() -> World {
    World::of(&[(
        "synthetic",
        Source::live(&["#event.dataset", "user"])
            .a(&[("#event.dataset", 100), ("user", 100)])
            .datasets(&[("a", 2), ("b", 1)]),
    )])
}
fn world_datasets(datasets: &[(&str, u64)]) -> World {
    World::of(&[(
        "synthetic",
        Source::live(&["#event.dataset", "user"])
            .a(&[("#event.dataset", 100), ("user", 100)])
            .datasets(datasets),
    )])
}

// ---------------------------------------------------------------------------------------------
// Activating range and admission minimum through the command line
// ---------------------------------------------------------------------------------------------

/// Answer each next-probe search: `quiet` sources have no events; a live source lists only a
/// counted tag, its cheapest path.
fn rows_for(query: &str, quiet: &[&str]) -> Value {
    if query.ends_with("count(as=events)") {
        let events = if quiet.iter().any(|name| query.contains(name)) {
            0
        } else {
            1000
        };
        json!([{"events":events}])
    } else if query.ends_with("fieldset()") {
        json!([{"field":"#Vendor"}])
    } else if query.contains("stats([count(as=total)") {
        let fields: Vec<&str> = query
            .split("count(field=")
            .skip(1)
            .map(|rest| rest.split(',').next().unwrap().trim_matches('"'))
            .collect();
        let mut row = json!({"total":100});
        for (index, field) in fields.iter().enumerate() {
            row[format!("c{index}")] = json!(if *field == "#Vendor" { 100 } else { 0 });
        }
        json!([row])
    } else {
        json!([])
    }
}
fn expected_cost(cost: &Value, used: u64, limit: u64) {
    let searches = &cost["searches"];
    assert_eq!(searches["used"], used, "{cost}");
    assert_eq!(searches["expected_low"], 5, "{cost}");
    assert_eq!(searches["minimum_all_live"], 8, "{cost}");
    assert_eq!(searches["expected_high"], 42, "{cost}");
    assert_eq!(searches["limit"], limit, "{cost}");
    assert_eq!(searches["below_expected_high"], true, "{cost}");
}
/// Prepare at `limit` and drive next-probe to the end, checking the cost on every reply.
fn drive(dir: &Temp, quiet: &[&str], limit: u64, brief: bool) -> (Value, u64) {
    let prepared = call_with(
        dir,
        prepare_action(&[], START, END, limit as u32),
        if brief { &["--brief"] } else { &[] },
    );
    if brief {
        expected_cost(&prepared["action"]["cost"], 0, limit);
    } else {
        expected_cost(&prepared["action"]["relay"]["cost"], 0, limit);
    }
    let mut step = next_probe(dir, None, brief);
    let mut used = 0;
    while step["state"] == "probe" {
        // The reply reserves its search; polling it again charges nothing more.
        expected_cost(&step["cost"], used + 1, limit);
        for _ in 0..2 {
            let polled = next_probe(dir, None, brief);
            assert_eq!(polled["probe"], step["probe"]);
            expected_cost(&polled["cost"], used + 1, limit);
        }
        let query = step["probe"]["arguments"]["query_string"]
            .as_str()
            .unwrap()
            .to_string();
        let file = answer_file(dir, rows_for(&query, quiet));
        step = next_probe(dir, Some((&step["probe"]["probe_id"], &file)), brief);
        used += 1;
    }
    expected_cost(&step["cost"], used, limit);
    (step, used)
}

#[test]
fn two_live_or_quiet() {
    let sources = [json!({"repo":"alpha_repo"}), json!({"repo":"bravo_repo"})];
    let planned = quick(&["alpha_repo", "bravo_repo"], json!({"max_queries":8}));
    assert_eq!(estimate(&planned), (5, 8, 42));
    let shown = discovery::initial_plan(&planned).unwrap();
    assert_eq!(
        shown["searches"],
        json!({"expected_low":5,"minimum_all_live":8,"expected_high":42,"limit":8,"below_expected_high":true})
    );

    // A limit of 7 cannot complete both sources live.
    let dir = Temp::new();
    confirm(&dir, "sources-and-fields", &sources);
    let before = (fs::read(dir.state()).unwrap(), entries(&dir));
    let refusal = refused_with(&dir, &prepare_action(&[], START, END, 7), &[]);
    assert!(
        refusal["message"]
            .as_str()
            .unwrap()
            .contains("Set max_queries to at least 8."),
        "{refusal}"
    );
    assert!((fs::read(dir.state()).unwrap(), entries(&dir)) == before);

    // At 8: one quiet and one live source activate at 5, both live at 8, all quiet fails at 2.
    for (quiet, brief, total, outcome) in [
        (vec!["alpha_repo"], true, 5, "complete"),
        (vec![], false, 8, "complete"),
        (vec!["alpha_repo", "bravo_repo"], false, 2, "failed"),
    ] {
        let dir = Temp::new();
        confirm(&dir, "sources-and-fields", &sources);
        let (step, used) = drive(&dir, &quiet, 8, brief);
        assert_eq!(used, total, "{quiet:?}");
        assert_eq!(step["state"], outcome, "{quiet:?}: {step}");
        assert_eq!(
            read(&dir.state())["active_pack"].is_object(),
            outcome == "complete"
        );
    }

    // The discovery-step brief projects the boolean beside the minimum.
    let dir = Temp::new();
    confirm(&dir, "sources-and-fields", &sources);
    call(&dir, prepare_action(&[], START, END, 8));
    let brief = call_with(
        &dir,
        json!({"kind":"discovery-step","answers":[]}),
        &["--brief"],
    );
    expected_cost(&brief["action"]["cost"], 0, 8);

    // Pause and resume keep the charge, the figures and the allowance.
    let paused = call(&dir, json!({"kind":"pause-discovery"}));
    assert_eq!(paused["action"]["relay"]["status"], "paused", "{paused}");
    let resumed = call(&dir, json!({"kind":"resume-discovery"}));
    expected_cost(&resumed["action"]["relay"]["cost"], 0, 8);
    let step = next_probe(&dir, None, false);
    expected_cost(&step["cost"], 1, 8);
    expected_cost(&next_probe(&dir, None, false)["cost"], 1, 8);
    // State and checkpoint round-trip byte for byte through this build's types.
    let bytes = fs::read(dir.state()).unwrap();
    let parsed: InventoryState = serde_json::from_slice(&bytes).unwrap();
    let mut again = serde_json::to_vec_pretty(&parsed).unwrap();
    again.push(b'\n');
    assert_eq!(again, bytes);
}

// ---------------------------------------------------------------------------------------------
// Reachable cost matrix
// ---------------------------------------------------------------------------------------------

/// The listing and counts of a source on its most expensive path: every facet measured, the
/// first discriminator multi-valued with ten rare kinds, and `fields` plain fields.
fn busiest(fields: Vec<String>, fallback: bool, kinds: &[&str]) -> Source {
    let mut listed = fields;
    listed.push("event.kind".into());
    listed.extend(kinds.iter().map(|kind| kind.to_string()));
    let mut tags: Vec<(&str, u64)> = TAGS.iter().map(|tag| (*tag, 50)).collect();
    tags.push(("event.kind", 50));
    tags.extend(kinds.iter().map(|kind| (*kind, 50)));
    let rare: Vec<String> = (0..12).map(|index| format!("k{index:02}")).collect();
    let rare: Vec<(&str, u64)> = rare.iter().map(|kind| (kind.as_str(), 10)).collect();
    let mut source = Source::live(&[])
        .a(&tags)
        .w(&tags)
        .datasets(&[("synthetic", 50)])
        .facet("#event_simpleName", &rare)
        .facet("#type", &[("synthetic", 50)])
        .facet("#event.kind", &[("synthetic", 50)])
        .facet("event.kind", &[("synthetic", 50)])
        .facet("#event.module", &[("synthetic", 50)]);
    if fallback {
        source.full = listed;
    } else {
        source.sample = listed;
    }
    source.rare = strings(&["rare-a", "rare-b"]);
    source
}

#[test]
fn reachable_cost_matrix() {
    // Quick, the full window longer than the sample window: cheapest 4, most 21.
    let r = quick(&["synthetic"], json!({}));
    assert_eq!(estimate(&r), (4, 4, 21));
    let cheapest = World::of(&[(
        "synthetic",
        Source::live(&["#Vendor"]).a(&[("#Vendor", 10)]),
    )]);
    let (probes, _, _) = witness(&r, &cheapest);
    assert_eq!(
        kinds(&probes),
        ["volume", "dataset-facet", "fieldset", "tag-batch"]
    );
    // Most without the fallback: two population windows; with the sparse fallback: one window
    // and the fallback listing.
    let split = {
        let mut source = busiest(plain("p", 19), false, &[]);
        source.rare = plain("r", 5);
        source
    };
    let probes = witness(&r, &World::of(&[("synthetic", split)])).0;
    assert_eq!(probes.len(), 21, "{:?}", kinds(&probes));
    assert_eq!(count(&probes, "population"), 2);
    assert_eq!(count(&probes, "record-kind-facet"), 5);
    assert_eq!(count(&probes, "kind-fieldset"), 10);
    let sparse = busiest(plain("p", 25), true, &[]);
    let probes = witness(&r, &World::of(&[("synthetic", sparse)])).0;
    assert_eq!(probes.len(), 21, "{:?}", kinds(&probes));
    assert_eq!(count(&probes, "fieldset"), 2);
    assert_eq!(count(&probes, "population"), 1);
    // Quick, equal windows: no fallback and one population window.
    let r = short(quick(&["synthetic"], json!({})));
    assert_eq!(estimate(&r), (4, 4, 20));
    let mut equal = busiest(plain("p", 30), false, &[]);
    equal.full = equal.sample.clone();
    equal.w = equal.a.clone();
    assert_eq!(searches(&r, &World::of(&[("synthetic", equal)])), 20);
    assert_eq!(searches(&r, &cheapest), 4);

    // Full, automatic fields: 4,000 listed fields after a fallback, no rare listings.
    let r = request(&["synthetic"], json!({}));
    assert_eq!(estimate(&r), (4, 4, 210));
    let full = busiest(plain("p", 3999), true, &[]);
    let probes = witness(&r, &World::of(&[("synthetic", full.clone())])).0;
    assert_eq!(probes.len(), 210);
    assert_eq!(count(&probes, "kind-fieldset"), 0);
    assert_eq!(searches(&r, &cheapest), 4);

    // Full, explicit plain fields: 1, 20, 21, 40 and 41 fields.
    for (n, low, high) in [
        (1, 5, 11),
        (20, 5, 11),
        (21, 6, 12),
        (40, 6, 12),
        (41, 7, 13),
    ] {
        let fields = plain("p", n);
        let r = request(
            &["synthetic"],
            json!({"population_fields":[{"source":{"repo":"synthetic"},"fields":fields}]}),
        );
        assert_eq!(estimate(&r), (low, low, high), "{n} fields");
        // Cheapest: every field listed in the sample window, nothing else measured.
        let mut listed = Source::live(&[]);
        listed.sample = fields.clone();
        assert_eq!(
            searches(&r, &World::of(&[("synthetic", listed)])),
            low as usize
        );
        // Most: split between the windows, or over the full window after a fallback.
        let most = [
            {
                let mut source = busiest(vec![], false, &[]);
                source.sample.extend(fields[..1].iter().cloned());
                source
            },
            busiest(fields.clone(), true, &[]),
        ]
        .into_iter()
        .map(|source| searches(&r, &World::of(&[("synthetic", source)])))
        .max()
        .unwrap();
        assert_eq!(most, high as usize, "{n} fields");
    }

    // Two sources with added kinds and values.
    let r = quick(
        &["alpha", "bravo"],
        json!({"kind_fields":["custom.kind"],"value_fields":["value.field"]}),
    );
    assert_eq!(estimate(&r), (6, 10, 46));
    let world = World::of(&[
        ("alpha", Source::quiet()),
        ("bravo", Source::live(&["#Vendor"]).a(&[("#Vendor", 10)])),
    ]);
    assert_eq!(searches(&r, &world), 6);
    let busy = busiest(plain("p", 25), true, &["custom.kind"]).facet("custom.kind", &[("one", 5)]);
    let world = World::of(&[("alpha", busy.clone()), ("bravo", busy)]);
    assert_eq!(searches(&r, &world), 46);

    // (a) Full with an explicit selection succeeds when it omits observed kind and value fields.
    let r = request(
        &["synthetic"],
        json!({"kind_fields":["custom.kind"],"value_fields":["value.field"],
            "population_fields":[{"source":{"repo":"synthetic"},"fields":["user"]}]}),
    );
    let world = World::of(&[(
        "synthetic",
        Source::live(&["user", "custom.kind", "value.field"])
            .a(&[("user", 10), ("custom.kind", 10), ("value.field", 10)])
            .facet("custom.kind", &[("one", 10)]),
    )]);
    let (_, pack, _) = witness(&r, &world);
    assert_eq!(
        pack.data.sources[0]["evidence"]["measured_fields"],
        json!(["user"])
    );

    // (b) Quick with an explicit selection that omits every requested stratified kind: no facet
    // and no rare listing can occur in a completed run.
    let stratified = [
        "#event_simpleName",
        "#type",
        "#event.kind",
        "event.kind",
        "#event.module",
        "#event.dataset",
    ];
    let omitting = |r: DiscoverRequest| {
        let mut r = r;
        r.kind_fields = strings(&stratified);
        r.population_fields =
            serde_json::from_value(json!([{"source":{"repo":"synthetic"},"fields":["user"]}]))
                .unwrap();
        r
    };
    let r = omitting(short(quick(&["synthetic"], json!({}))));
    assert_eq!(estimate(&r), (5, 5, 5));
    let user = Source::live(&["user"])
        .a(&[("user", 10)])
        .w(&[("user", 10)]);
    let probes = witness(&r, &World::of(&[("synthetic", user.clone())])).0;
    assert_eq!(probes.len(), 5);
    let r = omitting(quick(&["synthetic"], json!({})));
    assert_eq!(estimate(&r), (5, 5, 6));
    let unlisted = Source::live(&["other"]).w(&[("user", 10)]);
    assert_eq!(searches(&r, &World::of(&[("synthetic", unlisted)])), 5);
    let sparse = Source::live(&[])
        .full(strings(&["user"]))
        .w(&[("user", 10)]);
    let probes = witness(&r, &World::of(&[("synthetic", sparse)])).0;
    assert_eq!(probes.len(), 6);
    assert_eq!(
        count(&probes, "record-kind-facet") + count(&probes, "kind-fieldset"),
        0
    );
    // Any observed requested kind ends the run instead.
    let observed = user.datasets(&[("one", 5), ("two", 5)]);
    assert!(plan(&r, &World::of(&[("synthetic", observed)])).is_err());

    // (c) An explicit tagged kind candidate over a longer window costs 5: a listed one takes its
    // facet, an unlisted one a full-window count. With 20 plain fields, 6. Over equal windows,
    // or a tag that is no kind candidate, 4.
    let module = |extra: &[&str]| {
        let mut fields = vec!["#event.module".to_string()];
        fields.extend(extra.iter().map(|field| field.to_string()));
        json!({"population_fields":[{"source":{"repo":"synthetic"},"fields":fields}]})
    };
    let r = quick(&["synthetic"], module(&[]));
    assert_eq!(estimate(&r).0, 5);
    let listed = Source::live(&["#event.module"])
        .a(&[("#event.module", 10)])
        .facet("#event.module", &[("one", 10)]);
    assert_eq!(searches(&r, &World::of(&[("synthetic", listed)])), 5);
    let unlisted = Source::live(&["other"]).w(&[("#event.module", 10)]);
    assert_eq!(searches(&r, &World::of(&[("synthetic", unlisted)])), 5);
    let twenty = plain("p", 20);
    let twenty: Vec<&str> = twenty.iter().map(String::as_str).collect();
    // Twenty-one selected fields exceed the Quick cap, so Full selects them.
    let r = request(&["synthetic"], module(&twenty));
    assert_eq!(estimate(&r).0, 6);
    let mut listed = Source::live(&[]);
    listed.sample = strings(&twenty);
    assert_eq!(searches(&r, &World::of(&[("synthetic", listed)])), 6);
    let r = short(quick(&["synthetic"], module(&[])));
    assert_eq!(estimate(&r).0, 4);
    assert_eq!(
        searches(&r, &World::of(&[("synthetic", Source::live(&["other"]))])),
        4
    );
    let r = quick(
        &["synthetic"],
        json!({"population_fields":[{"source":{"repo":"synthetic"},"fields":["#Vendor"]}]}),
    );
    assert_eq!(estimate(&r).0, 4);
    assert_eq!(searches(&r, &cheapest), 4);

    // Requested kinds share the 20-field cap: 20 of 21 observed succeed, 21 or an omitted one in
    // an explicit selection fail.
    let custom = plain("kind.", 21);
    let r = quick(&["synthetic"], json!({"kind_fields":custom}));
    assert_eq!(estimate(&r).2, 41);
    let mut twenty = busiest(vec![], true, &[]);
    twenty.full.extend(custom[..20].iter().cloned());
    for kind in &custom[..20] {
        twenty = twenty.facet(kind, &[("one", 1)]).w(&[(kind.as_str(), 50)]);
    }
    let probes = witness(&r, &World::of(&[("synthetic", twenty)])).0;
    assert_eq!(probes.len(), 41);
    let mut all = Source::live(&[]);
    all.sample = custom.clone();
    assert!(plan(&r, &World::of(&[("synthetic", all)])).is_err());
    let mut omitted = quick(&["synthetic"], json!({"kind_fields":["kind.0001"]}));
    omitted.population_fields =
        serde_json::from_value(json!([{"source":{"repo":"synthetic"},"fields":["user"]}])).unwrap();
    let listed = Source::live(&["kind.0001", "user"]);
    assert!(plan(&omitted, &World::of(&[("synthetic", listed)])).is_err());

    // Above the 1,000-search ceiling: five Full sources can take 1,050 searches and 100 Quick
    // sources 2,100. The highest total within the limit completes; the most exhausts it.
    let names: Vec<String> = (0..5).map(|index| format!("full-{index}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let r = request(&refs, json!({}));
    assert_eq!(estimate(&r), (4 + 4, 20, 1050));
    let shown = discovery::initial_plan(&r).unwrap();
    assert_eq!(shown["searches"]["expected_high"], 1050);
    assert_eq!(shown["searches"]["below_expected_high"], true);
    let mut sources: Vec<(&str, Source)> =
        refs[..4].iter().map(|name| (*name, full.clone())).collect();
    // 4 × 210 plus 160: three base searches, the fallback, the tag batch, five facets and 150
    // population chunks.
    sources.push((refs[4], busiest(plain("p", 2999), true, &[])));
    let at_ceiling = World::of(&sources);
    let probes = witness(&r, &at_ceiling).0;
    assert_eq!(probes.len(), 1000);
    let most = World::of(
        &refs
            .iter()
            .map(|name| (*name, full.clone()))
            .collect::<Vec<_>>(),
    );
    let planned = plan(&r, &most).unwrap();
    assert_eq!(planned.len(), 1050);
    let (emitted, result, _) = native(&r, &most, &planned);
    assert_eq!(emitted.len(), 1000);
    assert!(
        result
            .unwrap_err()
            .message
            .contains("Discovery budget exhausted")
    );

    let names: Vec<String> = (0..100).map(|index| format!("quick-{index:03}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let r = quick(&refs, json!({}));
    assert_eq!(estimate(&r), (99 + 4, 400, 2100));
    let busy = {
        let mut source = busiest(plain("p", 19), false, &[]);
        source.rare = plain("r", 5);
        source
    };
    let mut sources: Vec<(&str, Source)> = refs[..45]
        .iter()
        .map(|name| (*name, busy.clone()))
        .collect();
    sources.extend(refs[45..].iter().map(|name| (*name, Source::quiet())));
    assert_eq!(witness(&r, &World::of(&sources)).0.len(), 1000);
    let most = World::of(
        &refs
            .iter()
            .map(|name| (*name, busy.clone()))
            .collect::<Vec<_>>(),
    );
    let planned = plan(&r, &most).unwrap();
    assert_eq!(planned.len(), 2100);
    let (emitted, result, _) = native(&r, &most, &planned);
    assert_eq!(emitted.len(), 1000);
    assert!(result.is_err());
}
