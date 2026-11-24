use cqlmatic::{
    contracts::*,
    discovery::{self, Capture, DiscoverReport, DiscoverRequest, Probe},
    environment::Pack,
    inventory::InventoryState,
    mcp::{self, Transport},
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
const SAMPLE_START: &str = "2026-01-01T00:45:00Z";
const END: &str = "2026-01-01T01:00:00Z";
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
            "cqlmatic-tag-batches-{}-{}",
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

/// A single-source request over [START, END] unless `extra` overrides any key.
fn request(extra: Value) -> DiscoverRequest {
    let mut value = json!({
        "schema_version":1,
        "environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":VIEW},
        "start":START,"end":END,
        "sources":[{"repo":"synthetic_repo"}],
        "max_queries":200,"max_seconds":3600
    });
    for (key, field) in extra.as_object().unwrap() {
        value[key] = field.clone();
    }
    serde_json::from_value(value).unwrap()
}
fn quick(extra: Value) -> DiscoverRequest {
    let mut extra = extra;
    extra["max_population_fields_per_source"] = json!(20);
    request(extra)
}

/// Synthetic answers. Counts and listings are kept per window: the sample window (the final 15
/// minutes) and the full request window, which holds the sample window.
#[derive(Clone, Default)]
struct World {
    sample: Vec<String>,
    full: Vec<String>,
    total_a: u64,
    total_w: u64,
    a: BTreeMap<String, u64>,
    w: BTreeMap<String, u64>,
    datasets: Vec<Value>,
    facets: BTreeMap<String, Vec<Value>>,
    rare: Vec<String>,
    bad_tags: Option<Vec<Value>>,
}
impl World {
    fn new(sample: &[&str]) -> Self {
        Self {
            sample: strings(sample),
            total_a: 100,
            total_w: 1000,
            ..Self::default()
        }
    }
    fn a(mut self, counts: &[(&str, u64)]) -> Self {
        self.a.extend(
            counts
                .iter()
                .map(|(field, count)| (field.to_string(), *count)),
        );
        self
    }
    fn w(mut self, counts: &[(&str, u64)]) -> Self {
        self.w.extend(
            counts
                .iter()
                .map(|(field, count)| (field.to_string(), *count)),
        );
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
    fn rows(&self, r: &DiscoverRequest, p: &Probe) -> Vec<Value> {
        let short = r.start == p.start && r.start != START;
        let full = p.start == r.start && !short;
        let (total, counts) = if full || short {
            (self.total_w, &self.w)
        } else {
            (self.total_a, &self.a)
        };
        let listing = |fields: &[String]| {
            fields
                .iter()
                .map(|field| json!({"field":field}))
                .collect::<Vec<_>>()
        };
        match p.kind.as_str() {
            "volume" => vec![json!({"events":self.total_w})],
            "dataset-facet" => self.datasets.clone(),
            "fieldset" if full => listing(&self.full),
            "fieldset" => listing(&self.sample),
            "tag-batch" if self.bad_tags.is_some() => self.bad_tags.clone().unwrap(),
            "tag-batch" | "population" => {
                let mut row = json!({"total":total});
                for (index, field) in p.fields.iter().enumerate() {
                    row[format!("c{index}")] = json!(counts.get(field).copied().unwrap_or(0));
                }
                vec![row]
            }
            "record-kind-facet" => self
                .facets
                .get(p.field.as_deref().unwrap())
                .cloned()
                .unwrap_or_default(),
            "kind-fieldset" => listing(&self.rare),
            "value-facet" => {
                let field = p.field.as_deref().unwrap();
                counts
                    .get(field)
                    .filter(|count| **count > 0)
                    .map(|count| vec![json!({field:"synthetic","events":count})])
                    .unwrap_or_default()
            }
            other => panic!("unexpected probe {other}"),
        }
    }
    /// Every probe the relay frontier emits, answered in emission order.
    fn relay(&self, r: &DiscoverRequest) -> (Vec<Probe>, BTreeMap<String, Vec<Value>>) {
        let mut answers = BTreeMap::new();
        let mut probes = Vec::new();
        loop {
            let pending = discovery::frontier(r, &answers).unwrap();
            if pending.is_empty() {
                return (probes, answers);
            }
            assert!(probes.len() < 200);
            for probe in pending {
                answers.insert(discovery::probe_id(VIEW, &probe), self.rows(r, &probe));
                probes.push(probe);
            }
        }
    }
}

struct Backend {
    answers: BTreeMap<String, Vec<Value>>,
    emitted: Vec<String>,
    retrievals: BTreeMap<String, String>,
}
impl SearchBackend for Backend {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        _: Instant,
    ) -> Result<String, Diagnostic> {
        let id = discovery::probe_id_for(view, query, start, end);
        assert!(self.answers.contains_key(&id), "unexpected search {query}");
        self.emitted.push(id.clone());
        Ok(id)
    }
    fn poll(&mut self, _: &str, job: &str, _: Instant) -> Result<Value, Diagnostic> {
        Ok(json!({"done":true,"cancelled":false,"metaData":{},"events":self.answers[job]}))
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        Ok(())
    }
    fn observed_at(&self, job: &str) -> Option<String> {
        Some(
            self.retrievals
                .get(job)
                .cloned()
                .unwrap_or_else(|| "2026-02-01T00:00:00Z".into()),
        )
    }
}

