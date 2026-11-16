//! Bounded, opt-in function cases. Evidence qualifies only its exact target and caller context.
use crate::{
    contracts::{Diagnostic, EnvironmentId},
    environment,
    inventory::{
        self, RawSnapshot, RefreshAttempt, RefreshHostEvidence, RefreshPhase, RefreshSettlement,
        RefreshTermination, SearchArguments,
    },
    mcp, sha256_hex,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const MAX_RUNS: usize = 100;
pub const MAX_ATTEMPTS: u32 = 1000;
pub const MAX_SECONDS: u64 = 14_400;
pub const EVIDENCE_DAYS: i64 = 30;
// Bump when verifier or normalizer acceptance semantics change, even if case inputs stay fixed.
const VERIFIER_SEMANTICS_REVISION: &str = "capability-case-v1";

#[cfg(test)]
std::thread_local! {
    pub(crate) static QUALIFICATION_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn fail(message: &str) -> Diagnostic {
    Diagnostic::error("CAPABILITY_INPUT", message)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum CaseId {
    ArrayUnion,
    ArrayIntersection,
    ArrayReduceRow,
    ArrayReduceColumn,
    MathSqrt,
}

impl CaseId {
    pub fn matches_call(self, call: &crate::language::ast::Call) -> bool {
        use crate::language::ast::Value;
        let Some(resolved) = crate::catalog::resolve(call) else {
            return false;
        };
        let keys: &[&str] = match self {
            Self::ArrayUnion | Self::ArrayIntersection => &["array", "as"],
            Self::ArrayReduceRow | Self::ArrayReduceColumn => &["array", "as", "var", "function"],
            Self::MathSqrt => &["field", "as"],
        };
        let args = &resolved.arguments;
        if resolved.function.name != self.function()
            || !resolved.diagnostics.is_empty()
            || args.len() != keys.len()
            || !keys.iter().all(|key| args.contains_key(*key))
            || args
                .iter()
                .filter(|(key, _)| !matches!(key.as_str(), "function" | "field"))
                .any(|(_, value)| !matches!(value, Value::Word(_) | Value::String(_)))
        {
            return false;
        }
        match self {
            Self::ArrayUnion | Self::ArrayIntersection => true,
            Self::MathSqrt => matches!(args.get("field"), Some(Value::Word(_))),
            Self::ArrayReduceRow | Self::ArrayReduceColumn => {
                let Some(Value::Call(call)) = args.get("function") else {
                    return false;
                };
                let Some(sum) = crate::catalog::resolve(call) else {
                    return false;
                };
                sum.function.name == "sum"
                    && sum.diagnostics.is_empty()
                    && sum.arguments.len() == 1
                    && matches!(
                        sum.arguments.get("field"),
                        Some(Value::Word(_) | Value::String(_))
                    )
                    && sum.arguments.get("field").and_then(|value| value.text())
                        == args.get("var").and_then(|value| value.text())
            }
        }
    }

    pub fn function(self) -> &'static str {
        match self {
            Self::ArrayUnion => "array:union",
            Self::ArrayIntersection => "array:intersection",
            Self::ArrayReduceRow => "array:reduceRow",
            Self::ArrayReduceColumn => "array:reduceColumn",
            Self::MathSqrt => "math:sqrt",
        }
    }

    pub fn query(self) -> &'static str {
        match self {
            Self::ArrayUnion => {
                r#"createEvents(["{\"values\":[1,2]}","{\"values\":[2,3]}"]) | parseJson() | array:union(array=values, as=out) | select([out[0], out[1], out[2], out[3]])"#
            }
            Self::ArrayIntersection => {
                r#"createEvents(["{\"values\":[1,2]}","{\"values\":[2,3]}"]) | parseJson() | array:intersection(array=values, as=out) | select([out[0], out[1]])"#
            }
            Self::ArrayReduceRow => {
                r#"createEvents(["{\"values\":[1,2]}"]) | parseJson() | array:reduceRow(array=values, var=x, function=sum(x), as=out) | select([out._sum])"#
            }
            Self::ArrayReduceColumn => {
                r#"createEvents(["{\"values\":[1,2]}","{\"values\":[3,4]}"]) | parseJson() | array:reduceColumn(array=values, var=x, function=sum(x), as=out) | select([out[0], out[1], out[2]])"#
            }
            Self::MathSqrt => {
                r#"createEvents(["{\"value\":9}"]) | parseJson() | math:sqrt(value, as=out) | select([out])"#
            }
        }
    }

    pub fn revision(self) -> String {
        sha256_hex(
            format!(
                "{VERIFIER_SEMANTICS_REVISION}\n{}\n{:?}",
                self.query(),
                self.expected()
            )
            .as_bytes(),
        )
    }

    fn expected(self) -> Vec<(&'static str, i64)> {
        match self {
            Self::ArrayUnion => vec![("out[0]", 1), ("out[1]", 2), ("out[2]", 3)],
            Self::ArrayIntersection => vec![("out[0]", 2)],
            Self::ArrayReduceRow => vec![("out._sum", 3)],
            Self::ArrayReduceColumn => vec![("out[0]", 4), ("out[1]", 6)],
            Self::MathSqrt => vec![("out", 3)],
        }
    }

    fn verifies(self, raw: &[u8]) -> bool {
        let Ok(result) = mcp::normalize(raw) else {
            return false;
        };
        let [row] = result.rows.as_slice() else {
            return false;
        };
        let expected = self.expected();
        if row.len() != expected.len() {
            return false;
        }
        let integer = |value: &Value| {
            if let Some(text) = value.as_str() {
                let integer = if let Some((integer, fraction)) = text.split_once('.') {
                    if fraction.is_empty() || !fraction.bytes().all(|byte| byte == b'0') {
                        return None;
                    }
                    integer
                } else {
                    text
                };
                return integer.parse::<i64>().ok();
            }
            let number = value.as_f64()?;
            expected
                .iter()
                .map(|(_, value)| *value)
                .find(|value| number == *value as f64)
        };
        if self == Self::ArrayUnion {
            let actual = expected
                .iter()
                .map(|(field, _)| row.get(*field).and_then(integer))
                .collect::<Option<BTreeSet<_>>>();
            actual == Some(BTreeSet::from([1, 2, 3]))
        } else {
            expected
                .iter()
                .all(|(field, value)| row.get(*field).and_then(integer) == Some(*value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ResourceBindings {
    None {},
}

impl Default for ResourceBindings {
    fn default() -> Self {
        Self::None {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum CapabilityOutcome {
    Verified,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CapabilityAttempt {
    pub case_id: CaseId,
    pub case_revision: String,
    pub resources: ResourceBindings,
    pub query_sha256: String,
    pub reservation: RefreshAttempt,
    pub capture: Option<RawSnapshot>,
    pub outcome: CapabilityOutcome,
}

impl CapabilityAttempt {
    fn settlement(&self) -> Result<Option<RefreshSettlement>, Diagnostic> {
        self.reservation
            .settlement
            .as_ref()
            .map(|snapshot| {
                serde_json::from_slice(&snapshot.bytes()?)
                    .map_err(|_| fail("Invalid saved capability settlement."))
            })
            .transpose()
    }

    pub fn blocked(&self) -> bool {
        matches!(
            self.reservation.phase,
            RefreshPhase::Dispatched | RefreshPhase::Unresolved
        )
    }

    fn outcome(&self) -> Result<CapabilityOutcome, Diagnostic> {
        Ok(
            if self.case_revision == self.case_id.revision()
                && self
                    .settlement()?
                    .is_some_and(|report| report.termination == RefreshTermination::Completed)
                && self.capture.as_ref().is_some_and(|capture| {
                    capture.bytes().is_ok_and(|raw| self.case_id.verifies(&raw))
                })
            {
                CapabilityOutcome::Verified
            } else {
                CapabilityOutcome::Unknown
            },
        )
    }

    fn validate(&self, run: &CapabilityRun) -> Result<(), Diagnostic> {
        let reservation = &self.reservation;
        if !run.case_ids.contains(&self.case_id)
            || run.case_revision(self.case_id) != Some(self.case_revision.as_str())
            || self.query_sha256 != sha256_hex(reservation.arguments.query_string.as_bytes())
            || reservation.environment != run.environment
            || reservation.session_id != run.session_id
            || reservation.arguments.repository != run.environment.view
            || self.case_revision == self.case_id.revision()
                && reservation.arguments.query_string != self.case_id.query()
            || reservation.arguments.query_string.contains('\0')
            || reservation.arguments.query_string.encode_utf16().count()
                > crate::contracts::MAX_QUERY_UTF16
            || reservation.host_evidence.sha256 != run.host_evidence.sha256
            || reservation.host_evidence.retrieved_at != run.host_evidence.retrieved_at
            || reservation.automatic
            || reservation.result.is_some()
            || reservation.dispatched_at.as_deref() != Some(&reservation.prepared_at)
            || environment::parse_time(&reservation.arguments.end)?
                != environment::parse_time(&reservation.prepared_at)?
            || environment::parse_time(&reservation.arguments.start)?
                >= environment::parse_time(&reservation.arguments.end)?
            || environment::parse_time(&reservation.arguments.end)?
                - environment::parse_time(&reservation.arguments.start)?
                != time::Duration::minutes(1)
        {
            return Err(fail(
                "Capability attempt differs from its exact reviewed case and authorization binding.",
            ));
        }
        let host: RefreshHostEvidence = serde_json::from_slice(&reservation.host_evidence.bytes()?)
            .map_err(|_| fail("Invalid saved capability host evidence."))?;
        inventory::validate_refresh_host(
            &host,
            &run.environment,
            &run.session_id,
            false,
            &reservation.prepared_at,
        )?;
        let report = self.settlement()?;
        let mut last_settlement = environment::parse_time(&reservation.prepared_at)?;
        for snapshot in reservation
            .settlement_history
            .iter()
            .chain(reservation.settlement.iter())
        {
            let report: RefreshSettlement = serde_json::from_slice(&snapshot.bytes()?)
                .map_err(|_| fail("Invalid capability settlement history."))?;
            inventory::validate_refresh_settlement(reservation, &report, &snapshot.retrieved_at)?;
            let retrieved = environment::parse_time(&snapshot.retrieved_at)?;
            if retrieved < last_settlement {
                return Err(fail(
                    "Capability settlement captures must remain in retrieval order.",
                ));
            }
            last_settlement = retrieved;
        }
        if reservation.settlement_history.len() > 20
            || reservation.settlement_history.iter().any(|snapshot| {
                !serde_json::from_slice::<RefreshSettlement>(&snapshot.bytes().unwrap_or_default())
                    .is_ok_and(|report| report.termination == RefreshTermination::Unresolved)
            })
        {
            return Err(fail(
                "Invalid capability settlement reconciliation history.",
            ));
        }
        let valid_phase = match (reservation.phase, report.as_ref()) {
            (RefreshPhase::Dispatched, None) => true,
            (RefreshPhase::Unresolved, Some(report)) => {
                report.termination == RefreshTermination::Unresolved
            }
            (RefreshPhase::Settled, Some(report)) => {
                report.termination != RefreshTermination::Unresolved
            }
            _ => false,
        };
        if !valid_phase {
            return Err(fail(
                "Invalid capability dispatch or settlement transition.",
            ));
        }
        if let Some(capture) = &self.capture {
            capture.bytes()?;
            if environment::parse_time(&capture.retrieved_at)?
                < environment::parse_time(&reservation.prepared_at)?
                || environment::parse_time(&capture.retrieved_at)? > last_settlement
            {
                return Err(fail("Capability result cannot precede dispatch."));
            }
        }
        if report
            .as_ref()
            .is_some_and(|report| report.termination == RefreshTermination::Completed)
            && self.capture.as_ref().is_none_or(|capture| {
                report
                    .as_ref()
                    .and_then(|report| report.result_sha256.as_ref())
                    != Some(&capture.sha256)
            })
        {
            return Err(fail(
                "Completed capability settlement requires its exact full capture.",
            ));
        }
        if self.case_revision == self.case_id.revision() && self.outcome != self.outcome()? {
            return Err(fail(
                "Capability outcome differs from replayed expected-output validation.",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
struct ActivePeriod {
    start: String,
    end: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CapabilityRun {
    pub id: String,
    pub environment: EnvironmentId,
    pub execution_context_id: String,
    pub session_id: String,
    pub case_ids: Vec<CaseId>,
    pub case_revisions: Vec<String>,
    pub max_queries: u32,
    pub max_seconds: u64,
    pub prepared_at: String,
    pub host_evidence: RawSnapshot,
    pub attempts: Vec<CapabilityAttempt>,
    periods: Vec<ActivePeriod>,
    pause_requested: bool,
    ordinal: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CapabilityStatus {
    pub run_id: String,
    pub execution_context_id: String,
    pub session_id: String,
    pub max_queries: u32,
    pub attempts: usize,
    pub max_seconds: u64,
    pub paused: bool,
    pub termination_pending: bool,
    pub cases: Vec<CaseStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CaseStatus {
    pub case_id: CaseId,
    pub case_revision: String,
    pub last_outcome: Option<CapabilityOutcome>,
    pub last_attempt_id: Option<String>,
    pub retrieved_at: Option<String>,
    pub result_sha256: Option<String>,
}

impl CapabilityRun {
    pub fn status(&self) -> CapabilityStatus {
        CapabilityStatus {
            run_id: self.id.clone(),
            execution_context_id: self.execution_context_id.clone(),
            session_id: self.session_id.clone(),
            max_queries: self.max_queries,
            attempts: self.attempts.len(),
            max_seconds: self.max_seconds,
            paused: self.paused(),
            termination_pending: self.blocked(),
            cases: self
                .case_ids
                .iter()
                .map(|case_id| {
                    let last = self
                        .attempts
                        .iter()
                        .rev()
                        .find(|attempt| attempt.case_id == *case_id);
                    CaseStatus {
                        case_id: *case_id,
                        case_revision: self.case_revision(*case_id).unwrap_or_default().into(),
                        last_outcome: last
                            .map(|attempt| attempt.outcome().unwrap_or(CapabilityOutcome::Unknown)),
                        last_attempt_id: last.map(|attempt| attempt.reservation.id.clone()),
                        retrieved_at: last.and_then(|attempt| {
                            attempt
                                .capture
                                .as_ref()
                                .map(|capture| capture.retrieved_at.clone())
                        }),
                        result_sha256: last.and_then(|attempt| {
                            attempt
                                .capture
                                .as_ref()
                                .map(|capture| capture.sha256.clone())
                        }),
                    }
                })
                .collect(),
        }
    }

    fn identity(&self, index: usize) -> Result<String, Diagnostic> {
        serde_json::to_vec(&(
            &self.environment,
            &self.execution_context_id,
            &self.session_id,
            &self.case_ids,
            &self.case_revisions,
            self.max_queries,
            self.max_seconds,
            &self.prepared_at,
            &self.host_evidence.sha256,
            index,
        ))
        .map(|raw| format!("capability:{}", sha256_hex(&raw)))
        .map_err(|_| fail("Cannot fingerprint capability authorization."))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare(
        environment: EnvironmentId,
        execution_context_id: String,
        session_id: String,
        case_ids: Vec<CaseId>,
        host_raw: &[u8],
        max_queries: u32,
        max_seconds: u64,
        now: &str,
        index: usize,
    ) -> Result<Self, Diagnostic> {
        if host_raw.len() > 64 * 1024 {
            return Err(fail(
                "Capability host evidence exceeds its bounded input limit.",
            ));
        }
        environment::validate_identity(&environment)?;
        text(&execution_context_id)?;
        text(&session_id)?;
        if case_ids.is_empty()
            || case_ids.len() != case_ids.iter().collect::<BTreeSet<_>>().len()
            || !(1..=MAX_ATTEMPTS).contains(&max_queries)
            || !(1..=MAX_SECONDS).contains(&max_seconds)
            || index >= MAX_RUNS
        {
            return Err(fail(
                "Choose distinct reviewed cases and finite capability budgets.",
            ));
        }
        let host: RefreshHostEvidence = serde_json::from_slice(host_raw)
            .map_err(|_| fail("Expected strict protected provider-lifetime evidence."))?;
        inventory::validate_refresh_host(&host, &environment, &session_id, false, now)?;
        let mut run = Self {
            id: String::new(),
            environment,
            execution_context_id,
            session_id,
            case_revisions: case_ids.iter().map(|case_id| case_id.revision()).collect(),
            case_ids,
            max_queries,
            max_seconds,
            prepared_at: now.into(),
            host_evidence: RawSnapshot::new(host_raw, now)?,
            attempts: vec![],
            periods: vec![ActivePeriod {
                start: now.into(),
                end: None,
            }],
            pause_requested: false,
            ordinal: index,
        };
        run.id = run.identity(index)?;
        run.validate(index)?;
        Ok(run)
    }

    pub fn blocked(&self) -> bool {
        self.attempts.iter().any(CapabilityAttempt::blocked)
    }

    fn case_revision(&self, case_id: CaseId) -> Option<&str> {
        self.case_ids
            .iter()
            .position(|id| *id == case_id)
            .and_then(|index| self.case_revisions.get(index))
            .map(String::as_str)
    }

    pub fn matches_current_cases(&self) -> bool {
        self.case_ids
            .iter()
            .all(|case_id| self.case_revision(*case_id) == Some(case_id.revision().as_str()))
    }

    pub fn paused(&self) -> bool {
        self.periods
            .last()
            .is_some_and(|period| period.end.is_some())
    }

    fn active_nanos(&self, now: &str) -> Result<u64, Diagnostic> {
        let now = environment::parse_time(now)?;
        let mut total = 0_u64;
        for period in &self.periods {
            let start = environment::parse_time(&period.start)?;
            if now < start {
                break;
            }
            let end = period
                .end
                .as_deref()
                .map(environment::parse_time)
                .transpose()?
                .unwrap_or(now)
                .min(now);
            let nanos = u64::try_from((end - start).whole_nanoseconds())
                .map_err(|_| fail("Invalid capability active-time period."))?;
            total = total
                .checked_add(nanos)
                .ok_or_else(|| fail("Capability active-time budget overflow."))?;
        }
        Ok(total)
    }

    pub fn remaining_seconds(&self, now: &str) -> Result<f64, Diagnostic> {
        self.ensure_now(now)?;
        Ok((self
            .max_seconds
            .saturating_mul(1_000_000_000)
            .saturating_sub(self.active_nanos(now)?) as f64)
            / 1_000_000_000.0)
    }

    fn ensure_now(&self, now: &str) -> Result<(), Diagnostic> {
        let now = environment::parse_time(now)?;
        let latest_period = self
            .periods
            .last()
            .map(|period| period.end.as_deref().unwrap_or(&period.start))
            .unwrap_or(&self.prepared_at);
        if now < environment::parse_time(latest_period)?
            || self.attempts.last().is_some_and(|attempt| {
                std::iter::once(attempt.reservation.prepared_at.as_str())
                    .chain(
                        attempt
                            .reservation
                            .settlement
                            .iter()
                            .map(|snapshot| snapshot.retrieved_at.as_str()),
                    )
                    .chain(
                        attempt
                            .capture
                            .iter()
                            .map(|snapshot| snapshot.retrieved_at.as_str()),
                    )
                    .any(|at| environment::parse_time(at).is_ok_and(|at| at > now))
            })
        {
            return Err(fail("Capability clock moved before retained evidence."));
        }
        Ok(())
    }

    fn attempt_id(
        &self,
        reservation: &RefreshAttempt,
        case_id: CaseId,
        index: usize,
    ) -> Result<String, Diagnostic> {
        let reservation_id = inventory::refresh_id(reservation, index)?;
        serde_json::to_vec(&(
            &self.id,
            case_id,
            self.case_revision(case_id)
                .ok_or_else(|| fail("Unknown authorized capability case."))?,
            ResourceBindings::None {},
            reservation_id,
        ))
        .map(|raw| format!("capability-attempt:{}", sha256_hex(&raw)))
        .map_err(|_| fail("Cannot fingerprint capability reservation."))
    }

    pub fn validate(&self, index: usize) -> Result<(), Diagnostic> {
        environment::validate_identity(&self.environment)?;
        text(&self.execution_context_id)?;
        text(&self.session_id)?;
        environment::parse_time(&self.prepared_at)?;
        if environment::parse_time(&self.host_evidence.retrieved_at)?
            != environment::parse_time(&self.prepared_at)?
        {
            return Err(fail(
                "Capability host capture must bind its original preparation time.",
            ));
        }
        if self.ordinal != index
            || self.id != self.identity(index)?
            || self.case_ids.is_empty()
            || self.case_revisions.len() != self.case_ids.len()
            || self.case_revisions.iter().any(|revision| {
                revision.len() != 64 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || self.case_ids.len() != self.case_ids.iter().collect::<BTreeSet<_>>().len()
            || !(1..=MAX_ATTEMPTS).contains(&self.max_queries)
            || !(1..=MAX_SECONDS).contains(&self.max_seconds)
            || self.attempts.len() > self.max_queries as usize
            || self.periods.is_empty()
            || self.periods.len() > 4096
            || self.periods[0].start != self.prepared_at
            || self
                .attempts
                .iter()
                .filter(|attempt| attempt.blocked())
                .count()
                > 1
        {
            return Err(fail(
                "Invalid capability checkpoint identity, budget or active history.",
            ));
        }
        let host: RefreshHostEvidence = serde_json::from_slice(&self.host_evidence.bytes()?)
            .map_err(|_| fail("Invalid saved capability host evidence."))?;
        inventory::validate_refresh_host(
            &host,
            &self.environment,
            &self.session_id,
            false,
            &self.prepared_at,
        )?;
        let mut previous = environment::parse_time(&self.prepared_at)?;
        for (index, period) in self.periods.iter().enumerate() {
            let start = environment::parse_time(&period.start)?;
            if start < previous || index + 1 < self.periods.len() && period.end.is_none() {
                return Err(fail(
                    "Capability active periods must be ordered and non-overlapping.",
                ));
            }
            previous = period
                .end
                .as_deref()
                .map(environment::parse_time)
                .transpose()?
                .unwrap_or(start);
            if previous < start {
                return Err(fail("Capability active period ends before it starts."));
            }
        }
        if self.paused() && self.blocked() || self.pause_requested && !self.blocked() {
            return Err(fail(
                "Unresolved capability attempts keep their active budget running.",
            ));
        }
        let mut prior_termination = environment::parse_time(&self.prepared_at)?;
        for (index, attempt) in self.attempts.iter().enumerate() {
            attempt.validate(self)?;
            let at = environment::parse_time(&attempt.reservation.prepared_at)?;
            if at < prior_termination
                || attempt.reservation.id
                    != self.attempt_id(&attempt.reservation, attempt.case_id, index)?
                || !self.periods.iter().any(|period| {
                    environment::parse_time(&period.start).is_ok_and(|start| at >= start)
                        && period.end.as_deref().is_none_or(|end| {
                            environment::parse_time(end).is_ok_and(|end| at <= end)
                        })
                })
                || self.active_nanos(&attempt.reservation.prepared_at)?
                    >= self.max_seconds * 1_000_000_000
            {
                return Err(fail(
                    "Capability reservations must follow settlement within the original active allowance.",
                ));
            }
            prior_termination = attempt
                .reservation
                .settlement
                .as_ref()
                .map(|snapshot| environment::parse_time(&snapshot.retrieved_at))
                .transpose()?
                .unwrap_or(at);
            if attempt.blocked() && index + 1 < self.attempts.len() {
                return Err(fail(
                    "An unresolved capability reservation blocks subsequent dispatch.",
                ));
            }
        }
        Ok(())
    }

    pub fn dispatch(
        &mut self,
        case_id: CaseId,
        execution_context_id: &str,
        session_id: &str,
        now: &str,
    ) -> Result<&CapabilityAttempt, Diagnostic> {
        self.validate(self.ordinal)?;
        let host: RefreshHostEvidence = serde_json::from_slice(&self.host_evidence.bytes()?)
            .map_err(|_| fail("Invalid saved capability host evidence."))?;
        if self.blocked()
            || self.paused()
            || !self.case_ids.contains(&case_id)
            || self.case_revision(case_id) != Some(case_id.revision().as_str())
            || execution_context_id != self.execution_context_id
            || session_id != self.session_id
            || self.attempts.len() >= self.max_queries as usize
            || self.remaining_seconds(now)? < host.max_search_seconds as f64
        {
            return Err(fail(
                "Dispatch requires the prepared context, reviewed case, settled prior attempts and remaining original budget.",
            ));
        }
        if self
            .attempts
            .last()
            .and_then(|attempt| attempt.reservation.settlement.as_ref())
            .is_some_and(|settlement| {
                environment::parse_time(&settlement.retrieved_at)
                    .is_ok_and(|at| environment::parse_time(now).is_ok_and(|now| now < at))
            })
        {
            return Err(fail(
                "Dispatch cannot precede retained settlement evidence.",
            ));
        }
        let end = environment::parse_time(now)?;
        let start = (end - time::Duration::minutes(1))
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| fail("Cannot format capability search window."))?;
        let mut reservation = RefreshAttempt {
            id: String::new(),
            environment: self.environment.clone(),
            session_id: self.session_id.clone(),
            arguments: SearchArguments {
                repository: self.environment.view.clone(),
                query_string: case_id.query().into(),
                start,
                end: now.into(),
            },
            automatic: false,
            host_evidence: self.host_evidence.clone(),
            prepared_at: now.into(),
            dispatched_at: Some(now.into()),
            phase: RefreshPhase::Dispatched,
            settlement: None,
            settlement_history: vec![],
            result: None,
        };
        reservation.id = self.attempt_id(&reservation, case_id, self.attempts.len())?;
        self.attempts.push(CapabilityAttempt {
            case_id,
            case_revision: case_id.revision(),
            resources: ResourceBindings::None {},
            query_sha256: sha256_hex(case_id.query().as_bytes()),
            reservation,
            capture: None,
            outcome: CapabilityOutcome::Unknown,
        });
        Ok(self.attempts.last().expect("just reserved"))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn submit(
        &mut self,
        attempt_id: &str,
        execution_context_id: &str,
        raw: Option<&[u8]>,
        settlement_raw: &[u8],
        retrieved_at: &str,
        now: &str,
    ) -> Result<(), Diagnostic> {
        if settlement_raw.len() > 64 * 1024 {
            return Err(fail(
                "Capability settlement exceeds its bounded input limit.",
            ));
        }
        self.validate(self.ordinal)?;
        if execution_context_id != self.execution_context_id
            || environment::parse_time(retrieved_at)? > environment::parse_time(now)?
        {
            return Err(fail(
                "Capability submission requires the exact context and actual retrieval time.",
            ));
        }
        self.ensure_now(now)?;
        let mut candidate = self.clone();
        let index = candidate
            .attempts
            .iter()
            .position(|attempt| attempt.reservation.id == attempt_id)
            .ok_or_else(|| fail("Unknown capability attempt."))?;
        let attempt = &mut candidate.attempts[index];
        let report: RefreshSettlement = serde_json::from_slice(settlement_raw)
            .map_err(|_| fail("Expected strict exact-attempt settlement evidence."))?;
        inventory::validate_refresh_settlement(&attempt.reservation, &report, now)?;
        if attempt
            .reservation
            .settlement
            .as_ref()
            .is_some_and(|snapshot| snapshot.sha256 == sha256_hex(settlement_raw))
            && raw.is_none_or(|raw| {
                attempt.capture.as_ref().is_some_and(|capture| {
                    capture.sha256 == sha256_hex(raw) && capture.retrieved_at == retrieved_at
                })
            })
        {
            return Ok(());
        }
        if matches!(
            attempt.reservation.phase,
            RefreshPhase::Settled | RefreshPhase::Imported
        ) {
            return Err(fail(
                "Capability attempt is already settled; prepare a distinct budgeted attempt to retry.",
            ));
        }
        if let Some(previous) = attempt.reservation.settlement.take() {
            if attempt.reservation.settlement_history.len() >= 20 {
                return Err(fail(
                    "Capability reconciliation history reached its finite limit.",
                ));
            }
            attempt.reservation.settlement_history.push(previous);
        }
        attempt.reservation.phase = if report.termination == RefreshTermination::Unresolved {
            RefreshPhase::Unresolved
        } else {
            RefreshPhase::Settled
        };
        attempt.reservation.settlement = Some(RawSnapshot::new(settlement_raw, now)?);
        if let Some(raw) = raw {
            let capture = RawSnapshot::new(raw, retrieved_at)?;
            if attempt.capture.as_ref().is_some_and(|old| {
                old.sha256 != capture.sha256 || old.retrieved_at != capture.retrieved_at
            }) {
                return Err(fail(
                    "Capability capture conflicts with the retained exact result.",
                ));
            }
            attempt.capture = Some(capture);
        }
        attempt.outcome = attempt.outcome()?;
        if candidate.pause_requested && !candidate.blocked() {
            candidate
                .periods
                .last_mut()
                .expect("validated active period")
                .end = Some(now.into());
            candidate.pause_requested = false;
        }
        candidate.validate(candidate.ordinal)?;
        *self = candidate;
        Ok(())
    }

    pub fn pause(&mut self, now: &str) -> Result<(), Diagnostic> {
        self.validate(self.ordinal)?;
        self.ensure_now(now)?;
        if self.blocked() {
            self.pause_requested = true;
        } else if let Some(period) = self.periods.last_mut()
            && period.end.is_none()
        {
            period.end = Some(now.into());
        }
        Ok(())
    }

    pub fn resume(&mut self, execution_context_id: &str, now: &str) -> Result<(), Diagnostic> {
        self.validate(self.ordinal)?;
        if self.blocked()
            || execution_context_id != self.execution_context_id
            || self.remaining_seconds(now)? <= 0.0
            || self.attempts.len() >= self.max_queries as usize
            || self.periods.len() >= 4096
        {
            return Err(fail(
                "Resume requires safe termination, the same authorization context and remaining original allowance.",
            ));
        }
        if self.paused() {
            self.periods.push(ActivePeriod {
                start: now.into(),
                end: None,
            });
        }
        Ok(())
    }
}

fn text(value: &str) -> Result<(), Diagnostic> {
    if value.trim().is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        Err(fail(
            "Capability context and session identifiers must be nonempty bounded text.",
        ))
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub struct CapabilityEvidence<'a> {
    pub runs: &'a [CapabilityRun],
    pub environment: &'a EnvironmentId,
    pub execution_context_id: Option<&'a str>,
    pub now: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QualifiedCapability {
    pub case_id: CaseId,
    pub case_revision: String,
    pub observed_at: String,
    pub expires_at: String,
    pub query_sha256: String,
    pub result_sha256: String,
}

pub fn qualify(
    evidence: CapabilityEvidence<'_>,
    case_id: CaseId,
    resources: ResourceBindings,
) -> Result<Option<QualifiedCapability>, Diagnostic> {
    #[cfg(test)]
    QUALIFICATION_CALLS.with(|calls| calls.set(calls.get() + 1));
    let Some(context) = evidence.execution_context_id else {
        return Ok(None);
    };
    text(context)?;
    let now = environment::parse_time(evidence.now)?;
    let mut latest = None;
    for (index, run) in evidence.runs.iter().enumerate() {
        run.validate(index)?;
        if &run.environment != evidence.environment || run.execution_context_id != context {
            continue;
        }
        for attempt in &run.attempts {
            if attempt.case_id != case_id || attempt.resources != resources {
                continue;
            }
            let at = environment::parse_time(&attempt.reservation.prepared_at)?;
            if at > now
                || attempt
                    .reservation
                    .settlement
                    .as_ref()
                    .is_some_and(|snapshot| {
                        environment::parse_time(&snapshot.retrieved_at).is_ok_and(|at| at > now)
                    })
                || attempt.capture.as_ref().is_some_and(|capture| {
                    environment::parse_time(&capture.retrieved_at).is_ok_and(|at| at > now)
                })
            {
                return Ok(None);
            }
            if latest.as_ref().is_none_or(|(previous, _)| at >= *previous) {
                latest = Some((at, attempt));
            }
        }
    }
    let Some((_, attempt)) = latest else {
        return Ok(None);
    };
    if attempt.outcome != CapabilityOutcome::Verified
        || attempt.blocked()
        || attempt.case_revision != case_id.revision()
    {
        return Ok(None);
    }
    let capture = attempt
        .capture
        .as_ref()
        .expect("validated verified outcome");
    let observed = environment::parse_time(&capture.retrieved_at)?;
    let expires = observed + time::Duration::days(EVIDENCE_DAYS);
    if observed > now || now >= expires {
        return Ok(None);
    }
    Ok(Some(QualifiedCapability {
        case_id,
        case_revision: attempt.case_revision.clone(),
        observed_at: capture.retrieved_at.clone(),
        expires_at: expires
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| fail("Cannot format capability expiry."))?,
        query_sha256: attempt.query_sha256.clone(),
        result_sha256: capture.sha256.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::Cloud;
    use serde_json::json;

    #[test]
    fn verifier_semantics_revision_preserves_existing_case_digests() {
        for (case, revision) in [
            (
                CaseId::ArrayUnion,
                "0fab66b6b9e9757a6876520727185f49fa0ce2bed37c41bb55a7bceef7d5b02f",
            ),
            (
                CaseId::ArrayIntersection,
                "b1c87c1b85c5c201ec5b8e6749e8be54a27849cfcb2630be85e2616bfce8bc9b",
            ),
            (
                CaseId::ArrayReduceRow,
                "a3939179554e3c89c0aefd6dc0293cc31b078d946894182ae48764ef082e14ca",
            ),
            (
                CaseId::ArrayReduceColumn,
                "858269370357f86f22aae401528ce2bb0a6eecb9461085eee54e4f5d4320e570",
            ),
            (
                CaseId::MathSqrt,
                "a66ea031af23956ccace9cfb6e50f2d3c3d7854f71e278823bb0736721e528ca",
            ),
        ] {
            assert_eq!(case.revision(), revision, "{case:?}");
        }
    }

    #[test]
    fn previous_case_revision_remains_readable_without_current_claim_or_dispatch() {
        let environment = EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "synthetic".into(),
        };
        let now = "2026-01-01T00:00:00Z";
        let host = serde_json::to_vec(&json!({"schema_version":1,"environment":environment,
            "session_id":"host","verified_at":now,"mechanism":"provider-enforced-search-lifetime",
            "max_search_seconds":1,"capability_reference":"Synthetic reference","verification_evidence":"Synthetic fixture"})).unwrap();
        let mut run = CapabilityRun::prepare(
            environment.clone(),
            "auth".into(),
            "host".into(),
            vec![CaseId::ArrayUnion],
            &host,
            2,
            10,
            now,
            0,
        )
        .unwrap();
        let id = run
            .dispatch(CaseId::ArrayUnion, "auth", "host", now)
            .unwrap()
            .reservation
            .id
            .clone();
        let raw = br#"{"result":[{"out[0]":1,"out[1]":2,"out[2]":3}]}"#;
        let settled = "2026-01-01T00:00:01Z";
        let mut report = json!({"schema_version":1,"attempt_id":id,"environment":environment,
            "session_id":"host","arguments":run.attempts[0].reservation.arguments,"termination":"completed",
            "recorded_at":settled,"result_sha256":sha256_hex(raw),"evidence":"Synthetic exact termination"});
        run.submit(
            &id,
            "auth",
            Some(raw),
            &serde_json::to_vec(&report).unwrap(),
            settled,
            settled,
        )
        .unwrap();
        run.case_revisions[0] = "f".repeat(64);
        run.id = run.identity(0).unwrap();
        run.attempts[0].case_revision = run.case_revisions[0].clone();
        let id = run
            .attempt_id(&run.attempts[0].reservation, CaseId::ArrayUnion, 0)
            .unwrap();
        run.attempts[0].reservation.id = id.clone();
        report["attempt_id"] = json!(id);
        run.attempts[0].reservation.settlement =
            Some(RawSnapshot::new(&serde_json::to_vec(&report).unwrap(), settled).unwrap());
        let restored: CapabilityRun =
            serde_json::from_slice(&serde_json::to_vec(&run).unwrap()).unwrap();
        restored.validate(0).unwrap();
        assert!(!restored.matches_current_cases());
        assert_eq!(
            restored.status().cases[0].last_outcome,
            Some(CapabilityOutcome::Unknown)
        );
        assert!(
            qualify(
                CapabilityEvidence {
                    runs: std::slice::from_ref(&restored),
                    environment: &environment,
                    execution_context_id: Some("auth"),
                    now: settled
                },
                CaseId::ArrayUnion,
                ResourceBindings::None {}
            )
            .unwrap()
            .is_none()
        );
        assert!(
            run.dispatch(CaseId::ArrayUnion, "auth", "host", settled)
                .is_err()
        );
    }
}
