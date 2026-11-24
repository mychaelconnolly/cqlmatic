//! Batch inventory requests: several local review and parser actions in one all-or-nothing
//! transaction.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const AT: &str = "2026-01-02T00:00:00Z";
const LOCAL_KINDS: [&str; 6] = [
    "bind-parser",
    "exclude-parser",
    "import-parsers",
    "parser-not-applicable",
    "reconcile",
    "review-source",
];

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-inventory-batch-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn environment() -> Value {
    json!({"cloud":"us1","tenant":"00000000000000000000000000000000","view":"search-all"})
}

fn send(dir: &Path, state: &Path, action: Value, flags: &[&str]) -> Output {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let request = dir.join(format!(
        "request-{}.json",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::write(
        &request,
        serde_json::to_vec(&json!({"schema_version":1,"action":action})).unwrap(),
    )
    .unwrap();
    Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["env", "inventory", "--json", "--state"])
        .arg(state)
        .arg("--request")
        .arg(&request)
        .args(flags)
        .output()
        .unwrap()
}

fn call(dir: &Path, state: &Path, action: Value) -> Value {
    let output = send(dir, state, action, &[]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn read(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn batch(actions: Vec<Value>) -> Value {
    json!({"kind":"batch","actions":actions})
}

fn review(source: &str, purpose: &str) -> Value {
    json!({"kind":"review-source","source_id":source,"purpose":purpose,
        "data_model":"Synthetic records","reviewed_at":AT,"evidence":"Synthetic review"})
}

fn bind(source: &str, parser: &(String, String)) -> Value {
    json!({"kind":"bind-parser","source_id":source,"parser_id":parser.0,"parser_sha256":parser.1,
        "reviewed_at":AT,"evidence":"Synthetic binding","semantics":[]})
}

fn not_applicable(source: &str) -> Value {
    json!({"kind":"parser-not-applicable","source_id":source,"reason":"platform-internal",
        "reviewed_at":AT,"evidence":"Synthetic internal source"})
}

fn exclude(parser: &(String, String)) -> Value {
    json!({"kind":"exclude-parser","parser_id":parser.0,"parser_sha256":parser.1,
        "reviewed_at":AT,"reason":"Synthetic exclusion"})
}

fn reconcile() -> Value {
    json!({"kind":"reconcile","reviewed_at":AT})
}

struct Fixture {
    dir: Temp,
    state: PathBuf,
    sources: Vec<String>,
    /// Exact parser IDs and snapshot digests; the last parser has a line that needs review.
    parsers: Vec<(String, String)>,
    import: Value,
}

fn source_ids(reply: &Value) -> Vec<String> {
    reply["status"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["source_id"].as_str().unwrap().to_owned())
        .collect()
}

fn parser_inputs(dir: &Path, count: usize) -> (Value, Vec<(String, String)>) {
    let mut details = Vec::new();
    let mut parsers = Vec::new();
    for index in 0..count {
        let id = format!("synthetic-parser-{index:02}");
        let script = if index + 1 == count {
            "items[] := split(raw, by=\",\")".to_owned()
        } else {
            format!("event.kind := \"event-{index:02}\"")
        };
        let file = dir.join(format!("{id}.json"));
        fs::write(
            &file,
            serde_json::to_vec(&json!({"resources":[{"id":id,"name":id,"script":script,
                "fields_to_tag":["event.kind"]}]}))
            .unwrap(),
        )
        .unwrap();
        parsers.push((id.clone(), cqlmatic::sha256_hex(&fs::read(&file).unwrap())));
        details.push(json!({"parser_id":id,"result_file":file}));
    }
    let page = dir.join("catalog.json");
    let entries: Vec<Value> = parsers
        .iter()
        .map(|(id, _)| json!({"ID":id,"Name":id}))
        .collect();
    fs::write(
        &page,
        serde_json::to_vec(&json!({"resources":entries,
            "meta":{"pagination":{"offset":0,"limit":100,"total":count}}}))
        .unwrap(),
    )
    .unwrap();
    (
        json!({"kind":"import-parsers","pages":[{"offset":0,"limit":100,"result_file":page}],
            "details":details,"retrieved_at":AT}),
        parsers,
    )
}

/// A confirmed Full inventory with `sources` selected sources and saved (not imported) parsers.
fn fixture(sources: usize, parsers: usize) -> Fixture {
    let dir = Temp::new();
    let state = dir.0.join("workflow/state.json");
    call(
        &dir.0,
        &state,
        json!({"kind":"set-discovery-mode","mode":"full"}),
    );
    let expected: Vec<Value> = (0..sources)
        .map(|i| json!({"repo":format!("synthetic-repo-{i:02}"),"type":format!("synthetic-type-{i:02}")}))
        .collect();
    let confirmed = call(
        &dir.0,
        &state,
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources",
            "context":"Synthetic batch target","expected_sources":expected}),
    );
    let (import, parsers) = parser_inputs(&dir.0, parsers);
    Fixture {
        sources: source_ids(&confirmed),
        dir,
        state,
        parsers,
        import,
    }
}

/// A Full inventory whose one source has a completed measurement run and pending field work
/// that only source review and a parser decision can complete.
fn measured_fixture() -> Fixture {
    let dir = Temp::new();
    let state = dir.0.join("workflow/state.json");
    let inventory = |action| call(&dir.0, &state, action);
    inventory(json!({"kind":"set-discovery-mode","mode":"full"}));
    let confirmed = inventory(
        json!({"kind":"confirm-target","environment":environment(),"scope":"selected-sources",
            "context":"Synthetic batch target","expected_sources":[{"type":"synthetic"}]}),
    );
    inventory(
        json!({"kind":"record-inventory-hints","environment":environment(),
        "hints":[{"source":{"type":"synthetic"},"fields":["user"]}]}),
    );
    let mut progress = inventory(
        json!({"kind":"prepare-discovery","start":"2026-01-01T00:00:00Z",
        "end":AT,"max_queries":40,"max_seconds":3600}),
    );
    let mut answers = 0;
    while progress["action"]["relay"]["status"] == "needs-probes" {
        let run = progress["action"]["run_index"].clone();
        for probe in progress["action"]["relay"]["probes"]
            .as_array()
            .unwrap()
            .clone()
        {
            let rows = match probe["kind"].as_str().unwrap() {
                "volume" => json!([{"events":10}]),
                "fieldset" => json!([{"field":"user"}]),
                "dataset-facet" => json!([]),
                "tag-detect" => json!([{"events":0}]),
                "population" => {
                    let mut row = json!({"total":10});
                    for index in 0..probe["fields"].as_array().unwrap().len() {
                        row[format!("c{index}")] = json!(10);
                    }
                    json!([row])
                }
                kind => panic!("unexpected probe {kind}"),
            };
            answers += 1;
            assert!(answers < 40, "relay did not finish");
            let file = dir.0.join(format!("answer-{answers}.json"));
            fs::write(&file, serde_json::to_vec(&json!({"result":rows})).unwrap()).unwrap();
            inventory(
                json!({"kind":"dispatch-discovery","run_index":run,"probe_id":probe["probe_id"]}),
            );
            progress = inventory(json!({"kind":"discovery-step","run_index":run,
                "answers":[{"probe_id":probe["probe_id"],"result_file":file}]}));
        }
    }
    assert_eq!(progress["action"]["relay"]["status"], "complete");
    assert_eq!(
        read(&state)["pending_inventory"][0]["resolution"],
        Value::Null
    );
    let (import, parsers) = parser_inputs(&dir.0, 3);
    Fixture {
        sources: source_ids(&confirmed),
        dir,
        state,
        parsers,
        import,
    }
}

fn outcomes(reply: &Value) -> Vec<String> {
    let results = reply["action"]["results"].as_array().unwrap();
    for (index, result) in results.iter().enumerate() {
        assert_eq!(result["index"], index, "{reply}");
    }
    results
        .iter()
        .map(|result| result["outcome"].as_str().unwrap().to_owned())
        .collect()
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|_| panic!("{output:?}"))
}

fn without_updated_at(mut state: Value) -> Value {
    state.as_object_mut().unwrap().remove("updated_at");
    state
}

fn status(state: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["status", "--json", "--state"])
        .arg(state)
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn inventory_schema_offers_a_batch_of_exactly_the_six_local_actions() {
    let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .args(["--schema", "inventory"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let schema: Value = serde_json::from_slice(&output.stdout).unwrap();
    let defs = &schema["$defs"];
    let variant = defs["InventoryAction"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|variant| variant["properties"]["kind"]["const"] == "batch")
        .expect("the request schema offers kind batch");
    let actions = &variant["properties"]["actions"];
    assert_eq!(actions["minItems"], 1);
    assert_eq!(actions["maxItems"], 32);
    let item = actions["items"]["$ref"]
        .as_str()
        .unwrap()
        .strip_prefix("#/$defs/")
        .unwrap();
    let mut kinds: Vec<&str> = defs[item]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|variant| variant["properties"]["kind"]["const"].as_str().unwrap())
        .collect();
    kinds.sort_unstable();
    assert_eq!(kinds, LOCAL_KINDS);
}

#[test]
fn malformed_batches_are_refused_before_any_item_is_applied() {
    let f = fixture(2, 0);
    // The well-formed control: the same kind of item in a batch is accepted.
    let accepted = call(
        &f.dir.0,
        &f.state,
        batch(vec![review(&f.sources[0], "Accepted")]),
    );
    assert_eq!(accepted["action"]["commit"], "committed");
    let before = fs::read(&f.state).unwrap();
    let mut unknown_field = review(&f.sources[1], "Unknown nested field");
    unknown_field["synthetic_extra"] = json!(true);
    let cases = [
        (
            batch(vec![reconcile(), review(&f.sources[1], "After reconcile")]),
            Some("Batch item 0 is reconcile"),
        ),
        (
            batch(vec![
                review(&f.sources[1], "First"),
                reconcile(),
                reconcile(),
            ]),
            Some("Batch item 1 is reconcile"),
        ),
        (
            batch(vec![
                json!({"kind":"import-pack","pack_file":"synthetic-pack.json"}),
            ]),
            None,
        ),
        (batch(vec![json!({"kind":"next-probe"})]), None),
        (batch(vec![batch(vec![reconcile()])]), None),
        (batch(vec![unknown_field]), None),
        (batch(vec![]), Some("A batch holds 1 to 32 actions")),
        (
            batch(
                (0..33)
                    .map(|i| review(&f.sources[0], &format!("Item {i}")))
                    .collect(),
            ),
            Some("A batch holds 1 to 32 actions"),
        ),
    ];
    for (action, message) in cases {
        let output = send(&f.dir.0, &f.state, action, &[]);
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(diagnostic["code"], "INVENTORY_INPUT", "{diagnostic}");
        if let Some(message) = message {
            assert!(
                diagnostic["message"].as_str().unwrap().contains(message),
                "{diagnostic}"
            );
        }
        assert_eq!(fs::read(&f.state).unwrap(), before);
    }
}

#[test]
fn a_failed_item_rolls_back_the_whole_batch() {
    let f = fixture(5, 2);
    call(&f.dir.0, &f.state, f.import.clone());
    let before = fs::read(&f.state).unwrap();
    let mut wrong_digest = bind(&f.sources[2], &f.parsers[0]);
    wrong_digest["parser_sha256"] = json!("0".repeat(64));
    let output = send(
        &f.dir.0,
        &f.state,
        batch(vec![
            review(&f.sources[0], "First"),
            review(&f.sources[1], "Second"),
            wrong_digest,
            review(&f.sources[3], "Fourth"),
            not_applicable(&f.sources[4]),
        ]),
        &[],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let reply = stdout_json(&output);
    assert_eq!(reply["action"]["kind"], "batch");
    assert_eq!(reply["action"]["commit"], "rolled-back");
    assert_eq!(
        outcomes(&reply),
        ["rolled-back", "rolled-back", "failed", "skipped", "skipped"]
    );
    let diagnostic = &reply["action"]["results"][2]["diagnostic"];
    assert_eq!(diagnostic["code"], "INVENTORY_EVIDENCE");
    assert!(
        diagnostic["message"]
            .as_str()
            .unwrap()
            .contains("exact saved parser"),
        "{diagnostic}"
    );
    assert_eq!(fs::read(&f.state).unwrap(), before);
    // The reply describes what is saved: nothing from the batch.
    assert_eq!(reply["state"], json!(f.state));
    for source in reply["status"]["sources"].as_array().unwrap() {
        assert!(source["review"].is_null(), "{source}");
        assert!(source["binding"].is_null(), "{source}");
    }
    let leftovers: Vec<_> = fs::read_dir(f.state.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");

    // Nothing was saved, so the corrected batch can be sent again whole.
    let corrected = call(
        &f.dir.0,
        &f.state,
        batch(vec![
            review(&f.sources[0], "First"),
            review(&f.sources[1], "Second"),
            bind(&f.sources[2], &f.parsers[0]),
            review(&f.sources[3], "Fourth"),
            not_applicable(&f.sources[4]),
        ]),
    );
    assert_eq!(corrected["action"]["commit"], "committed");
    assert_eq!(outcomes(&corrected), ["applied"; 5]);
}

#[cfg(unix)]
#[test]
fn a_write_failure_before_the_replacement_rolls_back_every_item() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture(2, 0);
    let before = fs::read(&f.state).unwrap();
    let workflow = f.state.parent().unwrap().to_path_buf();
    let mode = |mode| fs::set_permissions(&workflow, fs::Permissions::from_mode(mode)).unwrap();
    // The lock file exists, so only creating the new state file fails.
    mode(0o500);
    if fs::write(workflow.join("probe.tmp"), b"").is_ok() {
        mode(0o700);
        eprintln!("skipped: this account can write a directory without write permission");
        return;
    }
    let full = send(
        &f.dir.0,
        &f.state,
        batch(vec![
            review(&f.sources[0], "Not saved"),
            not_applicable(&f.sources[1]),
        ]),
        &[],
    );
    let brief = send(
        &f.dir.0,
        &f.state,
        batch(vec![review(&f.sources[0], "Not saved")]),
        &["--brief"],
    );
    mode(0o700);
    for output in [&full, &brief] {
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let reply = stdout_json(output);
        assert_eq!(reply["action"]["commit"], "rolled-back");
        assert!(
            outcomes(&reply)
                .iter()
                .all(|outcome| outcome == "rolled-back")
        );
        assert_eq!(reply["action"]["diagnostic"]["code"], "LOCAL_IO");
    }
    assert!(
        stdout_json(&full)["status"]["sources"]
            .as_array()
            .unwrap()
            .iter()
            .all(|source| source["review"].is_null())
    );
    assert_eq!(fs::read(&f.state).unwrap(), before);
}

#[test]
fn an_invalid_item_fails_at_its_own_index_even_when_a_later_item_replaces_it() {
    let f = fixture(1, 0);
    let before = fs::read(&f.state).unwrap();
    let mut invalid = review(&f.sources[0], "Invalid date");
    invalid["reviewed_at"] = json!("not a timestamp");
    let output = send(
        &f.dir.0,
        &f.state,
        batch(vec![invalid, review(&f.sources[0], "Valid replacement")]),
        &[],
    );
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let reply = stdout_json(&output);
    assert_eq!(reply["action"]["commit"], "rolled-back");
    assert_eq!(outcomes(&reply), ["failed", "skipped"]);
    assert_eq!(fs::read(&f.state).unwrap(), before);
}

#[test]
fn a_held_writer_refuses_the_batch_at_once() {
    let f = fixture(1, 0);
    let before = fs::read(&f.state).unwrap();
    let writer = cqlmatic::storage::Writer::acquire(&f.state).unwrap();
    let started = Instant::now();
    let output = send(
        &f.dir.0,
        &f.state,
        batch(vec![review(&f.sources[0], "Busy")]),
        &[],
    );
    let elapsed = started.elapsed();
    drop(writer);
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    let diagnostic: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(diagnostic["code"], "PACK_BUSY");
    assert!(elapsed < Duration::from_secs(5), "{elapsed:?}");
    assert_eq!(fs::read(&f.state).unwrap(), before);
}

#[test]
fn a_successful_batch_commits_every_item_and_a_replay_changes_nothing() {
    let f = fixture(3, 3);
    let actions = vec![
        f.import.clone(),
        review(&f.sources[0], "Reviewed in a batch"),
        bind(&f.sources[0], &f.parsers[0]),
        exclude(&f.parsers[1]),
        not_applicable(&f.sources[1]),
        reconcile(),
    ];
    let reply = call(&f.dir.0, &f.state, batch(actions.clone()));
    assert_eq!(reply["action"]["kind"], "batch");
    assert_eq!(reply["action"]["commit"], "committed");
    assert_eq!(outcomes(&reply), ["applied"; 6]);
    assert_eq!(reply["state"], json!(f.state));
    let saved = read(&f.state);
    let source = |id: &String| {
        saved["sources"]
            .as_array()
            .unwrap()
            .iter()
            .find(|source| source["id"] == json!(id))
            .unwrap()
            .clone()
    };
    assert_eq!(
        source(&f.sources[0])["review"]["purpose"],
        "Reviewed in a batch"
    );
    assert_eq!(
        source(&f.sources[0])["binding"]["parser_id"],
        json!(f.parsers[0].0)
    );
    assert_eq!(
        source(&f.sources[1])["binding"]["reason"],
        "platform-internal"
    );
    assert_eq!(
        saved["parser_exclusions"][0]["parser_id"],
        json!(f.parsers[1].0)
    );
    assert_eq!(saved["parsers"].as_array().unwrap().len(), 3);
    assert_eq!(saved["reconciled_at"], AT);
    assert_eq!(reply["status"]["reconciliation"], "done");

    let committed = fs::read(&f.state).unwrap();
    let replay = call(&f.dir.0, &f.state, batch(actions));
    assert_eq!(replay["action"]["commit"], "unchanged");
    assert_eq!(outcomes(&replay), ["applied"; 6]);
    assert_eq!(fs::read(&f.state).unwrap(), committed);
}

#[test]
fn brief_batch_replies_with_32_items_stay_within_4096_bytes() {
    let f = fixture(12, 0);
    let marker = "synthetic-input-marker";
    let reviews: Vec<Value> = (0..32)
        .map(|i| review(&f.sources[i % 12], &format!("{marker} {i}")))
        .collect();
    let brief = |action, code| {
        let output = send(&f.dir.0, &f.state, action, &["--brief"]);
        assert_eq!(output.status.code(), Some(code), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        assert!(output.stdout.len() <= 4096, "{}", output.stdout.len());
        assert_eq!(output.stdout.last(), Some(&b'\n'));
        let text = String::from_utf8(output.stdout.clone()).unwrap();
        assert!(!text.contains(marker), "{text}");
        assert!(!text.contains("env:repo:"), "{text}");
        stdout_json(&output)
    };

    let mut failing = reviews.clone();
    failing[31]["source_id"] = json!("env:repo:0000000000000000");
    let refused = brief(batch(failing), 2);
    let action = &refused["action"];
    assert_eq!(action["kind"], "batch");
    assert_eq!(action["commit"], "rolled-back");
    let mut expected = vec!["rolled-back"; 31];
    expected.push("failed");
    assert_eq!(outcomes(&refused), expected);
    assert_eq!(action["counts"], json!({"rolled-back":31,"failed":1}));
    assert_eq!(action["diagnostic"]["index"], 31);
    assert_eq!(action["diagnostic"]["code"], "INVENTORY_EVIDENCE");
    assert!(action["diagnostic"]["message"].is_string());

    let applied = brief(batch(reviews.clone()), 0);
    assert_eq!(applied["action"]["commit"], "committed");
    assert_eq!(outcomes(&applied), ["applied"; 32]);
    assert_eq!(applied["action"]["counts"], json!({"applied":32}));
    assert!(applied["action"]["diagnostic"].is_null());

    let summary = send(&f.dir.0, &f.state, batch(reviews), &["--summary"]);
    assert!(summary.status.success(), "{summary:?}");
    let summary = stdout_json(&summary);
    assert_eq!(summary["summary"], true);
    assert_eq!(summary["action"]["commit"], "unchanged");
    assert_eq!(outcomes(&summary), ["applied"; 32]);
}

#[cfg(unix)]
#[test]
fn unconfirmed_durability_after_the_replacement_is_reported_as_visible() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture(2, 0);
    let workflow = f.state.parent().unwrap().to_path_buf();
    let lock = |mode| fs::set_permissions(&workflow, fs::Permissions::from_mode(mode)).unwrap();
    // Without read access the directory cannot be opened to sync it after the rename.
    lock(0o300);
    if fs::read_dir(&workflow).is_ok() {
        lock(0o700);
        eprintln!("skipped: this account can read a directory without read permission");
        return;
    }
    let full = send(
        &f.dir.0,
        &f.state,
        batch(vec![
            review(&f.sources[0], "Visible"),
            not_applicable(&f.sources[1]),
        ]),
        &[],
    );
    let brief = send(
        &f.dir.0,
        &f.state,
        batch(vec![review(&f.sources[1], "Visible in brief")]),
        &["--brief"],
    );
    lock(0o700);
    for output in [&full, &brief] {
        assert_eq!(output.status.code(), Some(2), "{output:?}");
        assert!(output.stderr.is_empty(), "{output:?}");
        let reply = stdout_json(output);
        assert_eq!(reply["action"]["commit"], "visible-durability-unconfirmed");
        assert!(outcomes(&reply).iter().all(|outcome| outcome == "applied"));
        assert_eq!(reply["action"]["diagnostic"]["code"], "PACK_DURABILITY");
    }
    let saved = read(&f.state);
    let purposes: Vec<_> = saved["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|source| source["review"]["purpose"].clone())
        .collect();
    assert!(purposes.contains(&json!("Visible")), "{purposes:?}");
    assert!(
        purposes.contains(&json!("Visible in brief")),
        "{purposes:?}"
    );
}

#[test]
fn a_mixed_batch_leaves_the_same_state_and_status_as_single_actions() {
    let f = measured_fixture();
    let source = &f.sources[0];
    let actions = vec![
        f.import.clone(),
        review(source, "Synthetic measured source"),
        exclude(&f.parsers[0]),
        exclude(&f.parsers[1]),
        not_applicable(source),
        reconcile(),
    ];
    let batched = f.state.with_file_name("batched.json");
    fs::copy(&f.state, &batched).unwrap();
    for action in actions.clone() {
        call(&f.dir.0, &f.state, action);
    }
    let reply = call(&f.dir.0, &batched, batch(actions));
    assert_eq!(reply["action"]["commit"], "committed");
    let singly = read(&f.state);
    assert_eq!(
        singly["pending_inventory"][0]["resolution"]["kind"],
        "inventoried"
    );
    assert_eq!(
        without_updated_at(read(&batched)),
        without_updated_at(singly)
    );
    assert_eq!(status(&batched), status(&f.state));
}

#[test]
fn pending_work_is_judged_against_the_final_result_of_the_batch() {
    let f = measured_fixture();
    let source = &f.sources[0];
    // Not applicable completes the source; the later binding to a parser with an unreviewed
    // line reopens it. Sent singly, the work is completed in between and stays completed.
    let actions = vec![
        f.import.clone(),
        review(source, "Synthetic measured source"),
        not_applicable(source),
        bind(source, &f.parsers[2]),
    ];
    let batched = f.state.with_file_name("batched.json");
    fs::copy(&f.state, &batched).unwrap();
    for action in actions.clone() {
        call(&f.dir.0, &f.state, action);
    }
    call(&f.dir.0, &batched, batch(actions));
    assert_eq!(
        read(&f.state)["pending_inventory"][0]["resolution"]["kind"],
        "inventoried"
    );
    assert_eq!(
        read(&batched)["pending_inventory"][0]["resolution"],
        Value::Null
    );
    assert_eq!(status(&batched)["sources"][0]["status"], "review");
}