/// Native execution over the relay's answers: the probes the relay emitted and the native
/// search order, with the pack and report.
fn native(
    r: &DiscoverRequest,
    world: &World,
    retrieval: impl Fn(&Probe) -> Option<&'static str>,
) -> (Vec<Probe>, Vec<String>, Option<Pack>, DiscoverReport) {
    let (probes, answers) = world.relay(r);
    let retrievals = probes
        .iter()
        .filter_map(|probe| {
            retrieval(probe).map(|date| (discovery::probe_id(VIEW, probe), date.to_string()))
        })
        .collect();
    let mut backend = Backend {
        answers,
        emitted: vec![],
        retrievals,
    };
    let (pack, report) = discovery::execute(r, &mut backend);
    (probes, backend.emitted, pack, report)
}
fn completed(r: &DiscoverRequest, world: &World) -> (Vec<Probe>, Pack, DiscoverReport) {
    let (probes, emitted, pack, report) = native(r, world, |_| None);
    assert_eq!(report.status, "complete", "{:?}", report.diagnostics);
    let ids = probes
        .iter()
        .map(|probe| discovery::probe_id(VIEW, probe))
        .collect::<Vec<_>>();
    assert_eq!(emitted, ids, "native order differs from the relay");
    (probes, pack.unwrap(), report)
}
fn kinds(probes: &[Probe]) -> Vec<&str> {
    probes.iter().map(|probe| probe.kind.as_str()).collect()
}
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
fn measured(pack: &Pack) -> BTreeSet<String> {
    pack.data.sources[0]["evidence"]["measured_fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|field| field.as_str().unwrap().to_string())
        .collect()
}
/// The capture that a pack record's first provenance ID names.
fn proof<'a>(report: &'a DiscoverReport, record: &Value) -> &'a Capture {
    let id = record["provenance_ids"][0].as_str().unwrap();
    let index: usize = id.rsplit(':').next().unwrap().parse().unwrap();
    &report.captures[index]
}
/// A measured field's proof counts that exact field over the field's own window.
fn assert_genuine(report: &DiscoverReport, pack: &Pack, name: &str) {
    let record = field(pack, name);
    let capture = proof(report, record);
    assert!(
        matches!(capture.probe.kind.as_str(), "population" | "tag-batch"),
        "{name}: {:?}",
        capture.probe
    );
    assert!(
        capture.probe.fields.iter().any(|field| field == name),
        "{name}"
    );
    assert_eq!(
        record["observation_window"],
        format!("{}/{}", capture.probe.start, capture.probe.end),
        "{name}"
    );
}
/// Each namespaced field is counted at most once per concrete window across all count searches.
fn assert_no_repeated_columns(probes: &[Probe]) {
    let mut seen = BTreeSet::new();
    for probe in probes
        .iter()
        .filter(|probe| matches!(probe.kind.as_str(), "population" | "tag-batch"))
    {
        for field in &probe.fields {
            assert!(
                seen.insert((field.clone(), probe.start.clone(), probe.end.clone())),
                "{field} counted twice over {}",
                probe.start
            );
        }
    }
}

/// A Quick run whose recent listing names eight tags: three kind facets,
/// four rare kinds and four rare fields over the full window.
fn quick_rare_kinds() -> (DiscoverRequest, World) {
    let listed = [
        "#Cps.version",
        "#Vendor",
        "#ecs.version",
        "#event.dataset",
        "#event.kind",
        "#event.module",
        "#event.outcome",
        "#type",
    ];
    let mut sample = listed.to_vec();
    sample.extend(["source", "user"]);
    let a = listed
        .iter()
        .enumerate()
        .map(|(index, field)| (*field, 10 * (index as u64 + 1)))
        .chain([("source", 90), ("user", 100)])
        .collect::<Vec<_>>();
    let mut world = World::new(&sample)
        .a(&a)
        .w(&[
            ("rare_16", 5),
            ("rare_17", 6),
            ("rare_18", 7),
            ("rare_19", 8),
        ])
        .facet(
            "#type",
            &[
                ("type-0", 4000),
                ("type-1", 1000),
                ("type-2", 2000),
                ("type-3", 3000),
            ],
        )
        .facet("#event.kind", &[("synthetic", 1000)])
        .facet("#event.module", &[("synthetic", 1000)]);
    world.datasets = vec![json!({"#event.dataset":"synthetic","events":1000})];
    world.rare = strings(&["rare_16", "rare_17", "rare_18", "rare_19"]);
    (quick(json!({"max_queries":80})), world)
}
/// Full discovery over a listing of eight tags and seventeen ordinary fields.
fn full_tagged_listing() -> (DiscoverRequest, World) {
    let (_, quick) = quick_rare_kinds();
    let mut sample = quick.sample[..8].to_vec();
    sample.extend((1..=17).map(|index| format!("p{index:02}")));
    let mut world = quick;
    world.sample = sample;
    world.rare.clear();
    for index in 1..=17 {
        world.a.insert(format!("p{index:02}"), 50);
    }
    (request(json!({"max_queries":80})), world)
}

fn tag_row(total: u64, count: impl Fn(usize) -> Value) -> Vec<Value> {
    let mut row = json!({"total":total});
    for index in 0..TAGS.len() {
        row[format!("c{index}")] = count(index);
    }
    vec![row]
}

/// A standalone relay run: answer every emitted search from `world` until it settles.
struct Relay {
    dir: Temp,
    state: PathBuf,
    pack: PathBuf,
}
impl Relay {
    fn new(r: DiscoverRequest) -> Self {
        let dir = Temp::new();
        let (state, pack, receipt) = (
            dir.path("state.json"),
            dir.path("pack.json"),
            dir.path("receipt.json"),
        );
        mcp::init_resumable(r, &state, &pack, &receipt, Transport::Mcp).unwrap();
        Self { dir, state, pack }
    }
    fn next(&self) -> mcp::StepOutput {
        mcp::step_resumable(&self.state, &[]).unwrap()
    }
    fn answer(&self, probe: &Value, rows: Vec<Value>) -> mcp::StepOutput {
        let id = probe["probe_id"].as_str().unwrap();
        mcp::dispatch(&self.state, id).unwrap();
        let file = self.dir.path(&format!("{id}-{}.json", self.attempts()));
        write(&file, &json!({"result":rows}));
        mcp::step_resumable(&self.state, &[(id.into(), file)]).unwrap()
    }
    fn attempts(&self) -> usize {
        read(&self.state)["session"]["attempts"]
            .as_array()
            .unwrap()
            .len()
    }
    /// Answer searches until the run settles or `stop` matches the next search.
    fn drive(
        &self,
        r: &DiscoverRequest,
        world: &World,
        stop: impl Fn(&Probe) -> bool,
    ) -> mcp::StepOutput {
        let mut output = self.next();
        while output.status == "needs-probes" {
            let wire = output.probes[0].clone();
            let probe = wire_probe(&wire);
            if stop(&probe) {
                return output;
            }
            output = self.answer(&wire, world.rows(r, &probe));
        }
        output
    }
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
fn strict_check(dir: &Temp, pack: &Path, query: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
        .env("HOME", &dir.0)
        .args(["check", query, "--pack"])
        .arg(pack)
        .args(["--require-observed", "--json"])
        .output()
        .unwrap()
}
fn assert_grounded(dir: &Temp, pack: &Path, query: &str) {
    let output = strict_check(dir, pack, query);
    let result: Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stderr)));
    assert_eq!(output.status.code(), Some(0), "{query}: {result}");
    assert_eq!(result["outcome"], "ready", "{query}: {result}");
}

