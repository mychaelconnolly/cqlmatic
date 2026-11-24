use cqlmatic::{
    app,
    assess::{self, AssessOutcome, AssessRequest},
    capability::{CapabilityOutcome, CapabilityRun, CaseId},
    contracts::{
        CheckRequest, Cloud, Context, Diagnostic, EnvironmentId, GenerateInput, GenerateRequest,
        Outcome, Plan,
    },
    discovery::{self, DiscoverRequest, DiscoveryDepth, Origin, initial_plan},
    environment::{Pack, PackData, normalize_request_time},
    inventory::{InventoryState, QueryObservation, QueryResultAssertion, TargetScope},
    network::{Credentials, FalconClient, JobPolicy, SearchBackend, run_job},
    sha256_hex,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

const CID: &str = concat!("0123456789abcdef", "0123456789abcdef");
const DIFFERENT_CID: &str = concat!("1123456789abcdef", "0123456789abcdef");
const START: &str = "2026-01-01T00:00:00.120Z";
const END: &str = "2026-01-02T00:00:00.500Z";

fn environment(tenant: &str) -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: tenant.into(),
        view: "search-all".into(),
    }
}

fn confirm(
    state: &mut InventoryState,
    tenant: &str,
) -> Result<(), cqlmatic::contracts::Diagnostic> {
    state.confirm(
        environment(tenant),
        TargetScope::FullEnvironment,
        "Synthetic target".into(),
        vec![],
        END,
    )
}

fn discovery_request(start: &str, end: &str) -> DiscoverRequest {
    serde_json::from_value(json!({
        "schema_version": 1,
        "environment": environment(CID),
        "start": start,
        "end": end,
        "sources": [{"type": "synthetic"}],
        "max_queries": 10,
        "max_seconds": 60
    }))
    .unwrap()
}

#[test]
fn console_cid_is_stored_without_checksum() {
    let mut state = InventoryState::new(END).unwrap();
    confirm(&mut state, &format!("{}-12", CID.to_ascii_uppercase())).unwrap();
    assert_eq!(state.target.unwrap().environment.tenant, CID);
    let mut state = InventoryState::new(END).unwrap();
    confirm(&mut state, &format!("{CID}-aF")).unwrap();
    assert_eq!(state.target.unwrap().environment.tenant, CID);
}

#[test]
fn uppercase_cid_is_stored_lowercase() {
    let mut state = InventoryState::new(END).unwrap();
    confirm(&mut state, &CID.to_ascii_uppercase()).unwrap();
    assert_eq!(state.target.unwrap().environment.tenant, CID);
}

#[test]
fn legacy_uppercase_pack_matches_without_changing_digest() {
    let mut pack = Pack::new(environment(CID), END.into(), PackData::default()).unwrap();
    pack.environment.tenant = CID.to_ascii_uppercase();
    pack.content_digest = pack.digest().unwrap();
    let bytes = serde_json::to_vec(&pack).unwrap();
    let retained: Pack = serde_json::from_slice(&bytes).unwrap();

    let mut state = InventoryState::new(END).unwrap();
    confirm(&mut state, CID).unwrap();
    state.register_pack_sources(&retained).unwrap();

    assert_eq!(retained.digest().unwrap(), pack.content_digest);
    assert_eq!(serde_json::to_vec(&retained).unwrap(), bytes);
    let mut other = InventoryState::new(END).unwrap();
    confirm(&mut other, DIFFERENT_CID).unwrap();
    assert!(other.register_pack_sources(&retained).is_err());
}

#[test]
fn cid_spellings_match_a_confirmed_target() {
    let mut state = InventoryState::new(END).unwrap();
    confirm(&mut state, CID).unwrap();
    for tenant in [CID.to_ascii_uppercase(), format!("{CID}-12")] {
        let plan = state
            .prepare_scout(environment(&tenant), START.into(), END.into())
            .unwrap();
        assert_eq!(plan.environment.tenant, CID);
    }

    let error = state
        .prepare_scout(
            environment(&format!("{DIFFERENT_CID}-12")),
            START.into(),
            END.into(),
        )
        .unwrap_err();
    assert!(error.message.contains("different environment"));
}

