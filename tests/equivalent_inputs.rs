use cqlmatic::{
    contracts::{Cloud, EnvironmentId},
    discovery::{DiscoverRequest, initial_plan},
    environment::{Pack, PackData},
    inventory::{InventoryState, QueryObservation, QueryResultAssertion, TargetScope},
};
use serde_json::json;

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
    assert_eq!(probes[0]["start"], "2026-01-01T00:00:00.12Z");
    assert_eq!(probes[0]["end"], END);
    assert_eq!(probes[2]["start"], "2026-01-01T23:45:00.5Z");
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
