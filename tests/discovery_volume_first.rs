use cqlmatic::{
    contracts::Diagnostic,
    discovery::{self, DiscoverRequest, Probe},
    mcp::{self, Transport},
    network::{self, SearchBackend},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

// Execution reads the process-wide cancellation flag, which the native pause test sets.
static SIGNAL: Mutex<()> = Mutex::new(());

const BASE: [&str; 3] = ["volume", "dataset-facet", "fieldset"];

fn request(sources: &[&str], end: &str, quick: bool) -> DiscoverRequest {
    let mut value = json!({"schema_version":1,
        "environment":{"cloud":"us1","tenant":"00000000000000000000000000000000","view":"synthetic_view"},
        "start":"2026-01-01T00:00:00Z","end":end,
        "sources":sources.iter().map(|source| json!({"type":source})).collect::<Vec<_>>(),
        "max_queries":200,"max_seconds":600});
    if quick {
        value["max_population_fields_per_source"] = json!(20);
    }
    serde_json::from_value(value).unwrap()
}

fn sources_only(sources: &[&str]) -> DiscoverRequest {
    let mut r = request(sources, "2026-01-01T01:00:00Z", false);
    r.depth = discovery::DiscoveryDepth::SourcesOnly;
    r
}

/// Synthetic answers keyed by source type: volume, recent listing and full-window listing.
#[derive(Clone, Default)]
struct World {
    volume: BTreeMap<String, u64>,
    recent: BTreeMap<String, Vec<String>>,
    full: BTreeMap<String, Vec<String>>,
    census: Vec<Value>,
}
impl World {
    fn live(sources: &[&str]) -> Self {
        let mut world = Self::default();
        for source in sources {
            world.volume.insert(source.to_string(), 10);
            world
                .recent
                .insert(source.to_string(), vec!["alpha".into(), "beta".into()]);
            world
                .full
                .insert(source.to_string(), vec!["alpha".into(), "beta".into()]);
        }
        world
    }
    fn rows(&self, r: &DiscoverRequest, probe: &Probe) -> Vec<Value> {
        let source = source_of(probe);
        let listing = |fields: &[String]| {
            fields
                .iter()
                .map(|field| json!({"field":field}))
                .collect::<Vec<_>>()
        };
        match probe.kind.as_str() {
            "census" => self.census.clone(),
            "volume" => vec![json!({"events":self.volume.get(&source).copied().unwrap_or(10)})],
            "fieldset" if probe.start == r.start && window_minutes(r) > 15 => {
                listing(self.full.get(&source).map_or(&[][..], Vec::as_slice))
            }
            "fieldset" => listing(self.recent.get(&source).map_or(&[][..], Vec::as_slice)),
            "dataset-facet" | "record-kind-facet" | "kind-fieldset" | "value-facet" => vec![],
            "tag-detect" => vec![json!({"events":0})],
            "population" => {
                let mut row = json!({"total":10});
                for index in 0..probe.fields.len() {
                    row[format!("c{index}")] = json!(10);
                }
                vec![row]
            }
            other => panic!("Unexpected probe {other}"),
        }
    }
}

fn source_of(probe: &Probe) -> String {
    serde_json::to_value(&probe.source).unwrap()["type"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn window_minutes(r: &DiscoverRequest) -> i64 {
    let parse = |t: &str| {
        time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339).unwrap()
    };
    (parse(&r.end) - parse(&r.start)).whole_minutes()
}

/// Relay one search at a time, always taking the first emitted probe, as `next-probe` does.
fn relay(r: &DiscoverRequest, world: &World) -> (Vec<Probe>, Option<Diagnostic>) {
    let mut answers = BTreeMap::new();
    let mut order = Vec::new();
    loop {
        let pending = match discovery::frontier(r, &answers) {
            Ok(pending) => pending,
            Err(diagnostic) => return (order, Some(diagnostic)),
        };
        // A batch never mixes the census, volumes, listings and dependent measurements.
        assert!(
            pending
                .windows(2)
                .all(|pair| stage(&pair[0]) == stage(&pair[1])),
            "{:?}",
            kinds(&pending)
        );
        let Some(probe) = pending.into_iter().next() else {
            return (order, None);
        };
        assert!(order.len() < 500, "Relay did not finish");
        answers.insert(
            discovery::probe_id(&r.environment.view, &probe),
            world.rows(r, &probe),
        );
        order.push(probe);
    }
}

struct Recorder {
    request: DiscoverRequest,
    world: World,
    probes: BTreeMap<String, Probe>,
    started: Vec<String>,
    stops: usize,
    cancel_after: Option<usize>,
}
impl Recorder {
    fn new(request: &DiscoverRequest, world: &World) -> Self {
        Self {
            request: request.clone(),
            world: world.clone(),
            probes: BTreeMap::new(),
            started: vec![],
            stops: 0,
            cancel_after: None,
        }
    }
    fn learn(&mut self, probes: &[Probe]) {
        for probe in probes {
            self.probes.insert(
                discovery::probe_id(&self.request.environment.view, probe),
                probe.clone(),
            );
        }
    }
}
impl SearchBackend for Recorder {
    fn start(
        &mut self,
        view: &str,
        query: &str,
        start: &str,
        end: &str,
        _: Instant,
    ) -> Result<String, Diagnostic> {
        let id = discovery::probe_id_for(view, query, start, end);
        self.started.push(id.clone());
        Ok(format!("job-{}-{id}", self.started.len()))
    }
    fn poll(&mut self, _: &str, job: &str, _: Instant) -> Result<Value, Diagnostic> {
        let id = job
            .rsplit('-')
            .next()
            .map(|hex| format!("p-{hex}"))
            .unwrap();
        let probe = self.probes.get(&id).expect("probe was planned").clone();
        let rows = self.world.rows(&self.request, &probe);
        Ok(json!({"done":true,"cancelled":false,"metaData":{},"events":rows}))
    }
    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        self.stops += 1;
        if self.cancel_after == Some(self.stops) {
            network::CANCELLED.store(true, Ordering::Relaxed);
        }
        Ok(())
    }
}

/// Every probe the frontier can reach, so the recorder can answer whatever execution starts.
fn reachable(r: &DiscoverRequest, world: &World) -> Vec<Probe> {
    let mut answers = BTreeMap::new();
    let mut all = Vec::new();
    loop {
        let Ok(pending) = discovery::frontier(r, &answers) else {
            return all;
        };
        if pending.is_empty() {
            return all;
        }
        for probe in pending {
            answers.insert(
                discovery::probe_id(&r.environment.view, &probe),
                world.rows(r, &probe),
            );
            all.push(probe);
        }
    }
}

fn execute(r: &DiscoverRequest, world: &World) -> (Vec<Probe>, bool) {
    let mut backend = Recorder::new(r, world);
    backend.learn(&reachable(r, world));
    let (pack, report) = discovery::execute(r, &mut backend);
    (
        report
            .captures
            .iter()
            .map(|capture| capture.probe.clone())
            .collect(),
        pack.is_some(),
    )
}

fn ids(r: &DiscoverRequest, probes: &[Probe]) -> Vec<String> {
    probes
        .iter()
        .map(|probe| discovery::probe_id(&r.environment.view, probe))
        .collect()
}

fn assert_stage_order(probes: &[Probe], sources: &[&str]) {
    let volumes = &probes[..sources.len()];
    assert!(
        volumes.iter().all(|probe| probe.kind == "volume"),
        "{:?}",
        kinds(probes)
    );
    assert_eq!(
        volumes.iter().map(source_of).collect::<Vec<_>>(),
        sources.to_vec()
    );
    let first_dependent = probes
        .iter()
        .position(|probe| !BASE.contains(&probe.kind.as_str()))
        .expect("a field run has dependent probes");
    assert!(
        probes[first_dependent..]
            .iter()
            .all(|probe| !BASE.contains(&probe.kind.as_str())),
        "Every base listing precedes every tag, facet and population: {:?}",
        kinds(probes)
    );
    for source in sources {
        assert!(
            probes[..first_dependent]
                .iter()
                .any(|probe| probe.kind == "fieldset" && source_of(probe) == *source),
            "{source} is listed before dependent work: {:?}",
            kinds(probes)
        );
    }
}

fn stage(probe: &Probe) -> u8 {
    match probe.kind.as_str() {
        "census" => 0,
        "volume" => 1,
        "dataset-facet" | "fieldset" => 2,
        _ => 3,
    }
}

fn kinds(probes: &[Probe]) -> Vec<String> {
    probes
        .iter()
        .map(|probe| format!("{}:{}", source_of(probe), probe.kind))
        .collect()
}

#[test]
fn second_source_volume_is_the_second_search_and_base_listings_precede_dependent_work() {
    let _lock = SIGNAL.lock().unwrap();
    let sources = ["one", "two"];
    for quick in [false, true] {
        let r = request(&sources, "2026-01-01T01:00:00Z", quick);
        let world = World::live(&sources);
        let (relayed, failure) = relay(&r, &world);
        assert!(failure.is_none(), "{failure:?}");
        assert_eq!(relayed[1].kind, "volume");
        assert_eq!(source_of(&relayed[1]), "two");
        assert_stage_order(&relayed, &sources);
        let (executed, activated) = execute(&r, &world);
        assert!(activated, "quick={quick}");
        assert_stage_order(&executed, &sources);
        assert_eq!(ids(&r, &executed), ids(&r, &relayed), "quick={quick}");
    }
}

#[test]
fn sparse_fallback_listing_stays_in_the_base_pass() {
    let _lock = SIGNAL.lock().unwrap();
    let sources = ["one", "two"];
    let r = request(&sources, "2026-01-01T01:00:00Z", false);
    let mut world = World::live(&sources);
    world.recent.insert("one".into(), vec![]);
    let (relayed, failure) = relay(&r, &world);
    assert!(failure.is_none(), "{failure:?}");
    assert_stage_order(&relayed, &sources);
    let fallback = relayed
        .iter()
        .position(|probe| probe.kind == "fieldset" && probe.start == r.start)
        .expect("source one takes the full-window listing");
    assert_eq!(source_of(&relayed[fallback]), "one");
    assert!(fallback < 6, "{:?}", kinds(&relayed));
    let (executed, activated) = execute(&r, &world);
    assert!(activated);
    assert_eq!(ids(&r, &executed), ids(&r, &relayed));
}

#[test]
fn an_empty_second_listing_fails_after_three_searches_per_source() {
    let _lock = SIGNAL.lock().unwrap();
    let sources = ["one", "two"];
    // A 15 minute window has no wider listing; a longer window spends one fallback search.
    for (end, searches) in [("2026-01-01T00:15:00Z", 6), ("2026-01-01T01:00:00Z", 7)] {
        for quick in [false, true] {
            let r = request(&sources, end, quick);
            let mut world = World::live(&sources);
            world.recent.insert("two".into(), vec![]);
            world.full.insert("two".into(), vec![]);
            let (relayed, failure) = relay(&r, &world);
            let failure = failure.expect("empty listing fails the run");
            assert_eq!(
                failure.message,
                "Quiet or empty measurement windows cannot replace an existing schema."
            );
            assert_eq!(relayed.len(), searches, "{end} quick={quick}");
            assert!(
                relayed
                    .iter()
                    .all(|probe| BASE.contains(&probe.kind.as_str())),
                "{:?}",
                kinds(&relayed)
            );
            assert_eq!(source_of(relayed.last().unwrap()), "two");
            assert_eq!(relayed.last().unwrap().kind, "fieldset");
            let (executed, activated) = execute(&r, &world);
            assert!(!activated);
            assert_eq!(ids(&r, &executed), ids(&r, &relayed), "{end} quick={quick}");
        }
    }
}

#[test]
fn a_zero_first_source_fails_at_the_first_search_in_every_depth() {
    let _lock = SIGNAL.lock().unwrap();
    let sources = ["one", "two"];
    for r in [
        sources_only(&sources),
        request(&sources, "2026-01-01T01:00:00Z", false),
        request(&sources, "2026-01-01T01:00:00Z", true),
    ] {
        let mut world = World::live(&sources);
        world.volume.insert("one".into(), 0);
        let (relayed, failure) = relay(&r, &world);
        assert!(failure.is_some());
        assert_eq!(kinds(&relayed), ["one:volume"]);
        let (executed, activated) = execute(&r, &world);
        assert!(!activated);
        assert_eq!(kinds(&executed), ["one:volume"]);
    }
}

#[test]
fn census_is_the_only_first_probe_even_with_requested_sources() {
    let _lock = SIGNAL.lock().unwrap();
    for sources in [&["one", "two"][..], &[][..]] {
        for depth_quick in [None, Some(false), Some(true)] {
            let mut r = match depth_quick {
                None => sources_only(sources),
                Some(quick) => request(sources, "2026-01-01T01:00:00Z", quick),
            };
            r.census = true;
            let plan = discovery::initial_plan(&r).unwrap();
            let initial = plan["initial_probes"].as_array().unwrap();
            assert_eq!(initial.len(), 1, "{plan}");
            assert_eq!(initial[0]["kind"], "census");
            let pending = discovery::frontier(&r, &BTreeMap::new()).unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].kind, "census");
            let mut world = World::live(&["one", "two", "three"]);
            world.census = vec![json!({"#type":"three","events":5})];
            let (relayed, failure) = relay(&r, &world);
            assert!(failure.is_none(), "{failure:?}");
            assert_eq!(relayed[0].kind, "census");
            let mut expected = sources.to_vec();
            expected.push("three");
            assert!(
                relayed[1..=expected.len()]
                    .iter()
                    .all(|probe| probe.kind == "volume"),
                "{:?}",
                kinds(&relayed)
            );
            assert_eq!(
                relayed[1..=expected.len()]
                    .iter()
                    .map(source_of)
                    .collect::<Vec<_>>(),
                expected
            );
            let (executed, activated) = execute(&r, &world);
            assert!(activated);
            assert_eq!(ids(&r, &executed), ids(&r, &relayed));
        }
    }
}