#[test]
fn cid_error_explains_accepted_forms() {
    for tenant in [
        format!("{CID}-1"),
        format!("{CID}-123"),
        format!("{CID}-1!"),
        format!("{CID}-ZZ"),
        format!("{CID}-GZ"),
        format!("{CID}-"),
        format!("{CID} "),
        format!("API-CLIENT:{}", "ABCDEF01".repeat(8)),
        format!("{}g-12", &CID[..31]),
        format!(" {CID}"),
    ] {
        let mut state = InventoryState::new(END).unwrap();
        let error = confirm(&mut state, &tenant).unwrap_err();
        assert_eq!(error.code, "ENV_TENANT");
        assert!(error.message.contains("32 hexadecimal"), "{error:?}");
        assert!(error.message.contains("checksum"), "{error:?}");
        assert!(error.message.contains("api-client:<SHA-256>"), "{error:?}");
    }

    let mut state = InventoryState::new(END).unwrap();
    let fingerprint = format!("api-client:{}", "ABCDEF01".repeat(8));
    confirm(&mut state, &fingerprint).unwrap();
    assert_eq!(state.target.unwrap().environment.tenant, fingerprint);
}

#[test]
fn discovery_windows_convert_explicit_offsets_to_utc() {
    let expected = initial_plan(&discovery_request(START, END)).unwrap();
    let probes = expected["initial_probes"].as_array().unwrap();
    assert_eq!(probes.len(), 1);
    assert_eq!(probes[0]["kind"], "volume");
    assert_eq!(probes[0]["start"], "2026-01-01T00:00:00.12Z");
    assert_eq!(probes[0]["end"], END);
    for (start, end) in [
        (
            "2026-01-01T00:00:00.120+00:00",
            "2026-01-02T00:00:00.500+00:00",
        ),
        (
            "2025-12-31T19:00:00.120-05:00",
            "2026-01-02T05:30:00.500+05:30",
        ),
    ] {
        assert_eq!(
            initial_plan(&discovery_request(start, end)).unwrap(),
            expected
        );
    }
}

#[test]
fn scout_windows_convert_offsets_but_repeat_exact_arguments() {
    let mut state = InventoryState::new(END).unwrap();
    let expected = state
        .prepare_scout(environment(CID), START.into(), END.into())
        .unwrap();
    let offset_start = "2025-12-31T19:00:00.120-05:00";
    let actual = state
        .prepare_scout(environment(CID), offset_start.into(), END.into())
        .unwrap();
    assert_eq!(actual.arguments, expected.arguments);
    let mut different = actual.arguments.clone();
    different.start = offset_start.into();
    let raw = br##"{"result":[{"#repo":"synthetic","#type":"events","events":1}]}"##;
    let error = state.import_scout(different, raw, END).unwrap_err();
    assert!(error.message.contains("exact prepared census query/window"));
    state.import_scout(actual.arguments, raw, END).unwrap();
}

#[test]
fn windows_still_require_an_explicit_offset_and_increasing_instants() {
    for (start, end, expected_code) in [
        ("2026-01-01T00:00:00", END, "PACK_TIME"),
        ("2026-01-02T05:30:00.500+05:30", END, "DISCOVERY_EVIDENCE"),
        (START, "2026-01-08T00:00:00.121+00:00", "DISCOVERY_EVIDENCE"),
    ] {
        assert_eq!(
            initial_plan(&discovery_request(start, end))
                .unwrap_err()
                .code,
            expected_code
        );
    }
}

#[test]
fn inventory_discovery_retains_the_normalized_request() {
    let mut state = InventoryState::new(END).unwrap();
    state
        .confirm(
            environment(CID),
            TargetScope::SelectedSources,
            "Synthetic target".into(),
            vec![serde_json::from_value(json!({"type": "synthetic"})).unwrap()],
            END,
        )
        .unwrap();
    let request = state
        .discovery_request(
            "2025-12-31T19:00:00.120-05:00".into(),
            "2026-01-02T00:00:00.500+00:00".into(),
            &[],
            vec![],
            vec![],
            vec![],
            10,
            60,
        )
        .unwrap();
    assert_eq!(request.start, START);
    assert_eq!(request.end, END);
}

