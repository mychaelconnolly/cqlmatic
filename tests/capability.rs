use cqlmatic::{
    app,
    capability::{
        self, CapabilityEvidence, CapabilityOutcome, CapabilityRun, CaseId, ResourceBindings,
    },
    contracts::{CheckRequest, Cloud, Context, EnvironmentId, Outcome},
    inventory::{InventoryState, RefreshTermination},
    sha256_hex,
};
use serde_json::{Value, json};

const START: &str = "2026-01-01T00:00:00Z";
const ONE: &str = "2026-01-01T00:00:01Z";
const TWO: &str = "2026-01-01T00:00:02Z";

fn environment() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "synthetic".into(),
    }
}

fn host() -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":1,"environment":environment(),
        "session_id":"synthetic-session","verified_at":START,
        "mechanism":"provider-enforced-search-lifetime","max_search_seconds":1,
        "capability_reference":"Synthetic finite provider lifetime",
        "verification_evidence":"Synthetic lifecycle fixture"}))
    .unwrap()
}

fn run(case: CaseId, max_queries: u32, max_seconds: u64) -> CapabilityRun {
    CapabilityRun::prepare(
        environment(),
        "synthetic-auth".into(),
        "synthetic-session".into(),
        vec![case],
        &host(),
        max_queries,
        max_seconds,
        START,
        0,
    )
    .unwrap()
}

fn result(case: CaseId) -> Vec<u8> {
    serde_json::to_vec(&json!({"result":[match case {
        CaseId::ArrayUnion => json!({"out[0]":3,"out[1]":"1","out[2]":2}),
        CaseId::ArrayIntersection => json!({"out[0]":2}),
        CaseId::ArrayReduceRow => json!({"out._sum":3}),
        CaseId::ArrayReduceColumn => json!({"out[0]":4,"out[1]":6}),
        CaseId::MathSqrt => json!({"out":3}),
    }]}))
    .unwrap()
}

fn settlement(
    run: &CapabilityRun,
    termination: RefreshTermination,
    raw: Option<&[u8]>,
    now: &str,
) -> Vec<u8> {
    let attempt = run.attempts.last().unwrap();
    serde_json::to_vec(&json!({"schema_version":1,"attempt_id":attempt.reservation.id,
        "environment":run.environment,"session_id":run.session_id,"arguments":attempt.reservation.arguments,
        "termination":termination,"recorded_at":now,"search_started_at":null,
        "result_sha256":if termination==RefreshTermination::Completed { raw.map(sha256_hex) } else { None },
        "evidence":"Synthetic exact-attempt terminal report"})).unwrap()
}

fn dispatch(run: &mut CapabilityRun, case: CaseId, now: &str) -> String {
    run.dispatch(case, "synthetic-auth", "synthetic-session", now)
        .unwrap()
        .reservation
        .id
        .clone()
}

fn submit(run: &mut CapabilityRun, termination: RefreshTermination, raw: Option<&[u8]>, now: &str) {
    let report = settlement(run, termination, raw, now);
    let id = run.attempts.last().unwrap().reservation.id.clone();
    run.submit(&id, "synthetic-auth", raw, &report, now, now)
        .unwrap();
    run.validate(0).unwrap();
}

fn qualifies(
    run: &CapabilityRun,
    case: CaseId,
    context: Option<&str>,
    environment: &EnvironmentId,
    now: &str,
) -> bool {
    capability::qualify(
        CapabilityEvidence {
            runs: std::slice::from_ref(run),
            environment,
            execution_context_id: context,
            now,
        },
        case,
        ResourceBindings::None {},
    )
    .unwrap()
    .is_some()
}

#[test]
fn shipped_cases_have_fixed_queries_and_explicit_oracles() {
    for case in [
        CaseId::ArrayUnion,
        CaseId::ArrayIntersection,
        CaseId::ArrayReduceRow,
        CaseId::ArrayReduceColumn,
        CaseId::MathSqrt,
    ] {
        let query = case.query();
        let check = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                context: Context::default(),
            },
            None,
        );
        assert_eq!(
            check.outcome,
            Outcome::Ready,
            "{case:?}: {:?}",
            check.diagnostics
        );
        let mut run = run(case, 1, 10);
        dispatch(&mut run, case, START);
        let raw = result(case);
        submit(&mut run, RefreshTermination::Completed, Some(&raw), ONE);
        assert_eq!(run.attempts[0].outcome, CapabilityOutcome::Verified);
        assert!(qualifies(
            &run,
            case,
            Some("synthetic-auth"),
            &environment(),
            TWO
        ));
        let saved: CapabilityRun =
            serde_json::from_slice(&serde_json::to_vec(&run).unwrap()).unwrap();
        saved.validate(0).unwrap();
        assert_eq!(
            saved.attempts[0].capture.as_ref().unwrap().bytes().unwrap(),
            raw
        );
    }
}

