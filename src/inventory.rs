//! Pure inventory evidence and reconciliation. Declarations never become observations.
use crate::{
    contracts::{Diagnostic, EnvironmentId, Scope},
    discovery::{self, DiscoverRequest},
    environment::{self, Pack, ParserContract, RecordKind},
    mcp, sha256_hex,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const COMPLETION_MESSAGE: &str = "Complete full discovery + parser inventory to validate whether your environment has any remaining quiet or inaccessible sources.";
pub const DISCOVERY_WARNING: &str = "Initial environment discovery is extensive. It examines your selected sources through multiple searches and may take substantial time. Agent-assisted discovery can also consume significant model tokens. Review the scope and limits before starting. Completed discovery is saved and reused for future query writing.";
pub const SCOUT_QUERY: &str =
    "groupBy([#repo, #type, @sourcetype], function=count(as=events), limit=max)";
pub const MAX_STATE_BYTES: u64 = 67_108_864;
pub const STATE_VERSION: u32 = 3;
pub const REFRESH_OFF_WARNING: &str = "Turning off automatic inventory refresh can leave new repositories, sources, and types undiscovered. Manual refresh remains available.";
pub const REFRESH_SECONDS: u64 = 300;
pub const MANUAL_REFRESH_SECONDS: u64 = 600;
pub const AUTOMATIC_REFRESH_AVAILABLE: bool = false;
const REFRESH_REMINDER_DAYS: i64 = 30;
const MAX_PENDING_WORK: usize = 1000;
// shortcut: retain 200 refresh attempts per state, add protected archival for longer histories.
const MAX_REFRESH_ATTEMPTS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InventoryRequest {
    pub schema_version: u32,
    pub action: InventoryAction,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum InventoryAction {
    Init {},
    SetDiscoveryMode {
        mode: DiscoveryMode,
        #[serde(default)]
        quick_depth: QuickDepth,
    },
    SetRefreshSettings {
        automatic: bool,
        lookback: RefreshLookback,
    },
    PrepareRefresh {
        session_id: String,
        host_evidence_file: String,
        #[serde(default)]
        automatic: bool,
    },
    DispatchRefresh {
        attempt_id: String,
        session_id: String,
    },
    SettleRefresh {
        attempt_id: String,
        evidence_file: String,
    },
    ImportRefresh {
        attempt_id: String,
        result_file: String,
        retrieved_at: String,
    },
    RecordInventoryHints {
        environment: EnvironmentId,
        hints: Vec<InventoryHint>,
    },
    AcknowledgeInventoryWork {
        work: Vec<WorkRevision>,
        #[serde(default)]
        defer: bool,
    },
    AcknowledgeInventoryNotice {
        snapshot_sha256: String,
        #[serde(default)]
        defer: bool,
    },
    AcknowledgeRefreshReminder {
        cycle_sha256: String,
        #[serde(default)]
        defer: bool,
    },
    ExcludeInventoryWork {
        work: Vec<WorkRevision>,
        reason: String,
        reviewed_at: String,
    },
    ImportQueryObservations {
        query_file: String,
        result_file: String,
        environment: EnvironmentId,
        source: Scope,
        start: String,
        end: String,
        retrieved_at: String,
        caller_assertion: QueryResultAssertion,
    },
    PrepareScout {
        environment: EnvironmentId,
        start: String,
        end: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, schemars(range(min = 1, max = 600)))]
        max_seconds: Option<u64>,
    },
    ImportScout {
        result_file: String,
        arguments: SearchArguments,
        retrieved_at: String,
    },
    ConfirmTarget {
        environment: EnvironmentId,
        scope: TargetScope,
        context: String,
        expected_sources: Vec<Scope>,
    },
    ReviewSource {
        source_id: String,
        purpose: String,
        data_model: String,
        reviewed_at: String,
        evidence: String,
    },
    ImportParsers {
        pages: Vec<ParserPageInput>,
        details: Vec<ParserDetailInput>,
        retrieved_at: String,
    },
    FetchParsers {
        credentials_file: String,
        max_pages: u32,
        max_parsers: u32,
        max_seconds: u64,
    },
    BindParser {
        source_id: String,
        parser_id: String,
        parser_sha256: String,
        reviewed_at: String,
        evidence: String,
        semantics: Vec<SemanticReview>,
    },
    ParserNotApplicable {
        source_id: String,
        reason: NativeSourceReason,
        reviewed_at: String,
        evidence: String,
    },
    ExcludeParser {
        parser_id: String,
        parser_sha256: String,
        reviewed_at: String,
        reason: String,
    },
    PrepareDiscovery {
        start: String,
        end: String,
        #[serde(default)]
        source_ids: Vec<String>,
        #[serde(default)]
        value_fields: Vec<String>,
        #[serde(default)]
        kind_fields: Vec<String>,
        #[serde(default)]
        population_fields: Vec<discovery::FieldSelection>,
        max_queries: u32,
        max_seconds: u64,
    },
    DiscoveryStep {
        #[serde(default)]
        answers: Vec<DiscoveryAnswer>,
    },
    DispatchDiscovery {
        probe_id: String,
    },
    PauseDiscovery {},
    ResumeDiscovery {},
    ImportPack {
        pack_file: String,
    },
    Reconcile {
        reviewed_at: String,
    },
    RecordFailure {
        stage: String,
        source_id: Option<String>,
        message: String,
        recorded_at: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct StatusRequest {
    pub schema_version: u32,
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum TargetScope {
    FullEnvironment,
    SelectedSources,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum DiscoveryMode {
    #[default]
    Full,
    Quick,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum QuickDepth {
    SourcesOnly,
    #[default]
    SourcesAndFields,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub enum RefreshLookback {
    #[default]
    #[serde(rename = "24h")]
    Day,
    #[serde(rename = "7d")]
    Week,
    #[serde(rename = "14d")]
    Fortnight,
    #[serde(rename = "30d")]
    Month,
}
impl RefreshLookback {
    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "24h",
            Self::Week => "7d",
            Self::Fortnight => "14d",
            Self::Month => "30d",
        }
    }

    fn days(self) -> i64 {
        match self {
            Self::Day => 1,
            Self::Week => 7,
            Self::Fortnight => 14,
            Self::Month => 30,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefreshSettings {
    pub automatic: bool,
    pub lookback: RefreshLookback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefreshReminder {
    pub environment: EnvironmentId,
    pub baseline_at: String,
    pub notified_cycle_sha256: Option<String>,
    pub defer_until: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefreshReminderStatus {
    pub due: bool,
    pub show_startup_notice: bool,
    pub cycle_sha256: String,
    pub last_successful_window_end: Option<String>,
    pub due_at: String,
    pub clock_skew: bool,
}

/// Caller-supplied capability evidence; a client wait timeout is not a search lifetime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefreshHostEvidence {
    pub schema_version: u32,
    pub environment: EnvironmentId,
    pub session_id: String,
    pub verified_at: String,
    pub mechanism: RefreshLifetimeMechanism,
    #[cfg_attr(test, schemars(range(min = 1, max = 600)))]
    pub max_search_seconds: u64,
    pub capability_reference: String,
    pub verification_evidence: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RefreshLifetimeMechanism {
    ProviderEnforcedSearchLifetime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RefreshPhase {
    Prepared,
    Dispatched,
    Unresolved,
    Settled,
    Imported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum RefreshTermination {
    Completed,
    Cancelled,
    TimedOut,
    NotStarted,
    LifetimeExpired,
    Unresolved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefreshSettlement {
    pub schema_version: u32,
    pub attempt_id: String,
    pub environment: EnvironmentId,
    pub session_id: String,
    pub arguments: SearchArguments,
    pub termination: RefreshTermination,
    pub recorded_at: String,
    #[serde(default)]
    pub search_started_at: Option<String>,
    #[serde(default)]
    pub result_sha256: Option<String>,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefreshAttempt {
    pub id: String,
    pub environment: EnvironmentId,
    pub session_id: String,
    pub arguments: SearchArguments,
    pub automatic: bool,
    pub host_evidence: RawSnapshot,
    pub prepared_at: String,
    pub dispatched_at: Option<String>,
    pub phase: RefreshPhase,
    pub settlement: Option<RawSnapshot>,
    #[serde(default)]
    pub settlement_history: Vec<RawSnapshot>,
    pub result: Option<RawSnapshot>,
}
impl RefreshAttempt {
    pub fn search_lifetime_seconds(&self) -> Result<u64, Diagnostic> {
        let host: RefreshHostEvidence = serde_json::from_slice(&self.host_evidence.bytes()?)
            .map_err(|_| fail("Invalid saved refresh host capability evidence."))?;
        Ok(host.max_search_seconds)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RefreshStatus {
    pub attempt_id: String,
    pub session_id: String,
    pub environment: EnvironmentId,
    pub arguments: SearchArguments,
    pub phase: RefreshPhase,
    pub automatic: bool,
    pub prepared_at: String,
    pub dispatched_at: Option<String>,
    pub max_search_seconds: u64,
    pub termination: Option<RefreshTermination>,
    pub host_evidence_sha256: String,
    pub settlement_sha256: Option<String>,
    pub result_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InventoryHint {
    pub source: Scope,
    #[serde(default)]
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceMetadata {
    pub source: Scope,
    pub source_types: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct WorkRevision {
    pub id: String,
    pub revision: u64,
}

/// Content commitment for the selected state's complete unacknowledged notice set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryNoticeSnapshot {
    pub snapshot_sha256: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum WorkResolution {
    Inventoried {
        run: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<InventoryWorkRunProof>,
    },
    Excluded {
        reason: String,
        reviewed_at: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InventoryCaptureBoundary {
    pub run: String,
    pub request_sha256: String,
    pub accepted_probe_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InventoryWorkRunProof {
    pub run: String,
    pub request_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_probe_id: Option<String>,
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct InventoryRunEvidence {
    pub run: String,
    pub request_sha256: String,
    pub captures: Vec<discovery::Capture>,
}

impl InventoryCaptureBoundary {
    /// The I/O boundary verifies and locks the exact checkpoint before calling this pure helper.
    pub fn from_checkpoint(run: &str, relay: &mcp::State) -> Result<Self, Diagnostic> {
        text(run)?;
        discovery::validate_request(&relay.request)?;
        let request_sha256 = serde_json::to_vec(&relay.request)
            .map(|bytes| sha256_hex(&bytes))
            .map_err(|_| fail("Cannot fingerprint discovery request."))?;
        if relay.request_sha256 != request_sha256 {
            return Err(fail(
                "Discovery capture boundary requires its exact request.",
            ));
        }
        let mut accepted: BTreeSet<_> = relay
            .answers
            .iter()
            .filter(|(_, answer)| answer.outcome == "complete")
            .map(|(id, _)| id.clone())
            .collect();
        if let Some(session) = &relay.session {
            accepted.extend(session.captures.keys().cloned());
        }
        let boundary = Self {
            run: run.into(),
            request_sha256,
            accepted_probe_ids: accepted.into_iter().collect(),
        };
        boundary.validate(relay.request.max_queries)?;
        Ok(boundary)
    }

    fn validate(&self, max_queries: u32) -> Result<(), Diagnostic> {
        text(&self.run)?;
        if !sha256_valid(&self.request_sha256)
            || self.accepted_probe_ids.len() > max_queries.min(1000) as usize
            || self.accepted_probe_ids.iter().any(|id| !probe_id_valid(id))
        {
            return Err(fail("Invalid discovery capture boundary."));
        }
        distinct_text(&self.accepted_probe_ids)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PendingInventoryWork {
    pub id: String,
    pub source: Scope,
    pub fields: Vec<String>,
    pub source_types: Vec<String>,
    pub new_source: bool,
    pub revision: u64,
    pub notified_revision: u64,
    pub deferred: bool,
    pub after_run: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capture_boundary: Option<InventoryCaptureBoundary>,
    pub resolution: Option<WorkResolution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum QueryResultAssertion {
    ExactQueryResultTargetWindow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FieldObservation {
    pub field: String,
    pub positive_rows: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct QueryObservation {
    pub id: String,
    pub source_id: String,
    pub environment: EnvironmentId,
    pub query_sha256: String,
    pub result_sha256: String,
    pub audit_path: String,
    pub start: String,
    pub end: String,
    pub retrieved_at: String,
    pub caller_assertion: QueryResultAssertion,
    pub fields: Vec<FieldObservation>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct UnverifiedFieldLead {
    pub source: Scope,
    pub field: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Progress {
    Done,
    Pending,
    Review,
    Unknown,
    #[serde(rename = "n-a")]
    NotApplicable,
}
impl std::fmt::Display for Progress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Done => "DONE",
            Self::Pending => "PENDING",
            Self::Review => "REVIEW",
            Self::Unknown => "UNKNOWN",
            Self::NotApplicable => "N/A",
        })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum NativeSourceReason {
    FalconSensorNative,
    PlatformInternal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SearchArguments {
    pub repository: String,
    pub query_string: String,
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParserPageInput {
    pub offset: u32,
    pub limit: u32,
    pub result_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParserDetailInput {
    pub parser_id: String,
    pub result_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DiscoveryAnswer {
    pub probe_id: String,
    pub result_file: String,
}

/// A review records its exact parser snapshot and line. It cannot assert observed presence.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SemanticReview {
    pub line: u32,
    pub interpretation: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RawSnapshot {
    pub sha256: String,
    pub retrieved_at: String,
    pub raw_hex: String,
}

impl RawSnapshot {
    pub fn new(raw: &[u8], retrieved_at: &str) -> Result<Self, Diagnostic> {
        environment::parse_time(retrieved_at)?;
        if raw.len() > mcp::MAX_RESULT_BYTES {
            return Err(fail("Evidence exceeds the 8 MiB capture limit."));
        }
        let mut raw_hex = String::with_capacity(raw.len() * 2);
        use std::fmt::Write;
        for byte in raw {
            write!(raw_hex, "{byte:02x}").expect("writing to String");
        }
        Ok(Self {
            sha256: sha256_hex(raw),
            retrieved_at: retrieved_at.into(),
            raw_hex,
        })
    }
    pub fn bytes(&self) -> Result<Vec<u8>, Diagnostic> {
        environment::parse_time(&self.retrieved_at)?;
        if self.raw_hex.len() > mcp::MAX_RESULT_BYTES * 2 || !self.raw_hex.len().is_multiple_of(2) {
            return Err(fail("Invalid snapshot bytes."));
        }
        let mut bytes = Vec::with_capacity(self.raw_hex.len() / 2);
        for pair in self.raw_hex.as_bytes().as_chunks::<2>().0 {
            let text = std::str::from_utf8(pair).map_err(|_| fail("Invalid snapshot encoding."))?;
            bytes.push(
                u8::from_str_radix(text, 16).map_err(|_| fail("Invalid snapshot encoding."))?,
            );
        }
        if sha256_hex(&bytes) != self.sha256 {
            return Err(fail(
                "Snapshot digest does not match its exact saved bytes.",
            ));
        }
        Ok(bytes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ScoutPlan {
    pub environment: EnvironmentId,
    pub arguments: SearchArguments,
    pub query_sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, schemars(range(min = 1, max = 600)))]
    pub max_seconds: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ScoutEvidence {
    pub plan: ScoutPlan,
    pub snapshot: RawSnapshot,
    pub rows: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ConfirmedTarget {
    pub environment: EnvironmentId,
    pub scope: TargetScope,
    pub context: String,
    pub confirmed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceReview {
    pub purpose: String,
    pub data_model: String,
    pub reviewed_at: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ParserBinding {
    Parser {
        parser_id: String,
        parser_sha256: String,
        reviewed_at: String,
        evidence: String,
        semantics: Vec<SemanticReview>,
    },
    NotApplicable {
        reason: NativeSourceReason,
        reviewed_at: String,
        evidence: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InventorySource {
    pub id: String,
    pub selectors: Scope,
    pub scouted_events: Option<u64>,
    pub expected: bool,
    pub review: Option<SourceReview>,
    pub binding: Option<ParserBinding>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParserPage {
    pub offset: u32,
    pub limit: u32,
    pub snapshot: RawSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParserDeclarations {
    pub fields: Vec<String>,
    pub fields_to_tag: Vec<String>,
    pub array_fields: Vec<String>,
    pub review_lines: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParserSnapshot {
    pub id: String,
    pub name: String,
    pub snapshot: RawSnapshot,
    pub declarations: ParserDeclarations,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParserExclusion {
    pub parser_id: String,
    pub parser_sha256: String,
    pub reviewed_at: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PackReference {
    pub path: String,
    pub content_digest: String,
    pub imported_at: String,
    pub discovery_run: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DiscoveryRun {
    pub state_path: String,
    pub pack_path: String,
    pub receipt_path: String,
    pub start: String,
    pub end: String,
    pub source_ids: Vec<String>,
    pub value_fields: Vec<String>,
    pub max_queries: u32,
    pub max_seconds: u64,
    pub answered: usize,
    pub elapsed_seconds: f64,
    pub status: String,
    #[serde(default)]
    pub mode: DiscoveryMode,
    #[serde(default)]
    pub quick_depth: QuickDepth,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InventoryFailure {
    pub stage: String,
    pub source_id: Option<String>,
    pub message: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InventoryState {
    pub format: String,
    pub schema_version: u32,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub discovery_mode: DiscoveryMode,
    #[serde(default)]
    pub quick_depth: QuickDepth,
    #[serde(default)]
    pub unverified_leads: Vec<UnverifiedFieldLead>,
    #[serde(default)]
    pub query_observations: Vec<QueryObservation>,
    #[serde(default)]
    pub refresh_settings: RefreshSettings,
    #[serde(default)]
    pub refresh_attempts: Vec<RefreshAttempt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_reminder: Option<RefreshReminder>,
    #[serde(default)]
    pub source_metadata: Vec<SourceMetadata>,
    #[serde(default)]
    pub pending_inventory: Vec<PendingInventoryWork>,
    pub scout_plan: Option<ScoutPlan>,
    pub scouting: Vec<ScoutEvidence>,
    pub target: Option<ConfirmedTarget>,
    pub sources: Vec<InventorySource>,
    pub parser_pages: Vec<ParserPage>,
    pub parsers: Vec<ParserSnapshot>,
    pub parser_exclusions: Vec<ParserExclusion>,
    pub packs: Vec<PackReference>,
    #[serde(default)]
    pub active_pack: Option<PackReference>,
    pub discovery_runs: Vec<DiscoveryRun>,
    pub parser_runs: Vec<ParserRun>,
    pub failures: Vec<InventoryFailure>,
    pub reconciled_evidence_sha256: Option<String>,
    pub reconciled_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParserRun {
    pub started_at: String,
    pub max_pages: u32,
    pub max_parsers: u32,
    pub max_seconds: u64,
    pub pages_completed: usize,
    pub parsers_completed: usize,
    pub elapsed_seconds: f64,
    pub failure: Option<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FieldStatus {
    pub field: String,
    pub declared: bool,
    pub observed: bool,
    pub measured: bool,
    pub status: Progress,
    pub values: Progress,
    pub values_requested: bool,
    pub evidence_dates: Vec<String>,
    pub not_reobserved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceStatus {
    pub source_id: String,
    pub expected: bool,
    pub selectors: Scope,
    pub scouted_events: Option<u64>,
    pub review: Option<SourceReview>,
    pub binding: Option<ParserBinding>,
    pub status: Progress,
    pub interpretation: Progress,
    pub parser: Progress,
    pub measurements: Progress,
    pub observation: Progress,
    pub fields: Vec<FieldStatus>,
    pub record_kinds: Vec<RecordKind>,
    pub unobserved_declared_fields: Vec<String>,
    pub next_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParserSummary {
    pub id: String,
    pub name: String,
    pub sha256: String,
    pub retrieved_at: String,
    pub declared_fields: Vec<String>,
    pub fields_to_tag: Vec<String>,
    pub array_fields: Vec<String>,
    pub review_lines: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ScoutSummary {
    pub plan: ScoutPlan,
    pub raw_sha256: String,
    pub retrieved_at: String,
    pub row_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InventoryStatus {
    pub schema_version: u32,
    pub blank_canvas: bool,
    pub full_map_complete: bool,
    pub selected_scope_complete: bool,
    pub quick_scope_complete: bool,
    pub discovery_mode: DiscoveryMode,
    pub quick_depth: QuickDepth,
    pub unverified_lead_count: usize,
    pub query_observation_count: usize,
    pub refresh_settings: RefreshSettings,
    pub automatic_refresh_available: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_reminder: Option<RefreshReminderStatus>,
    pub refresh_attempts: Vec<RefreshStatus>,
    pub refresh_blocked: bool,
    pub pending_inventory: Vec<PendingInventoryWork>,
    pub inventory_notification_count: usize,
    pub settings_attention: bool,
    pub unverified_leads: Vec<UnverifiedFieldLead>,
    pub query_observations: Vec<QueryObservation>,
    pub message: String,
    pub target: Progress,
    pub environment: Option<EnvironmentId>,
    pub target_scope: Option<TargetScope>,
    pub context: Option<String>,
    pub scouting: Progress,
    pub parser_catalog: Progress,
    pub reconciliation: Progress,
    pub parser_count: usize,
    pub parser_page_count: usize,
    pub parsers: Vec<ParserSummary>,
    pub scouting_evidence: Vec<ScoutSummary>,
    pub unclassified_parser_ids: Vec<String>,
    pub sources: Vec<SourceStatus>,
    pub discovery_runs: Vec<DiscoveryRun>,
    pub parser_runs: Vec<ParserRun>,
    pub failures: Vec<InventoryFailure>,
    pub next_actions: Vec<String>,
    pub covered_queries_usable: bool,
    pub active_pack: Option<PackReference>,
}

impl QueryObservation {
    fn digest(&self) -> Result<String, Diagnostic> {
        let mut copy = self.clone();
        copy.id.clear();
        serde_json::to_vec(&copy)
            .map(|bytes| sha256_hex(&bytes))
            .map_err(|_| fail("Cannot fingerprint query observations."))
    }
    pub fn validate(&self) -> Result<(), Diagnostic> {
        environment::validate_identity(&self.environment)?;
        let request: DiscoverRequest = serde_json::from_value(json!({
            "schema_version":1,"environment":self.environment,"start":self.start,"end":self.end,
            "sources":[{"type":"observation-validation"}],"max_queries":1,"max_seconds":1
        }))
        .map_err(|_| fail("Invalid query observation window."))?;
        discovery::validate_request(&request)?;
        if environment::parse_time(&self.retrieved_at)? < environment::parse_time(&self.end)? {
            return Err(fail(
                "Query result retrieval must not precede its completed window.",
            ));
        }
        let digest_ok =
            |digest: &str| digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit());
        if !digest_ok(&self.query_sha256)
            || !digest_ok(&self.result_sha256)
            || self.id != self.digest()?
            || !std::path::Path::new(&self.audit_path).is_absolute()
            || self.fields.is_empty()
            || self.fields.len() > 4000
        {
            return Err(fail(
                "Invalid query observation fingerprints, audit path or fields.",
            ));
        }
        let mut fields = BTreeSet::new();
        for field in &self.fields {
            crate::language::plan::checked_field(&field.field)?;
            if field.positive_rows == 0
                || field.positive_rows >= 10_000
                || !fields.insert(&field.field)
            {
                return Err(fail(
                    "Query observations require distinct fields with 1..9999 positive rows.",
                ));
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn from_result(
        query: &[u8],
        result: &[u8],
        environment: EnvironmentId,
        source: Scope,
        start: String,
        end: String,
        retrieved_at: String,
        caller_assertion: QueryResultAssertion,
        audit_path: String,
    ) -> Result<Self, Diagnostic> {
        use crate::language::ast::{Filter, Pattern, Step, Value as AstValue};
        scope_ok(&source)?;
        let query_text = std::str::from_utf8(query)
            .map_err(|_| fail("Query file must contain exact UTF-8 CQL."))?;
        let pipeline = crate::language::parser::parse(query_text).map_err(|e| e.diagnostic)?;
        if crate::language::validate::validate(&pipeline, query_text).status
            != crate::contracts::Acceptance::Accepted
        {
            return Err(fail(
                "Query observations require a query accepted by the bundled language profile.",
            ));
        }
        // Lower the attested selector through the same literal encoder used by generated queries.
        let expected_query = crate::language::plan::lower(&crate::contracts::Plan {
            source: source.clone(),
            filters: vec![],
            steps: vec![],
        })?
        .render();
        let expected = crate::language::parser::parse(&expected_query).map_err(|e| e.diagnostic)?;
        let mut expected_patterns = BTreeMap::new();
        fn comparisons<'a>(
            filter: &'a Filter,
            out: &mut Vec<(&'a str, &'a str, &'a Pattern)>,
        ) -> Result<(), Diagnostic> {
            match filter {
                Filter::And(filters) => {
                    for filter in filters {
                        comparisons(filter, out)?;
                    }
                }
                Filter::Compare { field, op, value } => {
                    if let Pattern::Regex { pattern, .. } = value
                        && !crate::language::validate::regex_captures(pattern).is_empty()
                    {
                        return Err(fail(
                            "Regex capture fields cannot establish direct source observations.",
                        ));
                    }
                    out.push((field, op, value));
                }
                Filter::Or(filters) => {
                    let mut alternatives = vec![];
                    for filter in filters {
                        comparisons(filter, &mut alternatives)?;
                    }
                    if alternatives
                        .iter()
                        .any(|(field, _, _)| matches!(*field, "#repo" | "#type"))
                    {
                        return Err(fail(
                            "Alternate source selectors cannot establish one observed source.",
                        ));
                    }
                }
                Filter::Not(filter) => {
                    let mut negated = vec![];
                    comparisons(filter, &mut negated)?;
                    if negated
                        .iter()
                        .any(|(field, _, _)| matches!(*field, "#repo" | "#type"))
                    {
                        return Err(fail(
                            "Negated source selectors cannot establish one observed source.",
                        ));
                    }
                }
                Filter::Call(_) => {
                    return Err(fail(
                        "Computed filter calls cannot establish direct query observations.",
                    ));
                }
                Filter::Text(Pattern::Regex { pattern, .. })
                    if !crate::language::validate::regex_captures(pattern).is_empty() =>
                {
                    return Err(fail(
                        "Regex capture fields cannot establish direct source observations.",
                    ));
                }
                _ => {}
            }
            Ok(())
        }
        for step in &expected.steps {
            if let Step::Filter(filter) = step {
                let mut found = vec![];
                comparisons(filter, &mut found)?;
                for (field, _, pattern) in found {
                    expected_patterns.insert(field, pattern);
                }
            }
        }
        let mut seen_selectors = BTreeSet::new();
        let mut selected: Option<BTreeSet<String>> = None;
        for step in &pipeline.steps {
            match step {
                Step::Filter(filter) => {
                    let mut found = vec![];
                    comparisons(filter, &mut found)?;
                    for (field, op, pattern) in found {
                        if !matches!(field, "#repo" | "#type") {
                            continue;
                        }
                        let value = if field == "#repo" {
                            source.repo.as_deref()
                        } else {
                            source.type_.as_deref()
                        };
                        let exact = value.is_some_and(|value| {
                            matches!(pattern, Pattern::Text(text) if text == value && !text.contains('*'))
                                || expected_patterns.get(field).is_some_and(|expected| *expected == pattern)
                        });
                        if op != "=" || !exact {
                            return Err(fail(
                                "Query source selectors must exactly match one attested source.",
                            ));
                        }
                        seen_selectors.insert(field);
                    }
                }
                Step::Call(call) if call.name == "select" => {
                    let values = call
                        .args
                        .first()
                        .filter(|_| call.args.len() == 1)
                        .filter(|arg| arg.name.as_deref().is_none_or(|name| name == "fields"))
                        .map(|arg| &arg.value)
                        .ok_or_else(|| {
                            fail("Observation select requires one direct field list.")
                        })?;
                    let values = match values {
                        AstValue::Array(values) => values.as_slice(),
                        value => std::slice::from_ref(value),
                    };
                    let mut fields = BTreeSet::new();
                    for value in values {
                        let field = match value {
                            AstValue::Word(field) | AstValue::String(field) => field,
                            _ => {
                                return Err(fail(
                                    "Derived or aliased observation projections are unsupported.",
                                ));
                            }
                        };
                        crate::language::plan::checked_field(field)?;
                        if field.contains('*')
                            || selected
                                .as_ref()
                                .is_some_and(|previous| !previous.contains(field))
                        {
                            return Err(fail(
                                "Observation projections must preserve direct unchanged fields.",
                            ));
                        }
                        fields.insert(field.clone());
                    }
                    selected = Some(fields);
                }
                Step::Call(call) if matches!(call.name.as_str(), "head" | "tail" | "sort") => {
                    fn direct(value: &AstValue) -> bool {
                        match value {
                            AstValue::Word(_)
                            | AstValue::String(_)
                            | AstValue::Number(_)
                            | AstValue::Bool(_) => true,
                            AstValue::Array(values) => values.iter().all(direct),
                            _ => false,
                        }
                    }
                    if call.args.iter().any(|arg| !direct(&arg.value)) {
                        return Err(fail(
                            "Observation row selection cannot compute or import fields.",
                        ));
                    }
                }
                _ => {
                    return Err(fail(
                        "Derived, aliased, aggregate, join and synthetic result pipelines cannot establish direct field observations.",
                    ));
                }
            }
        }
        if expected_patterns
            .keys()
            .any(|field| !seen_selectors.contains(field))
        {
            return Err(fail(
                "Query must explicitly constrain every attested source selector.",
            ));
        }
        let normalized = mcp::normalize(result)?;
        if normalized.rows.is_empty() || normalized.rows.len() >= 10_000 {
            return Err(fail(
                "Empty or cap-sized results cannot establish field observations.",
            ));
        }
        let mut fields = BTreeMap::<String, u64>::new();
        for row in normalized.rows {
            for (selector, expected) in [("#repo", &source.repo), ("#type", &source.type_)] {
                if let Some(value) = row.get(selector)
                    && expected
                        .as_ref()
                        .is_some_and(|expected| value.as_str() != Some(expected))
                {
                    return Err(fail(
                        "Result source selectors differ from the attested query source.",
                    ));
                }
            }
            for (field, value) in row {
                if selected
                    .as_ref()
                    .is_some_and(|selected| !selected.contains(&field))
                {
                    return Err(fail(
                        "Result contains a field outside its unchanged projection.",
                    ));
                }
                crate::language::plan::checked_field(&field)?;
                let positive = match value {
                    Value::Null => false,
                    Value::String(value) => !value.trim().is_empty(),
                    Value::Array(value) => !value.is_empty(),
                    Value::Object(value) => !value.is_empty(),
                    _ => true,
                };
                if positive {
                    *fields.entry(field).or_default() += 1;
                }
            }
        }
        let mut observation = Self {
            id: String::new(),
            source_id: discovery::source_id(&source),
            environment,
            query_sha256: sha256_hex(query),
            result_sha256: sha256_hex(result),
            audit_path,
            start,
            end,
            retrieved_at,
            caller_assertion,
            fields: fields
                .into_iter()
                .map(|(field, positive_rows)| FieldObservation {
                    field,
                    positive_rows,
                })
                .collect(),
        };
        observation.id = observation.digest()?;
        observation.validate()?;
        Ok(observation)
    }
}

/// Compact opt-in status. Full reports retain their existing JSON contract.
pub fn summary(status: &InventoryStatus) -> Value {
    json!({
        "schema_version":status.schema_version,"summary":true,"blank_canvas":status.blank_canvas,
        "full_map_complete":status.full_map_complete,"selected_scope_complete":status.selected_scope_complete,
        "quick_scope_complete":status.quick_scope_complete,"discovery_mode":status.discovery_mode,"quick_depth":status.quick_depth,
        "message":status.message,"environment":status.environment,"target_scope":status.target_scope,
        "progress":{"target":status.target,"scouting":status.scouting,"parser_catalog":status.parser_catalog,"reconciliation":status.reconciliation},
        "counts":{"sources":status.sources.len(),"expected_sources":status.sources.iter().filter(|s|s.expected).count(),
            "parsers":status.parser_count,"parser_pages":status.parser_page_count,"unclassified_parsers":status.unclassified_parser_ids.len(),
            "discovery_runs":status.discovery_runs.len(),"parser_runs":status.parser_runs.len(),"failures":status.failures.len(),
            "unverified_leads":status.unverified_lead_count,"query_observations":status.query_observation_count,
            "pending_inventory":status.pending_inventory.len(),"inventory_notifications":status.inventory_notification_count},
        "refresh_settings":status.refresh_settings,"automatic_refresh_available":status.automatic_refresh_available,
        "refresh_reminder":status.refresh_reminder,"refresh_blocked":status.refresh_blocked,
        "settings_attention":status.settings_attention,
        "refresh_attempts":status.refresh_attempts,
        "pending_inventory":status.pending_inventory,
        "sources":status.sources.iter().map(|source| json!({"source_id":source.source_id,"selectors":source.selectors,
            "expected":source.expected,"status":source.status,"interpretation":source.interpretation,"parser":source.parser,
            "measurements":source.measurements,"observation":source.observation,"scouted_events":source.scouted_events,
            "fields":{"total":source.fields.len(),"declared":source.fields.iter().filter(|f|f.declared).count(),
                "observed":source.fields.iter().filter(|f|f.observed).count(),"measured":source.fields.iter().filter(|f|f.measured).count(),
                "unobserved_declared":source.unobserved_declared_fields.len()},"next_actions":source.next_actions})).collect::<Vec<_>>(),
        "active_pack":status.active_pack,"covered_queries_usable":status.covered_queries_usable,"next_actions":status.next_actions
    })
}

/// Fixed vocabulary and opaque references only; no retained evidence becomes agent instructions.
pub fn brief(status: &InventoryStatus, session_id: Option<&str>) -> Value {
    let reminder = status.refresh_reminder.as_ref().filter(|reminder| {
        reminder.cycle_sha256.len() == 64
            && reminder.cycle_sha256.bytes().all(|b| b.is_ascii_hexdigit())
            && environment::parse_time(&reminder.due_at).is_ok()
            && reminder
                .last_successful_window_end
                .as_ref()
                .is_none_or(|at| environment::parse_time(at).is_ok())
    });
    let reserved = status.refresh_attempts.iter().find(|attempt| {
        matches!(
            attempt.phase,
            RefreshPhase::Prepared | RefreshPhase::Dispatched | RefreshPhase::Unresolved
        )
    });
    let recorded = reserved
        .or_else(|| {
            status
                .refresh_attempts
                .iter()
                .rev()
                .find(|attempt| session_id.is_some_and(|session| attempt.session_id == session))
        })
        .or_else(|| status.refresh_attempts.last());
    let current_session_match = recorded
        .is_some_and(|attempt| session_id.is_some_and(|session| attempt.session_id == session));
    let allowance_used = session_id.map(|session| {
        status.refresh_attempts.iter().any(|attempt| {
            attempt.automatic && attempt.session_id == session && attempt.dispatched_at.is_some()
        })
    });
    let prepared_for_current_session = reserved
        .is_some_and(|attempt| attempt.phase == RefreshPhase::Prepared && current_session_match);
    let mut discovery_work: Vec<_> = status
        .discovery_runs
        .iter()
        .filter(|run| !matches!(run.status.as_str(), "complete" | "paused"))
        .collect();
    discovery_work.sort_by_key(|run| {
        (
            !matches!(run.status.as_str(), "cancel-requested" | "cleanup-pending"),
            sha256_hex(run.state_path.as_bytes()),
        )
    });
    let discovery_recovery = discovery_work
        .iter()
        .filter(|run| matches!(run.status.as_str(), "cancel-requested" | "cleanup-pending"))
        .count();
    let blocker = match reserved {
        Some(attempt) if attempt.phase == RefreshPhase::Unresolved => "termination-unresolved",
        Some(attempt) if attempt.phase == RefreshPhase::Dispatched => "awaiting-host-settlement",
        Some(attempt) if attempt.automatic && !status.automatic_refresh_available => {
            "automatic-unavailable"
        }
        Some(_) if prepared_for_current_session => "reserved-for-current-session",
        Some(_) => "reserved-for-other-or-unidentified-session",
        None if status.environment.is_none() => "target-required",
        None if discovery_recovery > 0 => "recorded-discovery-recovery-required",
        None if !discovery_work.is_empty() => "recorded-discovery-unfinished",
        None => "manual-refresh-available",
    };
    let mut notices = Vec::new();
    if blocker == "termination-unresolved" {
        notices.push(json!({"code":"REFRESH_SETTLEMENT_REQUIRED","severity":"warning",
            "message":"Refresh termination is unresolved. Reconcile the exact attempt before another search; a timeout or lost response does not establish termination."}));
    } else if blocker == "awaiting-host-settlement" {
        notices.push(json!({"code":"REFRESH_AWAITING_HOST_SETTLEMENT","severity":"info",
            "message":"Refresh dispatch was recorded. Await the original host result and exact settlement before another search. The recorded phase does not establish backend execution."}));
    } else if blocker == "automatic-unavailable" {
        notices.push(json!({"code":"REFRESH_RESERVED","severity":"info",
            "message":"A legacy automatic refresh is reserved. Safely settle it as not-started before preparing an explicit manual refresh."}));
    } else if reserved.is_some() {
        notices.push(json!({"code":"REFRESH_RESERVED","severity":"info",
            "message":"A refresh attempt is reserved. Dispatch requires its exact verified session. Reservation does not establish that a backend search is running."}));
    }
    if blocker == "target-required" {
        notices.push(json!({"code":"INVENTORY_TARGET_REQUIRED","severity":"info",
            "message":"Confirm the exact target before inventory refresh."}));
    }
    if discovery_recovery > 0 {
        notices.push(json!({"code":"DISCOVERY_RECOVERY_RECORDED","severity":"warning",
            "message":"Discovery has recorded unresolved termination or cleanup. Reconcile the exact saved attempt before another search. Recorded state does not establish current backend execution."}));
    } else if !discovery_work.is_empty() {
        notices.push(json!({"code":"DISCOVERY_UNFINISHED_RECORDED","severity":"info",
            "message":"Discovery has recorded unfinished work. Inspect its exact saved checkpoint before preparing refresh; recorded state does not establish current execution readiness."}));
    }
    if reminder.is_some_and(|reminder| reminder.show_startup_notice) {
        notices.push(json!({"code":"INVENTORY_REFRESH_DUE","severity":"info",
            "message":"Inventory refresh is due. Run an explicit manual refresh or choose Remind me in 30 days; acknowledgement does not complete refresh."}));
    }
    if !status.pending_inventory.is_empty() {
        notices.push(json!({"code":"INVENTORY_WORK_PENDING","severity":"warning",
            "message":"Settings *: inventory work remains pending. Acknowledgement or deferral updates its notice; applicable inventory or explicit exclusion resolves the work."}));
    }
    let mut work: Vec<_> = status
        .pending_inventory
        .iter()
        .filter(|work| work.resolution.is_none() && work.notified_revision < work.revision)
        .collect();
    work.sort_by_key(|work| (&work.id, work.revision));
    let mut references = Vec::new();
    let safe_id = |id: &str, prefix: &str, length: usize| {
        id.strip_prefix(prefix).is_some_and(|suffix| {
            suffix.len() == length && suffix.bytes().all(|b| b.is_ascii_hexdigit())
        })
    };
    if let Some(attempt) = reserved
        && safe_id(&attempt.attempt_id, "refresh:", 64)
    {
        references.push(json!({"kind":"refresh","id":attempt.attempt_id,"phase":attempt.phase}));
    }
    for run in &discovery_work {
        if references.len() == 5 {
            break;
        }
        let phase = match run.status.as_str() {
            "needs-probes" => "needs-probes",
            "activation-blocked" => "activation-blocked",
            "activating" => "activating",
            "failed" => "failed",
            "cancel-requested" => "cancel-requested",
            "cleanup-pending" => "cleanup-pending",
            _ => "unverified",
        };
        references.push(json!({"kind":"discovery","id":format!("discovery:{}",sha256_hex(run.state_path.as_bytes())),"phase":phase}));
    }
    for entry in &work {
        if references.len() == 5 {
            break;
        }
        if safe_id(&entry.id, "env:repo:", 16) {
            references.push(json!({"kind":"work","id":entry.id,"revision":entry.revision,
                "unacknowledged":entry.notified_revision < entry.revision,"deferred":entry.deferred}));
        }
    }
    for attempt in status.refresh_attempts.iter().rev() {
        if references.len() == 5 {
            break;
        }
        if reserved.is_none_or(|reserved| reserved.attempt_id != attempt.attempt_id)
            && safe_id(&attempt.attempt_id, "refresh:", 64)
        {
            references
                .push(json!({"kind":"refresh","id":attempt.attempt_id,"phase":attempt.phase}));
        }
    }
    json!({
        "schema_version":1,"brief":true,"blank_canvas":status.blank_canvas,
        "discovery_mode":status.discovery_mode,"quick_depth":status.quick_depth,
        "target_present":status.environment.is_some(),"full_map_complete":status.full_map_complete,
        "selected_scope_complete":status.selected_scope_complete,"quick_scope_complete":status.quick_scope_complete,
        "covered_queries_usable":status.covered_queries_usable,"has_active_pack":status.active_pack.is_some(),
        "refresh_blocked":status.refresh_blocked,
        "readiness":{"target":status.target,"scouting":status.scouting,"parser_catalog":status.parser_catalog,"reconciliation":status.reconciliation},
        "settings_attention":status.settings_attention,"settings_label":if status.settings_attention { "Settings *" } else { "Settings" },
        "refresh":{"automatic":status.refresh_settings.automatic,"automatic_available":status.automatic_refresh_available,"lookback":status.refresh_settings.lookback,
            "recorded_phase":recorded.map(|attempt| attempt.phase),"known_termination":recorded.and_then(|attempt| attempt.termination),
            "recorded_max_search_seconds":recorded.map(|attempt| attempt.max_search_seconds),
            "current_session_match":current_session_match,"session_allowance_used":allowance_used,
            "prepared_for_current_session":prepared_for_current_session,"blocker":blocker,
            "max_automatic_search_seconds":REFRESH_SECONDS,"max_manual_search_seconds":MANUAL_REFRESH_SECONDS},
        "refresh_reminder":reminder,
        "counts":{"sources":status.sources.len(),"expected_sources":status.sources.iter().filter(|source| source.expected).count(),
            "parsers":status.parser_count,"parser_pages":status.parser_page_count,"unclassified_parsers":status.unclassified_parser_ids.len(),
            "discovery_runs":status.discovery_runs.len(),"parser_runs":status.parser_runs.len(),"failures":status.failures.len(),
            "discovery_recovery":discovery_recovery,"discovery_unfinished":discovery_work.len(),
            "unverified_leads":status.unverified_lead_count,"query_observations":status.query_observation_count,
            "pending_inventory":status.pending_inventory.iter().filter(|work| work.resolution.is_none()).count(),
            "inventory_notifications":status.inventory_notification_count,
            "refresh_attempts":status.refresh_attempts.len()},
        "notification_snapshot":inventory_notice_snapshot(status.environment.as_ref(), &status.pending_inventory).ok().flatten(),
        "notices":notices,"omitted_references":work.len() + status.refresh_attempts.len() + discovery_work.len() - references.len(),"references":references
    })
}

fn inventory_notice_snapshot(
    environment: Option<&EnvironmentId>,
    pending: &[PendingInventoryWork],
) -> Result<Option<InventoryNoticeSnapshot>, Diagnostic> {
    let mut work: Vec<_> = pending
        .iter()
        .filter(|work| work.resolution.is_none() && work.notified_revision < work.revision)
        .map(|work| (&work.id, work.revision))
        .collect();
    if work.is_empty() {
        return Ok(None);
    }
    work.sort_unstable();
    let environment =
        environment.ok_or_else(|| fail("Inventory notices require a confirmed target."))?;
    let bytes = serde_json::to_vec(&("cqlmatic.inventory-notice", 1_u32, environment, &work))
        .map_err(|_| fail("Cannot fingerprint the inventory notice snapshot."))?;
    Ok(Some(InventoryNoticeSnapshot {
        snapshot_sha256: sha256_hex(&bytes),
        count: work.len(),
    }))
}

fn fail(message: &str) -> Diagnostic {
    Diagnostic::error("INVENTORY_EVIDENCE", message)
}
fn text(value: &str) -> Result<(), Diagnostic> {
    if value.trim().is_empty()
        || value.len() > 16_384
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(fail("Supply nonempty bounded review text."));
    }
    Ok(())
}

fn sha256_valid(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn probe_id_valid(value: &str) -> bool {
    value.strip_prefix("p-").is_some_and(|suffix| {
        suffix.len() == 16 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}

fn inventory_work_run(
    runs: &[DiscoveryRun],
    work: &PendingInventoryWork,
    path: &str,
    request_sha256: Option<&str>,
) -> Option<usize> {
    runs.iter().enumerate().find_map(|(index, run)| {
        (run.state_path == path && inventory_work_run_eligible(index, run, work, request_sha256))
            .then_some(index)
    })
}

fn inventory_work_run_eligible(
    index: usize,
    run: &DiscoveryRun,
    work: &PendingInventoryWork,
    request_sha256: Option<&str>,
) -> bool {
    run.status == "complete"
        && run.source_ids.contains(&work.id)
        && (index >= work.after_run
            || index + 1 == work.after_run
                && work.capture_boundary.as_ref().is_some_and(|boundary| {
                    boundary.run == run.state_path
                        && request_sha256 == Some(boundary.request_sha256.as_str())
                }))
}

fn validate_inventory_work_proof(
    runs: &[DiscoveryRun],
    work: &PendingInventoryWork,
    run: &str,
    evidence: &[InventoryWorkRunProof],
) -> Result<(), Diagnostic> {
    if evidence.is_empty() {
        return inventory_work_run(runs, work, run, None)
            .map(|_| ())
            .ok_or_else(|| {
                fail("Inventory work resolution requires its exact completed source run.")
            });
    }
    let mut proof_runs = BTreeSet::new();
    let mut fields = BTreeSet::new();
    let mut source_measured = false;
    for proof in evidence {
        let index = inventory_work_run(runs, work, &proof.run, Some(&proof.request_sha256))
            .ok_or_else(|| fail("Inventory work proof requires its exact eligible source run."))?;
        if !sha256_valid(&proof.request_sha256)
            || !proof_runs.insert(proof.run.as_str())
            || proof.fields.is_empty() && proof.source_probe_id.is_none()
            || proof
                .fields
                .keys()
                .any(|field| !work.fields.contains(field) || !fields.insert(field.as_str()))
        {
            return Err(fail("Invalid inventory work capture proof."));
        }
        for id in proof.fields.values().chain(proof.source_probe_id.iter()) {
            if !probe_id_valid(id)
                || index < work.after_run
                    && work
                        .capture_boundary
                        .as_ref()
                        .is_some_and(|boundary| boundary.accepted_probe_ids.contains(id))
            {
                return Err(fail(
                    "Inventory work proof must follow its capture boundary.",
                ));
            }
        }
        source_measured |= proof.source_probe_id.is_some();
    }
    if !proof_runs.contains(run)
        || fields.len() != work.fields.len()
        || work.fields.is_empty() && !source_measured
    {
        return Err(fail(
            "Inventory work proof must cover its exact requested measurements.",
        ));
    }
    Ok(())
}
fn scope_ok(scope: &Scope) -> Result<(), Diagnostic> {
    if (scope.repo.is_none() && scope.type_.is_none())
        || scope
            .repo
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        || scope
            .type_
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        || scope.event.is_some()
    {
        return Err(fail(
            "Inventory sources require repo or type, without an event selector.",
        ));
    }
    if let Some(repo) = &scope.repo {
        crate::language::plan::checked_text(repo)?;
    }
    if let Some(kind) = &scope.type_ {
        crate::language::plan::checked_text(kind)?;
    }
    Ok(())
}

impl InventoryState {
    fn validate_refresh(&self) -> Result<(), Diagnostic> {
        if let Some(reminder) = &self.refresh_reminder {
            environment::validate_identity(&reminder.environment)?;
            environment::parse_time(&reminder.baseline_at)?;
            if let Some(until) = &reminder.defer_until {
                environment::parse_time(until)?;
            }
            if self
                .target
                .as_ref()
                .is_none_or(|target| target.environment != reminder.environment)
                || reminder.notified_cycle_sha256.as_ref().is_some_and(|hash| {
                    hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
                })
            {
                return Err(fail(
                    "Refresh reminders require the exact confirmed target and a valid cycle SHA-256.",
                ));
            }
        }
        if self.target.is_none()
            && (!self.refresh_attempts.is_empty()
                || !self.source_metadata.is_empty()
                || !self.pending_inventory.is_empty())
        {
            return Err(fail(
                "Continuing inventory evidence requires a confirmed environment.",
            ));
        }
        if self.refresh_attempts.len() > MAX_REFRESH_ATTEMPTS
            || self.pending_inventory.len() > MAX_PENDING_WORK
            || self.source_metadata.len() > 10_000
        {
            return Err(fail(
                "Inventory refresh history or pending work exceeds its finite limit.",
            ));
        }
        let mut metadata = BTreeSet::new();
        for entry in &self.source_metadata {
            scope_ok(&entry.source)?;
            let id = discovery::source_id(&entry.source);
            if !metadata.insert(id.clone()) || !self.sources.iter().any(|s| s.id == id) {
                return Err(fail(
                    "Source metadata requires a distinct retained source identity.",
                ));
            }
            distinct_text(&entry.source_types)?;
        }
        let mut work_ids = BTreeSet::new();
        for work in &self.pending_inventory {
            scope_ok(&work.source)?;
            if work.id != discovery::source_id(&work.source)
                || !work_ids.insert(&work.id)
                || work.revision == 0
                || work.notified_revision > work.revision
                || work.after_run > self.discovery_runs.len()
                || (!work.new_source && work.fields.is_empty() && work.source_types.is_empty())
            {
                return Err(fail(
                    "Invalid pending inventory identity, revision or coverage boundary.",
                ));
            }
            distinct_text(&work.fields)?;
            distinct_text(&work.source_types)?;
            for field in &work.fields {
                crate::language::plan::checked_field(field)?;
            }
            if let Some(boundary) = &work.capture_boundary {
                let run = work
                    .after_run
                    .checked_sub(1)
                    .and_then(|index| self.discovery_runs.get(index))
                    .filter(|run| {
                        run.state_path == boundary.run && run.source_ids.contains(&work.id)
                    })
                    .ok_or_else(|| {
                        fail("Inventory capture boundary requires its exact source run.")
                    })?;
                boundary.validate(run.max_queries)?;
            }
            match &work.resolution {
                Some(WorkResolution::Excluded {
                    reason,
                    reviewed_at,
                }) => {
                    text(reason)?;
                    environment::parse_time(reviewed_at)?;
                }
                Some(WorkResolution::Inventoried { run, evidence }) => {
                    validate_inventory_work_proof(&self.discovery_runs, work, run, evidence)?;
                }
                _ => {}
            }
        }
        let mut attempt_ids = BTreeSet::new();
        for (index, attempt) in self.refresh_attempts.iter().enumerate() {
            environment::validate_identity(&attempt.environment)?;
            text(&attempt.session_id)?;
            if self
                .target
                .as_ref()
                .is_none_or(|t| t.environment != attempt.environment)
                || attempt.arguments.repository != attempt.environment.view
                || attempt.arguments.query_string != SCOUT_QUERY
                || attempt.id != refresh_id(attempt, index)?
                || !attempt_ids.insert(&attempt.id)
            {
                return Err(fail(
                    "Refresh must retain its exact confirmed target, session, query and identity.",
                ));
            }
            let start = environment::parse_time(&attempt.arguments.start)?;
            let end = environment::parse_time(&attempt.arguments.end)?;
            if !attempt.arguments.start.ends_with('Z')
                || !attempt.arguments.end.ends_with('Z')
                || start >= end
                || end - start > time::Duration::days(30)
                || end != environment::parse_time(&attempt.prepared_at)?
            {
                return Err(fail(
                    "Shallow refresh requires an absolute UTC window of at most 30 days.",
                ));
            }
            let host: RefreshHostEvidence = serde_json::from_slice(&attempt.host_evidence.bytes()?)
                .map_err(|_| fail("Invalid saved refresh host capability evidence."))?;
            validate_refresh_host(
                &host,
                &attempt.environment,
                &attempt.session_id,
                attempt.automatic,
                &attempt.prepared_at,
            )?;
            let dispatched = attempt
                .dispatched_at
                .as_ref()
                .map(|v| environment::parse_time(v))
                .transpose()?;
            if dispatched.is_some_and(|at| at < end)
                || (attempt.phase == RefreshPhase::Prepared && dispatched.is_some())
                || matches!(
                    attempt.phase,
                    RefreshPhase::Dispatched | RefreshPhase::Unresolved | RefreshPhase::Imported
                ) && dispatched.is_none()
            {
                return Err(fail("Invalid durable refresh dispatch transition."));
            }
            let settlement = attempt
                .settlement
                .as_ref()
                .map(|snapshot| {
                    let report: RefreshSettlement = serde_json::from_slice(&snapshot.bytes()?)
                        .map_err(|_| fail("Invalid saved refresh settlement."))?;
                    validate_refresh_settlement(attempt, &report, &snapshot.retrieved_at)?;
                    Ok::<_, Diagnostic>(report)
                })
                .transpose()?;
            if attempt.settlement_history.len() > 20 {
                return Err(fail("Refresh settlement history exceeds its finite limit."));
            }
            for snapshot in &attempt.settlement_history {
                let report: RefreshSettlement = serde_json::from_slice(&snapshot.bytes()?)
                    .map_err(|_| fail("Invalid retained refresh reconciliation evidence."))?;
                validate_refresh_settlement(attempt, &report, &snapshot.retrieved_at)?;
                if report.termination != RefreshTermination::Unresolved {
                    return Err(fail(
                        "Only unresolved settlements may precede final refresh reconciliation.",
                    ));
                }
            }
            if matches!(
                attempt.phase,
                RefreshPhase::Settled | RefreshPhase::Imported
            ) && settlement
                .as_ref()
                .is_none_or(|s| s.termination == RefreshTermination::Unresolved)
                || attempt.phase == RefreshPhase::Unresolved
                    && settlement
                        .as_ref()
                        .is_none_or(|s| s.termination != RefreshTermination::Unresolved)
                || matches!(
                    attempt.phase,
                    RefreshPhase::Prepared | RefreshPhase::Dispatched
                ) && settlement.is_some()
            {
                return Err(fail("Invalid refresh settlement phase."));
            }
            if let Some(result) = &attempt.result {
                if attempt.phase != RefreshPhase::Imported
                    || settlement
                        .as_ref()
                        .is_none_or(|s| s.termination != RefreshTermination::Completed)
                    || environment::parse_time(&result.retrieved_at)? < end
                    || settlement.as_ref().and_then(|s| s.result_sha256.as_ref())
                        != Some(&result.sha256)
                {
                    return Err(fail(
                        "Refresh result requires an exact completed settlement.",
                    ));
                }
                refresh_rows(&result.bytes()?)?;
            } else if attempt.phase == RefreshPhase::Imported {
                return Err(fail("Imported refresh is missing its original result."));
            }
        }
        if self
            .refresh_attempts
            .iter()
            .filter(|a| {
                matches!(
                    a.phase,
                    RefreshPhase::Prepared | RefreshPhase::Dispatched | RefreshPhase::Unresolved
                )
            })
            .count()
            > 1
        {
            return Err(fail("Only one unresolved refresh attempt may exist."));
        }
        Ok(())
    }

    pub fn refresh_blocked(&self) -> bool {
        self.refresh_attempts.iter().any(|a| {
            matches!(
                a.phase,
                RefreshPhase::Prepared | RefreshPhase::Dispatched | RefreshPhase::Unresolved
            )
        })
    }

    pub fn inventory_notifications(&self) -> Vec<PendingInventoryWork> {
        self.pending_inventory
            .iter()
            .filter(|w| w.resolution.is_none() && w.notified_revision < w.revision)
            .cloned()
            .collect()
    }

    pub fn notification_snapshot(&self) -> Result<Option<InventoryNoticeSnapshot>, Diagnostic> {
        self.validate()?;
        inventory_notice_snapshot(
            self.target.as_ref().map(|target| &target.environment),
            &self.pending_inventory,
        )
    }

    pub fn acknowledge_inventory_notice(
        &mut self,
        snapshot_sha256: &str,
        defer: bool,
    ) -> Result<InventoryNoticeSnapshot, Diagnostic> {
        let snapshot = self
            .notification_snapshot()?
            .filter(|snapshot| snapshot.snapshot_sha256 == snapshot_sha256)
            .ok_or_else(|| {
                fail("Inventory notice changed or is empty; inspect the selected state's current notice before acknowledging.")
            })?;
        let work: Vec<_> = self
            .inventory_notifications()
            .into_iter()
            .map(|work| WorkRevision {
                id: work.id,
                revision: work.revision,
            })
            .collect();
        self.acknowledge_inventory_work(&work, defer)?;
        Ok(snapshot)
    }

    fn refresh_reminder_status(
        &self,
        now: &str,
    ) -> Result<Option<RefreshReminderStatus>, Diagnostic> {
        let now = environment::parse_time(now)?;
        let Some(target) = &self.target else {
            return Ok(None);
        };
        let baseline = environment::parse_time(
            self.refresh_reminder
                .as_ref()
                .map_or(&target.confirmed_at, |reminder| &reminder.baseline_at),
        )?;
        let mut last_successful = None;
        for attempt in &self.refresh_attempts {
            if attempt.phase == RefreshPhase::Imported
                && attempt.environment == target.environment
                && let Some(result) = &attempt.result
                && refresh_rows(&result.bytes()?).is_ok()
            {
                let end = environment::parse_time(&attempt.arguments.end)?;
                last_successful = Some(last_successful.map_or(end, |previous| end.max(previous)));
            }
        }
        for scout in &self.scouting {
            if scout.plan.environment == target.environment
                && scout.plan.arguments.repository == target.environment.view
                && refresh_rows(&scout.snapshot.bytes()?).is_ok()
            {
                let end = environment::parse_time(&scout.plan.arguments.end)?;
                last_successful = Some(last_successful.map_or(end, |previous| end.max(previous)));
            }
        }
        let anchor = last_successful.map_or(baseline, |end| baseline.max(end));
        let interval = time::Duration::days(REFRESH_REMINDER_DAYS);
        let mut due_at = anchor
            .checked_add(interval)
            .ok_or_else(|| fail("Refresh reminder date exceeds the supported timestamp range."))?;
        let deferred = self
            .refresh_reminder
            .as_ref()
            .and_then(|reminder| reminder.defer_until.as_deref())
            .map(environment::parse_time)
            .transpose()?;
        if let Some(until) = deferred {
            due_at = due_at.max(until);
        }
        let canonical = |at: time::OffsetDateTime| {
            at.to_offset(time::UtcOffset::UTC)
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| fail("Cannot format refresh reminder date."))
        };
        let anchor = canonical(anchor)?;
        let due_at_text = canonical(due_at)?;
        let cycle = serde_json::to_vec(&(
            "cqlmatic.refresh-reminder",
            1_u32,
            &target.environment,
            anchor,
            &due_at_text,
        ))
        .map_err(|_| fail("Cannot fingerprint refresh reminder cycle."))?;
        let cycle_sha256 = sha256_hex(&cycle);
        let clock_skew = baseline > now
            || last_successful.is_some_and(|end| end > now)
            || deferred.is_some_and(|until| until - now > interval);
        let due = now >= due_at;
        Ok(Some(RefreshReminderStatus {
            due,
            show_startup_notice: due
                && !clock_skew
                && self
                    .refresh_reminder
                    .as_ref()
                    .and_then(|reminder| reminder.notified_cycle_sha256.as_ref())
                    != Some(&cycle_sha256),
            cycle_sha256,
            last_successful_window_end: last_successful.map(canonical).transpose()?,
            due_at: due_at_text,
            clock_skew,
        }))
    }

    pub fn acknowledge_refresh_reminder(
        &mut self,
        cycle_sha256: &str,
        defer: bool,
        now: &str,
    ) -> Result<RefreshReminderStatus, Diagnostic> {
        self.validate()?;
        if cycle_sha256.len() != 64 || !cycle_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(fail("Supply the current refresh reminder cycle SHA-256."));
        }
        let current = self
            .refresh_reminder_status(now)?
            .filter(|status| status.due && !status.clock_skew && status.cycle_sha256 == cycle_sha256)
            .ok_or_else(|| Diagnostic::error(
                "INVENTORY_REFRESH_REMINDER_STALE",
                "Refresh reminder changed or is not due; inspect the selected state's current cycle before acknowledging.",
            ))?;
        let mut candidate = self.clone();
        let target = candidate
            .target
            .as_ref()
            .expect("Due reminder has a target");
        let reminder = candidate
            .refresh_reminder
            .get_or_insert_with(|| RefreshReminder {
                environment: target.environment.clone(),
                baseline_at: target.confirmed_at.clone(),
                notified_cycle_sha256: None,
                defer_until: None,
            });
        reminder.notified_cycle_sha256 = Some(current.cycle_sha256);
        if defer {
            reminder.defer_until = Some(
                environment::parse_time(now)?
                    .checked_add(time::Duration::days(REFRESH_REMINDER_DAYS))
                    .ok_or_else(|| fail("Refresh deferral exceeds the supported timestamp range."))?
                    .to_offset(time::UtcOffset::UTC)
                    .format(&time::format_description::well_known::Rfc3339)
                    .map_err(|_| fail("Cannot format refresh deferral date."))?,
            );
        }
        candidate.validate()?;
        let status = candidate
            .refresh_reminder_status(now)?
            .expect("Acknowledged reminder retains its target");
        *self = candidate;
        Ok(status)
    }

    pub fn record_inventory_hints(
        &mut self,
        environment: &EnvironmentId,
        hints: Vec<InventoryHint>,
    ) -> Result<usize, Diagnostic> {
        self.record_inventory_hints_with_boundary(environment, hints, None)
    }

    pub fn record_inventory_hints_with_boundary(
        &mut self,
        environment: &EnvironmentId,
        hints: Vec<InventoryHint>,
        boundary: Option<InventoryCaptureBoundary>,
    ) -> Result<usize, Diagnostic> {
        if self
            .target
            .as_ref()
            .is_none_or(|t| &t.environment != environment)
        {
            return Err(fail(
                "Inventory hints require the exact confirmed environment.",
            ));
        }
        if let Some(boundary) = &boundary {
            let run = self
                .discovery_runs
                .last()
                .filter(|run| run.state_path == boundary.run && run.status != "complete")
                .ok_or_else(|| {
                    fail("Inventory hints require the exact unfinished discovery run.")
                })?;
            boundary.validate(run.max_queries)?;
        }
        let mut candidate = self.clone();
        let mut changed = 0;
        for hint in hints {
            scope_ok(&hint.source)?;
            for field in &hint.fields {
                crate::language::plan::checked_field(field)?;
            }
            let new_source = !candidate.sources.iter().any(|s| s.selectors == hint.source);
            let id = discovery::source_id(&hint.source);
            let queued =
                candidate.queue_inventory_work(hint.source, hint.fields, vec![], new_source)?;
            if queued {
                let boundary = boundary.as_ref().filter(|_| {
                    candidate
                        .discovery_runs
                        .last()
                        .is_some_and(|run| run.source_ids.contains(&id))
                });
                candidate
                    .pending_inventory
                    .iter_mut()
                    .find(|work| work.id == id)
                    .expect("Queued exact work identity")
                    .capture_boundary = boundary.cloned();
            }
            changed += usize::from(queued);
        }
        candidate.validate()?;
        *self = candidate;
        Ok(changed)
    }

    fn queue_inventory_work(
        &mut self,
        source: Scope,
        fields: Vec<String>,
        source_types: Vec<String>,
        new_source: bool,
    ) -> Result<bool, Diagnostic> {
        if !new_source && fields.is_empty() && source_types.is_empty() {
            return Ok(false);
        }
        let id = discovery::source_id(&source);
        let work = if let Some(index) = self.pending_inventory.iter().position(|w| w.id == id) {
            &mut self.pending_inventory[index]
        } else {
            if self.pending_inventory.len() >= MAX_PENDING_WORK {
                return Err(fail(
                    "Pending inventory work is limited to 1000 exact source identities.",
                ));
            }
            self.pending_inventory.push(PendingInventoryWork {
                id,
                source,
                fields: vec![],
                source_types: vec![],
                new_source: false,
                revision: 0,
                notified_revision: 0,
                deferred: false,
                after_run: self.discovery_runs.len(),
                capture_boundary: None,
                resolution: None,
            });
            self.pending_inventory
                .last_mut()
                .expect("Just appended pending inventory work")
        };
        let mut changed = new_source && !work.new_source;
        work.new_source |= new_source;
        for (incoming, retained) in [
            (fields, &mut work.fields),
            (source_types, &mut work.source_types),
        ] {
            for value in incoming {
                if !retained.contains(&value) {
                    retained.push(value);
                    changed = true;
                }
            }
            retained.sort();
        }
        if changed {
            work.revision = work
                .revision
                .checked_add(1)
                .ok_or_else(|| fail("Inventory work revision overflow."))?;
            work.deferred = false;
            work.resolution = None;
            work.after_run = self.discovery_runs.len();
            work.capture_boundary = None;
        }
        Ok(changed)
    }

    pub fn acknowledge_inventory_work(
        &mut self,
        work: &[WorkRevision],
        defer: bool,
    ) -> Result<(), Diagnostic> {
        let mut candidate = self.clone();
        for revision in work {
            let entry = candidate.pending_inventory.iter_mut().find(|w| w.id == revision.id && w.revision == revision.revision && w.resolution.is_none())
                .ok_or_else(|| fail("Inventory work changed; inspect its current revision before acknowledging."))?;
            entry.notified_revision = entry.revision;
            entry.deferred = defer;
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn exclude_inventory_work(
        &mut self,
        work: &[WorkRevision],
        reason: &str,
        reviewed_at: &str,
    ) -> Result<(), Diagnostic> {
        text(reason)?;
        environment::parse_time(reviewed_at)?;
        let mut candidate = self.clone();
        candidate.acknowledge_inventory_work(work, false)?;
        for revision in work {
            let entry = candidate
                .pending_inventory
                .iter_mut()
                .find(|w| w.id == revision.id)
                .expect("Acknowledged exact work identity");
            entry.resolution = Some(WorkResolution::Excluded {
                reason: reason.into(),
                reviewed_at: reviewed_at.into(),
            });
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn resolve_inventoried_work(&mut self, status: &InventoryStatus) -> Result<(), Diagnostic> {
        self.resolve_inventoried_work_with_evidence(status, &[])
    }

    pub fn resolve_inventoried_work_with_evidence(
        &mut self,
        status: &InventoryStatus,
        evidence: &[InventoryRunEvidence],
    ) -> Result<(), Diagnostic> {
        self.validate()?;
        if status.environment.as_ref() != self.target.as_ref().map(|target| &target.environment) {
            return Err(fail(
                "Inventory work status requires its exact confirmed environment.",
            ));
        }
        let Some(environment) = status.environment.as_ref() else {
            return if evidence.is_empty() {
                Ok(())
            } else {
                Err(fail("Inventory run evidence requires a confirmed target."))
            };
        };
        let mut reports = BTreeMap::new();
        for report in evidence {
            let run = self
                .discovery_runs
                .iter()
                .find(|run| run.state_path == report.run && run.status == "complete")
                .ok_or_else(|| {
                    fail("Inventory work evidence requires a completed discovery run.")
                })?;
            if !sha256_valid(&report.request_sha256)
                || reports.insert(report.run.as_str(), report).is_some()
                || report.captures.len() > run.max_queries.min(1000) as usize
            {
                return Err(fail("Invalid or duplicate inventory run evidence."));
            }
            let start = environment::parse_time(&run.start)?;
            let end = environment::parse_time(&run.end)?;
            let mut ids = BTreeSet::new();
            for capture in &report.captures {
                let probe = &capture.probe;
                scope_ok(&probe.source)?;
                for field in probe.fields.iter().chain(probe.field.iter()) {
                    crate::language::plan::checked_field(field)?;
                }
                let capture_start = environment::parse_time(&probe.start)?;
                let capture_end = environment::parse_time(&probe.end)?;
                if !run
                    .source_ids
                    .contains(&discovery::source_id(&probe.source))
                    || capture_start < start
                    || capture_end > end
                    || capture_start >= capture_end
                    || environment::parse_time(&capture.retrieved_at)? < capture_end
                    || capture.receipt.completion != "complete"
                    || !capture.receipt.cleanup_complete()
                    || capture.receipt.diagnostic.is_some()
                    || capture.receipt.query != probe.query
                    || capture.receipt.start != probe.start
                    || capture.receipt.end != probe.end
                    || matches!(probe.kind.as_str(), "tag-detect" | "record-kind-facet")
                        && probe.field.is_none()
                    || !ids.insert(discovery::probe_id(&environment.view, probe))
                {
                    return Err(fail(
                        "Inventory work requires exact completed measurement captures.",
                    ));
                }
                discovery::check_measurement(probe, &capture.receipt.events)?;
            }
        }
        let mut resolutions = Vec::new();
        for (work_index, work) in self
            .pending_inventory
            .iter()
            .enumerate()
            .filter(|(_, work)| work.resolution.is_none())
        {
            let Some(source) = status
                .sources
                .iter()
                .find(|s| s.source_id == work.id && s.status == Progress::Done)
            else {
                continue;
            };
            if !work
                .fields
                .iter()
                .all(|name| source.fields.iter().any(|f| &f.field == name && f.measured))
            {
                continue;
            }
            let mut captures = Vec::new();
            let mut capture_runs = Vec::new();
            for (run_index, run) in self.discovery_runs.iter().enumerate() {
                let Some(report) = reports.get(run.state_path.as_str()) else {
                    continue;
                };
                if !inventory_work_run_eligible(run_index, run, work, Some(&report.request_sha256))
                    || !work.fields.is_empty()
                        && run.mode == DiscoveryMode::Quick
                        && run.quick_depth == QuickDepth::SourcesOnly
                {
                    continue;
                }
                for capture in &report.captures {
                    if capture.probe.source != work.source {
                        continue;
                    }
                    let id = discovery::probe_id(&environment.view, &capture.probe);
                    if run_index < work.after_run
                        && work
                            .capture_boundary
                            .as_ref()
                            .is_some_and(|boundary| boundary.accepted_probe_ids.contains(&id))
                    {
                        continue;
                    }
                    captures.push(capture.clone());
                    capture_runs.push((run_index, id));
                }
            }
            let mut proofs = BTreeMap::new();
            if work.fields.is_empty() {
                let Some(index) = captures.iter().rposition(|capture| {
                    capture.probe.kind == "volume"
                        && capture.receipt.events.first().is_some_and(|row| {
                            row.get("events")
                                .or_else(|| row.get("_count"))
                                .and_then(|value| {
                                    value.as_u64().or_else(|| value.as_str()?.parse().ok())
                                })
                                .is_some_and(|count| count > 0)
                        })
                }) else {
                    continue;
                };
                let (run_index, id) = &capture_runs[index];
                let run = &self.discovery_runs[*run_index];
                proofs.insert(
                    *run_index,
                    InventoryWorkRunProof {
                        run: run.state_path.clone(),
                        request_sha256: reports[run.state_path.as_str()].request_sha256.clone(),
                        source_probe_id: Some(id.clone()),
                        fields: BTreeMap::new(),
                    },
                );
            } else {
                let measured = discovery::field_captures(&captures, &work.source)?;
                if !work.fields.iter().all(|field| measured.contains_key(field)) {
                    continue;
                }
                for field in &work.fields {
                    let (capture_index, _, _) = measured[field];
                    let (run_index, id) = &capture_runs[capture_index];
                    let run = &self.discovery_runs[*run_index];
                    proofs
                        .entry(*run_index)
                        .or_insert_with(|| InventoryWorkRunProof {
                            run: run.state_path.clone(),
                            request_sha256: reports[run.state_path.as_str()].request_sha256.clone(),
                            source_probe_id: None,
                            fields: BTreeMap::new(),
                        })
                        .fields
                        .insert(field.clone(), id.clone());
                }
            }
            let run = proofs
                .last_key_value()
                .expect("Exact work has capture proofs")
                .1
                .run
                .clone();
            let evidence: Vec<_> = proofs.into_values().collect();
            validate_inventory_work_proof(&self.discovery_runs, work, &run, &evidence)?;
            resolutions.push((work_index, WorkResolution::Inventoried { run, evidence }));
        }
        for (index, resolution) in resolutions {
            let work = &mut self.pending_inventory[index];
            work.resolution = Some(resolution);
            work.deferred = false;
            work.notified_revision = work.revision;
        }
        self.validate()
    }

    pub fn prepare_refresh(
        &mut self,
        session_id: String,
        host_raw: &[u8],
        automatic: bool,
        now: &str,
    ) -> Result<String, Diagnostic> {
        self.validate()?;
        if automatic && !AUTOMATIC_REFRESH_AVAILABLE {
            return Err(fail(
                "Automatic inventory refresh is unavailable. Use explicit manual refresh.",
            ));
        }
        if self.refresh_blocked() {
            return Err(fail(
                "Reconcile the exact unresolved refresh before preparing another attempt.",
            ));
        }
        if automatic && !self.refresh_settings.automatic {
            return Err(fail(
                "Automatic inventory refresh is off. Manual refresh remains available.",
            ));
        }
        if automatic
            && self
                .refresh_attempts
                .iter()
                .any(|a| a.automatic && a.session_id == session_id && a.dispatched_at.is_some())
        {
            return Err(fail(
                "Automatic inventory refresh was already dispatched in this host session. Manual refresh remains available.",
            ));
        }
        if self.refresh_attempts.len() >= MAX_REFRESH_ATTEMPTS {
            return Err(fail(
                "Refresh history reached its finite limit; retain it and use a new explicitly selected workflow state.",
            ));
        }
        if self
            .scout_plan
            .as_ref()
            .is_some_and(|p| !self.scouting.iter().any(|s| &s.plan == p))
        {
            return Err(fail(
                "Finish or safely reconcile pending scouting before preparing refresh; its progress is retained.",
            ));
        }
        let target = self
            .target
            .as_ref()
            .ok_or_else(|| fail("Confirm an exact target before inventory refresh."))?;
        let host: RefreshHostEvidence = serde_json::from_slice(host_raw).map_err(|_| fail("Expected strict protected host evidence for a provider-enforced search lifetime; a client timeout is insufficient."))?;
        validate_refresh_host(&host, &target.environment, &session_id, automatic, now)?;
        let end = environment::parse_time(now)?;
        let start = (end - time::Duration::days(self.refresh_settings.lookback.days()))
            .format(&time::format_description::well_known::Rfc3339)
            .map_err(|_| fail("Cannot format refresh window."))?;
        let mut attempt = RefreshAttempt {
            id: String::new(),
            environment: target.environment.clone(),
            session_id,
            arguments: SearchArguments {
                repository: target.environment.view.clone(),
                query_string: SCOUT_QUERY.into(),
                start,
                end: now.into(),
            },
            automatic,
            host_evidence: RawSnapshot::new(host_raw, now)?,
            prepared_at: now.into(),
            dispatched_at: None,
            phase: RefreshPhase::Prepared,
            settlement: None,
            settlement_history: vec![],
            result: None,
        };
        attempt.id = refresh_id(&attempt, self.refresh_attempts.len())?;
        let id = attempt.id.clone();
        self.refresh_attempts.push(attempt);
        self.validate()?;
        Ok(id)
    }

    pub fn dispatch_refresh(
        &mut self,
        attempt_id: &str,
        session_id: &str,
        now: &str,
    ) -> Result<SearchArguments, Diagnostic> {
        self.validate()?;
        if !AUTOMATIC_REFRESH_AVAILABLE
            && self
                .refresh_attempts
                .iter()
                .any(|attempt| attempt.id == attempt_id && attempt.automatic)
        {
            return Err(fail(
                "Automatic inventory refresh is unavailable; safely settle this prepared attempt as not-started before manual refresh.",
            ));
        }
        if self.refresh_attempts.iter().any(|a| {
            a.id == attempt_id
                && a.automatic
                && self.refresh_attempts.iter().any(|other| {
                    other.id != a.id
                        && other.automatic
                        && other.session_id == a.session_id
                        && other.dispatched_at.is_some()
                })
        }) {
            return Err(fail(
                "Another automatic refresh was already dispatched in this host session; this prepared attempt cannot renew that allowance.",
            ));
        }
        if !self.refresh_settings.automatic
            && self
                .refresh_attempts
                .iter()
                .any(|a| a.id == attempt_id && a.automatic)
        {
            return Err(fail(
                "Automatic refresh is off; settle the prepared attempt as not-started before preparing a manual refresh.",
            ));
        }
        let attempt = self
            .refresh_attempts
            .iter_mut()
            .find(|a| a.id == attempt_id)
            .ok_or_else(|| fail("Unknown exact refresh attempt."))?;
        if attempt.phase != RefreshPhase::Prepared || attempt.session_id != session_id {
            return Err(fail(
                "Dispatch requires the exact prepared refresh and verified host session; dispatched attempts must be reconciled, never retried.",
            ));
        }
        if environment::parse_time(now)? < environment::parse_time(&attempt.prepared_at)? {
            return Err(fail("Refresh dispatch cannot precede preparation."));
        }
        attempt.phase = RefreshPhase::Dispatched;
        attempt.dispatched_at = Some(now.into());
        let arguments = attempt.arguments.clone();
        self.validate()?;
        Ok(arguments)
    }

    pub fn settle_refresh(
        &mut self,
        attempt_id: &str,
        raw: &[u8],
        now: &str,
    ) -> Result<(), Diagnostic> {
        let report: RefreshSettlement = serde_json::from_slice(raw)
            .map_err(|_| fail("Expected strict exact-attempt host settlement evidence."))?;
        let mut candidate = self.clone();
        let attempt = candidate
            .refresh_attempts
            .iter_mut()
            .find(|a| a.id == attempt_id)
            .ok_or_else(|| fail("Unknown exact refresh attempt."))?;
        validate_refresh_settlement(attempt, &report, now)?;
        if attempt
            .settlement
            .as_ref()
            .is_some_and(|s| s.sha256 == sha256_hex(raw))
        {
            return Ok(());
        }
        if matches!(
            attempt.phase,
            RefreshPhase::Settled | RefreshPhase::Imported
        ) {
            return Err(fail("Refresh already has different settlement evidence."));
        }
        if let Some(previous) = attempt.settlement.take() {
            if attempt.settlement_history.len() >= 20 {
                return Err(fail(
                    "Refresh reconciliation history reached its finite limit; retain the unresolved attempt for inspection.",
                ));
            }
            attempt.settlement_history.push(previous);
        }
        attempt.phase = if report.termination == RefreshTermination::Unresolved {
            RefreshPhase::Unresolved
        } else {
            RefreshPhase::Settled
        };
        attempt.settlement = Some(RawSnapshot::new(raw, now)?);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }

    pub fn import_refresh(
        &mut self,
        attempt_id: &str,
        raw: &[u8],
        retrieved_at: &str,
    ) -> Result<usize, Diagnostic> {
        let rows = refresh_rows(raw)?;
        let mut candidate = self.clone();
        let index = candidate
            .refresh_attempts
            .iter()
            .position(|a| a.id == attempt_id)
            .ok_or_else(|| fail("Unknown exact refresh attempt."))?;
        let attempt = &candidate.refresh_attempts[index];
        if attempt.phase == RefreshPhase::Imported {
            let saved = attempt
                .result
                .as_ref()
                .expect("Validated imported refresh has its capture");
            if saved.sha256 == sha256_hex(raw)
                && environment::parse_time(&saved.retrieved_at)?
                    == environment::parse_time(retrieved_at)?
            {
                return Ok(0);
            }
            return Err(fail(
                "Refresh import conflicts with its retained exact capture.",
            ));
        }
        let settlement = attempt
            .settlement
            .as_ref()
            .ok_or_else(|| fail("Settle the exact refresh before importing its result."))?;
        let report: RefreshSettlement = serde_json::from_slice(&settlement.bytes()?)
            .map_err(|_| fail("Invalid refresh settlement."))?;
        if attempt.phase != RefreshPhase::Settled
            || report.termination != RefreshTermination::Completed
            || report.result_sha256.as_deref() != Some(sha256_hex(raw).as_str())
            || environment::parse_time(retrieved_at)?
                < environment::parse_time(&attempt.arguments.end)?
        {
            return Err(fail(
                "Only an exactly completed, settled refresh can contribute metadata.",
            ));
        }
        let mut changed = 0;
        for (source, source_types, events) in rows {
            let id = discovery::source_id(&source);
            let new_source = !candidate.sources.iter().any(|s| s.id == id);
            if new_source {
                candidate.sources.push(InventorySource {
                    id: id.clone(),
                    selectors: source.clone(),
                    scouted_events: Some(events),
                    expected: false,
                    review: None,
                    binding: None,
                });
            }
            let novel = if let Some(existing) = candidate
                .source_metadata
                .iter_mut()
                .find(|m| m.source == source)
            {
                let novel: Vec<_> = source_types
                    .iter()
                    .filter(|s| !existing.source_types.contains(s))
                    .cloned()
                    .collect();
                existing.source_types.extend(novel.clone());
                existing.source_types.sort();
                novel
            } else {
                candidate.source_metadata.push(SourceMetadata {
                    source: source.clone(),
                    source_types: source_types.clone(),
                });
                if new_source { source_types } else { vec![] }
            };
            changed +=
                usize::from(candidate.queue_inventory_work(source, vec![], novel, new_source)?);
        }
        let attempt = &mut candidate.refresh_attempts[index];
        attempt.result = Some(RawSnapshot::new(raw, retrieved_at)?);
        attempt.phase = RefreshPhase::Imported;
        candidate.validate()?;
        *self = candidate;
        Ok(changed)
    }

    pub fn new(now: &str) -> Result<Self, Diagnostic> {
        environment::parse_time(now)?;
        Ok(Self {
            format: "cqlmatic-inventory-state".into(),
            schema_version: STATE_VERSION,
            created_at: now.into(),
            updated_at: now.into(),
            discovery_mode: DiscoveryMode::Full,
            quick_depth: QuickDepth::SourcesAndFields,
            unverified_leads: vec![],
            query_observations: vec![],
            refresh_settings: RefreshSettings::default(),
            refresh_attempts: vec![],
            refresh_reminder: None,
            source_metadata: vec![],
            pending_inventory: vec![],
            scout_plan: None,
            scouting: vec![],
            target: None,
            sources: vec![],
            parser_pages: vec![],
            parsers: vec![],
            parser_exclusions: vec![],
            packs: vec![],
            active_pack: None,
            discovery_runs: vec![],
            parser_runs: vec![],
            failures: vec![],
            reconciled_evidence_sha256: None,
            reconciled_at: None,
        })
    }
    pub fn evidence_digest(&self) -> Result<String, Diagnostic> {
        let mut copy = self.clone();
        copy.reconciled_evidence_sha256 = None;
        copy.reconciled_at = None;
        copy.updated_at.clear();
        // Preserve historical reconciliation commitments when the runtime preference default changes.
        copy.refresh_settings = RefreshSettings {
            automatic: true,
            lookback: RefreshLookback::Day,
        };
        copy.refresh_attempts.clear();
        copy.refresh_reminder = None;
        for work in &mut copy.pending_inventory {
            work.notified_revision = 0;
            work.deferred = false;
        }
        serde_json::to_vec(&copy)
            .map(|b| sha256_hex(&b))
            .map_err(|_| fail("Cannot serialize inventory evidence."))
    }
    pub fn validate(&self) -> Result<(), Diagnostic> {
        if self.format != "cqlmatic-inventory-state"
            || !matches!(self.schema_version, 1..=STATE_VERSION)
        {
            return Err(fail("Unsupported inventory state."));
        }
        environment::parse_time(&self.created_at)?;
        environment::parse_time(&self.updated_at)?;
        self.validate_refresh()?;
        if let Some(active) = &self.active_pack
            && !self
                .packs
                .iter()
                .any(|p| p.path == active.path && p.content_digest == active.content_digest)
        {
            return Err(fail(
                "The active pack must reference a retained inventory snapshot.",
            ));
        }
        if let Some(target) = &self.target {
            environment::validate_identity(&target.environment)?;
            text(&target.context)?;
            environment::parse_time(&target.confirmed_at)?;
        }
        let mut ids = BTreeSet::new();
        for source in &self.sources {
            scope_ok(&source.selectors)?;
            if source.id != discovery::source_id(&source.selectors) || !ids.insert(&source.id) {
                return Err(fail("Inconsistent or duplicate source identity."));
            }
            if let Some(review) = &source.review {
                text(&review.purpose)?;
                text(&review.data_model)?;
                text(&review.evidence)?;
                environment::parse_time(&review.reviewed_at)?;
            }
            if let Some(binding) = &source.binding {
                self.validate_binding(binding)?;
            }
        }
        if let Some(plan) = &self.scout_plan {
            validate_scout_plan(plan)?;
        }
        for scout in &self.scouting {
            validate_scout_plan(&scout.plan)?;
            if let Some(target) = &self.target
                && target.environment != scout.plan.environment
            {
                return Err(fail("Scouting belongs to another environment."));
            }
            if mcp::normalize(&scout.snapshot.bytes()?)?.rows.len() != scout.rows {
                return Err(fail("Scouting row count differs from saved evidence."));
            }
        }
        if self.unverified_leads.len() > 1000 || self.query_observations.len() > 1000 {
            return Err(fail(
                "Inventory leads and query observations are limited to 1000 entries each.",
            ));
        }
        let mut leads = BTreeSet::new();
        for lead in &self.unverified_leads {
            scope_ok(&lead.source)?;
            crate::language::plan::checked_field(&lead.field)?;
            if !leads.insert((discovery::source_id(&lead.source), &lead.field)) {
                return Err(fail("Duplicate unverified field lead."));
            }
        }
        let mut observations = BTreeSet::new();
        for observation in &self.query_observations {
            observation.validate()?;
            if !observations.insert(&observation.id)
                || !self.sources.iter().any(|s| s.id == observation.source_id)
                || self
                    .target
                    .as_ref()
                    .is_none_or(|t| t.environment != observation.environment)
            {
                return Err(fail(
                    "Query observation must belong to its exact confirmed environment and saved source.",
                ));
            }
        }
        let mut parsers = BTreeSet::new();
        for parser in &self.parsers {
            let parsed = parse_parser(&parser.id, parser.snapshot.clone())?;
            if !parsers.insert(&parser.id)
                || parser.declarations != parsed.declarations
                || parser.name != parsed.name
            {
                return Err(fail(
                    "Parser declaration extraction differs from its saved snapshot.",
                ));
            }
        }
        catalog(self)?;
        for exclusion in &self.parser_exclusions {
            self.parser_version(&exclusion.parser_id, &exclusion.parser_sha256)?;
            environment::parse_time(&exclusion.reviewed_at)?;
            text(&exclusion.reason)?;
        }
        Ok(())
    }
    pub fn prepare_scout(
        &mut self,
        environment: EnvironmentId,
        start: String,
        end: String,
    ) -> Result<ScoutPlan, Diagnostic> {
        self.prepare_scout_with_budget(environment, start, end, None)
    }
    pub fn prepare_scout_with_budget(
        &mut self,
        environment: EnvironmentId,
        start: String,
        end: String,
        max_seconds: Option<u64>,
    ) -> Result<ScoutPlan, Diagnostic> {
        if self.discovery_mode == DiscoveryMode::Quick && max_seconds.is_none() {
            return Err(fail(
                "Quick scouting requires an explicit host search budget of 1..600 seconds.",
            ));
        }
        self.validate_scout_environment(&environment)?;
        let plan = ScoutPlan {
            arguments: SearchArguments {
                repository: self.scout_repository(&environment).into(),
                query_string: SCOUT_QUERY.into(),
                start,
                end,
            },
            environment,
            query_sha256: sha256_hex(SCOUT_QUERY.as_bytes()),
            max_seconds,
        };
        validate_scout_plan(&plan)?;
        self.scout_plan = Some(plan.clone());
        Ok(plan)
    }
    pub fn import_scout(
        &mut self,
        arguments: SearchArguments,
        raw: &[u8],
        retrieved_at: &str,
    ) -> Result<(), Diagnostic> {
        let plan = self
            .scout_plan
            .as_ref()
            .ok_or_else(|| fail("Prepare scouting before importing its saved response."))?;
        self.validate_scout_environment(&plan.environment)?;
        validate_scout_plan(plan)?;
        if self.discovery_mode == DiscoveryMode::Quick && plan.max_seconds.is_none() {
            return Err(fail(
                "Prepare Quick scouting with an explicit finite host budget before importing its response.",
            ));
        }
        if plan.arguments.repository != self.scout_repository(&plan.environment) {
            return Err(fail(
                "Scout repository differs from the current discovery mode. Prepare scouting again.",
            ));
        }
        if arguments != plan.arguments {
            return Err(fail(
                "Saved tool arguments differ from the exact prepared census query/window.",
            ));
        }
        let retrieval_time = environment::parse_time(retrieved_at)?;
        if retrieval_time < environment::parse_time(&plan.arguments.end)? {
            return Err(fail("Scout retrieval time cannot precede its window end."));
        }
        let result = mcp::normalize(raw)?;
        let mut observations = BTreeMap::<String, (Scope, u64)>::new();
        for row in &result.rows {
            let selectors = Scope {
                repo: row
                    .get("#repo")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                type_: row
                    .get("#type")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
                event: None,
            };
            scope_ok(&selectors)?;
            let events = row
                .get("events")
                .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
                .ok_or_else(|| fail("Census rows require nonnegative events counts."))?;
            let entry = observations
                .entry(discovery::source_id(&selectors))
                .or_insert((selectors, 0));
            entry.1 = entry
                .1
                .checked_add(events)
                .ok_or_else(|| fail("Census count overflow."))?;
        }
        let evidence = ScoutEvidence {
            plan: plan.clone(),
            snapshot: RawSnapshot::new(raw, retrieved_at)?,
            rows: result.rows.len(),
        };
        if self.scouting.iter().any(|scout| {
            scout.plan == evidence.plan
                && scout.snapshot.sha256 == evidence.snapshot.sha256
                && environment::parse_time(&scout.snapshot.retrieved_at)
                    .is_ok_and(|time| time == retrieval_time)
        }) {
            return Ok(());
        }
        let mut candidate = self.clone();
        for source in &mut candidate.sources {
            source.scouted_events = None;
        }
        for (id, (selectors, events)) in observations {
            if let Some(source) = candidate.sources.iter_mut().find(|s| s.id == id) {
                source.scouted_events = Some(events);
            } else {
                candidate.sources.push(InventorySource {
                    id,
                    selectors: selectors.clone(),
                    scouted_events: Some(events),
                    expected: self
                        .target
                        .as_ref()
                        .is_some_and(|t| t.scope == TargetScope::FullEnvironment),
                    review: None,
                    binding: None,
                });
                if candidate.target.is_some() {
                    candidate.queue_inventory_work(selectors, vec![], vec![], true)?;
                }
            }
        }
        candidate.scouting.push(evidence);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
    fn validate_scout_environment(&self, environment: &EnvironmentId) -> Result<(), Diagnostic> {
        if self
            .target
            .as_ref()
            .is_some_and(|target| &target.environment != environment)
            || self
                .scouting
                .iter()
                .any(|scout| &scout.plan.environment != environment)
        {
            return Err(fail("Start a new state for a different environment."));
        }
        Ok(())
    }
    fn scout_repository<'a>(&self, environment: &'a EnvironmentId) -> &'a str {
        match self.discovery_mode {
            DiscoveryMode::Full => "search-all",
            DiscoveryMode::Quick => &environment.view,
        }
    }
    pub fn confirm(
        &mut self,
        environment: EnvironmentId,
        scope: TargetScope,
        context: String,
        expected: Vec<Scope>,
        now: &str,
    ) -> Result<(), Diagnostic> {
        environment::validate_identity(&environment)?;
        text(&context)?;
        environment::parse_time(now)?;
        if self
            .scout_plan
            .as_ref()
            .is_some_and(|p| p.environment != environment)
            || self
                .target
                .as_ref()
                .is_some_and(|t| t.environment != environment)
        {
            return Err(fail(
                "Confirmed environment must match existing scouting; use a new state to change it.",
            ));
        }
        if scope == TargetScope::SelectedSources && expected.is_empty() {
            return Err(fail(
                "Selected-source scope requires explicit expected sources.",
            ));
        }
        if self.discovery_mode == DiscoveryMode::Quick && scope != TargetScope::SelectedSources {
            return Err(fail(
                "Quick discovery requires an explicitly selected-source target.",
            ));
        }
        for selectors in &expected {
            scope_ok(selectors)?;
        }
        for source in &mut self.sources {
            source.expected =
                scope == TargetScope::FullEnvironment || expected.contains(&source.selectors);
        }
        for selectors in expected {
            let id = discovery::source_id(&selectors);
            if self.sources.iter().any(|s| s.id == id) {
                continue;
            }
            self.sources.push(InventorySource {
                id,
                selectors,
                scouted_events: None,
                expected: true,
                review: None,
                binding: None,
            });
        }
        if self.refresh_reminder.is_none() {
            self.refresh_reminder = Some(RefreshReminder {
                environment: environment.clone(),
                baseline_at: self
                    .target
                    .as_ref()
                    .map_or(now, |target| &target.confirmed_at)
                    .into(),
                notified_cycle_sha256: None,
                defer_until: None,
            });
        }
        self.target = Some(ConfirmedTarget {
            environment,
            scope,
            context,
            confirmed_at: now.into(),
        });
        Ok(())
    }
    pub fn register_pack_sources(&mut self, pack: &Pack) -> Result<(), Diagnostic> {
        pack.validate()?;
        if self
            .target
            .as_ref()
            .is_none_or(|t| t.environment != pack.environment)
        {
            return Err(fail(
                "Imported selectors require the exact confirmed target environment.",
            ));
        }
        let mut candidate = self.clone();
        for record in &pack.data.sources {
            let Some(selectors) = environment::source(record)?.selectors else {
                continue;
            };
            let selectors = Scope {
                repo: selectors.repo,
                type_: selectors.type_,
                event: None,
            };
            if selectors.repo.is_none() && selectors.type_.is_none() {
                continue;
            }
            scope_ok(&selectors)?;
            let id = discovery::source_id(&selectors);
            if !candidate.sources.iter().any(|s| s.id == id) {
                candidate.sources.push(InventorySource {
                    id,
                    selectors: selectors.clone(),
                    scouted_events: None,
                    expected: false,
                    review: None,
                    binding: None,
                });
                candidate.queue_inventory_work(selectors, vec![], vec![], true)?;
            }
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
    pub fn add_unverified_leads(
        &mut self,
        leads: Vec<UnverifiedFieldLead>,
    ) -> Result<(), Diagnostic> {
        let mut candidate = self.clone();
        for lead in leads {
            if !candidate
                .unverified_leads
                .iter()
                .any(|p| p.source == lead.source && p.field == lead.field)
            {
                candidate.unverified_leads.push(lead);
            }
        }
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
    pub fn record_query_observation(
        &mut self,
        observation: QueryObservation,
    ) -> Result<(), Diagnostic> {
        let mut candidate = self.clone();
        if let Some(existing) = candidate
            .query_observations
            .iter()
            .find(|p| p.id == observation.id)
        {
            if existing != &observation {
                return Err(fail(
                    "Query observation identity conflicts with retained evidence.",
                ));
            }
            return Ok(());
        }
        candidate.query_observations.push(observation);
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
    pub fn source_mut(&mut self, id: &str) -> Result<&mut InventorySource, Diagnostic> {
        self.sources
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| fail("Unknown source ID; use the status source IDs."))
    }
    pub fn parser_version(&self, id: &str, hash: &str) -> Result<&ParserSnapshot, Diagnostic> {
        self.parsers
            .iter()
            .find(|p| p.id == id && p.snapshot.sha256 == hash)
            .ok_or_else(|| {
                fail("Binding must identify an exact saved parser ID and SHA-256 snapshot.")
            })
    }
    pub fn validate_binding(&self, binding: &ParserBinding) -> Result<(), Diagnostic> {
        match binding {
            ParserBinding::Parser {
                parser_id,
                parser_sha256,
                reviewed_at,
                evidence,
                semantics,
            } => {
                let parser = self.parser_version(parser_id, parser_sha256)?;
                let mut reviewed = BTreeSet::new();
                for semantic in semantics {
                    text(&semantic.interpretation)?;
                    text(&semantic.evidence)?;
                    if !parser.declarations.review_lines.contains(&semantic.line)
                        || !reviewed.insert(semantic.line)
                    {
                        return Err(fail(
                            "Semantic review must refer to distinct pending parser lines.",
                        ));
                    }
                }
                environment::parse_time(reviewed_at)?;
                text(evidence)?;
            }
            ParserBinding::NotApplicable {
                reviewed_at,
                evidence,
                ..
            } => {
                environment::parse_time(reviewed_at)?;
                text(evidence)?;
            }
        }
        Ok(())
    }
    pub fn import_parser_inventory(
        &mut self,
        pages: Vec<ParserPage>,
        parsers: Vec<ParserSnapshot>,
    ) -> Result<(), Diagnostic> {
        let mut candidate = self.clone();
        candidate.parser_pages = pages;
        candidate.parsers = parsers;
        // Snapshot changes deliberately invalidate old bindings and exclusions.
        for source in &mut candidate.sources {
            if let Some(ParserBinding::Parser {
                parser_id,
                parser_sha256,
                ..
            }) = &source.binding
                && !candidate
                    .parsers
                    .iter()
                    .any(|p| &p.id == parser_id && &p.snapshot.sha256 == parser_sha256)
            {
                source.binding = None;
            }
        }
        candidate.parser_exclusions.retain(|e| {
            candidate
                .parsers
                .iter()
                .any(|p| p.id == e.parser_id && p.snapshot.sha256 == e.parser_sha256)
        });
        candidate.validate()?;
        *self = candidate;
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    pub fn discovery_request(
        &self,
        start: String,
        end: String,
        ids: &[String],
        values: Vec<String>,
        kinds: Vec<String>,
        population_fields: Vec<discovery::FieldSelection>,
        max_queries: u32,
        max_seconds: u64,
    ) -> Result<DiscoverRequest, Diagnostic> {
        let target = self
            .target
            .as_ref()
            .ok_or_else(|| fail("Confirm CID, cloud, view and target scope before discovery."))?;
        if self.discovery_mode == DiscoveryMode::Quick
            && target.scope != TargetScope::SelectedSources
        {
            return Err(fail(
                "Confirm a selected-source target before Quick discovery.",
            ));
        }
        let mut sources = vec![];
        let mut contracts = vec![];
        for source in self
            .sources
            .iter()
            .filter(|s| s.expected && (ids.is_empty() || ids.contains(&s.id)))
        {
            sources.push(source.selectors.clone());
            if let Some(ParserBinding::Parser {
                parser_id,
                parser_sha256,
                ..
            }) = &source.binding
                && !(self.discovery_mode == DiscoveryMode::Quick
                    && self.quick_depth == QuickDepth::SourcesOnly)
            {
                let parser = self.parser_version(parser_id, parser_sha256)?;
                let declaration = &parser.declarations;
                contracts.push(json!({"source":source.selectors,"contract":ParserContract { source:"Explicit reviewed inventory parser binding".into(), parser:parser.id.clone(), retrieved_at:parser.snapshot.retrieved_at.clone(), source_hash:Some(parser.snapshot.sha256.clone()), declared_fields:declaration.fields.clone(), fields_to_tag:declaration.fields_to_tag.clone(), array_fields:declaration.array_fields.clone(), details:None }}));
            }
        }
        if sources.is_empty()
            || ids
                .iter()
                .any(|id| !self.sources.iter().any(|s| s.expected && &s.id == id))
        {
            return Err(fail(
                "Discovery requires known sources in the confirmed target scope.",
            ));
        }
        let mut request: DiscoverRequest = serde_json::from_value(json!({"schema_version":1,"environment":target.environment,"start":start,"end":end,"sources":sources,"value_fields":values,"kind_fields":kinds,"population_fields":population_fields,"contracts":contracts,"max_queries":max_queries,"max_seconds":max_seconds})).map_err(|_| fail("Cannot build typed discovery request."))?;
        if self.discovery_mode == DiscoveryMode::Quick {
            if self.quick_depth == QuickDepth::SourcesOnly {
                request.depth = discovery::DiscoveryDepth::SourcesOnly;
            } else {
                request.max_population_fields_per_source = Some(20);
            }
        }
        discovery::validate_request(&request)?;
        Ok(request)
    }
}

fn distinct_text(values: &[String]) -> Result<(), Diagnostic> {
    let mut seen = BTreeSet::new();
    for value in values {
        text(value)?;
        if !seen.insert(value) {
            return Err(fail("Inventory metadata and work fields must be distinct."));
        }
    }
    if values.len() > 4000 {
        return Err(fail(
            "Inventory metadata and work fields are limited to 4000 values.",
        ));
    }
    Ok(())
}

fn refresh_id(attempt: &RefreshAttempt, index: usize) -> Result<String, Diagnostic> {
    serde_json::to_vec(&(
        &attempt.environment,
        &attempt.session_id,
        &attempt.arguments,
        &attempt.host_evidence.sha256,
        &attempt.prepared_at,
        index,
    ))
    .map(|bytes| format!("refresh:{}", sha256_hex(&bytes)))
    .map_err(|_| fail("Cannot fingerprint refresh attempt."))
}

fn validate_refresh_host(
    host: &RefreshHostEvidence,
    environment: &EnvironmentId,
    session_id: &str,
    automatic: bool,
    now: &str,
) -> Result<(), Diagnostic> {
    text(session_id)?;
    text(&host.capability_reference)?;
    text(&host.verification_evidence)?;
    if host.schema_version != 1
        || &host.environment != environment
        || host.session_id != session_id
        || !(1..=if automatic {
            REFRESH_SECONDS
        } else {
            MANUAL_REFRESH_SECONDS
        })
            .contains(&host.max_search_seconds)
        || environment::parse_time(&host.verified_at)? > environment::parse_time(now)?
    {
        return Err(fail(
            "Host evidence must bind this exact target/session to a verified provider-enforced search lifetime of 1..300 seconds for automatic refresh or 1..600 seconds for explicit manual refresh. Client wait timeouts do not qualify.",
        ));
    }
    Ok(())
}

fn validate_refresh_settlement(
    attempt: &RefreshAttempt,
    report: &RefreshSettlement,
    now: &str,
) -> Result<(), Diagnostic> {
    text(&report.evidence)?;
    let recorded = environment::parse_time(&report.recorded_at)?;
    if (report.termination == RefreshTermination::Completed) != report.result_sha256.is_some()
        || report
            .result_sha256
            .as_ref()
            .is_some_and(|hash| hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(fail(
            "Completed refresh settlement requires the exact full result SHA-256; other outcomes cannot attest a result.",
        ));
    }
    if report.schema_version != 1
        || report.attempt_id != attempt.id
        || report.environment != attempt.environment
        || report.session_id != attempt.session_id
        || report.arguments != attempt.arguments
        || recorded > environment::parse_time(now)?
        || recorded < environment::parse_time(&attempt.prepared_at)?
        || (attempt.dispatched_at.is_none() && report.termination != RefreshTermination::NotStarted)
        || attempt
            .dispatched_at
            .as_ref()
            .is_some_and(|at| environment::parse_time(at).is_ok_and(|at| recorded < at))
    {
        return Err(fail(
            "Settlement must identify the exact target, session, attempt and arguments after dispatch; a lost response is unresolved.",
        ));
    }
    if report.termination == RefreshTermination::LifetimeExpired {
        let dispatched = environment::parse_time(
            attempt
                .dispatched_at
                .as_deref()
                .ok_or_else(|| fail("No dispatched refresh lifetime exists."))?,
        )?;
        let started = environment::parse_time(report.search_started_at.as_deref().ok_or_else(|| fail("Lifetime reconciliation requires host evidence of actual provider search start, not client dispatch time."))?)?;
        if started < dispatched
            || recorded - started
                < time::Duration::seconds(attempt.search_lifetime_seconds()? as i64)
        {
            return Err(fail(
                "The verified provider search lifetime has not elapsed; refresh remains unresolved.",
            ));
        }
    }
    if let Some(started) = &report.search_started_at {
        let started = environment::parse_time(started)?;
        if report.termination == RefreshTermination::NotStarted
            || started > recorded
            || attempt
                .dispatched_at
                .as_ref()
                .is_none_or(|at| environment::parse_time(at).is_ok_and(|at| started < at))
        {
            return Err(fail(
                "Reported provider search start must follow exact dispatch and precede settlement.",
            ));
        }
    }
    Ok(())
}

type RefreshRows = Vec<(Scope, Vec<String>, u64)>;
fn refresh_rows(raw: &[u8]) -> Result<RefreshRows, Diagnostic> {
    let result = mcp::normalize(raw)?;
    if result.rows.len() >= 10_000 {
        return Err(fail(
            "Cap-sized refresh results cannot establish a complete metadata capture.",
        ));
    }
    let mut sources = BTreeMap::<String, (Scope, BTreeSet<String>, u64)>::new();
    for row in result.rows {
        let selector = |key| -> Result<Option<String>, Diagnostic> {
            match row.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(s)) if s.is_empty() => Ok(None),
                Some(Value::String(s)) => Ok(Some(s.clone())),
                _ => Err(fail(
                    "Census selectors and source-type metadata must be strings.",
                )),
            }
        };
        let source = Scope {
            repo: selector("#repo")?,
            type_: selector("#type")?,
            event: None,
        };
        scope_ok(&source)?;
        let source_type = selector("@sourcetype")?;
        let events = row
            .get("events")
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .ok_or_else(|| fail("Census rows require nonnegative events counts."))?;
        let entry =
            sources
                .entry(discovery::source_id(&source))
                .or_insert((source, BTreeSet::new(), 0));
        if let Some(value) = source_type {
            text(&value)?;
            entry.1.insert(value);
        }
        entry.2 = entry
            .2
            .checked_add(events)
            .ok_or_else(|| fail("Census count overflow."))?;
    }
    Ok(sources
        .into_values()
        .map(|(source, types, count)| (source, types.into_iter().collect(), count))
        .collect())
}

fn validate_scout_plan(plan: &ScoutPlan) -> Result<(), Diagnostic> {
    environment::validate_identity(&plan.environment)?;
    if plan
        .max_seconds
        .is_some_and(|seconds| !(1..=600).contains(&seconds))
    {
        return Err(fail("Scout host budgets must be 1..600 seconds."));
    }
    if !["search-all", plan.environment.view.as_str()].contains(&plan.arguments.repository.as_str())
        || plan.arguments.query_string != SCOUT_QUERY
        || plan.query_sha256 != sha256_hex(plan.arguments.query_string.as_bytes())
    {
        return Err(fail(
            "Scouting requires the exact built-in census query and its declared searchable target.",
        ));
    }
    let request: DiscoverRequest = serde_json::from_value(json!({"schema_version":1,"environment":plan.environment,"start":plan.arguments.start,"end":plan.arguments.end,"census":true})).map_err(|_| fail("Invalid scouting window."))?;
    discovery::validate_request(&request)
}

#[derive(Debug, Clone)]
pub struct CatalogPage {
    pub ids: Vec<String>,
    pub total: Option<u64>,
}

pub fn parse_catalog_page(page: &ParserPage) -> Result<CatalogPage, Diagnostic> {
    if !(1..=100).contains(&page.limit) || page.offset > 9999 {
        return Err(fail(
            "Parser page limits must be 1..100 and offsets at most 9999.",
        ));
    }
    let value: Value = serde_json::from_slice(&page.snapshot.bytes()?)
        .map_err(|_| fail("Parser catalog response must be JSON."))?;
    if value
        .get("errors")
        .is_some_and(|e| !e.is_null() && e.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(fail("Native parser catalog returned errors."));
    }
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("Native parser catalog requires resources."))?;
    if resources.len() > page.limit as usize {
        return Err(fail("Parser catalog page exceeds requested limit."));
    }
    let mut ids = Vec::new();
    let mut seen = BTreeSet::new();
    for resource in resources {
        let id = resource
            .as_str()
            .or_else(|| resource.get("id").and_then(Value::as_str))
            .or_else(|| resource.get("ID").and_then(Value::as_str))
            .ok_or_else(|| fail("Parser catalog resource has no exact ID."))?;
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) || !seen.insert(id) {
            return Err(fail("Invalid or duplicate parser catalog ID."));
        }
        ids.push(id.into());
    }
    let total = value
        .pointer("/meta/pagination/total")
        .and_then(Value::as_u64);
    if value
        .pointer("/meta/pagination/offset")
        .and_then(Value::as_u64)
        .is_some_and(|o| o != u64::from(page.offset))
    {
        return Err(fail(
            "Parser response offset does not match saved request arguments.",
        ));
    }
    Ok(CatalogPage { ids, total })
}

fn catalog(state: &InventoryState) -> Result<(bool, BTreeSet<String>), Diagnostic> {
    let mut pages: Vec<_> = state.parser_pages.iter().collect();
    pages.sort_by_key(|p| p.offset);
    let mut ids = BTreeSet::new();
    let mut next = 0;
    let mut total = None;
    let mut complete = !pages.is_empty();
    for page in pages {
        let parsed = parse_catalog_page(page)?;
        if page.offset != next {
            complete = false;
        }
        next = page.offset + parsed.ids.len() as u32;
        if let Some(n) = parsed.total {
            if total.is_some_and(|t| t != n) {
                return Err(fail(
                    "Parser catalog totals changed between pages; repeat a bounded inventory.",
                ));
            }
            total = Some(n);
        }
        for id in parsed.ids {
            if !ids.insert(id) {
                return Err(fail("Parser catalog repeats an ID across pages."));
            }
        }
    }
    complete &= total.is_some_and(|t| t == ids.len() as u64);
    complete &= ids
        .iter()
        .all(|id| state.parsers.iter().any(|p| &p.id == id));
    if state.parsers.iter().any(|p| !ids.contains(&p.id)) {
        return Err(fail(
            "Parser detail ID is absent from the saved catalog pages.",
        ));
    }
    Ok((complete, ids))
}

pub fn parse_parser(id: &str, snapshot: RawSnapshot) -> Result<ParserSnapshot, Diagnostic> {
    let value: Value = serde_json::from_slice(&snapshot.bytes()?)
        .map_err(|_| fail("Native parser response must be JSON."))?;
    if value
        .get("errors")
        .is_some_and(|e| !e.is_null() && e.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(fail("Native parser response returned errors."));
    }
    let resources = value
        .get("resources")
        .and_then(Value::as_array)
        .ok_or_else(|| fail("Native parser detail requires resources."))?;
    if resources.len() != 1 {
        return Err(fail(
            "Each parser detail capture must contain exactly one resource.",
        ));
    }
    let parser = &resources[0];
    if parser.get("id").and_then(Value::as_str) != Some(id) {
        return Err(fail(
            "Parser detail ID differs from the explicitly requested catalog ID.",
        ));
    }
    let name = parser
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_owned();
    let script = parser
        .get("script")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            fail("Native parser resource needs a script string; unsupported shapes remain pending.")
        })?;
    let mut tags = vec![];
    if let Some(value) = parser.get("fields_to_tag") {
        for tag in value
            .as_array()
            .ok_or_else(|| fail("fields_to_tag must be an array of field names."))?
        {
            tags.push(
                tag.as_str()
                    .ok_or_else(|| fail("fields_to_tag entries must be strings."))?
                    .to_owned(),
            );
        }
    }
    let declarations = extract_declarations(script, &tags)?;
    Ok(ParserSnapshot {
        id: id.into(),
        name,
        snapshot,
        declarations,
    })
}

/// Conservative lexical extraction. Conditional and unsupported lines need explicit review.
pub fn extract_declarations(
    script: &str,
    tags: &[String],
) -> Result<ParserDeclarations, Diagnostic> {
    if script.len() > mcp::MAX_RESULT_BYTES {
        return Err(fail("Parser script exceeds the extraction budget."));
    }
    let mut fields = BTreeSet::new();
    let mut arrays = BTreeSet::new();
    let mut review = BTreeSet::new();
    for (line_index, line) in script.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let masked = mask_literals(line);
        let mut assignments = 0;
        for (index, _) in masked.match_indices(":=") {
            let prefix = masked[..index].trim_end();
            let start = prefix
                .char_indices()
                .rev()
                .find(|(_, c)| !is_field_char(*c))
                .map_or(0, |(i, c)| i + c.len_utf8());
            let field = &prefix[start..];
            if !field.is_empty() && crate::language::plan::checked_field(field).is_ok() {
                assignments += 1;
                fields.insert(field.to_owned());
                if field.contains("[]") {
                    arrays.insert(field.to_owned());
                }
            }
        }
        // Functions can export fields indirectly, and branches do not prove execution.
        if assignments == 0
            || masked.contains('(')
            || masked.contains('{')
            || masked.contains('}')
            || masked.contains("case")
            || masked.contains("match")
            || masked.contains("->")
        {
            review.insert(line_index as u32 + 1);
        }
    }
    let mut fields_to_tag = BTreeSet::new();
    for tag in tags {
        crate::language::plan::checked_field(tag)?;
        fields.insert(tag.clone());
        fields_to_tag.insert(tag.clone());
    }
    Ok(ParserDeclarations {
        fields: fields.into_iter().collect(),
        fields_to_tag: fields_to_tag.into_iter().collect(),
        array_fields: arrays.into_iter().collect(),
        review_lines: review.into_iter().collect(),
    })
}

fn is_field_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '#' | '@' | '[' | ']' | '-')
}
fn mask_literals(line: &str) -> String {
    let mut result = String::with_capacity(line.len());
    let mut quoted = false;
    let mut escaped = false;
    for c in line.chars() {
        if quoted {
            result.extend(std::iter::repeat_n(' ', c.len_utf8()));
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        if c == '"' {
            quoted = true;
            result.push(' ');
        } else {
            result.push(c);
        }
    }
    if let Some(index) = result.find("//") {
        result.truncate(index);
    }
    result
}

pub fn assess(state: &InventoryState, packs: &[Pack]) -> Result<InventoryStatus, Diagnostic> {
    state.validate()?;
    let target = state.target.as_ref();
    let mut verified_packs = BTreeMap::new();
    for pack in packs {
        pack.validate()?;
        if target.is_some_and(|t| t.environment != pack.environment) {
            return Err(fail(
                "Referenced pack belongs to another target environment.",
            ));
        }
        verified_packs.insert(&pack.content_digest, pack);
    }
    if state
        .packs
        .iter()
        .any(|r| !verified_packs.contains_key(&r.content_digest))
    {
        return Err(fail(
            "A referenced pack is missing or its digest changed; import the new snapshot explicitly.",
        ));
    }
    let (catalog_complete, parser_ids) = catalog(state)?;
    let mut unclassified = Vec::new();
    for id in parser_ids {
        let bound = state.sources.iter().any(|s| matches!(&s.binding, Some(ParserBinding::Parser { parser_id, .. }) if parser_id == &id));
        let excluded = state.parser_exclusions.iter().any(|e| e.parser_id == id);
        if !bound && !excluded {
            unclassified.push(id);
        }
    }
    let mut sources = Vec::new();
    for source in &state.sources {
        sources.push(assess_source(state, source, &verified_packs));
    }
    let reconciliation = if state
        .reconciled_evidence_sha256
        .as_ref()
        .is_some_and(|d| state.evidence_digest().is_ok_and(|expected| &expected == d))
    {
        Progress::Done
    } else {
        Progress::Pending
    };
    let scout = if state.scouting.is_empty() {
        Progress::Pending
    } else {
        Progress::Done
    };
    let parser_catalog = if catalog_complete {
        Progress::Done
    } else if state.parser_pages.is_empty() {
        Progress::Unknown
    } else {
        Progress::Pending
    };
    let expected_sources: Vec<_> = sources.iter().filter(|s| s.expected).collect();
    let scoped_complete = !expected_sources.is_empty()
        && expected_sources.iter().all(|s| s.status == Progress::Done)
        && reconciliation == Progress::Done;
    let complete = state.discovery_mode == DiscoveryMode::Full
        && target.is_some_and(|t| t.scope == TargetScope::FullEnvironment)
        && state.active_pack.is_some()
        && scout == Progress::Done
        && state
            .scouting
            .iter()
            .any(|s| s.plan.arguments.repository == "search-all")
        && catalog_complete
        && unclassified.is_empty()
        && sources.iter().all(|s| s.expected)
        && state
            .pending_inventory
            .iter()
            .all(|w| w.resolution.is_some() || !w.new_source && w.source_types.is_empty())
        && scoped_complete
        && reconciliation == Progress::Done;
    let mut next_actions = Vec::new();
    if state
        .refresh_attempts
        .iter()
        .any(|a| matches!(a.phase, RefreshPhase::Dispatched | RefreshPhase::Unresolved))
    {
        next_actions.push("Refresh search termination is unresolved. Reconcile the exact attempt with safe settlement before another search; a client timeout or lost response does not establish termination.".into());
    }
    let prepared = state
        .refresh_attempts
        .iter()
        .find(|a| a.phase == RefreshPhase::Prepared);
    if prepared.is_some_and(|attempt| attempt.automatic) {
        next_actions.push("Automatic inventory refresh is unavailable. Safely settle the reserved legacy attempt as not-started before preparing explicit manual refresh.".into());
    }
    if let Some(attempt) = prepared.filter(|a| !a.automatic) {
        next_actions.push(format!("A manual refresh is prepared for recorded host session {:?}; dispatch only from that exact verified session.", attempt.session_id));
    }
    if target.is_none() {
        next_actions.push("Confirm CID, cloud, searchable view, target scope and context.".into());
    }
    if scout != Progress::Done && state.discovery_mode == DiscoveryMode::Full {
        next_actions.push("Prepare scouting and relay the emitted census through the existing CrowdStrike MCP; save the entire result.".into());
    }
    if !catalog_complete && state.discovery_mode == DiscoveryMode::Full {
        next_actions.push("Complete the native parser catalog and retrieve each exact parser ID; API credentials stay in the supplied protected file.".into());
    }
    if !unclassified.is_empty() {
        next_actions.push("Bind or explicitly exclude every unclassified parser snapshot; parser-name similarity is not evidence.".into());
    }
    if target.is_some_and(|t| t.scope == TargetScope::FullEnvironment)
        && sources.iter().any(|s| !s.expected)
    {
        next_actions.push("Confirm the known source selectors within the target scope before completing a full environment map.".into());
    }
    if target.is_some_and(|t| t.scope == TargetScope::SelectedSources) {
        next_actions
            .push("Selected-source coverage cannot establish a full environment map.".into());
    }
    if reconciliation != Progress::Done {
        next_actions.push(
            "Reconcile after completing the applicable source measurements and parser reviews."
                .into(),
        );
    }
    let pending_inventory: Vec<_> = state
        .pending_inventory
        .iter()
        .filter(|w| w.resolution.is_none())
        .cloned()
        .collect();
    if !pending_inventory.is_empty() {
        next_actions.push("Settings *: review new source, source-type metadata or field work. Deferral preserves it; inventory or explicitly exclude the exact revision.".into());
    }
    Ok(InventoryStatus {
        schema_version: 1,
        blank_canvas: target.is_none()
            && state.scouting.is_empty()
            && state.parsers.is_empty()
            && state.packs.is_empty(),
        full_map_complete: complete,
        selected_scope_complete: state.discovery_mode == DiscoveryMode::Full
            && target.is_some_and(|t| t.scope == TargetScope::SelectedSources)
            && scoped_complete,
        quick_scope_complete: state.discovery_mode == DiscoveryMode::Quick
            && target.is_some_and(|t| t.scope == TargetScope::SelectedSources)
            && scoped_complete,
        discovery_mode: state.discovery_mode,
        quick_depth: state.quick_depth,
        unverified_lead_count: state.unverified_leads.len(),
        query_observation_count: state.query_observations.len(),
        refresh_settings: state.refresh_settings.clone(),
        automatic_refresh_available: AUTOMATIC_REFRESH_AVAILABLE,
        refresh_reminder: None,
        refresh_attempts: state
            .refresh_attempts
            .iter()
            .map(|a| {
                let termination = a
                    .settlement
                    .as_ref()
                    .map(|snapshot| {
                        serde_json::from_slice::<RefreshSettlement>(&snapshot.bytes()?)
                            .map(|settlement| settlement.termination)
                            .map_err(|_| fail("Invalid saved refresh settlement."))
                    })
                    .transpose()?;
                Ok(RefreshStatus {
                    attempt_id: a.id.clone(),
                    session_id: a.session_id.clone(),
                    environment: a.environment.clone(),
                    arguments: a.arguments.clone(),
                    phase: a.phase,
                    automatic: a.automatic,
                    prepared_at: a.prepared_at.clone(),
                    dispatched_at: a.dispatched_at.clone(),
                    max_search_seconds: a.search_lifetime_seconds()?,
                    termination,
                    host_evidence_sha256: a.host_evidence.sha256.clone(),
                    settlement_sha256: a.settlement.as_ref().map(|s| s.sha256.clone()),
                    result_sha256: a.result.as_ref().map(|s| s.sha256.clone()),
                })
            })
            .collect::<Result<Vec<_>, Diagnostic>>()?,
        refresh_blocked: state.refresh_blocked(),
        inventory_notification_count: state.inventory_notifications().len(),
        settings_attention: !pending_inventory.is_empty(),
        pending_inventory,
        unverified_leads: state.unverified_leads.clone(),
        query_observations: state.query_observations.clone(),
        message: if state.discovery_mode == DiscoveryMode::Quick {
            "Quick discovery covers only the selected sources, window and depth. Parser review and a full environment map remain separate work.".into()
        } else {
            COMPLETION_MESSAGE.into()
        },
        target: if target.is_some() {
            Progress::Done
        } else {
            Progress::Pending
        },
        environment: target.map(|t| t.environment.clone()),
        target_scope: target.map(|t| t.scope),
        context: target.map(|t| t.context.clone()),
        scouting: scout,
        parser_catalog,
        reconciliation,
        parser_count: state.parsers.len(),
        parser_page_count: state.parser_pages.len(),
        parsers: state
            .parsers
            .iter()
            .map(|p| ParserSummary {
                id: p.id.clone(),
                name: p.name.clone(),
                sha256: p.snapshot.sha256.clone(),
                retrieved_at: p.snapshot.retrieved_at.clone(),
                declared_fields: p.declarations.fields.clone(),
                fields_to_tag: p.declarations.fields_to_tag.clone(),
                array_fields: p.declarations.array_fields.clone(),
                review_lines: p.declarations.review_lines.clone(),
            })
            .collect(),
        scouting_evidence: state
            .scouting
            .iter()
            .map(|s| ScoutSummary {
                plan: s.plan.clone(),
                raw_sha256: s.snapshot.sha256.clone(),
                retrieved_at: s.snapshot.retrieved_at.clone(),
                row_count: s.rows,
            })
            .collect(),
        unclassified_parser_ids: unclassified,
        sources,
        discovery_runs: state.discovery_runs.clone(),
        parser_runs: state.parser_runs.clone(),
        failures: state.failures.clone(),
        next_actions,
        covered_queries_usable: true,
        active_pack: state.active_pack.clone(),
    })
}

pub fn assess_at(
    state: &InventoryState,
    packs: &[Pack],
    now: &str,
) -> Result<InventoryStatus, Diagnostic> {
    let mut status = assess(state, packs)?;
    status.refresh_reminder = state.refresh_reminder_status(now)?;
    if status
        .refresh_reminder
        .as_ref()
        .is_some_and(|reminder| reminder.due)
    {
        status.settings_attention = true;
        status.next_actions.push("Inventory refresh is due. Run explicit manual refresh or choose Remind me in 30 days; acknowledgement does not complete refresh.".into());
    }
    Ok(status)
}

fn assess_source(
    state: &InventoryState,
    source: &InventorySource,
    packs: &BTreeMap<&String, &Pack>,
) -> SourceStatus {
    let mut fields = BTreeMap::<String, FieldStatus>::new();
    let mut record_kinds = Vec::new();
    let mut observed_source = false;
    let mut parser_status = Progress::Pending;
    if let Some(binding) = &source.binding {
        match binding {
            ParserBinding::NotApplicable { .. } => {
                parser_status = Progress::NotApplicable;
            }
            ParserBinding::Parser {
                parser_id,
                parser_sha256,
                semantics,
                ..
            } => {
                if let Ok(parser) = state.parser_version(parser_id, parser_sha256) {
                    parser_status = if parser
                        .declarations
                        .review_lines
                        .iter()
                        .all(|line| semantics.iter().any(|s| s.line == *line))
                    {
                        Progress::Done
                    } else {
                        Progress::Review
                    };
                    for field in &parser.declarations.fields {
                        let name = if parser.declarations.fields_to_tag.contains(field)
                            && !field.starts_with('#')
                        {
                            format!("#{field}")
                        } else {
                            field.clone()
                        };
                        let entry = field_entry(&mut fields, &name);
                        entry.declared = true;
                        entry
                            .evidence_dates
                            .push(parser.snapshot.retrieved_at.clone());
                    }
                }
            }
        }
    }
    // The active snapshot already includes prior batches. Historical snapshots must never
    // override its explicit zero or not-reobserved evidence.
    let mut references: Vec<_> = state
        .active_pack
        .as_ref()
        .map_or_else(|| state.packs.iter().collect(), |active| vec![active]);
    references.sort_by_key(|r| {
        packs
            .get(&r.content_digest)
            .and_then(|p| environment::parse_time(&p.created_at).ok())
    });
    for reference in references {
        let Some(pack) = packs.get(&reference.content_digest) else {
            continue;
        };
        let Some(pack_source) = pack
            .data
            .sources
            .iter()
            .filter_map(|s| environment::source(s).ok())
            .find(|s| {
                s.selectors.as_ref().is_some_and(|q| {
                    q.repo == source.selectors.repo && q.type_ == source.selectors.type_
                })
            })
        else {
            continue;
        };
        observed_source = pack_source
            .evidence
            .as_ref()
            .is_some_and(|e| e.observed == Some(true))
            && !source_evidence_dates(pack, &pack_source.provenance_ids).is_empty();
        if let Some(relationship) = pack_source.relationship {
            record_kinds.extend(relationship.record_kinds);
        }
        for field in pack
            .data
            .fields
            .iter()
            .filter_map(|f| environment::field(f).ok())
            .filter(|f| f.source_id == pack_source.id)
        {
            let name = if field.is_tag == Some(true) && !field.path.starts_with('#') {
                format!("#{}", field.path)
            } else {
                field.path.clone()
            };
            let entry = field_entry(&mut fields, &name);
            let dates = source_evidence_dates(pack, &field.provenance_ids);
            // fieldset presence alone is insufficient: population/tag/kind observations must qualify it.
            entry.not_reobserved = field.refresh_status.as_deref() == Some("not-reobserved");
            entry.observed = !entry.not_reobserved
                && field.evidence_kind == "observed"
                && field
                    .coverage
                    .as_ref()
                    .is_some_and(|c| c.observed == Some(true))
                && !dates.is_empty();
            entry.measured = environment::field_measurement(pack, &field).is_some()
                || !dates.is_empty()
                    && (entry.observed
                        || field.coverage.as_ref().is_some_and(|c| {
                            c.rate.is_some() && (!entry.not_reobserved || c.rate == Some(0.0))
                        }));
            entry.declared |= field
                .coverage
                .as_ref()
                .is_some_and(|c| c.declared == Some(true));
            entry.evidence_dates.extend(dates);
            if pack
                .data
                .values
                .iter()
                .any(|v| v["field_id"] == field.id && v["evidence_kind"] == "observed")
            {
                entry.values = Progress::Done;
            }
        }
    }
    let runs: Vec<_> = state
        .discovery_runs
        .iter()
        .filter(|r| r.source_ids.contains(&source.id))
        .collect();
    let quick_run = state.discovery_mode == DiscoveryMode::Quick
        && runs.last().is_some_and(|r| {
            r.mode == DiscoveryMode::Quick
                && (r.quick_depth == QuickDepth::SourcesAndFields
                    || state.quick_depth == QuickDepth::SourcesOnly)
        });
    let measured = runs.last().is_some_and(|r| {
        r.status == "complete"
            && (r.mode == DiscoveryMode::Full || quick_run)
            && state
                .packs
                .iter()
                .any(|p| p.discovery_run.as_ref() == Some(&r.state_path))
    });
    for run in &runs {
        for name in &run.value_fields {
            let field = field_entry(&mut fields, name);
            field.values_requested = true;
            if field.values != Progress::Done {
                field.values = if run.status == "complete" {
                    Progress::Done
                } else {
                    Progress::Pending
                };
            }
        }
    }
    let mut missing = Vec::new();
    for field in fields.values_mut() {
        field.evidence_dates.sort();
        field.evidence_dates.dedup();
        field.status = if field.observed {
            Progress::Done
        } else if field.declared {
            missing.push(field.field.clone());
            Progress::Review
        } else {
            Progress::Unknown
        };
    }
    let interpretation = if source.review.is_some() {
        Progress::Done
    } else {
        Progress::Review
    };
    let quick = quick_run;
    let measured = measured
        && (quick
            || fields
                .values()
                .all(|f| f.measured && (!f.values_requested || f.values == Progress::Done)));
    let measurements = if measured {
        Progress::Done
    } else if runs.is_empty() {
        Progress::Unknown
    } else {
        Progress::Pending
    };
    let status = if measurements == Progress::Done
        && observed_source
        && (quick
            || interpretation == Progress::Done
                && matches!(parser_status, Progress::Done | Progress::NotApplicable))
    {
        Progress::Done
    } else if parser_status == Progress::Review || interpretation == Progress::Review {
        Progress::Review
    } else {
        Progress::Pending
    };
    let mut next_actions = Vec::new();
    if interpretation != Progress::Done {
        next_actions.push(if state.discovery_mode == DiscoveryMode::Quick {
            "Full discovery: review source purpose and data-model interpretation using explicit evidence.".into()
        } else { "Review source purpose and data-model interpretation using explicit evidence.".into() });
    }
    if !matches!(parser_status, Progress::Done | Progress::NotApplicable) {
        next_actions.push(if state.discovery_mode == DiscoveryMode::Quick {
            "Full discovery: resolve the explicit parser binding and all pending parser semantics."
                .into()
        } else {
            "Resolve the explicit parser binding and all pending parser semantics.".into()
        });
    }
    if !measured {
        next_actions.push("Complete a finite discovery run for this source; imported pack history alone is unknown.".into());
    }
    if !missing.is_empty() {
        next_actions.push(
            "Review declared fields without qualifying observations; quiet does not mean absent."
                .into(),
        );
    }
    SourceStatus {
        source_id: source.id.clone(),
        expected: source.expected,
        selectors: source.selectors.clone(),
        scouted_events: source.scouted_events,
        review: source.review.clone(),
        binding: source.binding.clone(),
        status,
        interpretation,
        parser: parser_status,
        measurements,
        observation: if observed_source {
            Progress::Done
        } else {
            Progress::Unknown
        },
        fields: fields.into_values().collect(),
        record_kinds,
        unobserved_declared_fields: missing,
        next_actions,
    }
}

fn source_evidence_dates(pack: &Pack, ids: &[String]) -> Vec<String> {
    pack.data
        .provenance
        .iter()
        .filter(|p| {
            ids.iter().any(|id| p["id"] == *id)
                && matches!(
                    p["source_type"].as_str(),
                    Some("ngsiem-api" | "mcp-observation")
                )
                && matches!(
                    p["coverage_status"].as_str(),
                    Some("complete" | "observed-only" | "reconciled")
                )
        })
        .filter_map(|p| p["retrieved_at"].as_str().map(str::to_owned))
        .collect()
}
fn field_entry<'a>(
    fields: &'a mut BTreeMap<String, FieldStatus>,
    name: &str,
) -> &'a mut FieldStatus {
    fields.entry(name.into()).or_insert_with(|| FieldStatus {
        field: name.into(),
        declared: false,
        observed: false,
        measured: false,
        status: Progress::Unknown,
        values: Progress::NotApplicable,
        values_requested: false,
        evidence_dates: vec![],
        not_reobserved: false,
    })
}