#[test]
fn query_observations_store_utc_windows() {
    for assertion in [
        QueryResultAssertion::ExactQueryResultTargetWindow,
        QueryResultAssertion::ClientHookCapture,
    ] {
        let observe = |start: &str, end: &str, retrieved: &str| {
            QueryObservation::from_result(
                b"#type=synthetic | select([value])",
                br#"{"result":[{"value":1}]}"#,
                environment(CID),
                serde_json::from_value(json!({"type":"synthetic"})).unwrap(),
                start.into(),
                end.into(),
                retrieved.into(),
                assertion,
                std::env::temp_dir()
                    .join("cqlmatic-synthetic-audit.json")
                    .to_string_lossy()
                    .into_owned(),
            )
        };
        let utc = observe(START, END, END).unwrap();
        let shifted = observe(
            "2025-12-31T19:00:00.120-05:00",
            "2026-01-02T05:30:00.500+05:30",
            END,
        )
        .unwrap();
        assert_eq!(shifted, utc);
        assert!(observe("2026-01-01T00:00:00.120", END, END).is_err());
    }
}

#[test]
fn noncanonical_timestamp_separators_and_offsets_emit_utc() {
    let expected = "2026-01-01T00:00:00Z";
    for timestamp in [
        "2026-01-01 00:00:00Z",
        "2026-01-01t00:00:00Z",
        "2026-01-01T00:00:00z",
        "2026-01-01T00:00:00-00:00",
        "2026-01-01 00:00:00z",
        "2026-01-01t00:00:00+00:00",
    ] {
        assert_eq!(
            normalize_request_time(timestamp).unwrap(),
            expected,
            "{timestamp}"
        );
    }
    assert_eq!(normalize_request_time(START).unwrap(), START);
}

#[test]
fn invalid_timestamp_separators_and_utc_year_shifts_are_rejected() {
    for timestamp in [
        "2026-01-01X00:00:00+00:00",
        "2026-01-01X00:00:00Z",
        "0000-01-01T00:00:00+05:00",
        "9999-12-31T23:59:59-05:00",
    ] {
        assert_eq!(
            normalize_request_time(timestamp).unwrap_err().code,
            "PACK_TIME",
            "{timestamp}"
        );
    }
}

#[test]
fn lowercase_zone_and_utc_year_boundaries_normalize() {
    assert_eq!(
        normalize_request_time("2026-01-01T00:00:00z").unwrap(),
        "2026-01-01T00:00:00Z"
    );
    for timestamp in ["0000-01-01T00:00:00Z", "9999-12-31T23:59:59Z"] {
        assert_eq!(normalize_request_time(timestamp).unwrap(), timestamp);
    }
}

#[test]
fn discovery_rejects_more_than_seven_days_at_full_precision() {
    let start = "2026-01-01T00:00:00.0000000001+00:00";
    let limit = "2026-01-08T00:00:00.0000000001+00:00";
    let over = "2026-01-08T00:00:00.0000000002+00:00";
    assert!(initial_plan(&discovery_request(start, limit)).is_ok());
    assert_eq!(
        initial_plan(&discovery_request(start, over))
            .unwrap_err()
            .code,
        "DISCOVERY_EVIDENCE"
    );
}

#[test]
fn discovery_accepts_increasing_subnanosecond_instants() {
    let start = "2026-01-01T00:00:00.0000000001+00:00";
    let end = "2026-01-01T00:00:00.0000000002+00:00";
    assert!(initial_plan(&discovery_request(start, end)).is_ok());
    assert!(initial_plan(&discovery_request(end, start)).is_err());
    assert!(
        initial_plan(&discovery_request(
            start,
            "2026-01-01T00:00:00.00000000010Z"
        ))
        .is_err()
    );
}

const QUERY: &str = r#"#type="synthetic" | count(as=events)"#;
const ACCEPTED_CID_FORM: &str = "two-digit hexadecimal checksum suffix";

fn check_and_generate(context: Context, pack: Option<&Pack>) -> [cqlmatic::contracts::Analysis; 2] {
    [
        app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: QUERY.into(),
                context: context.clone(),
            },
            pack,
        ),
        app::generate(
            GenerateRequest {
                schema_version: 1,
                id: None,
                input: GenerateInput::Plan {
                    plan: serde_json::from_value::<Plan>(json!({
                        "source":{"type":"synthetic"}, "steps":[]
                    }))
                    .unwrap(),
                },
                context,
            },
            pack,
        ),
    ]
}

fn assessment_request(context: Context) -> AssessRequest {
    AssessRequest {
        schema_version: 1,
        id: None,
        query: QUERY.into(),
        result: json!({"result":[{"events":1}]}),
        context,
    }
}