#[test]
fn numeric_oracles_accept_equivalent_wire_numbers_without_rounding_to_a_match() {
    for (value, expected) in [
        (json!(3.0), true),
        (json!("3.000"), true),
        (json!("3.1"), false),
        (json!("3.000000000000000001"), false),
        (json!("NaN"), false),
        (json!(true), false),
    ] {
        let mut run = run(CaseId::MathSqrt, 1, 10);
        dispatch(&mut run, CaseId::MathSqrt, START);
        let raw = serde_json::to_vec(&json!({"result":[{"out":value}]})).unwrap();
        submit(&mut run, RefreshTermination::Completed, Some(&raw), ONE);
        assert_eq!(
            run.attempts[0].outcome == CapabilityOutcome::Verified,
            expected
        );
    }
}

#[test]
fn positive_evidence_is_exact_context_case_environment_and_time_bound() {
    let mut run = run(CaseId::ArrayUnion, 2, 10);
    dispatch(&mut run, CaseId::ArrayUnion, START);
    submit(
        &mut run,
        RefreshTermination::Completed,
        Some(&result(CaseId::ArrayUnion)),
        ONE,
    );
    assert!(!qualifies(
        &run,
        CaseId::ArrayIntersection,
        Some("synthetic-auth"),
        &environment(),
        TWO
    ));
    assert!(!qualifies(
        &run,
        CaseId::ArrayUnion,
        None,
        &environment(),
        TWO
    ));
    assert!(!qualifies(
        &run,
        CaseId::ArrayUnion,
        Some("other-auth-same-CID"),
        &environment(),
        TWO
    ));
    for target in [
        EnvironmentId {
            view: "other".into(),
            ..environment()
        },
        EnvironmentId {
            cloud: Cloud::Eu1,
            ..environment()
        },
        EnvironmentId {
            tenant: "1".repeat(32),
            ..environment()
        },
    ] {
        assert!(!qualifies(
            &run,
            CaseId::ArrayUnion,
            Some("synthetic-auth"),
            &target,
            TWO
        ));
    }
    assert!(!qualifies(
        &run,
        CaseId::ArrayUnion,
        Some("synthetic-auth"),
        &environment(),
        START
    ));
    assert!(qualifies(
        &run,
        CaseId::ArrayUnion,
        Some("synthetic-auth"),
        &environment(),
        "2026-01-31T00:00:00Z"
    ));
    assert!(!qualifies(
        &run,
        CaseId::ArrayUnion,
        Some("synthetic-auth"),
        &environment(),
        "2026-01-31T00:00:01Z"
    ));
    dispatch(&mut run, CaseId::ArrayUnion, TWO);
    assert!(!qualifies(
        &run,
        CaseId::ArrayUnion,
        Some("synthetic-auth"),
        &environment(),
        TWO
    ));
    submit(
        &mut run,
        RefreshTermination::TimedOut,
        None,
        "2026-01-01T00:00:03Z",
    );
    assert!(!qualifies(
        &run,
        CaseId::ArrayUnion,
        Some("synthetic-auth"),
        &environment(),
        "2026-01-01T00:00:04Z"
    ));
}

#[test]
fn latest_attempt_supersedes_only_evidence_from_its_own_execution_context() {
    let mut first = run(CaseId::ArrayUnion, 1, 10);
    dispatch(&mut first, CaseId::ArrayUnion, START);
    submit(
        &mut first,
        RefreshTermination::Completed,
        Some(&result(CaseId::ArrayUnion)),
        ONE,
    );
    for later_context in ["synthetic-auth", "other-auth"] {
        let mut later = CapabilityRun::prepare(
            environment(),
            later_context.into(),
            "synthetic-session".into(),
            vec![CaseId::ArrayUnion],
            &host(),
            1,
            10,
            TWO,
            1,
        )
        .unwrap();
        let id = later
            .dispatch(CaseId::ArrayUnion, later_context, "synthetic-session", TWO)
            .unwrap()
            .reservation
            .id
            .clone();
        let settled = "2026-01-01T00:00:03Z";
        let report = settlement(&later, RefreshTermination::TimedOut, None, settled);
        later
            .submit(&id, later_context, None, &report, settled, settled)
            .unwrap();
        let runs = [first.clone(), later];
        for (index, run) in runs.iter().enumerate() {
            run.validate(index).unwrap();
        }
        for context in ["synthetic-auth", "other-auth"] {
            let qualified = capability::qualify(
                CapabilityEvidence {
                    runs: &runs,
                    environment: &environment(),
                    execution_context_id: Some(context),
                    now: "2026-01-01T00:00:04Z",
                },
                CaseId::ArrayUnion,
                ResourceBindings::None {},
            )
            .unwrap();
            assert_eq!(
                qualified.is_some(),
                context == "synthetic-auth" && later_context != context
            );
        }
    }
}