#[test]
fn tag_stage_order() {
    // Quick: base, then one tag batch, then the kind facets and rare listings it gates, then
    // population. Relay and native execution issue the same searches in the same order.
    let (r, world) = quick_rare_kinds();
    let (probes, _, _) = completed(&r, &world);
    assert_eq!(
        kinds(&probes),
        [
            "volume",
            "dataset-facet",
            "fieldset",
            "tag-batch",
            "record-kind-facet",
            "record-kind-facet",
            "record-kind-facet",
            "kind-fieldset",
            "kind-fieldset",
            "kind-fieldset",
            "kind-fieldset",
            "population",
            "population"
        ]
    );
    assert_eq!(probes[3].fields, TAGS);
    assert_eq!(probes[3].start, SAMPLE_START);
    assert!(probes[3].query.ends_with(
        "stats([count(as=total), count(field=\"#event_simpleName\", as=c0), count(field=\"#event.dataset\", as=c1), count(field=\"#event.module\", as=c2), count(field=\"#event.outcome\", as=c3), count(field=\"#ecs.version\", as=c4), count(field=\"#Cps.version\", as=c5), count(field=\"#Vendor\", as=c6), count(field=\"#type\", as=c7), count(field=\"#event.kind\", as=c8)])"
    ));
    assert_no_repeated_columns(&probes);

    // Full: the tag batch answers alone, before population, so no tag column repeats.
    let (r, world) = full_tagged_listing();
    let mut answers = BTreeMap::new();
    let mut stages = Vec::new();
    loop {
        let pending = discovery::frontier(&r, &answers).unwrap();
        if pending.is_empty() {
            break;
        }
        stages.push(kinds(&pending).join(","));
        for probe in pending {
            answers.insert(discovery::probe_id(VIEW, &probe), world.rows(&r, &probe));
        }
    }
    assert_eq!(
        stages,
        [
            "volume",
            "dataset-facet,fieldset",
            "tag-batch",
            "population,record-kind-facet,record-kind-facet,record-kind-facet"
        ]
    );
    let (probes, _, _) = completed(&r, &world);
    assert_eq!(probes.len(), 8);
    assert_no_repeated_columns(&probes);
    assert!(
        probes
            .iter()
            .filter(|probe| probe.kind == "population")
            .flat_map(|probe| &probe.fields)
            .all(|field| !field.starts_with('#'))
    );

    // Twenty-one tag candidates: two chunks, each numbering its aliases from c0, both before
    // any dependent search.
    let tagged = (0..12)
        .map(|index| format!("#kind.k{index:02}"))
        .collect::<Vec<_>>();
    let r = request(json!({"kind_fields":tagged,"max_queries":80}));
    let (probes, _, _) = completed(&r, &World::new(&["user"]).a(&[("user", 100)]));
    let batches = probes
        .iter()
        .filter(|probe| probe.kind == "tag-batch")
        .collect::<Vec<_>>();
    let mut expected = strings(&TAGS);
    expected.extend(tagged.iter().cloned());
    assert_eq!(batches.len(), 2);
    assert_eq!(batches[0].fields, expected[..20]);
    assert_eq!(batches[1].fields, expected[20..]);
    assert!(
        batches[1]
            .query
            .ends_with("count(field=\"#kind.k11\", as=c0)])")
    );
    assert_eq!(kinds(&probes[3..5]), ["tag-batch", "tag-batch"]);
    assert_no_repeated_columns(&probes);

    // Malformed batch answers are refused at intake: native runs stop without a pack, and a
    // relay attempt pauses charged, without acceptance or activation; the retry is another attempt.
    let small = request(json!({"max_queries":10}));
    let small_world = World::new(&["user"]).a(&[("user", 10)]);
    let mut missing = tag_row(10, |_| json!(0));
    missing[0].as_object_mut().unwrap().remove("c3");
    for (label, rows) in [
        ("zero total", tag_row(0, |_| json!(0))),
        ("missing alias", missing),
        (
            "two rows",
            [tag_row(10, |_| json!(0)), tag_row(10, |_| json!(0))].concat(),
        ),
        (
            "non-integer",
            tag_row(10, |index| if index == 0 { json!("1.5") } else { json!(0) }),
        ),
        (
            "negative",
            tag_row(10, |index| if index == 0 { json!(-1) } else { json!(0) }),
        ),
        (
            "above total",
            tag_row(10, |index| if index == 0 { json!(11) } else { json!(0) }),
        ),
    ] {
        let mut bad = small_world.clone();
        bad.bad_tags = Some(rows.clone());
        let (_, answers) = {
            let mut answers = BTreeMap::new();
            let mut probes = Vec::new();
            for _ in 0..3 {
                for probe in discovery::frontier(&small, &answers).unwrap() {
                    answers.insert(discovery::probe_id(VIEW, &probe), bad.rows(&small, &probe));
                    probes.push(probe);
                }
            }
            (probes, answers)
        };
        let mut backend = Backend {
            answers,
            emitted: vec![],
            retrievals: BTreeMap::new(),
        };
        let (pack, report) = discovery::execute(&small, &mut backend);
        assert!(pack.is_none(), "{label}");
        assert_eq!(report.diagnostics[0].code, "DISCOVERY_EVIDENCE", "{label}");
        assert_eq!(backend.emitted.len(), 4, "{label}");

        let relay = Relay::new(small.clone());
        let output = relay.drive(&small, &small_world, |probe| probe.kind == "tag-batch");
        let batch = output.probes[0].clone();
        let refused = relay.answer(&batch, rows);
        assert_eq!(refused.status, "paused", "{label}");
        assert_eq!(refused.cost.searches.used, 4, "{label}");
        let saved = read(&relay.state);
        assert_eq!(
            saved["session"]["attempts"][3]["outcome"], "completed-unusable",
            "{label}"
        );
        let batch_id = batch["probe_id"].as_str().unwrap();
        assert!(
            !saved["session"]["captures"]
                .as_object()
                .unwrap()
                .contains_key(batch_id),
            "{label}"
        );
        assert!(!relay.pack.exists(), "{label}");
        let resumed = mcp::resume(&relay.state).unwrap();
        assert_eq!(resumed.probes[0]["probe_id"], batch["probe_id"], "{label}");
        let retried = relay.answer(&batch, small_world.rows(&small, &wire_probe(&batch)));
        assert_eq!(retried.cost.searches.used, 5, "{label}");
        // Replay applies the same check to the saved batch answer: the same edit with a count
        // within its total replays, an above-total count fails the run without activation.
        if label == "above total" {
            let original = fs::read(&relay.state).unwrap();
            for (count, readable) in [(100, true), (101, false)] {
                let mut saved: Value = serde_json::from_slice(&original).unwrap();
                let mut rows = small_world.rows(&small, &wire_probe(&batch));
                rows[0]["c0"] = json!(count);
                let raw = serde_json::to_vec(&json!({"result":rows})).unwrap();
                let hex = raw
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                let digest = sha256_hex(&raw);
                saved["answers"][batch_id]["raw_hex"] = json!(hex);
                saved["answers"][batch_id]["raw_sha256"] = json!(digest);
                for attempt in saved["session"]["attempts"].as_array_mut().unwrap() {
                    if attempt["outcome"] == "completed" && attempt["probe"]["kind"] == "tag-batch"
                    {
                        attempt["terminal"]["raw_hex"] = json!(hex);
                        attempt["terminal"]["raw_sha256"] = json!(digest);
                    }
                }
                write(&relay.state, &saved);
                let replayed = relay.next();
                if readable {
                    assert_eq!(
                        replayed.status, "needs-probes",
                        "{:?}",
                        replayed.diagnostics
                    );
                } else {
                    assert_eq!(replayed.status, "failed");
                    assert_eq!(replayed.diagnostics[0].code, "DISCOVERY_EVIDENCE");
                    assert_eq!(
                        replayed.diagnostics[0].message,
                        "Field count exceeds its matching denominator."
                    );
                    assert!(!relay.pack.exists());
                }
            }
            fs::write(&relay.state, original).unwrap();
        }
        let done = relay.drive(&small, &small_world, |_| false);
        assert_eq!(done.status, "complete", "{label}: {:?}", done.diagnostics);
    }
}