fn query_target(tenant: &str) -> Context {
    Context {
        environment: Some(environment(tenant)),
        ..Default::default()
    }
}

#[test]
fn check_and_generate_explain_accepted_cid_forms() {
    let pack = Pack::new(environment(CID), END.into(), PackData::default()).unwrap();
    for result in check_and_generate(query_target(&format!("{CID}-zz")), Some(&pack)) {
        assert_eq!(result.outcome, Outcome::NeedsContext);
        assert!(
            result
                .grounding
                .conflicting
                .iter()
                .any(|message| message.contains(ACCEPTED_CID_FORM)),
            "{:?}",
            result.grounding.conflicting
        );
    }
}

#[test]
fn assessment_explains_accepted_cid_forms_with_or_without_a_pack() {
    let pack = Pack::new(environment(CID), END.into(), PackData::default()).unwrap();
    for pack in [Some(&pack), None] {
        let result = assess::assess(assessment_request(query_target(&format!("{CID}-zz"))), pack);
        assert_eq!(result.outcome, AssessOutcome::Error);
        assert!(
            result.findings.iter().any(|finding| {
                finding.code == "ENV_TENANT" && finding.message.contains(ACCEPTED_CID_FORM)
            }),
            "{:?}",
            result.findings
        );
    }
}

#[test]
fn query_consumers_match_equivalent_cids_and_refuse_a_foreign_cid() {
    let pack = Pack::new(environment(CID), END.into(), PackData::default()).unwrap();
    let suffixed = format!("{}-aF", CID.to_ascii_uppercase());
    assert_eq!(environment(&suffixed), pack.environment);
    assert_ne!(
        environment(&format!("{DIFFERENT_CID}-aF")),
        pack.environment
    );
    for spelling in [CID.to_ascii_uppercase(), suffixed] {
        let context = query_target(&spelling);
        for result in check_and_generate(context.clone(), Some(&pack)) {
            assert!(
                result.grounding.conflicting.is_empty(),
                "{:?}",
                result.grounding.conflicting
            );
            assert_eq!(
                result.grounding.pack_id.as_deref(),
                Some(pack.content_digest.as_str())
            );
        }
        assert_ne!(
            assess::assess(assessment_request(context), Some(&pack)).outcome,
            AssessOutcome::Error
        );
    }
    let context = query_target(&format!("{DIFFERENT_CID}-aF"));
    for result in check_and_generate(context.clone(), Some(&pack)) {
        assert_eq!(result.outcome, Outcome::NeedsContext);
        assert!(
            result
                .grounding
                .conflicting
                .iter()
                .any(|message| message.contains("conflicts with selected pack"))
        );
    }
    let result = assess::assess(assessment_request(context), Some(&pack));
    assert!(
        result
            .findings
            .iter()
            .any(|finding| finding.code == "PACK_ENVIRONMENT")
    );
}

#[test]
fn reconfirming_legacy_inventory_preserves_target_spelling_and_digest() {
    let mut state = InventoryState::new(END).unwrap();
    confirm(&mut state, CID).unwrap();
    let uppercase = CID.to_ascii_uppercase();
    state.target.as_mut().unwrap().environment.tenant = uppercase.clone();
    let digest = state.evidence_digest().unwrap();

    let mut legacy: InventoryState =
        serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
    legacy.validate().unwrap();
    confirm(&mut legacy, &format!("{CID}-aF")).unwrap();
    assert_eq!(legacy.evidence_digest().unwrap(), digest);
    assert_eq!(legacy.target.unwrap().environment.tenant, uppercase);
}

#[derive(Default)]
struct VolumeBackend {
    windows: Vec<(String, String)>,
}

impl SearchBackend for VolumeBackend {
    fn start(
        &mut self,
        _: &str,
        _: &str,
        start: &str,
        end: &str,
        _: Instant,
    ) -> Result<String, Diagnostic> {
        self.windows.push((start.into(), end.into()));
        Ok("synthetic-job".into())
    }

    fn poll(&mut self, _: &str, _: &str, _: Instant) -> Result<Value, Diagnostic> {
        Ok(json!({"done":true,"cancelled":false,"metaData":{},"events":[{"events":1}]}))
    }

    fn stop(&mut self, _: &str, _: &str, _: Instant) -> Result<(), Diagnostic> {
        Ok(())
    }
}