#[test]
fn errors_empty_extra_wrong_and_ambiguous_outputs_never_confirm_a_case() {
    for raw in [
        json!({"result":[]}),
        json!({"result":[{}]}),
        json!({"result":[{"out._sum":0}]}),
        json!({"result":[{"_sum":3}]}),
        json!({"result":[{"out._sum":3,"unexpected":1}]}),
        json!({"result":[{"out._sum":3}],"partial":true}),
        json!({"isError":true,"content":[{"type":"text","text":"function unavailable"}]}),
        json!({"error":{"code":"FUNCTION_NOT_FOUND"},"result":[]}),
    ] {
        let mut run = run(CaseId::ArrayReduceRow, 1, 10);
        dispatch(&mut run, CaseId::ArrayReduceRow, START);
        let raw = serde_json::to_vec(&raw).unwrap();
        submit(&mut run, RefreshTermination::Completed, Some(&raw), ONE);
        assert_eq!(run.attempts[0].outcome, CapabilityOutcome::Unknown);
        assert!(!qualifies(
            &run,
            CaseId::ArrayReduceRow,
            Some("synthetic-auth"),
            &environment(),
            TWO
        ));
    }
}

#[test]
fn pause_waits_for_safe_termination_and_never_renews_either_budget() {
    let mut run = run(CaseId::ArrayUnion, 2, 4);
    dispatch(&mut run, CaseId::ArrayUnion, START);
    submit(&mut run, RefreshTermination::Unresolved, None, ONE);
    run.pause(TWO).unwrap();
    assert!(!run.paused());
    assert!(run.blocked());
    assert!(
        run.dispatch(
            CaseId::ArrayUnion,
            "synthetic-auth",
            "synthetic-session",
            TWO
        )
        .is_err()
    );
    assert!(run.resume("synthetic-auth", TWO).is_err());
    submit(
        &mut run,
        RefreshTermination::Cancelled,
        None,
        "2026-01-01T00:00:03Z",
    );
    assert!(run.paused());
    assert_eq!(run.remaining_seconds("2026-02-01T00:00:00Z").unwrap(), 1.0);
    run.resume("synthetic-auth", "2026-02-01T00:00:00Z")
        .unwrap();
    dispatch(&mut run, CaseId::ArrayUnion, "2026-02-01T00:00:00Z");
    submit(
        &mut run,
        RefreshTermination::NotStarted,
        None,
        "2026-02-01T00:00:01Z",
    );
    run.pause("2026-02-01T00:00:01Z").unwrap();
    assert_eq!(run.attempts.len(), 2);
    assert!(
        run.resume("synthetic-auth", "2026-03-01T00:00:00Z")
            .is_err()
    );
    assert!(
        run.dispatch(
            CaseId::ArrayUnion,
            "synthetic-auth",
            "synthetic-session",
            "2026-03-01T00:00:00Z"
        )
        .is_err()
    );
}

#[test]
fn dispatch_and_settlement_cannot_cross_authorization_contexts_or_attempts() {
    let mut first = run(CaseId::ArrayUnion, 1, 10);
    assert!(
        first
            .dispatch(
                CaseId::ArrayUnion,
                "different-auth",
                "synthetic-session",
                START
            )
            .is_err()
    );
    assert!(
        first
            .dispatch(
                CaseId::ArrayUnion,
                "synthetic-auth",
                "different-session",
                START
            )
            .is_err()
    );
    let id = dispatch(&mut first, CaseId::ArrayUnion, START);
    let raw = result(CaseId::ArrayUnion);
    let report = settlement(&first, RefreshTermination::Completed, Some(&raw), ONE);
    assert!(
        first
            .submit(&id, "different-auth", Some(&raw), &report, ONE, ONE)
            .is_err()
    );
    assert!(
        first
            .submit(&id, "synthetic-auth", Some(b"different"), &report, ONE, ONE)
            .is_err()
    );
    assert!(first.blocked());
    first
        .submit(&id, "synthetic-auth", Some(&raw), &report, ONE, ONE)
        .unwrap();
    let saved = serde_json::to_vec(&first).unwrap();
    first
        .submit(&id, "synthetic-auth", Some(&raw), &report, ONE, TWO)
        .unwrap();
    assert_eq!(serde_json::to_vec(&first).unwrap(), saved);
    let mut second = CapabilityRun::prepare(
        environment(),
        "other-auth".into(),
        "synthetic-session".into(),
        vec![CaseId::ArrayUnion],
        &host(),
        1,
        10,
        START,
        0,
    )
    .unwrap();
    second
        .dispatch(CaseId::ArrayUnion, "other-auth", "synthetic-session", START)
        .unwrap();
    assert_ne!(
        first.attempts[0].reservation.id,
        second.attempts[0].reservation.id
    );
    let second_id = second.attempts[0].reservation.id.clone();
    assert!(
        second
            .submit(&second_id, "other-auth", Some(&raw), &report, ONE, ONE)
            .is_err()
    );
}

