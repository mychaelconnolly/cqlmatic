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
    PrepareScout {
        environment: EnvironmentId,
        start: String,
        end: String,
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum TargetScope {
    FullEnvironment,
    SelectedSources,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ScoutPlan {
    pub environment: EnvironmentId,
    pub arguments: SearchArguments,
    pub query_sha256: String,
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
fn scope_ok(scope: &Scope) -> Result<(), Diagnostic> {
    if scope.repo.as_ref().is_none_or(|r| r.trim().is_empty()) || scope.event.is_some() {
        return Err(fail(
            "Inventory sources require repo and optional type, without an event selector.",
        ));
    }
    crate::language::plan::checked_text(scope.repo.as_deref().expect("repository checked"))?;
    if let Some(kind) = &scope.type_ {
        crate::language::plan::checked_text(kind)?;
    }
    Ok(())
}

impl InventoryState {
    pub fn new(now: &str) -> Result<Self, Diagnostic> {
        environment::parse_time(now)?;
        Ok(Self {
            format: "cqlmatic-inventory-state".into(),
            schema_version: 1,
            created_at: now.into(),
            updated_at: now.into(),
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
        serde_json::to_vec(&copy)
            .map(|b| sha256_hex(&b))
            .map_err(|_| fail("Cannot serialize inventory evidence."))
    }
    pub fn validate(&self) -> Result<(), Diagnostic> {
        if self.format != "cqlmatic-inventory-state" || self.schema_version != 1 {
            return Err(fail("Unsupported inventory state."));
        }
        environment::parse_time(&self.created_at)?;
        environment::parse_time(&self.updated_at)?;
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
        if self
            .target
            .as_ref()
            .is_some_and(|t| t.environment != environment)
        {
            return Err(fail("Start a new state for a different environment."));
        }
        let plan = ScoutPlan {
            environment,
            arguments: SearchArguments {
                repository: "search-all".into(),
                query_string: SCOUT_QUERY.into(),
                start,
                end,
            },
            query_sha256: sha256_hex(SCOUT_QUERY.as_bytes()),
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
        if arguments != plan.arguments {
            return Err(fail(
                "Saved tool arguments differ from the exact prepared census query/window.",
            ));
        }
        let result = mcp::normalize(raw)?;
        let mut observations = BTreeMap::<String, (Scope, u64)>::new();
        for row in &result.rows {
            let selectors = Scope {
                repo: row.get("#repo").and_then(Value::as_str).map(str::to_owned),
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
        for source in &mut self.sources {
            source.scouted_events = None;
        }
        for (id, (selectors, events)) in observations {
            if let Some(source) = self.sources.iter_mut().find(|s| s.id == id) {
                source.scouted_events = Some(events);
            } else {
                self.sources.push(InventorySource {
                    id,
                    selectors,
                    scouted_events: Some(events),
                    expected: self
                        .target
                        .as_ref()
                        .is_some_and(|t| t.scope == TargetScope::FullEnvironment),
                    review: None,
                    binding: None,
                });
            }
        }
        self.scouting.push(evidence);
        Ok(())
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
        self.target = Some(ConfirmedTarget {
            environment,
            scope,
            context,
            confirmed_at: now.into(),
        });
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
        let request: DiscoverRequest = serde_json::from_value(json!({"schema_version":1,"environment":target.environment,"start":start,"end":end,"sources":sources,"value_fields":values,"kind_fields":kinds,"population_fields":population_fields,"contracts":contracts,"max_queries":max_queries,"max_seconds":max_seconds})).map_err(|_| fail("Cannot build typed discovery request."))?;
        discovery::validate_request(&request)?;
        Ok(request)
    }
}

fn validate_scout_plan(plan: &ScoutPlan) -> Result<(), Diagnostic> {
    environment::validate_identity(&plan.environment)?;
    if plan.arguments.repository != "search-all"
        || plan.arguments.query_string != SCOUT_QUERY
        || plan.query_sha256 != sha256_hex(plan.arguments.query_string.as_bytes())
    {
        return Err(fail(
            "Scouting requires the exact built-in census query and search-all repository.",
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
    for source in state
        .sources
        .iter()
        .filter(|s| s.expected || target.is_none())
    {
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
    let complete = target.is_some_and(|t| t.scope == TargetScope::FullEnvironment)
        && state.active_pack.is_some()
        && scout == Progress::Done
        && catalog_complete
        && unclassified.is_empty()
        && !sources.is_empty()
        && sources.iter().all(|s| s.status == Progress::Done)
        && reconciliation == Progress::Done;
    let mut next_actions = Vec::new();
    if target.is_none() {
        next_actions.push("Confirm CID, cloud, searchable view, target scope and context.".into());
    }
    if scout != Progress::Done {
        next_actions.push("Prepare scouting and relay the emitted census through the existing CrowdStrike MCP; save the entire result.".into());
    }
    if !catalog_complete {
        next_actions.push("Complete the native parser catalog and retrieve each exact parser ID; API credentials stay in the supplied protected file.".into());
    }
    if !unclassified.is_empty() {
        next_actions.push("Bind or explicitly exclude every unclassified parser snapshot; parser-name similarity is not evidence.".into());
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
    Ok(InventoryStatus {
        schema_version: 1,
        blank_canvas: target.is_none()
            && state.scouting.is_empty()
            && state.parsers.is_empty()
            && state.packs.is_empty(),
        full_map_complete: complete,
        message: COMPLETION_MESSAGE.into(),
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
            entry.measured = !dates.is_empty()
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
    let measured = runs.last().is_some_and(|r| {
        r.status == "complete"
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
    let measured = measured
        && fields
            .values()
            .all(|f| f.measured && (!f.values_requested || f.values == Progress::Done));
    let measurements = if measured {
        Progress::Done
    } else if runs.is_empty() {
        Progress::Unknown
    } else {
        Progress::Pending
    };
    let status = if interpretation == Progress::Done
        && matches!(parser_status, Progress::Done | Progress::NotApplicable)
        && measurements == Progress::Done
        && observed_source
    {
        Progress::Done
    } else if parser_status == Progress::Review || interpretation == Progress::Review {
        Progress::Review
    } else {
        Progress::Pending
    };
    let mut next_actions = Vec::new();
    if interpretation != Progress::Done {
        next_actions.push(
            "Review source purpose and data-model interpretation using explicit evidence.".into(),
        );
    }
    if !matches!(parser_status, Progress::Done | Progress::NotApplicable) {
        next_actions
            .push("Resolve the explicit parser binding and all pending parser semantics.".into());
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