const OFFSET_START: &str = "2025-12-31T19:00:00-05:00";
const OFFSET_END: &str = "2026-01-02T05:30:00+05:30";
const UTC_START: &str = "2026-01-01T00:00:00Z";
const UTC_END: &str = "2026-01-02T00:00:00Z";

#[test]
fn native_job_passes_normalized_bounds_to_the_backend() {
    let mut backend = VolumeBackend::default();
    let receipt = run_job(
        &mut backend,
        "search-all",
        QUERY,
        OFFSET_START,
        OFFSET_END,
        &JobPolicy::default(),
    );
    assert_eq!(receipt.completion, "complete", "{:?}", receipt.diagnostic);
    assert_eq!(backend.windows, [(UTC_START.into(), UTC_END.into())]);
    assert_eq!(
        (receipt.start.as_str(), receipt.end.as_str()),
        (UTC_START, UTC_END)
    );
}

#[test]
fn public_discovery_execution_normalizes_identity_and_observation_windows() {
    let mut request = discovery_request(OFFSET_START, OFFSET_END);
    request.environment = environment(&format!("{}-12", CID.to_ascii_uppercase()));
    request.depth = DiscoveryDepth::SourcesOnly;

    let mut backend = VolumeBackend::default();
    let direct = discovery::execute(&request, &mut backend);
    let with_origin = discovery::execute_with_origin(
        &request,
        &mut backend,
        Origin::Api,
        Duration::from_secs(10),
    );
    for (pack, report) in [direct, with_origin] {
        let pack = pack.unwrap_or_else(|| panic!("{:?}", report.diagnostics));
        assert_eq!(pack.environment.tenant, CID);
        assert_eq!(report.environment.tenant, CID);
        assert_eq!(
            pack.data.sources[0]["evidence"]["observation_window"],
            format!("{UTC_START}/{UTC_END}")
        );
    }
}

struct CliFixture {
    dir: PathBuf,
}

impl CliFixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "cqlmatic-equivalent-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&dir).unwrap();
        Self { dir }
    }

    fn file(&self, name: &str, bytes: &[u8]) -> PathBuf {
        let path = self.dir.join(name);
        cqlmatic::storage::Writer::acquire(&path)
            .unwrap()
            .create_new_bytes(bytes, 64 * 1024)
            .unwrap();
        path
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
        for var in [
            "HOME",
            "USERPROFILE",
            "LOCALAPPDATA",
            "XDG_STATE_HOME",
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
        ] {
            command.env(var, &self.dir);
        }
        command
    }

    fn state(&self) -> PathBuf {
        self.dir.join("workflow/state.json")
    }

    fn inventory(&self, action: Value) -> Output {
        let path = self.dir.join("request.json");
        fs::write(
            &path,
            json!({"schema_version":1,"action":action}).to_string(),
        )
        .unwrap();
        self.command()
            .args(["env", "inventory", "--json", "--state"])
            .arg(self.state())
            .arg("--request")
            .arg(path)
            .output()
            .unwrap()
    }

    fn apply(&self, action: Value) -> Value {
        let output = self.inventory(action);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }

    fn confirm_sources(&self, sources: Value) {
        self.apply(
            json!({"kind":"set-discovery-mode","mode":"quick","quick_depth":"sources-only"}),
        );
        self.apply(
            json!({"kind":"confirm-target","environment":environment(CID),
            "scope":"selected-sources","context":"Synthetic target","expected_sources":sources}),
        );
    }
}

impl Drop for CliFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn credentials_and_falcon_client_match_equivalent_member_cid() {
    let fixture = CliFixture::new();
    let path = fixture.file(
        "credentials.json",
        json!({
            "client_id":"synthetic-id", "client_secret":"synthetic-placeholder",
            "member_cid":format!("{}-aF", CID.to_ascii_uppercase())
        })
        .to_string()
        .as_bytes(),
    );
    let identity = environment(CID);
    let credentials = Credentials::load(Some(&path), &identity).unwrap();
    assert!(FalconClient::new(&identity, credentials).is_ok());
    let foreign = Credentials::load(Some(&path), &environment(DIFFERENT_CID));
    assert!(matches!(foreign, Err(error) if error.code == "AUTH_TENANT"));
}