#[test]
fn reread_revalidates_rehashed_raw_output_and_case_binding() {
    let mut run = run(CaseId::ArrayUnion, 1, 10);
    dispatch(&mut run, CaseId::ArrayUnion, START);
    submit(
        &mut run,
        RefreshTermination::Completed,
        Some(&result(CaseId::ArrayUnion)),
        ONE,
    );
    let mut changed = run.clone();
    changed.attempts[0].case_revision = "different-review".into();
    assert!(changed.validate(0).is_err());
    let mut changed = run.clone();
    let raw = br#"{"result":[{"out[0]":1,"out[1]":1,"out[2]":3}]}"#;
    changed.attempts[0].capture = Some(cqlmatic::inventory::RawSnapshot::new(raw, ONE).unwrap());
    let report = settlement(&changed, RefreshTermination::Completed, Some(raw), ONE);
    changed.attempts[0].reservation.settlement =
        Some(cqlmatic::inventory::RawSnapshot::new(&report, ONE).unwrap());
    assert!(changed.validate(0).is_err());
}

#[test]
fn legacy_states_and_request_serialization_gain_no_capability_claims() {
    for version in 1..=3 {
        let mut state = serde_json::to_value(InventoryState::new(START).unwrap()).unwrap();
        state["schema_version"] = json!(version);
        assert!(state.get("capability_runs").is_none());
        let state: InventoryState = serde_json::from_value(state).unwrap();
        state.validate().unwrap();
        assert!(state.capability_runs.is_empty());
    }
    let context = serde_json::to_value(Context::default()).unwrap();
    assert!(context.get("execution_context_id").is_none());
    assert!(serde_json::from_value::<CaseId>(json!("unknown-probe-kind")).is_err());
    assert!(
        serde_json::from_value::<ResourceBindings>(json!({"kind":"none","query":"arbitrary"}))
            .is_err()
    );
}

#[test]
fn capability_only_writes_preserve_each_admitted_field_reconciliation_commitment() {
    use cqlmatic::inventory::{self, ConfirmedTarget, Progress, TargetScope};
    for version in 1..=3 {
        let mut state = InventoryState::new(START).unwrap();
        state.schema_version = version;
        state.target = Some(ConfirmedTarget {
            environment: environment(),
            scope: TargetScope::FullEnvironment,
            context: "Synthetic retained field reconciliation".into(),
            confirmed_at: START.into(),
        });
        let digest = state.evidence_digest().unwrap();
        state.reconciled_evidence_sha256 = Some(digest.clone());
        state.reconciled_at = Some(START.into());
        assert_eq!(
            inventory::assess(&state, &[]).unwrap().reconciliation,
            Progress::Done
        );
        state.field_evidence_schema_version = Some(state.schema_version);
        state.schema_version = 4;
        state.capability_runs.push(run(CaseId::ArrayUnion, 1, 10));
        state.validate().unwrap();
        assert_eq!(state.evidence_digest().unwrap(), digest, "schema {version}");
        assert_eq!(
            inventory::assess(&state, &[]).unwrap().reconciliation,
            Progress::Done
        );
        let restored: InventoryState =
            serde_json::from_slice(&serde_json::to_vec(&state).unwrap()).unwrap();
        assert_eq!(restored.evidence_digest().unwrap(), digest);
    }
}

#[test]
fn equivalent_names_and_positional_binding_keep_only_the_reviewed_call_form() {
    use cqlmatic::language::{ast::Step, parser};
    for (case, query, expected) in [
        (CaseId::ArrayUnion, "array:union(items,as=unique)", true),
        (
            CaseId::ArrayUnion,
            "array:union(array=\"other\",as=\"unique\")",
            true,
        ),
        (CaseId::ArrayUnion, "array:union(items)", false),
        (
            CaseId::ArrayIntersection,
            "array:union(items,as=unique)",
            false,
        ),
        (
            CaseId::ArrayReduceRow,
            "array:reduceRow(other,var=element,function=sum(field=element),as=total)",
            true,
        ),
        (
            CaseId::ArrayReduceColumn,
            "array:reduceColumn(other,var=element,function=sum(element),as=totals)",
            true,
        ),
        (
            CaseId::ArrayReduceRow,
            "array:reduceRow(other,var=element,function=avg(element),as=total)",
            false,
        ),
        (
            CaseId::ArrayReduceRow,
            "array:reduceRow(other,var=element,function=sum(unrelated),as=total)",
            false,
        ),
        (
            CaseId::ArrayReduceRow,
            "array:reduceRow(other,var=element,function=sum(element,as=amount),as=total)",
            false,
        ),
        (
            CaseId::ArrayReduceRow,
            "array:reduceRow(other,var=element,function={sum(element)},as=total)",
            false,
        ),
        (CaseId::MathSqrt, "math:sqrt(other,as=root)", true),
        (CaseId::MathSqrt, "math:sqrt(other)", false),
        (CaseId::MathSqrt, "math:sqrt(other+1,as=root)", false),
    ] {
        let pipeline = parser::parse(query).unwrap();
        let Step::Call(call) = &pipeline.steps[0] else {
            panic!("expected call")
        };
        assert_eq!(case.matches_call(call), expected, "{query}");
    }
}