#[test]
fn cap_and_tags() {
    // Twenty plain fields and a plain Vendor in the recent listing, the nine tag candidates, an
    // unlisted requested tagged kind with events and an unlisted one without.
    let mut sample = (0..20)
        .map(|index| format!("f{index:02}"))
        .collect::<Vec<_>>();
    sample.push("Vendor".into());
    let mut world = World {
        sample,
        total_a: 100,
        total_w: 1000,
        ..World::default()
    }
    .a(&[("#Vendor", 7), ("Vendor", 30), ("#requested.kind", 12)])
    .w(&[("#requested.kind", 120), ("rare.one", 4)])
    .facet("#requested.kind", &[("alpha", 5), ("beta", 7)]);
    world.rare = strings(&["rare.one"]);
    for index in 0..20 {
        world.a.insert(format!("f{index:02}"), 40);
    }
    let r = quick(json!({
        "kind_fields":["#requested.kind","#zero.kind"],
        "value_fields":["#requested.kind"],
        "max_queries":80
    }));
    let (probes, pack, report) = completed(&r, &world);
    let batch = probes
        .iter()
        .find(|probe| probe.kind == "tag-batch")
        .unwrap();
    let mut columns = strings(&TAGS);
    columns.extend(strings(&["#requested.kind", "#zero.kind"]));
    assert_eq!(batch.fields, columns);

    // Eligibility and the discriminator come from the batch's positive unlisted candidate.
    let facets = probes
        .iter()
        .filter(|probe| probe.kind == "record-kind-facet")
        .map(|probe| probe.field.as_deref().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(facets, ["#requested.kind"]);
    let rare = probes
        .iter()
        .filter(|probe| probe.kind == "kind-fieldset")
        .collect::<Vec<_>>();
    assert_eq!(rare.len(), 2);
    assert!(
        rare.iter()
            .all(|probe| probe.query.contains("#requested.kind"))
    );

    // The cap holds the logical selection: the observed requested kind, the new rare field, then
    // the recent listing in order. Auxiliary tag columns take no slot.
    let mut selected = BTreeSet::from([
        "#requested.kind".to_string(),
        "rare.one".to_string(),
        "Vendor".to_string(),
    ]);
    selected.extend((0..17).map(|index| format!("f{index:02}")));
    assert_eq!(measured(&pack), selected);
    let counted = probes
        .iter()
        .filter(|probe| probe.kind == "population")
        .flat_map(|probe| probe.fields.iter().cloned())
        .collect::<BTreeSet<_>>();
    assert_eq!(counted, selected);
    assert_no_repeated_columns(&probes);

    // Presence, rate, window and proof keep each namespace apart.
    let tagged_vendor = field(&pack, "#Vendor");
    assert_eq!(tagged_vendor["coverage"]["observed"], true);
    assert!(tagged_vendor["coverage"]["rate"].is_null());
    assert_eq!(
        tagged_vendor["observation_window"],
        format!("{SAMPLE_START}/{END}")
    );
    assert_eq!(proof(&report, tagged_vendor).probe.kind, "tag-batch");
    let vendor = field(&pack, "Vendor");
    assert_eq!(vendor["coverage"]["rate"], 0.3);
    assert_eq!(proof(&report, vendor).probe.kind, "population");
    assert_ne!(vendor["id"], tagged_vendor["id"]);
    let zero = field(&pack, "#zero.kind");
    assert_eq!(zero["coverage"]["observed"], false);
    assert!(zero["coverage"]["rate"].is_null());
    assert_eq!(zero["observation_window"], format!("{SAMPLE_START}/{END}"));
    let requested = field(&pack, "#requested.kind");
    assert_eq!(requested["coverage"]["observed"], true);
    assert_eq!(requested["coverage"]["rate"], 0.12);
    assert_eq!(requested["observation_window"], format!("{START}/{END}"));
    assert_genuine(&report, &pack, "#requested.kind");
    for name in ["f17", "f18", "f19"] {
        assert!(
            field(&pack, name)["coverage"]["observed"].is_null(),
            "{name}"
        );
    }
    // The observed requested kind also gets its value sample.
    let id = requested["id"].clone();
    assert_eq!(
        pack.data
            .values
            .iter()
            .filter(|value| value["field_id"] == id)
            .count(),
        1
    );

    // Observed requested kinds beyond the cap are refused once the batch shows them, before
    // any population search.
    let mut over = world.clone();
    over.a.insert("#other.kind".into(), 3);
    let mut r = r.clone();
    r.kind_fields.push("#other.kind".into());
    r.max_population_fields_per_source = Some(1);
    let mut answers = BTreeMap::new();
    let error = loop {
        match discovery::frontier(&r, &answers) {
            Ok(pending) => {
                assert!(pending.iter().all(|probe| probe.kind != "population"));
                for probe in pending {
                    answers.insert(discovery::probe_id(VIEW, &probe), over.rows(&r, &probe));
                }
            }
            Err(error) => break error,
        }
    };
    assert_eq!(
        error.message,
        "Population cap cannot include all observed requested kind/value fields."
    );
}

#[test]
fn residual_windows() {
    // The Quick run takes 13 searches, where separate tag checks took 21, with the same logical
    // fields, counts and genuine proofs.
    let (r, world) = quick_rare_kinds();
    let (probes, pack, report) = completed(&r, &world);
    assert_eq!(probes.len(), 13);
    let mut logical = strings(&[
        "#Cps.version",
        "#Vendor",
        "#ecs.version",
        "#event.dataset",
        "#event.kind",
        "#event.module",
        "#event.outcome",
        "#type",
        "source",
        "user",
        "rare_16",
        "rare_17",
        "rare_18",
        "rare_19",
    ]);
    assert_eq!(measured(&pack), logical.iter().cloned().collect());
    for name in &logical {
        let (count, total) = match world.a.get(name) {
            Some(count) => (*count, 100),
            None => (world.w[name], 1000),
        };
        assert_eq!(
            field(&pack, name)["coverage"]["rate"],
            count as f64 / total as f64,
            "{name}"
        );
        assert_genuine(&report, &pack, name);
    }
    let residual = probes
        .iter()
        .filter(|probe| probe.kind == "population")
        .map(|probe| (probe.start.as_str(), probe.fields.clone()))
        .collect::<Vec<_>>();
    assert_eq!(
        residual,
        [
            (SAMPLE_START, strings(&["source", "user"])),
            (
                START,
                strings(&["rare_16", "rare_17", "rare_18", "rare_19"])
            )
        ]
    );
    // The unselected zero tag stays an auxiliary observation without a rate.
    let simple = field(&pack, "#event_simpleName");
    assert_eq!(simple["coverage"]["observed"], false);
    assert!(simple["coverage"]["rate"].is_null());

    // Full over 25 listed fields: one tag batch and one residual batch of the seventeen
    // ordinary fields.
    let (r, world) = full_tagged_listing();
    let (probes, pack, report) = completed(&r, &world);
    assert_eq!(probes.iter().filter(|p| p.kind == "tag-batch").count(), 1);
    let residual = probes
        .iter()
        .filter(|probe| probe.kind == "population")
        .collect::<Vec<_>>();
    assert_eq!(residual.len(), 1);
    assert_eq!(residual[0].fields.len(), 17);
    assert_eq!(measured(&pack).len(), 25);
    logical.truncate(8);
    for name in &logical {
        assert_genuine(&report, &pack, name);
    }

    // W = A: equal windows share one residual search, and a tag the batch counted over that
    // same window is reused even though its normal window is the full one.
    let r = request(
        json!({"start":SAMPLE_START,"population_fields":[{"source":{"repo":"synthetic_repo"},"fields":["user","#Vendor","extra"]}]}),
    );
    let world = World::new(&["user"]).w(&[("user", 900), ("#Vendor", 300), ("extra", 10)]);
    let (probes, pack, report) = completed(&r, &world);
    assert_eq!(
        kinds(&probes),
        [
            "volume",
            "dataset-facet",
            "fieldset",
            "tag-batch",
            "population"
        ]
    );
    assert_eq!(probes[4].fields, strings(&["user", "extra"]));
    assert_eq!(field(&pack, "#Vendor")["coverage"]["rate"], 0.3);
    assert_genuine(&report, &pack, "#Vendor");
    assert_eq!(
        proof(&report, field(&pack, "#Vendor")).probe.kind,
        "tag-batch"
    );

    // Sparse fallback: the batch and every residual column use the full window.
    let mut world = World::new(&[])
        .w(&[("user", 900), ("#type", 1000)])
        .facet("#type", &[("only", 1000)]);
    world.full = strings(&["#type", "user"]);
    let r = request(json!({}));
    let (probes, pack, report) = completed(&r, &world);
    let batch = probes.iter().find(|p| p.kind == "tag-batch").unwrap();
    assert_eq!(batch.start, START);
    let residual = probes
        .iter()
        .filter(|probe| probe.kind == "population")
        .collect::<Vec<_>>();
    assert_eq!(residual.len(), 1);
    assert_eq!(residual[0].fields, strings(&["user"]));
    assert_eq!(residual[0].start, START);
    assert_eq!(field(&pack, "#type")["coverage"]["rate"], 1.0);
    assert_genuine(&report, &pack, "#type");

    // W > 15 minutes, absent from the recent window but present over the full one. The active
    // pack keeps the full-window measurement and strict checks ground the field.
    let window = format!("{START}/{END}");
    // (a) An explicitly selected tag the recent listing did not name.
    let r = request(
        json!({"population_fields":[{"source":{"repo":"synthetic_repo"},"fields":["user","#event.module"]}]}),
    );
    let world = World::new(&["user"])
        .a(&[("user", 100)])
        .w(&[("user", 1000), ("#event.module", 40)]);
    let run = Relay::new(r.clone());
    assert_eq!(run.drive(&r, &world, |_| false).status, "complete");
    let active = storage::read_pack(&run.pack).unwrap();
    let module = field(&active, "#event.module");
    assert_eq!(module["coverage"]["observed"], true);
    assert_eq!(module["coverage"]["rate"], 0.04);
    assert_eq!(module["observation_window"], window);
    let (_, _, _, report) = native(&r, &world, |_| None);
    let capture = proof(&report, module);
    assert_eq!(capture.probe.kind, "population");
    assert_eq!(capture.probe.start, START);
    assert_grounded(
        &run.dir,
        &run.pack,
        "#repo=synthetic_repo #event.module=* | head(1)",
    );
    // A recent count of the same selected tag stays auxiliary: the selected whole-window
    // population count keeps the rate, window and proof.
    let world = world
        .a(&[("#event.module", 5)])
        .facet("#event.module", &[("synthetic", 40)]);
    let (_, pack, report) = completed(&r, &world);
    let module = field(&pack, "#event.module");
    assert_eq!(module["coverage"]["rate"], 0.04);
    assert_eq!(module["observation_window"], window);
    assert_eq!(proof(&report, module).probe.kind, "population");

    // (b) A Quick rare-kind field, #event_simpleName, found only by a full-window listing.
    let r = quick(json!({}));
    let mut world = World::new(&["#type", "user"])
        .a(&[("#type", 100), ("user", 100)])
        .w(&[("#event_simpleName", 30)])
        .facet("#type", &[("t1", 10), ("t2", 20)]);
    world.rare = strings(&["#event_simpleName"]);
    let run = Relay::new(r.clone());
    assert_eq!(run.drive(&r, &world, |_| false).status, "complete");
    let active = storage::read_pack(&run.pack).unwrap();
    let simple = field(&active, "#event_simpleName");
    assert!(measured(&active).contains("#event_simpleName"));
    assert_eq!(simple["coverage"]["observed"], true);
    assert_eq!(simple["coverage"]["rate"], 0.03);
    assert_eq!(simple["observation_window"], window);
    let (_, _, _, report) = native(&r, &world, |_| None);
    assert_eq!(proof(&report, simple).probe.kind, "population");
    assert_eq!(proof(&report, simple).probe.start, START);
    assert_grounded(
        &run.dir,
        &run.pack,
        "#repo=synthetic_repo #event_simpleName=* | groupBy([#type])",
    );

    // (c) #event.dataset, seen by the full-window dataset facet and not in the recent window.
    let r = request(json!({}));
    let mut world = World::new(&["user"]).a(&[("user", 100)]);
    world.datasets = vec![json!({"#event.dataset":"synthetic","events":500})];
    let run = Relay::new(r.clone());
    assert_eq!(run.drive(&r, &world, |_| false).status, "complete");
    let active = storage::read_pack(&run.pack).unwrap();
    let dataset = field(&active, "#event.dataset");
    assert_eq!(dataset["coverage"]["observed"], true);
    assert_eq!(dataset["observation_window"], window);
    let (_, _, _, report) = native(&r, &world, |_| None);
    assert_eq!(proof(&report, dataset).probe.kind, "dataset-facet");
    assert_grounded(
        &run.dir,
        &run.pack,
        "#repo=synthetic_repo #event.dataset=synthetic | head(1)",
    );
}

#[test]
fn rate_and_work_boundary() {
    // W = A, repo-only: the tag batch counts #type in every event and its singleton kind facet,
    // retrieved later over the same window, agrees. The batch rate and proof are kept.
    let r = request(json!({"start":SAMPLE_START}));
    let base = World::new(&["#type", "user"]).w(&[("#type", 1000), ("user", 500)]);
    let singleton = base.clone().facet("#type", &[("only", 1000)]);
    let later =
        |probe: &Probe| (probe.kind == "record-kind-facet").then_some("2026-02-02T00:00:00Z");
    for (world, retrieval, rate, kind) in [
        (&singleton, Some(()), Some(1.0), "tag-batch"),
        (&singleton, None, Some(1.0), "tag-batch"),
        (&base, Some(()), None, "record-kind-facet"),
    ] {
        let (_, _, pack, report) = native(&r, world, |probe| retrieval.and_then(|_| later(probe)));
        let pack = pack.unwrap_or_else(|| panic!("{:?}", report.diagnostics));
        let record = field(&pack, "#type");
        assert_eq!(record["coverage"]["rate"], json!(rate), "{kind}");
        assert_eq!(record["coverage"]["observed"], rate.is_some(), "{kind}");
        assert_eq!(proof(&report, record).probe.kind, kind);
    }
    // A contradiction retrieved at the same instant is refused.
    let (_, _, pack, report) = native(&r, &base, |_| None);
    assert!(pack.is_none());
    assert!(
        report.diagnostics[0]
            .message
            .contains("Tied field measurements")
    );

    // Inventory work: a #Vendor hint queued before the tag batch is answered resolves with the
    // batch's probe ID; one queued after it does not.
    let world = World {
        sample: strings(&["Vendor", "user"]),
        total_a: 10,
        total_w: 100,
        ..World::default()
    }
    .a(&[("#Vendor", 5), ("Vendor", 10), ("user", 10)]);
    let mut batch_ids = Vec::new();
    for before in [true, false] {
        let dir = Temp::new();
        let inventory = Inventory::new(&dir);
        let mut progress = inventory.prepare();
        let mut hinted = false;
        let mut paused = false;
        while progress["action"]["relay"]["status"] == "needs-probes" {
            let probes = progress["action"]["relay"]["probes"]
                .as_array()
                .unwrap()
                .clone();
            for wire in probes {
                let probe = wire_probe(&wire);
                if probe.kind == "tag-batch" && before && !hinted {
                    inventory.hint();
                    hinted = true;
                }
                progress = inventory.answer(&wire, world.rows(&inventory.request(), &probe));
                if probe.kind == "tag-batch" {
                    batch_ids.push(wire["probe_id"].as_str().unwrap().to_string());
                    if !before {
                        inventory.hint();
                    } else if !paused {
                        // Reopen the run across a pause after the batch was accepted.
                        inventory.call(json!({"kind":"pause-discovery"}));
                        inventory.status();
                        progress = inventory.call(json!({"kind":"resume-discovery"}));
                        paused = true;
                        break;
                    }
                }
            }
        }
        assert_eq!(
            progress["action"]["relay"]["status"], "complete",
            "{progress}"
        );
        let saved = read(&inventory.state());
        let resolution = &saved["pending_inventory"][0]["resolution"];
        if before {
            assert_eq!(resolution["kind"], "inventoried", "{saved}");
            assert_eq!(
                resolution["evidence"][0]["fields"]["#Vendor"], batch_ids[0],
                "{resolution}"
            );
        } else {
            assert!(resolution.is_null(), "{resolution}");
        }

        if before {
            // The checkpoint reuses the existing probe fields; only the role is new.
            let checkpoint =
                PathBuf::from(saved["discovery_runs"][0]["state_path"].as_str().unwrap());
            let relay = read(&checkpoint);
            let attempt = relay["session"]["attempts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|attempt| attempt["probe"]["kind"] == "tag-batch")
                .unwrap();
            assert_eq!(attempt["probe"]["fields"], json!(TAGS));
            assert!(attempt["probe"]["field"].is_null());
            let keys = attempt["probe"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>();
            assert_eq!(
                keys,
                ["end", "field", "fields", "kind", "query", "source", "start"]
            );
            // State and pack round-trip byte for byte through this build's types.
            let bytes = fs::read(inventory.state()).unwrap();
            let parsed: InventoryState = serde_json::from_slice(&bytes).unwrap();
            let mut again = serde_json::to_vec_pretty(&parsed).unwrap();
            again.push(b'\n');
            assert_eq!(again, bytes);
            let pack_path = PathBuf::from(saved["active_pack"]["path"].as_str().unwrap());
            let pack_bytes = fs::read(&pack_path).unwrap();
            let pack = storage::read_pack(&pack_path).unwrap();
            assert_eq!(
                format!("{}\n", serde_json::to_string_pretty(&pack).unwrap()).into_bytes(),
                pack_bytes
            );
            let inspected = mcp::inspect_state(&checkpoint).unwrap();
            assert_eq!(inspected.status, "activated");

            // Earlier-plan evidence: reopened work still resolves from the same capture, and the
            // cost keeps the earlier plan's separate tag searches without running its graph.
            let v3 = mcp::cost(&inspected, 0).searches;
            let mut reopened = saved.clone();
            reopened["pending_inventory"][0]
                .as_object_mut()
                .unwrap()
                .remove("resolution");
            write(&inventory.state(), &reopened);
            as_earlier_plan(&checkpoint);
            let checkpoint_bytes = fs::read(&checkpoint).unwrap();
            let earlier = mcp::cost(&mcp::inspect_state(&checkpoint).unwrap(), 0).searches;
            assert_eq!((v3.expected_low, v3.expected_high), (Some(5), Some(22)));
            assert_eq!(
                (earlier.expected_low, earlier.expected_high),
                (Some(13), Some(30))
            );
            inventory.call(json!({"kind":"reconcile","reviewed_at":"2026-01-03T00:00:00Z"}));
            let resolved = read(&inventory.state());
            assert_eq!(
                resolved["pending_inventory"][0]["resolution"]["evidence"][0]["fields"]["#Vendor"],
                batch_ids[0]
            );
            assert_eq!(fs::read(&checkpoint).unwrap(), checkpoint_bytes);
        }
    }
}

/// An inventory with one confirmed Quick source and a run over [START, START + 1 day].
struct Inventory<'a> {
    dir: &'a Temp,
}
impl<'a> Inventory<'a> {
    fn new(dir: &'a Temp) -> Self {
        let inventory = Self { dir };
        inventory.call(
            json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-and-fields"}),
        );
        inventory.call(json!({"kind":"confirm-target","environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":VIEW},"scope":"selected-sources","context":"Synthetic tag batch target","expected_sources":[{"type":"synthetic"}]}));
        inventory
    }
    fn state(&self) -> PathBuf {
        self.dir.0.join("workflow").join("state.json")
    }
    fn call(&self, action: Value) -> Value {
        let file = self.dir.path("request.json");
        write(&file, &json!({"schema_version":1,"action":action}));
        let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .env("HOME", &self.dir.0)
            .args(["env", "inventory", "--json", "--state"])
            .arg(self.state())
            .arg("--request")
            .arg(file)
            .output()
            .unwrap();
        assert!(
            output.status.success() || output.status.code() == Some(3),
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn status(&self) -> Value {
        let output = Command::new(env!("CARGO_BIN_EXE_cqlmatic"))
            .env("HOME", &self.dir.0)
            .args(["status", "--json", "--state"])
            .arg(self.state())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn prepare(&self) -> Value {
        self.call(json!({"kind":"prepare-discovery","start":START,"end":"2026-01-02T00:00:00Z","max_queries":80,"max_seconds":600}))
    }
    fn request(&self) -> DiscoverRequest {
        let saved = read(&self.state());
        let checkpoint = read(Path::new(
            saved["discovery_runs"][0]["state_path"].as_str().unwrap(),
        ));
        serde_json::from_value(checkpoint["request"].clone()).unwrap()
    }
    fn hint(&self) {
        self.call(json!({"kind":"record-inventory-hints","environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":VIEW},"hints":[{"source":{"type":"synthetic"},"fields":["#Vendor"]}]}));
    }
    fn answer(&self, wire: &Value, rows: Vec<Value>) -> Value {
        let id = wire["probe_id"].clone();
        self.call(json!({"kind":"dispatch-discovery","probe_id":id}));
        let file = self.dir.path(&format!("{}.json", id.as_str().unwrap()));
        write(&file, &json!({"result":rows}));
        self.call(json!({"kind":"discovery-step","answers":[{"probe_id":id,"result_file":file}]}))
    }
}

/// Rewrite a completed checkpoint as an earlier build wrote it: the earlier plan label and the
/// binding a reader recomputes for it.
fn as_earlier_plan(path: &Path) {
    let mut saved = read(path);
    saved["discovery_algorithm"] = json!("bounded-discovery-v2");
    let session = &saved["session"];
    let mut bytes = serde_json::to_vec(&(
        &saved["request_sha256"],
        &saved["created_at"],
        &saved["pack"],
        &saved["receipt"],
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
    saved["session"]["binding_sha256"] = json!(sha256_hex(&bytes));
    write(path, &saved);
    mcp::inspect_state(path).unwrap();
}

#[test]
fn intervening_recent_capture_does_not_hide_equal_time_contradiction() {
    // Full discovery over W = 00:00..01:00 with an intervening positive A tag-batch between
    // a positive W dataset-facet and a contradictory W measurement at the exact same retrieval time.
    let explicit_zero = World {
        datasets: vec![json!({"#event.dataset":"synthetic","events":50})],
        ..World::new(&["user"])
            .a(&[("#event.dataset", 1)])
            .w(&[("user", 100), ("#event.dataset", 0)])
    };
    let r = request(json!({
        "population_fields":[{"source":{"repo":"synthetic_repo"},"fields":["user","#event.dataset"]}]
    }));
    let (_, _, pack, report) = native(&r, &explicit_zero, |_| Some("2026-02-01T00:00:00Z"));
    assert_eq!(report.status, "error", "{:?}", report.diagnostics);
    assert!(pack.is_none());
    assert!(
        report.diagnostics[0]
            .message
            .contains("Tied field measurements"),
        "{:?}",
        report.diagnostics
    );

    // Same pattern when an empty W record-kind-facet contradicts the positive W dataset-facet.
    let empty_kind = World {
        datasets: vec![json!({"#event.dataset":"synthetic","events":50})],
        ..World::new(&["user"])
            .a(&[("#event.dataset", 1)])
            .w(&[("user", 100)])
    };
    let r_kind = request(json!({"kind_fields":["#event.dataset"]}));
    let (_, _, pack_kind, report_kind) =
        native(&r_kind, &empty_kind, |_| Some("2026-02-01T00:00:00Z"));
    assert_eq!(report_kind.status, "error", "{:?}", report_kind.diagnostics);
    assert!(pack_kind.is_none());
    assert!(
        report_kind.diagnostics[0]
            .message
            .contains("Tied field measurements"),
        "{:?}",
        report_kind.diagnostics
    );
}

#[test]
fn two_runs_apply_within_run_precedence_separately() {
    // Two v3 runs: Run 0 has positive W=00:00..01:00 dataset-facet + zero A tag-batch;
    // Run 1 has empty W=00:45..01:00 dataset-facet + zero tag-batch + `later` field.
    // The stored work proof for `#event.dataset` must cite Run 1's tag-batch (matching
    // the active pack's observed=false), not Run 0's positive dataset-facet.
    let dir = Temp::new();
    let inventory = Inventory::new(&dir);
    inventory.call(json!({
        "kind":"record-inventory-hints",
        "environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":VIEW},
        "hints":[{"source":{"type":"synthetic"},"fields":["#event.dataset","later"]}]
    }));
    let saved = read(&inventory.state());
    let sid = saved["sources"][0]["id"].as_str().unwrap().to_string();
    inventory.call(json!({
        "kind":"review-source","source_id":sid,"purpose":"Synthetic",
        "data_model":"Synthetic","reviewed_at":"2026-01-02T02:00:00Z","evidence":"Synthetic"
    }));
    inventory.call(json!({
        "kind":"parser-not-applicable","source_id":sid,"reason":"falcon-sensor-native",
        "reviewed_at":"2026-01-02T02:00:00Z","evidence":"Synthetic"
    }));

    let drive_run = |start: &str, end: &str, world: &World, run_index: usize| -> (Value, String) {
        let mut step = inventory.call(json!({
            "kind":"prepare-discovery","start":start,"end":end,"max_queries":40,"max_seconds":600
        }));
        let state = read(&inventory.state());
        let checkpoint = read(Path::new(
            state["discovery_runs"][run_index]["state_path"]
                .as_str()
                .unwrap(),
        ));
        let req: DiscoverRequest = serde_json::from_value(checkpoint["request"].clone()).unwrap();
        let mut batch_id = String::new();
        while step["action"]["relay"]["status"] == "needs-probes" {
            let wire = step["action"]["relay"]["probes"][0].clone();
            if wire["kind"] == "tag-batch" {
                batch_id = wire["probe_id"].as_str().unwrap().to_string();
            }
            let rows = world.rows(&req, &wire_probe(&wire));
            let id = wire["probe_id"].clone();
            inventory
                .call(json!({"kind":"dispatch-discovery","run_index":run_index,"probe_id":id}));
            let file = dir.path(&format!("run{run_index}-{}.json", id.as_str().unwrap()));
            write(&file, &json!({"result":rows}));
            step = inventory.call(json!({
                "kind":"discovery-step","run_index":run_index,
                "answers":[{"probe_id":id,"result_file":file}]
            }));
        }
        assert_eq!(step["action"]["relay"]["status"], "complete", "{step}");
        (step, batch_id)
    };

    let run0_world = World {
        total_a: 100,
        total_w: 100,
        datasets: vec![json!({"#event.dataset":"synthetic","events":50})],
        ..World::new(&["user"]).a(&[("user", 20)]).w(&[("user", 20)])
    };
    drive_run(
        "2026-01-02T00:00:00Z",
        "2026-01-02T01:00:00Z",
        &run0_world,
        0,
    );
    assert!(read(&inventory.state())["pending_inventory"][0]["resolution"].is_null());

    let run1_world = World {
        total_a: 100,
        total_w: 100,
        ..World::new(&["later"])
            .a(&[("later", 20)])
            .w(&[("later", 20)])
    };
    let (done, run1_batch_id) = drive_run(
        "2026-01-02T00:45:00Z",
        "2026-01-02T01:00:00Z",
        &run1_world,
        1,
    );
    let active_pack = PathBuf::from(done["status"]["active_pack"]["path"].as_str().unwrap());
    let pack: Pack = serde_json::from_slice(&fs::read(&active_pack).unwrap()).unwrap();
    assert_eq!(
        field(&pack, "#event.dataset")["coverage"]["observed"],
        false
    );
    let check = strict_check(
        &dir,
        &active_pack,
        r##"#type="synthetic" #event.dataset=* | head(1)"##,
    );
    assert_eq!(check.status.code(), Some(1));

    let resolved = read(&inventory.state());
    let evidence = resolved["pending_inventory"][0]["resolution"]["evidence"]
        .as_array()
        .unwrap();
    let dataset_proof = evidence
        .iter()
        .find_map(|entry| {
            entry["fields"]
                .get("#event.dataset")
                .and_then(Value::as_str)
        })
        .expect("resolved #event.dataset proof");
    assert_eq!(dataset_proof, run1_batch_id);
}

#[test]
fn post_batch_hint_keeps_population_proof_over_later_compatible_facet() {
    // Full discovery on a 10-minute window with plain `event.kind` measured by population (20/100)
    // and then a singleton record-kind-facet (`event.kind=e`). Queueing the hint right after the
    // tag-batch (before population) must still apply current-plan precedence and keep population
    // as the resolved field proof.
    let dir = Temp::new();
    let inventory = Inventory::new(&dir);
    inventory.call(json!({"kind":"set-discovery-mode","mode":"full"}));
    let saved = read(&inventory.state());
    let sid = saved["sources"][0]["id"].as_str().unwrap().to_string();
    inventory.call(json!({
        "kind":"review-source","source_id":sid,"purpose":"Synthetic",
        "data_model":"Synthetic","reviewed_at":"2026-01-02T02:00:00Z","evidence":"Synthetic"
    }));
    inventory.call(json!({
        "kind":"parser-not-applicable","source_id":sid,"reason":"falcon-sensor-native",
        "reviewed_at":"2026-01-02T02:00:00Z","evidence":"Synthetic"
    }));

    let mut step = inventory.call(json!({
        "kind":"prepare-discovery",
        "start":"2026-01-02T00:00:00Z",
        "end":"2026-01-02T00:10:00Z",
        "max_queries":40,
        "max_seconds":600
    }));
    let req = inventory.request();
    let world = World {
        total_a: 100,
        total_w: 100,
        ..World::new(&["event.kind"])
            .a(&[("event.kind", 20)])
            .w(&[("event.kind", 20)])
            .facet("event.kind", &[("e", 20)])
    };
    let mut population_id = String::new();
    let mut hinted = false;
    while step["action"]["relay"]["status"] == "needs-probes" {
        let probes = step["action"]["relay"]["probes"]
            .as_array()
            .unwrap()
            .clone();
        // Answer one probe per step so population runs before record-kind-facet and the hint
        // arrives strictly after tag-batch and before population.
        let wire = probes
            .iter()
            .find(|p| p["kind"] == "population")
            .or_else(|| probes.first())
            .unwrap()
            .clone();
        let kind = wire["kind"].as_str().unwrap().to_string();
        let probe_id = wire["probe_id"].as_str().unwrap().to_string();
        if kind == "population" {
            population_id = probe_id.clone();
        }
        let rows = world.rows(&req, &wire_probe(&wire));
        step = inventory.answer(&wire, rows);
        if kind == "tag-batch" && !hinted {
            hinted = true;
            inventory.call(json!({
                "kind":"record-inventory-hints",
                "environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":VIEW},
                "hints":[{"source":{"type":"synthetic"},"fields":["event.kind"]}]
            }));
        }
    }
    assert_eq!(step["action"]["relay"]["status"], "complete", "{step}");
    assert!(!population_id.is_empty());

    let resolved = read(&inventory.state());
    let evidence = resolved["pending_inventory"][0]["resolution"]["evidence"]
        .as_array()
        .unwrap();
    assert_eq!(
        evidence[0]["fields"]["event.kind"], population_id,
        "post-batch hint should keep population proof: {evidence:?}"
    );
}
