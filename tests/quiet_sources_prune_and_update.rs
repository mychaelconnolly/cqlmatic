use cqlmatic::{
    contracts::{Cloud, EnvironmentId, Scope},
    discovery,
    environment::{Pack, PackData},
    storage,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

const VIEW: &str = "synthetic_view";
const BRAVO_FIELD: &str = "#repo=bravo_repo #type=bravo_type | bravo_field=* | count()";
const NEVER_FIELD: &str = "#repo=bravo_repo #type=bravo_type | never_field=* | count()";

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-quiet-prune-{}-{}",
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
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn day(day: u32) -> String {
    format!("2026-01-{day:02}T00:00:00Z")
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
fn write(path: &Path, value: &impl serde::Serialize) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
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
fn pack(created: u32, source: Value) -> Pack {
    Pack::new(
        environment_id(),
        day(created),
        PackData {
            sources: vec![source],
            provenance: vec![
                json!({"id":"prov:synthetic","source_type":"mcp-observation",
                "canonical_source":"Synthetic pack","retrieved_at":day(created),
                "redaction_status":"none","coverage_status":"observed-only"}),
            ],
            ..Default::default()
        },
    )
    .unwrap()
}

fn cqlmatic(dir: &Temp) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    command.env("HOME", &dir.0);
    command
}
fn call(dir: &Temp, action: Value) -> Value {
    let request = dir.0.join("request.json");
    write(&request, &json!({"schema_version":1,"action":action}));
    let output = cqlmatic(dir)
        .args(["env", "inventory", "--json", "--state"])
        .arg(dir.state())
        .arg("--request")
        .arg(request)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{action}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
fn import(dir: &Temp, pack: &Pack, mode: &str) -> Value {
    let file = dir.0.join("import.json");
    write(&file, pack);
    call(
        dir,
        json!({"kind":"import-pack","pack_file":file,"mode":mode}),
    )
}
fn confirm(dir: &Temp, mode: Value, names: &[&str]) {
    call(dir, mode);
    call(
        dir,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources",
            "context":"Synthetic quiet-source target","expected_sources":names.iter().map(|n| scope(n)).collect::<Vec<_>>()}),
    );
}
fn full() -> Value {
    json!({"kind":"set-discovery-mode","mode":"full"})
}
fn quick() -> Value {
    json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-and-fields"})
}
fn prepare(dir: &Temp, start: u32, end: u32, names: &[&str]) {
    let mut action = json!({"kind":"prepare-discovery","start":day(start),"end":day(end),
        "max_queries":200,"max_seconds":600});
    if !names.is_empty() {
        action["source_ids"] = json!(names.iter().map(|n| id(n)).collect::<Vec<_>>());
    }
    call(dir, action);
}
fn hints(dir: &Temp, names: &[&str]) {
    let hints: Vec<_> = names
        .iter()
        .map(|n| json!({"source":scope(n),"fields":[format!("{n}_field")]}))
        .collect();
    call(
        dir,
        json!({"kind":"record-inventory-hints","environment":environment(),"hints":hints}),
    );
}