fn checked_with_capability(
    query: &str,
    run: &CapabilityRun,
    context: Context,
    now: &str,
) -> cqlmatic::contracts::Analysis {
    app::check_with_evidence(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context,
        },
        None,
        Some(app::GroundingEvidence {
            environment: &run.environment,
            active_pack: None,
            query_observations: &[],
            captured_observations: &[],
            capabilities: Some(CapabilityEvidence {
                runs: std::slice::from_ref(run),
                environment: &run.environment,
                execution_context_id: Some("synthetic-auth"),
                now,
            }),
        }),
    )
}

#[test]
fn check_and_generate_change_only_a_matching_prerequisite_to_a_dated_notice() {
    use cqlmatic::contracts::{GenerateRequest, Severity};
    let mut run = run(CaseId::ArrayUnion, 2, 10);
    dispatch(&mut run, CaseId::ArrayUnion, START);
    submit(
        &mut run,
        RefreshTermination::Completed,
        Some(&result(CaseId::ArrayUnion)),
        ONE,
    );
    let context = Context {
        environment: Some(environment()),
        execution_context_id: Some("synthetic-auth".into()),
        ..Context::default()
    };
    let query = "array:union(array=items,as=unique)";
    let observed = checked_with_capability(query, &run, context.clone(), TWO);
    assert_eq!(
        observed.outcome,
        Outcome::Ready,
        "{:?}",
        observed.diagnostics
    );
    let notice = observed
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "CQL_TARGET_OBSERVED")
        .unwrap();
    assert_eq!(notice.severity, Severity::Info);
    assert!(notice.message.contains(ONE));
    assert!(
        observed
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "CQL_TARGET_REQUIREMENT")
    );
    assert!(
        observed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_FUNCTION_LIFECYCLE")
    );

    let repeated = checked_with_capability(
        "array:union(items,as=out)|array:union(out,as=again)|array:union(again,as=last)",
        &run,
        context.clone(),
        TWO,
    );
    assert_eq!(
        repeated.outcome,
        Outcome::Ready,
        "{:?}",
        repeated.diagnostics
    );
    assert!(
        repeated
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_TARGET_OBSERVED")
    );
    assert!(
        repeated
            .diagnostics
            .iter()
            .all(|d| d.code != "CQL_TARGET_REQUIREMENT")
    );

    for query in [
        "array:union(items,as=out)|array:union(out)",
        "array:union(out)|array:union(items,as=out)",
        "defineTable(name=local,query={array:union(items,as=out)},include=[out])|remoteTable(connection=external,name=t,query={array:union(items,as=out)},include=[out])|readFile(t)",
        "remoteTable(connection=external,name=t,query={array:union(items,as=out)},include=[out])|readFile(t)|array:union(items,as=out)",
        "array:union(items,as=out)|join(query={array:union(items,as=out)},field=host,view=other)",
    ] {
        let mixed = checked_with_capability(query, &run, context.clone(), TWO);
        assert_eq!(
            mixed.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            mixed.diagnostics
        );
        assert!(
            mixed
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_TARGET_REQUIREMENT"
                    && diagnostic.message.starts_with("array:union ")),
            "{query}: {:?}",
            mixed.diagnostics
        );
    }

    for (query, context, now) in [
        (
            query,
            Context {
                execution_context_id: None,
                ..context.clone()
            },
            TWO,
        ),
        (
            query,
            Context {
                execution_context_id: Some("different-auth".into()),
                ..context.clone()
            },
            TWO,
        ),
        (
            query,
            Context {
                environment: Some(EnvironmentId {
                    tenant: "1".repeat(32),
                    ..environment()
                }),
                ..context.clone()
            },
            TWO,
        ),
        (
            query,
            Context {
                environment: Some(EnvironmentId {
                    view: "other".into(),
                    ..environment()
                }),
                ..context.clone()
            },
            TWO,
        ),
        (query, context.clone(), "2026-01-31T00:00:01Z"),
        (query, context.clone(), START),
        (
            "array:intersection(array=items,as=common)",
            context.clone(),
            TWO,
        ),
        ("array:union(items)", context.clone(), TWO),
        (
            "array:union(array=items,unknown=true)",
            context.clone(),
            TWO,
        ),
        ("beta:repeating()", context.clone(), TWO),
        (
            "join(query={array:union(array=items,as=unique)},field=host,view=other)",
            context.clone(),
            TWO,
        ),
        (
            "remoteTable(connection=external,name=t,query={array:union(array=items,as=unique)},include=[unique])|readFile(t)",
            context.clone(),
            TWO,
        ),
    ] {
        let checked = checked_with_capability(query, &run, context, now);
        assert!(
            checked
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_TARGET_REQUIREMENT"),
            "{query}: {:?}",
            checked.diagnostics
        );
        assert!(
            checked
                .diagnostics
                .iter()
                .all(|diagnostic| diagnostic.code != "CQL_TARGET_OBSERVED"),
            "{query}: {:?}",
            checked.diagnostics
        );
    }

    let mut request: GenerateRequest = serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[
        {"op":"call","function":"array:union","args":[{"name":"array","value":{"kind":"literal","value":"items"}},
            {"name":"as","value":{"kind":"literal","value":"unique"}}]}]}}})).unwrap();
    request.context = context;
    let generated = app::generate_with_evidence(
        request.clone(),
        None,
        Some(app::GroundingEvidence {
            environment: &run.environment,
            active_pack: None,
            query_observations: &[],
            captured_observations: &[],
            capabilities: Some(CapabilityEvidence {
                runs: std::slice::from_ref(&run),
                environment: &run.environment,
                execution_context_id: Some("synthetic-auth"),
                now: TWO,
            }),
        }),
    );
    assert_eq!(
        generated.outcome,
        Outcome::Ready,
        "{:?}",
        generated.diagnostics
    );
    assert!(
        generated
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_TARGET_OBSERVED")
    );
    assert!(
        app::generate(request, None)
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_TARGET_REQUIREMENT")
    );
}

