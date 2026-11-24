use cqlmatic::{
    capability::{CapabilityRun, CaseId},
    contracts::{Cloud, Diagnostic, EnvironmentId},
    discovery,
    environment::{Pack, PackData},
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
    thread,
    time::Duration,
};

const START: &str = "2026-01-01T00:00:00Z";
const CONTEXT: &str = "synthetic-auth";
const SESSION: &str = "synthetic-session";

const UNRESOLVED: &str = "An earlier attempt in this capability checkpoint is unresolved. Reconcile it with submit-capability and its terminal settlement before another dispatch.";
const OTHER_CONTEXT: &str = "The execution_context_id differs from the one this capability checkpoint was prepared with. Dispatch with that context, or use prepare-capabilities for the new one.";
const OTHER_SESSION: &str = "The session_id differs from the host session this capability checkpoint was prepared with. Dispatch with that session, or use prepare-capabilities with host evidence for the new session.";
const NOT_IN_RUN: &str = "This case is not in the capability checkpoint. Pause the checkpoint with pause-capabilities, then use prepare-capabilities for the cases it does not hold.";
const NO_ATTEMPTS: &str = "The capability checkpoint has used all of its max_queries attempts. Use prepare-capabilities for a new run; the checkpoint keeps its history.";
const BELOW_LIFETIME: &str = "The capability checkpoint's remaining active time is below the host evidence's max_search_seconds, so it cannot dispatch again. Use prepare-capabilities for a new run; the checkpoint keeps its history.";
const PAUSED: &str = "The capability checkpoint is paused. Resume it with resume-capabilities and its execution_context_id before dispatching.";
const RESUME_RETAINED: &str =
    "Resume the retained capability checkpoint with its original remaining allowance.";
const PAUSE_FIRST: &str = "A retained capability checkpoint for this execution context and session can still dispatch. Pause it with pause-capabilities before preparing cases it does not hold.";
const RECONCILE_FIRST: &str =
    "Reconcile the exact capability attempt before another inventory search.";

fn too_short(lifetime: u64) -> String {
    format!(
        "max_seconds must exceed the host evidence's max_search_seconds ({lifetime}): every dispatch needs that much active time left, and active time counts from preparation. Use at least {}; the time above {lifetime} is what remains for reaching each dispatch.",
        lifetime + 1
    )
}

fn environment() -> EnvironmentId {
    EnvironmentId {
        cloud: Cloud::Us1,
        tenant: "0".repeat(32),
        view: "synthetic".into(),
    }
}

fn host(lifetime: u64, session: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":1,"environment":environment(),
        "session_id":session,"verified_at":START,
        "mechanism":"provider-enforced-search-lifetime","max_search_seconds":lifetime,
        "capability_reference":"Synthetic provider search lifetime",
        "verification_evidence":"Synthetic host verification"}))
    .unwrap()
}

fn prepared(
    cases: Vec<CaseId>,
    lifetime: u64,
    max_queries: u32,
    max_seconds: u64,
) -> Result<CapabilityRun, Diagnostic> {
    CapabilityRun::prepare(
        environment(),
        CONTEXT.into(),
        SESSION.into(),
        cases,
        &host(lifetime, SESSION),
        max_queries,
        max_seconds,
        START,
        0,
    )
}

fn refusal<T: std::fmt::Debug>(result: Result<T, Diagnostic>) -> String {
    let error = result.unwrap_err();
    assert_eq!(error.code, "CAPABILITY_INPUT", "{error:?}");
    error.message
}

fn settle(run: &mut CapabilityRun, now: &str) {
    let attempt = run.attempts.last().unwrap();
    let report = serde_json::to_vec(&json!({"schema_version":1,"attempt_id":attempt.reservation.id,
        "environment":run.environment,"session_id":run.session_id,"arguments":attempt.reservation.arguments,
        "termination":"cancelled","recorded_at":now,"evidence":"Synthetic exact-attempt terminal report"}))
    .unwrap();
    let id = attempt.reservation.id.clone();
    run.submit(&id, CONTEXT, None, &report, now, now).unwrap();
}

#[test]
fn prepare_refuses_max_seconds_that_cannot_dispatch_one_case() {
    for max_seconds in [1, 59, 60] {
        assert_eq!(
            refusal(prepared(vec![CaseId::MathSqrt], 60, 2, max_seconds)),
            too_short(60)
        );
    }
    let mut smallest = prepared(vec![CaseId::MathSqrt], 60, 2, 61).unwrap();
    smallest
        .dispatch(CaseId::MathSqrt, CONTEXT, SESSION, "2026-01-01T00:00:01Z")
        .unwrap();
}