#[test]
fn initial_plan_lists_only_volumes_and_names_the_gated_stages() {
    let sources = ["one", "two"];
    for quick in [false, true] {
        let r = request(&sources, "2026-01-01T01:00:00Z", quick);
        let plan = discovery::initial_plan(&r).unwrap();
        let initial = plan["initial_probes"].as_array().unwrap();
        assert_eq!(initial.len(), 2, "{plan}");
        assert!(initial.iter().all(|probe| probe["kind"] == "volume"));
        assert_eq!(initial[1]["source"]["type"], "two");
        let stages = plan["deferred_stages"].to_string();
        assert!(stages.contains("dataset facet"), "{stages}");
        assert!(stages.contains("fieldset"), "{stages}");
    }
}

struct Native {
    directory: PathBuf,
    state: PathBuf,
}
impl Native {
    fn new(request: &DiscoverRequest) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "cqlmatic-volume-first-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let state = directory.join("state.json");
        mcp::init_resumable(
            request.clone(),
            &state,
            &directory.join("pack.json"),
            &directory.join("receipt.json"),
            Transport::Native,
        )
        .unwrap();
        Self { directory, state }
    }
}
impl Drop for Native {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[test]
fn native_runs_follow_the_same_order_across_pause_and_resume() {
    let _lock = SIGNAL.lock().unwrap();
    network::CANCELLED.store(false, Ordering::Relaxed);
    let sources = ["one", "two"];
    let r = request(&sources, "2026-01-01T01:00:00Z", false);
    let world = World::live(&sources);
    let (relayed, _) = relay(&r, &world);
    let expected = ids(&r, &relayed);

    let straight = Native::new(&r);
    let mut backend = Recorder::new(&r, &world);
    backend.learn(&reachable(&r, &world));
    assert_eq!(
        mcp::run_native(&straight.state, &mut backend)
            .unwrap()
            .status,
        "complete"
    );
    assert_eq!(backend.started, expected);

    for pause_after in [1, 3] {
        let interrupted = Native::new(&r);
        let mut backend = Recorder::new(&r, &world);
        backend.learn(&reachable(&r, &world));
        backend.cancel_after = Some(pause_after);
        assert_eq!(
            mcp::run_native(&interrupted.state, &mut backend)
                .unwrap()
                .status,
            "paused"
        );
        network::CANCELLED.store(false, Ordering::Relaxed);
        assert_eq!(backend.started, expected[..pause_after]);
        mcp::resume(&interrupted.state).unwrap();
        assert_eq!(
            mcp::run_native(&interrupted.state, &mut backend)
                .unwrap()
                .status,
            "complete"
        );
        assert_eq!(backend.started, expected, "pause after {pause_after}");
    }
}