#[test]
fn prerequisite_advice_keeps_the_selected_view_when_capability_evidence_is_unavailable() {
    let mut verified = run(CaseId::ArrayUnion, 1, 10);
    dispatch(&mut verified, CaseId::ArrayUnion, START);
    submit(
        &mut verified,
        RefreshTermination::Completed,
        Some(&result(CaseId::ArrayUnion)),
        ONE,
    );
    for evidence_state in [
        "valid",
        "absent",
        "invalid",
        "discarded",
        "other-selected-view",
    ] {
        let mut run = verified.clone();
        if evidence_state == "invalid" {
            run.attempts[0].capture.as_mut().unwrap().sha256 = "0".repeat(64);
        }
        let mut selected = environment();
        if evidence_state == "other-selected-view" {
            selected.view = "other".into();
        }
        let context = Context {
            environment: Some(selected.clone()),
            execution_context_id: Some(
                if evidence_state == "discarded" {
                    "different-auth"
                } else {
                    "synthetic-auth"
                }
                .into(),
            ),
            ..Context::default()
        };
        for view in ["synthetic", "other"] {
            for target_argument in ["view", "repo"] {
                let query = format!(
                    "join(query={{array:union(items,as=out)}},field=host,{target_argument}={view})"
                );
                let checked = if evidence_state == "absent" {
                    app::check(
                        CheckRequest {
                            schema_version: 1,
                            id: None,
                            query,
                            context: context.clone(),
                        },
                        None,
                    )
                } else {
                    checked_with_capability(&query, &run, context.clone(), TWO)
                };
                let observed = evidence_state == "valid" && view == selected.view;
                assert_eq!(
                    checked
                        .diagnostics
                        .iter()
                        .any(|d| d.code == "CQL_TARGET_OBSERVED"),
                    observed,
                    "{evidence_state}/{target_argument}={view}: {:?}",
                    checked.diagnostics
                );
                if observed {
                    continue;
                }
                let warning = checked
                    .diagnostics
                    .iter()
                    .find(|d| {
                        d.code == "CQL_TARGET_REQUIREMENT" && d.message.contains("ArrayFunctions")
                    })
                    .unwrap();
                assert_eq!(warning.severity, cqlmatic::contracts::Severity::Warning);
                assert_eq!(
                    warning
                        .message
                        .contains("Run the optional capability check for this form"),
                    view == selected.view,
                    "{evidence_state}/{target_argument}={view}: {warning:?}"
                );
                assert_eq!(
                    warning
                        .message
                        .contains("capability evidence from a matching selected target"),
                    view != selected.view,
                    "{evidence_state}/{target_argument}={view}: {warning:?}"
                );
                assert!(
                    !warning
                        .message
                        .contains("no automated capability case covers this form")
                );
            }
        }
    }
}

#[test]
fn prerequisite_advice_offers_automated_checks_only_for_covered_call_forms() {
    for (query, covered) in [
        ("array:union(items,as=out)", true),
        ("array:union(items)", false),
        (
            "remoteTable(connection=external,name=t,query={array:union(items,as=out)},include=[out])|readFile(t)",
            false,
        ),
        ("beta:repeating()", false),
    ] {
        let checked = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: query.into(),
                context: Context::default(),
            },
            None,
        );
        assert_eq!(checked.outcome, Outcome::Ready, "{:?}", checked.diagnostics);
        let warning = checked
            .diagnostics
            .iter()
            .find(|d| d.code == "CQL_TARGET_REQUIREMENT")
            .unwrap();
        assert_eq!(
            warning
                .message
                .contains("Run the optional capability check for this form"),
            covered
        );
        assert_eq!(
            warning
                .message
                .contains("no automated capability case covers this form"),
            !covered
        );
        assert_eq!(warning.severity, cqlmatic::contracts::Severity::Warning);
    }
}

fn protected_file(directory: &std::path::Path, name: &str, raw: &[u8]) -> String {
    let path = directory.join(name);
    std::fs::write(&path, raw).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path.to_string_lossy().into_owned()
}