#[test]
fn inventory_assessment_explains_accepted_cid_forms() {
    let fixture = CliFixture::new();
    fixture.confirm_sources(json!([{"type":"synthetic"}]));
    let request =
        serde_json::to_vec(&assessment_request(query_target(&format!("{CID}-zz")))).unwrap();
    let path = fixture.file("assess.json", &request);
    let output = fixture
        .command()
        .args(["assess", "--json", "--inventory-state"])
        .arg(fixture.state())
        .arg("--request")
        .arg(path)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    let findings = result["findings"].as_array().unwrap();
    assert!(
        findings.iter().any(|finding| {
            finding["code"] == "ENV_TENANT"
                && finding["message"]
                    .as_str()
                    .unwrap()
                    .contains(ACCEPTED_CID_FORM)
        }),
        "{result}"
    );
    assert!(
        findings
            .iter()
            .all(|finding| finding["code"] != "CQL_QUICK_TARGET_MISMATCH"),
        "{result}"
    );
}

#[test]
fn equivalent_paused_discovery_keeps_its_original_allowance_and_captures() {
    let fixture = CliFixture::new();
    fixture.confirm_sources(json!([{"type":"synthetic-a"},{"type":"synthetic-b"}]));
    let mut request = json!({"kind":"prepare-discovery","start":"2026-01-01t00:00:00Z",
        "end":UTC_END,"max_queries":4,"max_seconds":120});
    let prepared = fixture.apply(request.clone());
    let checkpoint = prepared["status"]["discovery_runs"][0]["state_path"]
        .as_str()
        .unwrap();
    let first = fixture.apply(json!({"kind":"next-probe","run_index":0}));
    fixture.apply(json!({"kind":"pause-discovery","run_index":0}));
    let answer = fixture.file("volume.json", br#"{"result":[{"events":1}]}"#);
    let paused = fixture.apply(json!({"kind":"next-probe","run_index":0,"answer":{
        "probe_id":first["action"]["probe"]["probe_id"],"result_file":answer
    }}));
    assert_eq!(paused["action"]["state"], "paused", "{paused}");
    assert_eq!(paused["action"]["settled"], true);
    assert_eq!(paused["action"]["cost"]["searches"]["used"], 1);
    let before = fs::read(fixture.state()).unwrap();
    let checkpoint_before = fs::read(checkpoint).unwrap();

    for equivalent in ["2026-01-01t00:00:00+00:00", "2026-01-01T00:00:00.000Z"] {
        request["start"] = json!(equivalent);
        let output = fixture.inventory(request.clone());
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{equivalent}: {output:?}");
        assert!(
            diagnostic.contains("resume-discovery with run_index 0"),
            "{diagnostic}"
        );
        assert!(fs::read(fixture.state()).unwrap() == before);
        assert!(fs::read(checkpoint).unwrap() == checkpoint_before);
    }

    request["start"] = json!("2026-01-01T00:00:00.0000000001Z");
    let replacement = fixture.apply(request);
    assert_eq!(
        replacement["status"]["discovery_runs"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn imported_observation_normalizes_retrieval_time_and_replays_without_new_id() {
    let fixture = CliFixture::new();
    fixture.confirm_sources(json!([{"type":"synthetic"}]));
    let query = fixture.file("query.cql", br#"#type="synthetic" | select([value])"#);
    let result = fixture.file("result.json", br#"{"result":[{"value":1}]}"#);
    let mut request = json!({"kind":"import-query-observations","environment":environment(CID),
        "source":{"type":"synthetic"},"query_file":query,"result_file":result,
        "start":UTC_START,"end":UTC_END,"retrieved_at":OFFSET_END,
        "caller_assertion":"exact-query-result-target-window"});
    let imported = fixture.apply(request.clone());
    let audit_path = imported["action"]["audit_path"].as_str().unwrap();
    let audit = fs::read(audit_path).unwrap();
    let observation: Value = serde_json::from_slice(&audit).unwrap();
    assert_eq!(observation["observation"]["retrieved_at"], UTC_END);

    request["retrieved_at"] = json!(UTC_END);
    let replay = fixture.apply(request);
    assert_eq!(
        replay["action"]["query_observation_id"],
        imported["action"]["query_observation_id"]
    );
    assert_eq!(replay["action"]["audit_path"], audit_path);
    assert_eq!(replay["status"]["query_observation_count"], 1);
    assert_eq!(fs::read(audit_path).unwrap(), audit);
}

#[test]
fn capability_replay_matches_retained_retrieval_instant_without_changing_capture() {
    let target = environment(CID);
    let host = json!({"schema_version":1,"environment":target,"session_id":"synthetic-session",
        "verified_at":UTC_END,"mechanism":"provider-enforced-search-lifetime","max_search_seconds":1,
        "capability_reference":"Synthetic finite provider lifetime","verification_evidence":"Synthetic fixture"});
    let case = CaseId::MathSqrt;
    let mut run = CapabilityRun::prepare(
        target.clone(),
        "synthetic-auth".into(),
        "synthetic-session".into(),
        vec![case],
        host.to_string().as_bytes(),
        4,
        120,
        UTC_END,
        0,
    )
    .unwrap();
    let attempt = run
        .dispatch(case, "synthetic-auth", "synthetic-session", UTC_END)
        .unwrap();
    let id = attempt.reservation.id.clone();
    let at = "2026-01-02T00:00:01Z";
    let offset = "2026-01-02T00:00:01+00:00";
    let raw = br#"{"result":[{"out":3}]}"#;
    let settlement = json!({"schema_version":1,"attempt_id":id,"environment":target,
        "session_id":"synthetic-session","arguments":attempt.reservation.arguments,
        "termination":"completed","recorded_at":at,"search_started_at":null,
        "result_sha256":sha256_hex(raw),"evidence":"Synthetic exact-attempt evidence"})
    .to_string();
    run.submit(
        &id,
        "synthetic-auth",
        Some(raw),
        settlement.as_bytes(),
        offset,
        at,
    )
    .unwrap();
    assert_eq!(run.attempts[0].outcome, CapabilityOutcome::Verified);

    let normalized = run.attempts[0]
        .capture
        .as_ref()
        .unwrap()
        .retrieved_at
        .clone();
    // Model a retained capture written before retrieval times were normalized.
    run.attempts[0].capture.as_mut().unwrap().retrieved_at = offset.into();
    let saved = serde_json::to_vec(&run).unwrap();
    run.submit(
        &id,
        "synthetic-auth",
        Some(raw),
        settlement.as_bytes(),
        at,
        at,
    )
    .unwrap();
    assert_eq!(serde_json::to_vec(&run).unwrap(), saved);

    let later = "2026-01-02T00:00:02Z";
    let distinct = "2026-01-02T00:00:01.0000000001Z";
    let error = run
        .submit(
            &id,
            "synthetic-auth",
            Some(raw),
            settlement.as_bytes(),
            distinct,
            later,
        )
        .unwrap_err();
    assert!(error.message.contains("already settled"));
    assert_eq!(serde_json::to_vec(&run).unwrap(), saved);
    assert_eq!(normalized, at);
}

#[test]
fn retained_discovery_replays_exact_checkpoint_bounds() {
    let fixture = CliFixture::new();
    let checkpoint = fixture.dir.join("relay.json");
    let mut request = discovery_request(UTC_START, UTC_END);
    request.depth = DiscoveryDepth::SourcesOnly;
    cqlmatic::mcp::init(
        request,
        &checkpoint,
        &fixture.dir.join("pack.json"),
        &fixture.dir.join("receipt.json"),
    )
    .unwrap();

    // An earlier producer kept the lowercase separator in the request and emitted end.
    let mut state = cqlmatic::mcp::inspect_state(&checkpoint).unwrap();
    state.request.end = "2026-01-02t00:00:00Z".into();
    state.request_sha256 = sha256_hex(&serde_json::to_vec(&state.request).unwrap());
    fs::write(&checkpoint, serde_json::to_vec(&state).unwrap()).unwrap();

    let pending = cqlmatic::mcp::step(&checkpoint, &[]).unwrap();
    let probe = &pending.probes[0];
    assert_eq!(probe["mcp"]["arguments"]["end"], state.request.end);
    let id = probe["probe_id"].as_str().unwrap().into();
    let answer = fixture.file("volume.json", br#"{"result":[{"events":1}]}"#);
    let complete = cqlmatic::mcp::step(&checkpoint, &[(id, answer)]).unwrap();
    assert_eq!(complete.status, "complete", "{:?}", complete.diagnostics);
    let retained = cqlmatic::mcp::inspect_state(&checkpoint).unwrap();
    assert_eq!(retained.request_sha256, state.request_sha256);
}