/// Synthetic answers: each source's volume, and a listing of `user` and `<source>_field`.
struct World(BTreeMap<String, u64>);
impl World {
    fn new(volumes: &[(&str, u64)]) -> Self {
        Self(volumes.iter().map(|(n, e)| (n.to_string(), *e)).collect())
    }
    fn rows(&self, query: &str) -> Value {
        let source = self
            .0
            .keys()
            .find(|name| query.contains(&format!("{name}_repo")))
            .unwrap_or_else(|| panic!("unknown source in {query}"));
        let counts = |value: u64| {
            let mut row = json!({"total":10});
            for index in 0..query.matches(", as=c").count() {
                row[format!("c{index}")] = json!(value);
            }
            json!([row])
        };
        if query.ends_with("fieldset()") {
            json!([{"field":"user"},{"field":format!("{source}_field")}])
        } else if query.contains("count(field=\"#event_simpleName\"") {
            counts(0)
        } else if query.contains("stats([count(as=total)") {
            counts(10)
        } else if query.contains("groupBy([") {
            json!([])
        } else if query.ends_with("count(as=events)") {
            json!([{"events":self.0[source]}])
        } else {
            panic!("unexpected probe {query}")
        }
    }
}
/// Answers every search the latest run asks for through `next-probe`.
fn drive(dir: &Temp, world: &World) -> Value {
    let mut answer: Option<(String, PathBuf)> = None;
    for count in 0..400 {
        let mut command = cqlmatic(dir);
        command.args(["env", "inventory", "--json", "--next-probe"]);
        if let Some((probe, file)) = &answer {
            command
                .arg("--probe-id")
                .arg(probe)
                .arg("--result-file")
                .arg(file);
        }
        let output = command.arg("--state").arg(dir.state()).output().unwrap();
        let reply: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
        let Some(probe) = reply["action"]["probe"].as_object() else {
            let run = dir.saved()["discovery_runs"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .clone();
            assert_eq!(run["status"], "complete", "{run}");
            return run;
        };
        let file = dir.0.join(format!("answer-{count}.json"));
        let query = probe["arguments"]["query_string"].as_str().unwrap();
        write(&file, &json!({"result":world.rows(query)}));
        answer = Some((probe["probe_id"].as_str().unwrap().into(), file));
    }
    panic!("run did not finish");
}
fn finish(dir: &Temp, window: (u32, u32), names: &[&str], world: &World) -> Value {
    prepare(dir, window.0, window.1, names);
    drive(dir, world)
}

fn run(dir: &Temp, index: usize) -> Value {
    dir.saved()["discovery_runs"][index].clone()
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
fn prune(dir: &Temp) -> Value {
    call(dir, json!({"kind":"prune","dry_run":false}))["action"].clone()
}
fn kept_reason(pruned: &Value, index: usize) -> Option<String> {
    pruned["kept"]
        .as_array()
        .unwrap()
        .iter()
        .find(|unit| unit["run_index"] == index)
        .map(|unit| unit["reason"].as_str().unwrap().to_owned())
}
fn snapshot_files(run: &Value) -> Vec<bool> {
    ["state_path", "pack_path", "receipt_path"]
        .iter()
        .map(|key| Path::new(run[key].as_str().unwrap()).exists())
        .collect()
}
fn strict(dir: &Temp, query: &str) -> (Option<i32>, Value) {
    let output = cqlmatic(dir)
        .args([
            "check",
            query,
            "--require-observed",
            "--json",
            "--inventory-state",
        ])
        .arg(dir.state())
        .output()
        .unwrap();
    let result: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{query}: {}", String::from_utf8_lossy(&output.stderr)));
    (output.status.code(), result["outcome"].clone())
}
fn status(dir: &Temp) -> Value {
    let output = cqlmatic(dir)
        .args(["status", "--json", "--state"])
        .arg(dir.state())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}
/// A's per-source status and the scope completion flags.
fn completion(dir: &Temp) -> Value {
    let report = status(dir);
    let alpha = report["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|source| source["source_id"] == id("alpha"))
        .cloned()
        .unwrap();
    json!({"alpha":alpha,"flags":[report["quick_scope_complete"],
        report["selected_scope_complete"],report["full_map_complete"]]})
}

#[test]
fn quiet_only_work_leaves_a_mixed_run_prunable_and_live_work_pins_it() {
    let dir = Temp::new();
    confirm(&dir, full(), &["alpha", "bravo"]);
    finish(
        &dir,
        (1, 2),
        &[],
        &World::new(&[("alpha", 7), ("bravo", 9)]),
    );
    hints(&dir, &["alpha", "bravo"]);
    let mixed = finish(
        &dir,
        (2, 3),
        &[],
        &World::new(&[("alpha", 5), ("bravo", 0)]),
    );
    assert_eq!(mixed["quiet_source_ids"], json!([id("bravo")]));
    finish(&dir, (3, 4), &["alpha"], &World::new(&[("alpha", 5)]));
    assert_eq!(strict(&dir, BRAVO_FIELD), (Some(0), json!("ready")));

    // A is not reviewed yet, so its work stays open, and the mixed run measured A.
    assert!(work(&dir, "alpha")["resolution"].is_null());
    let pinned = prune(&dir);
    assert_eq!(
        kept_reason(&pinned, 1).as_deref(),
        Some("pending-inventory-work"),
        "{pinned}"
    );
    assert!(run(&dir, 1)["pruned_at"].is_null());
    assert_eq!(snapshot_files(&mixed), [true; 3]);

    for action in [
        json!({"kind":"review-source","source_id":id("alpha"),"purpose":"Synthetic events",
            "data_model":"Synthetic model","reviewed_at":day(4),"evidence":"Reviewed the saved measurement"}),
        json!({"kind":"parser-not-applicable","source_id":id("alpha"),"reason":"falcon-sensor-native",
            "reviewed_at":day(4),"evidence":"Synthetic native source"}),
    ] {
        call(&dir, action);
    }
    assert_eq!(work(&dir, "alpha")["resolution"]["kind"], "inventoried");
    let bravo = work(&dir, "bravo");
    assert!(bravo["resolution"].is_null(), "{bravo}");
    assert_eq!(bravo["after_run"], 1, "{bravo}");

    // Only B's work is open, and B was quiet in the mixed run.
    let pruned = prune(&dir);
    assert_eq!(kept_reason(&pruned, 1), None, "{pruned}");
    assert!(run(&dir, 1)["pruned_at"].is_string(), "{pruned}");
    assert_eq!(snapshot_files(&mixed), [false; 3]);
    assert_eq!(work(&dir, "bravo"), bravo);
    assert_eq!(strict(&dir, BRAVO_FIELD), (Some(0), json!("ready")));
    assert_eq!(strict(&dir, NEVER_FIELD), (Some(1), json!("needs-context")));
}

#[test]
fn boundary_bound_quiet_work_leaves_a_mixed_run_prunable_and_keeps_saved_proof() {
    let dir = Temp::new();
    confirm(&dir, quick(), &["alpha", "bravo"]);
    finish(
        &dir,
        (1, 2),
        &[],
        &World::new(&[("alpha", 7), ("bravo", 9)]),
    );
    prepare(&dir, 2, 3, &[]);
    let mixed_path = run(&dir, 1)["state_path"].clone();
    // Recorded while the mixed run is unfinished, so each item binds to it by a boundary.
    hints(&dir, &["alpha", "bravo"]);
    for name in ["alpha", "bravo"] {
        let work = work(&dir, name);
        assert_eq!(work["after_run"], 2, "{work}");
        assert_eq!(work["capture_boundary"]["run"], mixed_path, "{work}");
    }
    let mixed = drive(&dir, &World::new(&[("alpha", 5), ("bravo", 0)]));
    assert_eq!(mixed["quiet_source_ids"], json!([id("bravo")]));
    let proof = work(&dir, "alpha");
    assert_eq!(proof["resolution"]["run"], mixed_path, "{proof}");
    let bravo = work(&dir, "bravo");
    assert!(bravo["resolution"].is_null(), "{bravo}");
    assert_eq!(bravo["capture_boundary"]["run"], mixed_path, "{bravo}");

    // An unrelated import makes the mixed run historical without measuring A again.
    import(
        &dir,
        &pack(
            3,
            json!({"id":"env:repo:unrelated","kind":"source","name":"Unrelated",
                "inventory_status":"observed-only","selectors":{"type":"unrelated_type"},
                "provenance_ids":["prov:synthetic"]}),
        ),
        "append",
    );
    let before = completion(&dir);
    assert_eq!(before["alpha"]["status"], "done", "{before}");
    assert_eq!(strict(&dir, BRAVO_FIELD), (Some(0), json!("ready")));
    assert_eq!(work(&dir, "bravo"), bravo);

    let pruned = prune(&dir);
    assert_eq!(kept_reason(&pruned, 1), None, "{pruned}");
    assert!(run(&dir, 1)["pruned_at"].is_string(), "{pruned}");
    assert_eq!(snapshot_files(&mixed), [false; 3]);
    assert_eq!(work(&dir, "alpha"), proof);
    assert_eq!(completion(&dir), before);
    assert_eq!(work(&dir, "bravo"), bravo);
    assert_eq!(strict(&dir, BRAVO_FIELD), (Some(0), json!("ready")));
    assert_eq!(strict(&dir, NEVER_FIELD), (Some(1), json!("needs-context")));
}

fn active_pack(dir: &Temp) -> Pack {
    storage::read_pack(Path::new(
        dir.saved()["active_pack"]["path"].as_str().unwrap(),
    ))
    .unwrap()
}
fn selectors(pack: &Pack, name: &str) -> Value {
    pack.data
        .sources
        .iter()
        .find(|source| source["id"] == id(name))
        .unwrap_or_else(|| panic!("{name} in {:?}", pack.data.sources))["selectors"]
        .clone()
}

#[test]
fn quiet_sources_accept_selector_replacement_in_update_imports() {
    let dir = Temp::new();
    confirm(&dir, quick(), &["alpha", "bravo"]);
    // B's record comes from an import, never from a discovery measurement.
    import(
        &dir,
        &pack(
            1,
            json!({"id":id("bravo"),"kind":"source","name":"Bravo",
                "inventory_status":"observed-only","selectors":{"repo":"bravo_repo","type":"bravo_type"},
                "provenance_ids":["prov:synthetic"]}),
        ),
        "append",
    );
    for window in [(1, 2), (2, 3)] {
        let run = finish(
            &dir,
            window,
            &[],
            &World::new(&[("alpha", 5), ("bravo", 0)]),
        );
        assert_eq!(run["quiet_source_ids"], json!([id("bravo")]));
    }

    // Each source's selector is replaced whole by one with an event family.
    let before = active_pack(&dir);
    let restricted = |name: &str| json!({"repo":format!("{name}_repo"),"type":format!("{name}_type"),"event_family":"Example"});
    let mut update = before.clone();
    for source in &mut update.data.sources {
        for name in ["alpha", "bravo"] {
            if source["id"] == id(name) {
                source["selectors"] = restricted(name);
            }
        }
    }
    update.content_digest = update.digest().unwrap();
    let reply = import(&dir, &update, "update");
    assert_eq!(reply["action"]["pack_updated"], true, "{reply}");
    let refused: Vec<_> = reply["action"]["not_applied"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["id"].clone(), item["reason"].clone()))
        .collect();
    assert_eq!(
        refused,
        [(
            json!(id("alpha")),
            json!("discovery-evidence-selector-kept")
        )],
        "{reply}"
    );
    let after = active_pack(&dir);
    assert_eq!(selectors(&after, "bravo"), restricted("bravo"));
    assert_eq!(selectors(&after, "alpha"), selectors(&before, "alpha"));
    assert_eq!(after.data.sources.len(), before.data.sources.len());
}