fn inventory_command(directory: &std::path::Path, action: Value) -> std::process::Command {
    let request = protected_file(
        directory,
        "request.json",
        &serde_json::to_vec(&json!({"schema_version":1,"action":action})).unwrap(),
    );
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    command
        .args(["env", "inventory", "--json", "--state"])
        .arg(directory.join("state.json"))
        .arg("--request")
        .arg(request);
    command
}

fn inventory_call(directory: &std::path::Path, action: Value) -> Value {
    let output = inventory_command(directory, action).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-capability-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn optional_cli_phase_preserves_the_active_field_pack_and_blocks_other_dispatch() {
    use cqlmatic::{
        discovery,
        environment::{Pack, PackData},
    };
    let directory = Temp::new();
    let directory = &directory.0;
    let state_file = directory.join("state.json");
    inventory_call(directory, json!({"kind":"init"}));
    inventory_call(
        directory,
        json!({"kind":"confirm-target","environment":environment(),"scope":"full-environment",
        "context":"Synthetic offline capability fixture","expected_sources":[]}),
    );
    let host_file = protected_file(directory, "host.json", &host());
    let preparation = json!({"kind":"prepare-capabilities","execution_context_id":"synthetic-auth","session_id":"synthetic-session",
        "case_ids":["array-union"],"host_evidence_file":host_file,"max_queries":2,"max_seconds":60});
    assert!(
        !inventory_command(directory, preparation.clone())
            .output()
            .unwrap()
            .status
            .success()
    );
    let pack = Pack::new(environment(), START.into(), PackData::default()).unwrap();
    let pack_file = protected_file(
        directory,
        "pack.json",
        &serde_json::to_vec_pretty(&pack).unwrap(),
    );
    inventory_call(
        directory,
        json!({"kind":"import-pack","pack_file":pack_file}),
    );
    let original: InventoryState =
        serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    let active = original.active_pack.as_ref().unwrap();
    let pack_bytes = std::fs::read(&active.path).unwrap();
    let digest = original.evidence_digest().unwrap();
    let prepared = inventory_call(directory, preparation);
    assert_eq!(prepared["action"]["executes_searches"], false);
    let run_id = prepared["action"]["capability"]["run_id"].as_str().unwrap();
    let dispatched = inventory_call(
        directory,
        json!({"kind":"dispatch-capability","run_id":run_id,"case_id":"array-union",
        "execution_context_id":"synthetic-auth","session_id":"synthetic-session"}),
    );
    assert_eq!(dispatched["action"]["executes_searches"], false);
    assert_eq!(dispatched["status"]["capability_blocked"], true);
    let reserved: InventoryState =
        serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    reserved.validate().unwrap();
    assert!(reserved.capability_blocked());
    assert_eq!(reserved.evidence_digest().unwrap(), digest);
    let mut other_host: Value = serde_json::from_slice(&host()).unwrap();
    other_host["session_id"] = json!("other-session");
    let other_host_file = protected_file(
        directory,
        "other-host.json",
        &serde_json::to_vec(&other_host).unwrap(),
    );
    for action in [
        json!({"kind":"dispatch-capability","run_id":run_id,"case_id":"array-union","execution_context_id":"synthetic-auth","session_id":"synthetic-session"}),
        json!({"kind":"prepare-refresh","session_id":"synthetic-session","host_evidence_file":host_file}),
        json!({"kind":"prepare-scout","environment":environment(),"start":START,"end":ONE}),
        json!({"kind":"prepare-capabilities","execution_context_id":"other-auth","session_id":"synthetic-session",
            "case_ids":["array-union"],"host_evidence_file":host_file,"max_queries":2,"max_seconds":60}),
        json!({"kind":"prepare-capabilities","execution_context_id":"synthetic-auth","session_id":"other-session",
            "case_ids":["array-union"],"host_evidence_file":other_host_file,"max_queries":2,"max_seconds":60}),
    ] {
        let output = inventory_command(directory, action).output().unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("capability"));
    }
    let raw = serde_json::to_vec(
        &json!({"isError":true,"content":[{"type":"text","text":"synthetic generic failure"}]}),
    )
    .unwrap();
    let now = discovery::timestamp();
    let run = &reserved.capability_runs[0];
    let report = settlement(run, RefreshTermination::Completed, Some(&raw), &now);
    let result_file = protected_file(directory, "result.json", &raw);
    let settlement_file = protected_file(directory, "settlement.json", &report);
    let submitted = inventory_call(
        directory,
        json!({"kind":"submit-capability","run_id":run_id,
        "attempt_id":run.attempts[0].reservation.id,"execution_context_id":"synthetic-auth",
        "result_file":result_file,"settlement_file":settlement_file,"retrieved_at":now}),
    );
    assert_eq!(submitted["status"]["capability_blocked"], false);
    assert_eq!(
        submitted["action"]["capability"]["cases"][0]["last_outcome"],
        "unknown"
    );
    let after: InventoryState =
        serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    assert_eq!(after.schema_version, 4);
    assert_eq!(
        after.active_pack.as_ref().unwrap().content_digest,
        active.content_digest
    );
    assert_eq!(std::fs::read(&active.path).unwrap(), pack_bytes);
    assert_eq!(after.evidence_digest().unwrap(), digest);
    assert_eq!(
        after.capability_runs[0].attempts[0]
            .capture
            .as_ref()
            .unwrap()
            .bytes()
            .unwrap(),
        raw
    );
    let old_run = serde_json::to_value(&after.capability_runs[0]).unwrap();
    inventory_call(
        directory,
        json!({"kind":"prepare-capabilities","execution_context_id":"other-auth","session_id":"synthetic-session",
        "case_ids":["array-union"],"host_evidence_file":host_file,"max_queries":2,"max_seconds":60}),
    );
    let next: InventoryState =
        serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    assert_eq!(next.capability_runs.len(), 2);
    assert_eq!(
        serde_json::to_value(&next.capability_runs[0]).unwrap(),
        old_run
    );
}