#[test]
fn dispatch_still_refuses_below_the_host_search_lifetime() {
    for pause in [false, true] {
        let mut run = prepared(vec![CaseId::MathSqrt], 60, 2, 61).unwrap();
        if pause {
            run.pause("2026-01-01T00:00:01.000000001Z").unwrap();
        }
        assert_eq!(
            refusal(run.dispatch(
                CaseId::MathSqrt,
                CONTEXT,
                SESSION,
                "2026-01-01T00:00:01.000000001Z"
            )),
            BELOW_LIFETIME
        );
    }
}

#[test]
fn dispatch_names_the_condition_that_stopped_it() {
    let mut run = prepared(vec![CaseId::ArrayUnion, CaseId::MathSqrt], 60, 2, 600).unwrap();
    let at = |second: u32| format!("2026-01-01T00:00:{second:02}Z");
    assert_eq!(
        refusal(run.dispatch(CaseId::ArrayUnion, "other-auth", SESSION, &at(1))),
        OTHER_CONTEXT
    );
    assert_eq!(
        refusal(run.dispatch(CaseId::ArrayUnion, CONTEXT, "other-session", &at(1))),
        OTHER_SESSION
    );
    assert_eq!(
        refusal(run.dispatch(CaseId::ArrayIntersection, CONTEXT, SESSION, &at(1))),
        NOT_IN_RUN
    );
    run.pause(&at(1)).unwrap();
    assert_eq!(
        refusal(run.dispatch(CaseId::ArrayUnion, CONTEXT, SESSION, &at(2))),
        PAUSED
    );
    run.resume(CONTEXT, &at(2)).unwrap();
    run.dispatch(CaseId::ArrayUnion, CONTEXT, SESSION, &at(2))
        .unwrap();
    assert_eq!(
        refusal(run.dispatch(CaseId::MathSqrt, CONTEXT, SESSION, &at(3))),
        UNRESOLVED
    );
    settle(&mut run, &at(3));
    run.dispatch(CaseId::MathSqrt, CONTEXT, SESSION, &at(4))
        .unwrap();
    settle(&mut run, &at(5));
    assert_eq!(
        refusal(run.dispatch(CaseId::MathSqrt, CONTEXT, SESSION, &at(6))),
        NO_ATTEMPTS
    );

    // A paused run points to resuming only while resuming could still lead to a dispatch.
    let mut short = prepared(vec![CaseId::MathSqrt], 60, 2, 61).unwrap();
    short.pause("2026-01-01T00:00:00.5Z").unwrap();
    assert_eq!(
        refusal(short.dispatch(CaseId::MathSqrt, CONTEXT, SESSION, &at(9))),
        PAUSED
    );
    let mut spent = prepared(vec![CaseId::MathSqrt], 60, 2, 61).unwrap();
    spent.pause("2026-01-01T00:00:01.5Z").unwrap();
    assert_eq!(
        refusal(spent.dispatch(CaseId::MathSqrt, CONTEXT, SESSION, &at(9))),
        BELOW_LIFETIME
    );
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "cqlmatic-capability-recovery-{}-{}",
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

fn protected_file(directory: &Path, name: &str, raw: &[u8]) -> String {
    let path = directory.join(name);
    std::fs::write(&path, raw).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    path.to_string_lossy().into_owned()
}

fn inventory_command(directory: &Path, action: Value) -> Command {
    let request = protected_file(
        directory,
        "request.json",
        &serde_json::to_vec(&json!({"schema_version":1,"action":action})).unwrap(),
    );
    let mut command = Command::new(env!("CARGO_BIN_EXE_cqlmatic"));
    command
        .args(["env", "inventory", "--json", "--state"])
        .arg(directory.join("state.json"))
        .arg("--request")
        .arg(request);
    command
}

fn call(directory: &Path, action: Value) -> Value {
    let output = inventory_command(directory, action).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Runs a refused action and returns its diagnostic; the saved inventory must be unchanged.
fn refused(directory: &Path, action: Value) -> Value {
    let state = directory.join("state.json");
    let before = std::fs::read(&state).unwrap();
    let output = inventory_command(directory, action).output().unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(output.stdout.is_empty(), "{output:?}");
    assert_eq!(std::fs::read(&state).unwrap(), before);
    serde_json::from_slice(&output.stderr).unwrap()
}

fn active_inventory() -> Temp {
    let directory = Temp::new();
    call(&directory.0, json!({"kind":"init"}));
    call(
        &directory.0,
        json!({"kind":"confirm-target","environment":environment(),"scope":"full-environment",
        "context":"Synthetic capability recovery","expected_sources":[]}),
    );
    let pack = Pack::new(environment(), START.into(), PackData::default()).unwrap();
    let pack_file = protected_file(
        &directory.0,
        "pack.json",
        &serde_json::to_vec(&pack).unwrap(),
    );
    call(
        &directory.0,
        json!({"kind":"import-pack","pack_file":pack_file}),
    );
    directory
}

fn preparation(
    directory: &Path,
    context: &str,
    session: &str,
    cases: &[&str],
    lifetime: u64,
    max_queries: u32,
    max_seconds: u64,
) -> Value {
    let host_file = protected_file(
        directory,
        &format!("host-{session}-{lifetime}.json"),
        &host(lifetime, session),
    );
    json!({"kind":"prepare-capabilities","execution_context_id":context,"session_id":session,
        "case_ids":cases,"host_evidence_file":host_file,"max_queries":max_queries,"max_seconds":max_seconds})
}

fn prepare(
    directory: &Path,
    cases: &[&str],
    lifetime: u64,
    max_queries: u32,
    max_seconds: u64,
) -> Value {
    preparation(
        directory,
        CONTEXT,
        SESSION,
        cases,
        lifetime,
        max_queries,
        max_seconds,
    )
}

fn dispatch(run_id: &Value, case: &str) -> Value {
    json!({"kind":"dispatch-capability","run_id":run_id,"case_id":case,
        "execution_context_id":CONTEXT,"session_id":SESSION})
}

fn pause(run_id: &Value) -> Value {
    json!({"kind":"pause-capabilities","run_id":run_id})
}

fn cancel(directory: &Path, run_id: &Value, reserved: &Value) -> Value {
    let now = discovery::timestamp();
    let settlement = protected_file(
        directory,
        "settlement.json",
        &serde_json::to_vec(&json!({"schema_version":1,"attempt_id":reserved["action"]["attempt_id"],
            "environment":environment(),"session_id":SESSION,"arguments":reserved["action"]["arguments"],
            "termination":"cancelled","recorded_at":now,"evidence":"Synthetic exact-attempt terminal report"}))
        .unwrap(),
    );
    json!({"kind":"submit-capability","run_id":run_id,"attempt_id":reserved["action"]["attempt_id"],
        "execution_context_id":CONTEXT,"settlement_file":settlement,"retrieved_at":now})
}

fn run_id(prepared: &Value) -> Value {
    prepared["action"]["capability"]["run_id"].clone()
}

fn runs(directory: &Path) -> Vec<Value> {
    let state: Value =
        serde_json::from_slice(&std::fs::read(directory.join("state.json")).unwrap()).unwrap();
    state["capability_runs"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

fn assert_reserved(directory: &Path, run_id: &Value, case: &str) {
    let reserved = call(directory, dispatch(run_id, case));
    assert_eq!(reserved["action"]["phase"], "dispatched");
    assert_eq!(reserved["status"]["capability_blocked"], true);
}

#[test]
fn prepare_refuses_before_retaining_a_run_that_cannot_dispatch() {
    let directory = active_inventory();
    let directory = &directory.0;
    for max_seconds in [299, 300] {
        let error = refused(
            directory,
            prepare(directory, &["math-sqrt"], 300, 4, max_seconds),
        );
        assert_eq!(error["code"], "CAPABILITY_INPUT");
        assert_eq!(error["message"], too_short(300));
    }
    assert!(runs(directory).is_empty());
    let accepted = call(directory, prepare(directory, &["math-sqrt"], 300, 4, 301));
    assert_eq!(accepted["action"]["capability"]["max_seconds"], 301);
}

#[test]
fn a_run_that_cannot_dispatch_again_does_not_block_preparing() {
    for paused in [false, true] {
        let directory = active_inventory();
        let directory = &directory.0;
        let first = run_id(&call(
            directory,
            prepare(directory, &["math-sqrt"], 5, 4, 6),
        ));
        thread::sleep(Duration::from_millis(1500));
        if paused {
            call(directory, pause(&first));
        }
        let retained = runs(directory)[0].clone();
        let second = call(directory, prepare(directory, &["math-sqrt"], 5, 4, 600));
        let after = runs(directory);
        assert_eq!(after.len(), 2);
        assert_eq!(after[0], retained, "the retained run keeps its history");
        let granted = &second["action"]["capability"];
        assert_eq!(granted["max_seconds"], 600);
        assert_eq!(granted["max_queries"], 4);
        assert_eq!(granted["attempts"], 0);
        assert_eq!(
            refused(directory, dispatch(&first, "math-sqrt"))["message"],
            BELOW_LIFETIME
        );
        assert_reserved(directory, &run_id(&second), "math-sqrt");
    }
}

#[test]
fn preparing_still_waits_for_an_unresolved_attempt() {
    let directory = active_inventory();
    let directory = &directory.0;
    let first = run_id(&call(
        directory,
        prepare(directory, &["math-sqrt"], 5, 4, 10),
    ));
    call(directory, dispatch(&first, "math-sqrt"));
    // Past the point where this run could dispatch again, its unresolved attempt still blocks.
    thread::sleep(Duration::from_millis(5500));
    for cases in [&["math-sqrt"][..], &["array-union"][..]] {
        let error = refused(directory, prepare(directory, cases, 5, 4, 600));
        assert_eq!(error["code"], "INVENTORY_INPUT");
        assert_eq!(error["message"], RECONCILE_FIRST);
    }
}

#[test]
fn dispatch_still_waits_for_an_unresolved_attempt() {
    let directory = active_inventory();
    let directory = &directory.0;
    let first = run_id(&call(
        directory,
        prepare(directory, &["array-union", "math-sqrt"], 1, 4, 600),
    ));
    call(directory, dispatch(&first, "array-union"));
    let error = refused(directory, dispatch(&first, "math-sqrt"));
    assert_eq!(error["code"], "INVENTORY_INPUT");
    assert_eq!(error["message"], RECONCILE_FIRST);
}

#[test]
fn a_paused_run_does_not_block_cases_it_does_not_hold() {
    for settled_first in [false, true] {
        let directory = active_inventory();
        let directory = &directory.0;
        let first = run_id(&call(
            directory,
            prepare(directory, &["array-union"], 30, 4, 300),
        ));
        if settled_first {
            let reserved = call(directory, dispatch(&first, "array-union"));
            call(directory, cancel(directory, &first, &reserved));
        }
        call(directory, pause(&first));
        let retained = runs(directory)[0].clone();
        let second = call(directory, prepare(directory, &["math-sqrt"], 30, 4, 300));
        assert_eq!(runs(directory)[0], retained);
        assert_eq!(
            refused(directory, dispatch(&first, "math-sqrt"))["message"],
            NOT_IN_RUN
        );
        for cases in [&["array-union"][..], &["array-union", "math-sqrt"][..]] {
            assert_eq!(
                refused(directory, prepare(directory, cases, 30, 4, 300))["message"],
                RESUME_RETAINED,
                "the same cases reuse the retained allowance"
            );
        }
        assert_reserved(directory, &run_id(&second), "math-sqrt");
    }
}

#[test]
fn an_active_run_is_paused_before_other_cases_are_prepared() {
    let directory = active_inventory();
    let directory = &directory.0;
    let first = run_id(&call(
        directory,
        prepare(directory, &["array-union"], 30, 4, 300),
    ));
    assert_eq!(
        refused(directory, prepare(directory, &["math-sqrt"], 30, 4, 300))["message"],
        PAUSE_FIRST
    );
    call(directory, pause(&first));
    let second = call(directory, prepare(directory, &["math-sqrt"], 30, 4, 300));
    assert_reserved(directory, &run_id(&second), "math-sqrt");
}

#[test]
fn dispatch_refusals_lead_to_a_next_action_that_works() {
    let directory = active_inventory();
    let directory = &directory.0;
    let first = run_id(&call(
        directory,
        prepare(directory, &["array-union"], 1, 1, 600),
    ));

    let mut other_context = dispatch(&first, "array-union");
    other_context["execution_context_id"] = json!("other-auth");
    assert_eq!(refused(directory, other_context)["message"], OTHER_CONTEXT);
    call(
        directory,
        preparation(
            directory,
            "other-auth",
            SESSION,
            &["array-union"],
            1,
            1,
            600,
        ),
    );

    let mut other_session = dispatch(&first, "array-union");
    other_session["session_id"] = json!("other-session");
    assert_eq!(refused(directory, other_session)["message"], OTHER_SESSION);
    call(
        directory,
        preparation(
            directory,
            CONTEXT,
            "other-session",
            &["array-union"],
            1,
            1,
            600,
        ),
    );

    call(directory, pause(&first));
    assert_eq!(
        refused(directory, dispatch(&first, "array-union"))["message"],
        PAUSED
    );
    call(
        directory,
        json!({"kind":"resume-capabilities","run_id":first,"execution_context_id":CONTEXT}),
    );
    let reserved = call(directory, dispatch(&first, "array-union"));
    call(directory, cancel(directory, &first, &reserved));

    assert_eq!(
        refused(directory, dispatch(&first, "array-union"))["message"],
        NO_ATTEMPTS
    );
    let next = run_id(&call(
        directory,
        prepare(directory, &["array-union"], 1, 2, 600),
    ));

    assert_eq!(
        refused(directory, dispatch(&next, "math-sqrt"))["message"],
        NOT_IN_RUN
    );
    call(directory, pause(&next));
    let other = call(directory, prepare(directory, &["math-sqrt"], 1, 2, 600));
    assert_reserved(directory, &run_id(&other), "math-sqrt");
}