#[test]
fn paused_capability_allowance_is_preserved_when_a_new_binding_is_explicitly_prepared() {
    use cqlmatic::{
        discovery,
        environment::{Pack, PackData},
    };
    for (new_context, new_session) in [
        ("other-auth", "synthetic-session"),
        ("synthetic-auth", "other-session"),
    ] {
        let directory = Temp::new();
        let directory = &directory.0;
        let state_file = directory.join("state.json");
        inventory_call(directory, json!({"kind":"init"}));
        inventory_call(
            directory,
            json!({"kind":"confirm-target","environment":environment(),"scope":"full-environment",
            "context":"Synthetic binding transition","expected_sources":[]}),
        );
        let pack = Pack::new(environment(), START.into(), PackData::default()).unwrap();
        let pack_file = protected_file(directory, "pack.json", &serde_json::to_vec(&pack).unwrap());
        inventory_call(
            directory,
            json!({"kind":"import-pack","pack_file":pack_file}),
        );
        let first_host = protected_file(directory, "first-host.json", &host());
        let first_preparation = json!({"kind":"prepare-capabilities","execution_context_id":"synthetic-auth","session_id":"synthetic-session",
            "case_ids":["array-union"],"host_evidence_file":first_host,"max_queries":2,"max_seconds":60});
        let prepared = inventory_call(directory, first_preparation.clone());
        let run_id = prepared["action"]["capability"]["run_id"].as_str().unwrap();
        inventory_call(
            directory,
            json!({"kind":"pause-capabilities","run_id":run_id}),
        );
        let paused: InventoryState =
            serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
        let old_run = &paused.capability_runs[0];
        assert!(old_run.paused());
        assert!(!old_run.blocked());
        let remaining = old_run.remaining_seconds(&discovery::timestamp()).unwrap();
        assert!(remaining > 0.0);

        let before = std::fs::read(&state_file).unwrap();
        let rejected = inventory_command(directory, first_preparation.clone())
            .output()
            .unwrap();
        assert!(!rejected.status.success());
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("original remaining allowance"));
        assert_eq!(std::fs::read(&state_file).unwrap(), before);

        let mut new_host: Value = serde_json::from_slice(&host()).unwrap();
        new_host["session_id"] = json!(new_session);
        let new_host_file = protected_file(
            directory,
            "next-host.json",
            &serde_json::to_vec(&new_host).unwrap(),
        );
        let next = inventory_call(
            directory,
            json!({"kind":"prepare-capabilities","execution_context_id":new_context,"session_id":new_session,
            "case_ids":["array-union"],"host_evidence_file":new_host_file,"max_queries":3,"max_seconds":120}),
        );
        assert_eq!(next["action"]["executes_searches"], false);
        let new_run_id = next["action"]["capability"]["run_id"].as_str().unwrap();
        let state: InventoryState =
            serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
        assert_eq!(state.capability_runs.len(), 2);
        assert_eq!(
            serde_json::to_value(&state.capability_runs[0]).unwrap(),
            serde_json::to_value(old_run).unwrap()
        );
        assert_eq!(
            state.capability_runs[0]
                .remaining_seconds(&discovery::timestamp())
                .unwrap(),
            remaining
        );
        assert_eq!(state.capability_runs[1].execution_context_id, new_context);
        assert_eq!(state.capability_runs[1].session_id, new_session);
        assert_eq!(state.capability_runs[1].max_queries, 3);

        let before = std::fs::read(&state_file).unwrap();
        let rejected = inventory_command(directory, first_preparation)
            .output()
            .unwrap();
        assert!(
            !rejected.status.success(),
            "a different latest binding must not renew an earlier allowance"
        );
        assert!(String::from_utf8_lossy(&rejected.stderr).contains("original remaining allowance"));
        assert_eq!(std::fs::read(&state_file).unwrap(), before);
        let dispatched = inventory_call(
            directory,
            json!({"kind":"dispatch-capability","run_id":new_run_id,"case_id":"array-union",
            "execution_context_id":new_context,"session_id":new_session}),
        );
        assert_eq!(dispatched["action"]["executes_searches"], false);
        assert_eq!(dispatched["status"]["capability_blocked"], true);
    }
}
