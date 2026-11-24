//! Pure pack contracts, validation, grounding and evidence-preserving merge.
use crate::{
    contracts::{Diagnostic, EnvironmentId, PROFILE},
    sha256_hex,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_PACK_BYTES: u64 = 67_108_864;
const MAX_MEASURED_FIELDS: usize = 4000;
pub const KNOWLEDGE_REVISION: &str =
    "b71fce37fea7286d253252ca224899b70ad35ba0ed84e8a2b2d986a0ad46284a";
const LEGACY_KNOWLEDGE_REVISION: &str = "2158f99fc529e234d5fd6fd4d4af899d7d83a848";
const COMPATIBLE_PACKS: &[(u32, &str, &str)] = &[
    (1, "cqlmatic-ngsiem-core-v1", LEGACY_KNOWLEDGE_REVISION),
    (1, "cqlmatic-ngsiem-core-v2", LEGACY_KNOWLEDGE_REVISION),
    (
        1,
        PROFILE,
        "29db942fa5364af3a454e8e6525d20b7d9337075958807280f3d998dc8504357",
    ),
    (1, PROFILE, KNOWLEDGE_REVISION),
];
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct PackData {
    #[cfg_attr(test, schemars(with = "Vec<LegacySource>"))]
    pub sources: Vec<Value>,
    #[cfg_attr(test, schemars(with = "Vec<LegacyField>"))]
    pub fields: Vec<Value>,
    #[cfg_attr(test, schemars(with = "Vec<LegacyValue>"))]
    pub values: Vec<Value>,
    #[cfg_attr(test, schemars(with = "Vec<LegacyProvenance>"))]
    pub provenance: Vec<Value>,
    #[cfg_attr(test, schemars(with = "Vec<LegacyAnnotation>"))]
    #[serde(default)]
    pub annotations: Vec<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Pack {
    pub format_version: u32,
    pub environment: EnvironmentId,
    pub producer_version: String,
    pub profile_id: String,
    pub knowledge_revision: String,
    pub created_at: String,
    pub content_digest: String,
    pub data: PackData,
}
impl Pack {
    pub fn new(
        environment: EnvironmentId,
        created_at: String,
        data: PackData,
    ) -> Result<Self, Diagnostic> {
        let mut p = Self {
            format_version: 1,
            environment,
            producer_version: env!("CARGO_PKG_VERSION").into(),
            profile_id: PROFILE.into(),
            knowledge_revision: KNOWLEDGE_REVISION.into(),
            created_at,
            content_digest: String::new(),
            data,
        };
        p.content_digest = p.digest()?;
        p.validate()?;
        Ok(p)
    }
    pub fn digest(&self) -> Result<String, Diagnostic> {
        let mut copy = self.clone();
        copy.content_digest.clear();
        let bytes = serde_json::to_vec(&copy)
            .map_err(|_| Diagnostic::error("PACK_SERIALIZATION", "Cannot serialize pack."))?;
        Ok(sha256_hex(&bytes))
    }
    pub fn validate(&self) -> Result<(), Diagnostic> {
        if self.format_version != 1 {
            return Err(Diagnostic::error(
                "PACK_VERSION",
                "Unsupported pack format; no automatic migration was performed.",
            ));
        }
        validate_identity(&self.environment)?;
        if !COMPATIBLE_PACKS.contains(&(
            self.format_version,
            self.profile_id.as_str(),
            self.knowledge_revision.as_str(),
        )) {
            return Err(Diagnostic::error(
                "PACK_PROFILE",
                "Pack uses an incompatible capability or knowledge revision.",
            ));
        }
        parse_time(&self.created_at)?;
        if self.content_digest != self.digest()? {
            return Err(Diagnostic::error(
                "PACK_DIGEST",
                "Pack content digest does not match.",
            ));
        }
        self.data.validate()
    }
}
pub fn parse_time(s: &str) -> Result<time::OffsetDateTime, Diagnostic> {
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).map_err(|_| {
        Diagnostic::error(
            "PACK_TIME",
            "Expected an RFC3339 timestamp with an explicit offset.",
        )
    })
}
pub fn validate_identity(e: &EnvironmentId) -> Result<(), Diagnostic> {
    let cid = e.tenant.len() == 32 && e.tenant.bytes().all(|c| c.is_ascii_hexdigit());
    let client = e
        .tenant
        .strip_prefix("api-client:")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()));
    if !cid && !client {
        return Err(Diagnostic::error(
            "ENV_TENANT",
            "Account binding must be an explicit Falcon CID or api-client:<SHA-256> fingerprint.",
        ));
    }
    if e.view.trim().is_empty() || e.view.len() > 256 || e.view.chars().any(char::is_control) {
        return Err(Diagnostic::error(
            "ENV_VIEW",
            "Supply a nonempty searchable target without control characters.",
        ));
    }
    Ok(())
}

// Typed legacy records validate structure; original JSON is stored to preserve absent/null values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LegacySource {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub inventory_status: String,
    pub provenance_ids: Vec<String>,
    #[serde(default)]
    pub selectors: Option<Selectors>,
    #[serde(default)]
    pub relationship: Option<Relationship>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub evidence: Option<SourceEvidence>,
    #[serde(default)]
    pub parser_contract: Option<ParserContract>,
    #[serde(default)]
    pub retained_context: Option<RetainedContext>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Selectors {
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default, rename = "type")]
    pub type_: Option<String>,
    #[serde(default)]
    pub event_family: Option<String>,
    #[serde(default)]
    pub sourcetype: Option<String>,
}
pub fn distinct_sourcetype(selectors: &Selectors) -> Option<&str> {
    selectors
        .sourcetype
        .as_deref()
        .filter(|sourcetype| selectors.type_.as_deref() != Some(*sourcetype))
}

pub fn effective_restrictions(selectors: &Selectors) -> (Option<&str>, Option<&str>) {
    (
        selectors.event_family.as_deref(),
        distinct_sourcetype(selectors),
    )
}

pub fn discovery_compatible_restrictions(selectors: &Selectors) -> bool {
    effective_restrictions(selectors) == (None, None)
}

pub fn compatible_selector_restrictions(retained: &Selectors, incoming: &Selectors) -> bool {
    effective_restrictions(retained) == effective_restrictions(incoming)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    #[serde(default)]
    pub parent_repo_id: Option<String>,
    #[serde(default)]
    pub datasets: Vec<String>,
    #[serde(default)]
    pub record_kinds: Vec<RecordKind>,
    #[serde(default)]
    pub parsers: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RecordKind {
    pub field: String,
    pub values: Vec<String>,
    pub truncated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    #[serde(default)]
    pub declared: Option<bool>,
    #[serde(default)]
    pub observed: Option<bool>,
    #[serde(default)]
    pub observation_window: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_fields: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_depth: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ParserContract {
    pub source: String,
    pub parser: String,
    pub retrieved_at: String,
    #[serde(default)]
    pub source_hash: Option<String>,
    pub declared_fields: Vec<String>,
    pub fields_to_tag: Vec<String>,
    #[serde(default)]
    pub array_fields: Vec<String>,
    #[serde(default)]
    pub details: Option<Value>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RetainedContext {
    pub text: String,
    pub evidence_retrieved_at: Vec<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LegacyField {
    pub id: String,
    pub source_id: String,
    pub path: String,
    pub data_type: String,
    pub evidence_kind: String,
    pub provenance_ids: Vec<String>,
    #[serde(default)]
    pub schema_variant: Option<String>,
    #[serde(default)]
    pub is_tag: Option<bool>,
    #[serde(default)]
    pub is_array: Option<bool>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub observation_window: Option<String>,
    #[serde(default)]
    pub coverage: Option<Coverage>,
    #[serde(default)]
    pub retained_context: Option<RetainedContext>,
    #[serde(default)]
    pub refresh_status: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Coverage {
    #[serde(default)]
    pub declared: Option<bool>,
    #[serde(default)]
    pub observed: Option<bool>,
    #[serde(default)]
    pub population: Option<String>,
    #[serde(default)]
    pub rate: Option<f64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LegacyValue {
    pub id: String,
    pub field_id: String,
    pub value_kind: String,
    pub vocabulary: String,
    pub evidence_kind: String,
    pub provenance_ids: Vec<String>,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub sanitized: Option<bool>,
    #[serde(default)]
    pub meaning: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LegacyProvenance {
    pub id: String,
    pub source_type: String,
    pub canonical_source: String,
    pub retrieved_at: String,
    pub redaction_status: String,
    pub coverage_status: String,
    #[serde(default)]
    pub source_version: Option<String>,
    #[serde(default)]
    pub source_hash: Option<String>,
    #[serde(default)]
    pub observation_window: Option<String>,
    #[serde(default)]
    pub sampling: Option<Sampling>,
    #[serde(default)]
    pub notes: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Sampling {
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub budget: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct LegacyAnnotation {
    pub source_id: String,
    pub reviewed_at: String,
    pub notes: String,
}

pub fn source(v: &Value) -> Result<LegacySource, Diagnostic> {
    typed(v, "source")
}
pub fn field(v: &Value) -> Result<LegacyField, Diagnostic> {
    typed(v, "field")
}

#[derive(PartialEq, Eq)]
pub(crate) enum Observation {
    Qualified,
    Stale,
    Missing,
}

pub(crate) fn observation(pack: &Pack, ids: &[String]) -> Observation {
    let mut stale = false;
    for evidence in &pack.data.provenance {
        if !evidence["id"]
            .as_str()
            .is_some_and(|id| ids.iter().any(|candidate| candidate == id))
            || !matches!(
                evidence["source_type"].as_str(),
                Some("ngsiem-api" | "mcp-observation" | "console-export")
            )
        {
            continue;
        }
        match evidence["coverage_status"].as_str() {
            Some("complete" | "observed-only" | "reconciled") => return Observation::Qualified,
            Some("stale") => stale = true,
            _ => {}
        }
    }
    if stale {
        Observation::Stale
    } else {
        Observation::Missing
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct FieldMeasurement {
    pub start: time::OffsetDateTime,
    pub end: time::OffsetDateTime,
    pub retrieved_at: time::OffsetDateTime,
    pub observed: bool,
    pub rate: Option<f64>,
}

impl FieldMeasurement {
    pub(crate) fn key(
        self,
    ) -> (
        time::OffsetDateTime,
        time::OffsetDateTime,
        time::OffsetDateTime,
    ) {
        (self.end, self.start, self.retrieved_at)
    }
}

fn deployment_provenance(value: &Value) -> bool {
    matches!(
        value["source_type"].as_str(),
        Some("ngsiem-api" | "mcp-observation" | "console-export")
    )
}

pub fn linked_stale(pack: &Pack, ids: &[String]) -> bool {
    pack.data.provenance.iter().any(|value| {
        deployment_provenance(value)
            && value["coverage_status"] == "stale"
            && ids.iter().any(|id| value["id"] == *id)
    })
}

fn measurement_key(
    pack: &Pack,
    provenance_ids: &[String],
    window: &str,
) -> Option<(
    time::OffsetDateTime,
    time::OffsetDateTime,
    time::OffsetDateTime,
)> {
    let (start, end) = window.split_once('/')?;
    let start = parse_time(start).ok()?;
    let end = parse_time(end).ok()?;
    let created = parse_time(&pack.created_at).ok()?;
    if start >= end {
        return None;
    }
    let retrieved = pack
        .data
        .provenance
        .iter()
        .filter(|value| {
            deployment_provenance(value)
                && provenance_ids.iter().any(|id| value["id"] == *id)
                && matches!(
                    value["coverage_status"].as_str(),
                    Some("complete" | "observed-only" | "reconciled")
                )
                && value["observation_window"].as_str() == Some(window)
        })
        .filter_map(|value| parse_time(value["retrieved_at"].as_str()?).ok())
        .filter(|retrieved| end <= *retrieved && *retrieved <= created)
        .max()?;
    Some((end, start, retrieved))
}

pub fn source_measurement(
    pack: &Pack,
    source: &LegacySource,
) -> Option<(
    time::OffsetDateTime,
    time::OffsetDateTime,
    time::OffsetDateTime,
)> {
    let evidence = source.evidence.as_ref()?;
    evidence.observed?;
    measurement_key(
        pack,
        &source.provenance_ids,
        evidence.observation_window.as_deref()?,
    )
}

pub(crate) fn field_measurement(pack: &Pack, field: &LegacyField) -> Option<FieldMeasurement> {
    if field.evidence_kind != "observed"
        || field.refresh_status.is_some()
        || linked_stale(pack, &field.provenance_ids)
        || field.is_tag.is_none()
        || field.path.starts_with('#') && field.is_tag == Some(false)
    {
        return None;
    }
    let observed = field.coverage.as_ref()?.observed?;
    let source = source(
        pack.data
            .sources
            .iter()
            .find(|value| value["id"] == field.source_id)?,
    )
    .ok()?;
    let selectors = source.selectors?;
    if !selectors
        .repo
        .as_ref()
        .is_some_and(|value| required_text(value))
        && !selectors
            .type_
            .as_ref()
            .is_some_and(|value| required_text(value))
    {
        return None;
    }
    let (end, start, retrieved_at) = measurement_key(
        pack,
        &field.provenance_ids,
        field.observation_window.as_deref()?,
    )?;
    Some(FieldMeasurement {
        start,
        end,
        retrieved_at,
        observed,
        rate: field.coverage.as_ref().and_then(|coverage| coverage.rate),
    })
}

fn compatible_field_decisions(left: &LegacyField, right: &LegacyField) -> bool {
    let decision = |field: &LegacyField| {
        if field.refresh_status.is_some() {
            Some(false)
        } else {
            field
                .coverage
                .as_ref()
                .and_then(|coverage| coverage.observed)
        }
    };
    let rates = left
        .coverage
        .as_ref()
        .and_then(|coverage| coverage.rate)
        .zip(right.coverage.as_ref().and_then(|coverage| coverage.rate));
    decision(left)
        .zip(decision(right))
        .is_none_or(|(a, b)| a == b)
        && rates.is_none_or(|(a, b)| a == b)
}

fn typed<T: serde::de::DeserializeOwned>(v: &Value, kind: &str) -> Result<T, Diagnostic> {
    serde_json::from_value(v.clone())
        .map_err(|e| Diagnostic::error("PACK_RECORD", format!("Invalid {kind} structure: {e}")))
}
fn invalid(s: &str) -> Diagnostic {
    Diagnostic::error("PACK_INTEGRITY", s)
}
fn id(v: &Value) -> &str {
    v.get("id").and_then(Value::as_str).unwrap_or("")
}
fn valid_entity_id(s: &str, prefix: &str) -> bool {
    s.starts_with(prefix) && s.len() > prefix.len() && s.chars().all(|c| !c.is_control())
}
fn required_text(s: &str) -> bool {
    !s.trim().is_empty() && !s.contains('\0')
}
fn provenance_ids(ids: &[String], all: &BTreeSet<String>) -> Result<(), Diagnostic> {
    if ids.is_empty() || ids.iter().any(|i| !all.contains(i)) {
        Err(invalid("Missing or dangling provenance reference."))
    } else {
        Ok(())
    }
}
fn date_prefix(s: &str) -> bool {
    s.get(..10).is_some_and(|s| {
        time::Date::parse(s, time::macros::format_description!("[year]-[month]-[day]")).is_ok()
    })
}

impl PackData {
    pub fn validate(&self) -> Result<(), Diagnostic> {
        if self.sources.len()
            + self.fields.len()
            + self.values.len()
            + self.provenance.len()
            + self.annotations.len()
            > 200_000
        {
            return Err(invalid("Pack record budget exceeded."));
        }
        let mut all = BTreeSet::new();
        for records in [&self.sources, &self.fields, &self.values, &self.provenance] {
            for v in records {
                if !required_text(id(v)) || !all.insert(id(v).to_string()) {
                    return Err(invalid("Empty or duplicate record ID."));
                }
            }
        }
        let mut pids = BTreeSet::new();
        for v in &self.provenance {
            let p: LegacyProvenance = typed(v, "provenance")?;
            if !valid_entity_id(&p.id, "prov:")
                || !date_prefix(&p.retrieved_at)
                || !required_text(&p.canonical_source)
                || !matches!(
                    p.source_type.as_str(),
                    "official-docs"
                        | "mcp-observation"
                        | "console-export"
                        | "ngsiem-api"
                        | "manual-curation"
                )
                || !matches!(
                    p.redaction_status.as_str(),
                    "none" | "sanitized" | "redacted"
                )
                || !matches!(
                    p.coverage_status.as_str(),
                    "complete"
                        | "partial"
                        | "declared-only"
                        | "observed-only"
                        | "reconciled"
                        | "stale"
                        | "unavailable"
                        | "incomplete"
                        | "error"
                )
            {
                return Err(invalid(
                    "Invalid provenance identity, date, type or coverage.",
                ));
            }
            pids.insert(p.id);
        }
        let mut sids = BTreeSet::new();
        let mut selectors = BTreeSet::new();
        for v in &self.sources {
            let s = source(v)?;
            if !["env:repo:", "env:source:", "env:view:"]
                .iter()
                .any(|p| valid_entity_id(&s.id, p))
                || !matches!(s.kind.as_str(), "repo" | "source" | "view")
                || !required_text(&s.name)
                || !matches!(
                    s.inventory_status.as_str(),
                    "inventoried"
                        | "declared-only"
                        | "observed-only"
                        | "reconciled"
                        | "stale"
                        | "unavailable"
                        | "incomplete"
                        | "not-applicable"
                )
            {
                return Err(invalid(
                    "Invalid source identity, kind or inventory status.",
                ));
            }
            provenance_ids(&s.provenance_ids, &pids)?;
            if s.evidence.as_ref().is_some_and(|e| {
                e.discovery_depth
                    .as_deref()
                    .is_some_and(|depth| depth != "sources-only")
                    || e.discovery_depth.is_some() && e.measured_fields.is_some()
            }) {
                return Err(invalid(
                    "Sources-only depth cannot assert a measured field batch.",
                ));
            }
            if let Some(evidence) = s.evidence.as_ref()
                && let Some(fields) = &evidence.measured_fields
            {
                let unique = fields.iter().collect::<BTreeSet<_>>();
                if fields.is_empty()
                    || fields.len() > MAX_MEASURED_FIELDS
                    || unique.len() != fields.len()
                    || fields
                        .iter()
                        .any(|field| !required_text(field) || field.chars().any(char::is_control))
                {
                    return Err(invalid(
                        "Measured field batches must contain 1..4000 unique field names.",
                    ));
                }
                let window = evidence
                    .observation_window
                    .as_deref()
                    .and_then(|window| window.split_once('/'));
                let valid_window = window.is_some_and(|(start, end)| {
                    matches!((parse_time(start), parse_time(end)), (Ok(start), Ok(end)) if start < end)
                });
                let measured = evidence.observed == Some(true)
                    && valid_window
                    && s.selectors.as_ref().is_some_and(|selectors| {
                        selectors.repo.as_ref().is_some_and(|v| required_text(v))
                            || selectors.type_.as_ref().is_some_and(|v| required_text(v))
                    })
                    && self.provenance.iter().any(|provenance| {
                        s.provenance_ids.iter().any(|pid| provenance["id"] == *pid)
                            && matches!(
                                provenance["source_type"].as_str(),
                                Some("ngsiem-api" | "mcp-observation" | "console-export")
                            )
                            && matches!(
                                provenance["coverage_status"].as_str(),
                                Some("complete" | "observed-only" | "reconciled")
                            )
                            && provenance["observation_window"].as_str()
                                == evidence.observation_window.as_deref()
                    });
                if !measured {
                    return Err(invalid(
                        "Measured field batches require scoped, dated source observation provenance.",
                    ));
                }
            }
            sids.insert(s.id.clone());
            if let Some(x) = &s.selectors
                && !selectors.insert((x.repo.clone(), x.type_.clone()))
            {
                return Err(invalid("Duplicate source selectors."));
            }
            if let Some(c) = &s.parser_contract
                && (!date_prefix(&c.retrieved_at)
                    || !required_text(&c.parser)
                    || c.declared_fields.iter().any(|f| !required_text(f)))
            {
                return Err(invalid("Invalid parser contract."));
            }
        }
        for v in &self.sources {
            let s = source(v)?;
            if let Some(parent) = s.relationship.and_then(|r| r.parent_repo_id)
                && !sids.contains(&parent)
            {
                return Err(invalid("Dangling source parent."));
            }
        }
        let mut fids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        for v in &self.fields {
            let f = field(v)?;
            if !valid_entity_id(&f.id, "env:field:")
                || !sids.contains(&f.source_id)
                || !required_text(&f.path)
                || !matches!(
                    f.data_type.as_str(),
                    "string"
                        | "int"
                        | "long"
                        | "float"
                        | "boolean"
                        | "timestamp"
                        | "array"
                        | "object"
                        | "unknown"
                )
                || !matches!(
                    f.evidence_kind.as_str(),
                    "declared" | "observed" | "documented"
                )
            {
                return Err(invalid(
                    "Invalid field identity, source, path, type or evidence.",
                ));
            }
            if !paths.insert((
                f.source_id.clone(),
                f.path.clone(),
                f.is_tag.unwrap_or(false),
            )) {
                return Err(invalid("Duplicate field path/namespace within a source."));
            }
            if f.refresh_status
                .as_ref()
                .is_some_and(|s| s != "not-reobserved")
            {
                return Err(invalid("Invalid historical-field marker."));
            }
            if let Some(c) = f.coverage
                && c.rate
                    .is_some_and(|n| !n.is_finite() || !(0.0..=1.0).contains(&n))
            {
                return Err(invalid(
                    "Field population rate must lie between zero and one.",
                ));
            }
            provenance_ids(&f.provenance_ids, &pids)?;
            fids.insert(f.id);
        }
        for v in &self.values {
            let x: LegacyValue = typed(v, "value")?;
            if !valid_entity_id(&x.id, "env:value:")
                || !fids.contains(&x.field_id)
                || x.value.is_array()
                || x.value.is_object()
                || !matches!(
                    x.value_kind.as_str(),
                    "enum"
                        | "sample"
                        | "boolean"
                        | "numeric"
                        | "string"
                        | "missing"
                        | "null"
                        | "empty"
                )
                || !matches!(x.vocabulary.as_str(), "exhaustive" | "sampled" | "unknown")
                || !matches!(
                    x.evidence_kind.as_str(),
                    "declared" | "observed" | "documented"
                )
            {
                return Err(invalid("Invalid typed value sample or field reference."));
            }
            provenance_ids(&x.provenance_ids, &pids)?;
        }
        let mut annotated = BTreeSet::new();
        for v in &self.annotations {
            let a: LegacyAnnotation = typed(v, "annotation")?;
            if !sids.contains(&a.source_id)
                || !annotated.insert(a.source_id)
                || !date_prefix(&a.reviewed_at)
                || !required_text(&a.notes)
            {
                return Err(invalid("Invalid or duplicate source annotation."));
            }
        }
        Ok(())
    }
}

/// Import compatible historical records without refreshing or weakening retained evidence.
pub fn import_history(old: &Pack, candidate: &Pack) -> Result<Pack, Diagnostic> {
    old.validate()?;
    candidate.validate()?;
    if old.environment != candidate.environment {
        return Err(Diagnostic::error(
            "PACK_ENVIRONMENT_MISMATCH",
            "Cloud, tenant and searchable target must all match.",
        ));
    }
    if old.content_digest == candidate.content_digest {
        return Ok(old.clone());
    }
    let mut data = old.data.clone();
    for (retained, imported, key) in [
        (&mut data.sources, &candidate.data.sources, "id"),
        (&mut data.fields, &candidate.data.fields, "id"),
        (&mut data.values, &candidate.data.values, "id"),
        (&mut data.provenance, &candidate.data.provenance, "id"),
        (
            &mut data.annotations,
            &candidate.data.annotations,
            "source_id",
        ),
    ] {
        for record in imported {
            if let Some(previous) = retained
                .iter()
                .find(|previous| previous[key] == record[key])
            {
                if previous != record {
                    return Err(invalid(
                        "Imported record ID conflicts with retained evidence.",
                    ));
                }
            } else {
                retained.push(record.clone());
            }
        }
    }
    if data == old.data {
        return Ok(old.clone());
    }
    let created_at = if parse_time(&candidate.created_at)? > parse_time(&old.created_at)? {
        candidate.created_at.clone()
    } else {
        old.created_at.clone()
    };
    Pack::new(old.environment.clone(), created_at, data)
}

fn merge_id(
    prefix: &str,
    identity: impl Fn(u64) -> Vec<u8>,
    available: impl Fn(&str) -> bool,
) -> String {
    (0..)
        .map(|counter| {
            let hash = sha256_hex(&identity(counter));
            format!("{prefix}:merge:{}", &hash[..16])
        })
        .find(|id| available(id))
        .expect("finite pack record IDs cannot exhaust the merge ID space")
}

pub fn canonical_field_path(path: &str, is_tag: bool) -> &str {
    if is_tag {
        path.strip_prefix('#')
            .filter(|path| !path.is_empty())
            .unwrap_or(path)
    } else {
        path
    }
}

pub fn field_identity<'a>(
    source_id: &'a str,
    path: &'a str,
    is_tag: bool,
) -> (&'a str, &'a str, bool) {
    (source_id, canonical_field_path(path, is_tag), is_tag)
}

pub fn measured_field_identity(name: &str) -> (&str, bool) {
    name.strip_prefix('#')
        .map_or((name, false), |path| (path, true))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum FieldMerge {
    Historical,
    Canonical,
}

impl FieldMerge {
    fn identity<'a>(
        self,
        source: &'a str,
        path: &'a str,
        is_tag: bool,
    ) -> (&'a str, &'a str, bool) {
        match self {
            Self::Historical => (source, path, is_tag),
            Self::Canonical => field_identity(source, path, is_tag),
        }
    }

    fn batch_entry(self, name: &str) -> (&str, bool) {
        match self {
            Self::Historical => (name, false),
            Self::Canonical => measured_field_identity(name),
        }
    }

    fn batch_contains(self, batch: &BTreeSet<(&str, bool)>, path: &str, is_tag: bool) -> bool {
        match self {
            Self::Historical if is_tag => batch.contains(&(format!("#{path}").as_str(), false)),
            Self::Historical => batch.contains(&(path, false)),
            Self::Canonical => batch.contains(&(canonical_field_path(path, is_tag), is_tag)),
        }
    }
}

fn record_field_parts(record: &Value) -> (&str, &str, bool) {
    (
        record["source_id"]
            .as_str()
            .expect("validated field source"),
        record["path"].as_str().expect("validated field path"),
        record["is_tag"].as_bool().unwrap_or(false),
    )
}

enum FieldUpdate {
    Keep,
    Replace,
    Unapplied,
}

fn field_update(
    old: &Pack,
    retained: &LegacyField,
    fresh: &LegacyField,
    retained_measurement: Option<FieldMeasurement>,
    incoming_measurement: Option<FieldMeasurement>,
    in_batch: Option<bool>,
    field_merge: FieldMerge,
) -> Result<FieldUpdate, Diagnostic> {
    let incoming_observed = fresh
        .coverage
        .as_ref()
        .and_then(|coverage| coverage.observed);
    if incoming_observed.is_none() || (in_batch == Some(false) && incoming_measurement.is_none()) {
        return Ok(FieldUpdate::Keep);
    }
    let retained_negative = retained.refresh_status.is_some()
        || linked_stale(old, &retained.provenance_ids)
        || retained
            .coverage
            .as_ref()
            .and_then(|coverage| coverage.observed)
            == Some(false);
    let after_undated_negative = retained_measurement.is_none()
        && retained_negative
        && incoming_measurement.is_some_and(|measurement| {
            parse_time(&old.created_at).is_ok_and(|created| measurement.start > created)
        });
    let conflict = |reason| {
        invalid(&format!(
            "Field measurement ordering conflict for source {} field {}: {reason}",
            retained.source_id, fresh.path
        ))
    };
    if retained_measurement.is_none() && retained_negative && !after_undated_negative {
        let compatible_rate = retained
            .coverage
            .as_ref()
            .and_then(|coverage| coverage.rate)
            .zip(fresh.coverage.as_ref().and_then(|coverage| coverage.rate))
            .is_none_or(|(previous, incoming)| previous == incoming);
        if incoming_observed == Some(false) && compatible_rate {
            return Ok(FieldUpdate::Keep);
        }
        // Preserve undated negative evidence when the new window cannot clear its boundary.
        if field_merge == FieldMerge::Canonical && incoming_measurement.is_some() {
            return Ok(FieldUpdate::Unapplied);
        }
        return Err(conflict(
            "contradictory evidence has no orderable measurement after the retained negative snapshot boundary.",
        ));
    }
    let compatible = retained_measurement.zip(incoming_measurement).map_or_else(
        || compatible_field_decisions(retained, fresh),
        |(previous, incoming)| {
            previous.observed == incoming.observed
                && previous.rate.zip(incoming.rate).is_none_or(|(a, b)| a == b)
        },
    ) && !(retained_negative
        && incoming_measurement.is_some_and(|measurement| measurement.observed));
    match (retained_measurement, incoming_measurement) {
        (Some(previous), Some(incoming)) if incoming.key() <= previous.key() => {
            if !compatible {
                return Err(conflict(
                    "an older or tied contradictory measurement cannot replace current evidence.",
                ));
            }
            Ok(FieldUpdate::Keep)
        }
        (Some(_), None) | (None, Some(_)) | (None, None)
            if !compatible && !after_undated_negative =>
        {
            Err(conflict(
                "contradictory evidence has no orderable current measurement.",
            ))
        }
        (Some(_), None) => Ok(FieldUpdate::Keep),
        _ => Ok(FieldUpdate::Replace),
    }
}

fn retain_field_metadata(fresh: &mut Value, matches: &[&Value], provenance: &[Value]) {
    // Preserve explicit nulls on single-record refreshes. A fold can fill them from another match.
    let metadata_present = |value: &&Value| matches.len() == 1 || !value.is_null();
    if let Some(variant) = matches
        .iter()
        .find_map(|p| p.get("schema_variant").filter(metadata_present))
    {
        fresh["schema_variant"] = variant.clone();
    }
    for prior in matches {
        if let Some(context) = prior.get("retained_context").filter(metadata_present) {
            fresh["retained_context"] = context.clone();
            break;
        }
        if let Some(description) = prior["description"].as_str().filter(|s| !s.is_empty()) {
            let dates = prior["provenance_ids"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|pid| provenance.iter().find(|p| p["id"] == *pid))
                .filter_map(|p| p["retrieved_at"].as_str())
                .collect::<BTreeSet<_>>();
            fresh["retained_context"] =
                serde_json::json!({"text":description,"evidence_retrieved_at":dates});
            break;
        }
    }

    let mut evidence = fresh["provenance_ids"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for prior in matches {
        if fresh["data_type"] == "unknown"
            && prior["data_type"].as_str().is_some_and(|s| s != "unknown")
        {
            fresh["data_type"] = prior["data_type"].clone();
        }
        if fresh["is_array"].is_null() && prior.get("is_array").is_some() {
            fresh["is_array"] = prior["is_array"].clone();
        }
        if let Some(declared) = prior.pointer("/coverage/declared").filter(|_| {
            fresh
                .pointer("/coverage/declared")
                .is_none_or(Value::is_null)
        }) {
            if fresh.get("coverage").is_none() {
                fresh["coverage"] = serde_json::json!({});
            }
            fresh["coverage"]["declared"] = declared.clone();
        }
        for proof in prior["provenance_ids"].as_array().into_iter().flatten() {
            if !evidence.contains(proof)
                && !provenance
                    .iter()
                    .any(|p| p["id"] == *proof && deployment_provenance(p))
            {
                evidence.push(proof.clone());
            }
        }
    }
    fresh["provenance_ids"] = Value::Array(evidence);
}

/// Replace only fully measured selected sources. Quiet fields retain their original evidence.
pub fn validate_incoming_fields(candidate: &Pack) -> Result<(), Diagnostic> {
    let mut incoming_fields = BTreeSet::new();
    for record in &candidate.data.fields {
        let _ = field(record)?;
        let (source_id, path, is_tag) = record_field_parts(record);
        let identity = FieldMerge::Canonical.identity(source_id, path, is_tag);
        if !incoming_fields.insert(identity) {
            let prefix = if is_tag { "#" } else { "" };
            return Err(invalid(&format!(
                "Duplicate incoming field for source {source_id} field {prefix}{}.",
                identity.1
            )));
        }
    }
    Ok(())
}

pub fn merge(old: &Pack, candidate: &Pack) -> Result<Pack, Diagnostic> {
    merge_with_notices(old, candidate).map(|(pack, _)| pack)
}

pub fn merge_with_notices(
    old: &Pack,
    candidate: &Pack,
) -> Result<(Pack, Vec<Diagnostic>), Diagnostic> {
    merge_fields(old, candidate, FieldMerge::Canonical)
}

/// Reconstruct saved snapshots with their original field identity and ordering rules.
pub fn merge_historical(old: &Pack, candidate: &Pack) -> Result<Pack, Diagnostic> {
    merge_fields(old, candidate, FieldMerge::Historical).map(|(pack, _)| pack)
}

fn merge_fields(
    old: &Pack,
    candidate: &Pack,
    field_merge: FieldMerge,
) -> Result<(Pack, Vec<Diagnostic>), Diagnostic> {
    old.validate()?;
    candidate.validate()?;
    if old.environment != candidate.environment {
        return Err(Diagnostic::error(
            "PACK_ENVIRONMENT_MISMATCH",
            "Cloud, tenant and searchable target must all match.",
        ));
    }
    if parse_time(&candidate.created_at)? <= parse_time(&old.created_at)? {
        return Err(invalid(
            "An older or same-time snapshot cannot replace current evidence.",
        ));
    }
    let mut data = old.data.clone();
    let retained_sources: BTreeSet<_> = old.data.sources.iter().map(id).collect();
    let mut retained_positions: BTreeMap<_, _> = old
        .data
        .sources
        .iter()
        .enumerate()
        .filter_map(|(index, record)| {
            source(record)
                .ok()?
                .selectors
                .map(|s| ((s.repo, s.type_), index))
        })
        .collect();
    let fresh_sources = candidate
        .data
        .sources
        .iter()
        .map(source)
        .collect::<Result<Vec<_>, _>>()?;
    let positions = fresh_sources
        .iter()
        .map(|fresh| {
            let selector = fresh.selectors.clone().unwrap_or_default();
            // A pack can contain several records without selectors. Claim a retained ID once.
            retained_positions.remove(&(selector.repo, selector.type_))
        })
        .collect::<Vec<_>>();
    let mut used_ids = [
        &old.data.sources,
        &old.data.fields,
        &old.data.values,
        &old.data.provenance,
        &candidate.data.fields,
        &candidate.data.values,
        &candidate.data.provenance,
    ]
    .into_iter()
    .flatten()
    .map(|record| id(record).to_owned())
    .collect::<BTreeSet<_>>();

    // Reserve the retained and unchanged IDs before assigning IDs to colliding sources.
    let mut source_ids = BTreeMap::new();
    for (fresh, previous) in fresh_sources.iter().zip(&positions) {
        if let Some(index) = previous {
            source_ids.insert(fresh.id.as_str(), id(&old.data.sources[*index]).to_owned());
        } else if used_ids.insert(fresh.id.clone()) {
            source_ids.insert(fresh.id.as_str(), fresh.id.clone());
        }
    }
    for fresh in &fresh_sources {
        if !source_ids.contains_key(fresh.id.as_str()) {
            let selector = fresh.selectors.as_ref();
            let repo = selector.and_then(|s| s.repo.as_deref());
            let type_ = selector.and_then(|s| s.type_.as_deref());
            let final_id = merge_id(
                "env:repo",
                |counter| serde_json::to_vec(&("source", &fresh.id, repo, type_, counter)).unwrap(),
                |id| !used_ids.contains(id),
            );
            used_ids.insert(final_id.clone());
            source_ids.insert(fresh.id.as_str(), final_id);
        }
    }
    let mut field_batches = BTreeMap::new();
    let mut omission_measurements = BTreeMap::new();
    for ((fresh, f), previous) in candidate
        .data
        .sources
        .iter()
        .zip(&fresh_sources)
        .zip(positions)
    {
        let selector = f.selectors.clone().unwrap_or_default();
        if !matches!(f.inventory_status.as_str(), "reconciled" | "observed-only") {
            return Err(invalid(
                "Only completed source measurements can replace a source.",
            ));
        }
        let mut merged = fresh.clone();
        let source_id = source_ids[f.id.as_str()].clone();
        merged["id"] = Value::String(source_id.clone());
        if let Some(parent) = f
            .relationship
            .as_ref()
            .and_then(|r| r.parent_repo_id.as_deref())
        {
            merged["relationship"]["parent_repo_id"] = Value::String(source_ids[parent].clone());
        }
        if let Some(fields) = f.evidence.as_ref().and_then(|e| e.measured_fields.as_ref()) {
            field_batches.insert(
                source_id.clone(),
                fields
                    .iter()
                    .map(|name| field_merge.batch_entry(name))
                    .collect::<BTreeSet<_>>(),
            );
        } else if f
            .evidence
            .as_ref()
            .is_some_and(|e| e.discovery_depth.as_deref() == Some("sources-only"))
        {
            field_batches.insert(source_id.clone(), BTreeSet::new());
        }
        let incoming_source = source_measurement(candidate, f);
        if let Some(measurement) = incoming_source {
            omission_measurements.insert(source_id.clone(), measurement);
        }
        if let Some(index) = previous {
            let prior = data.sources[index].clone();
            let retained = source(&prior)?;
            let old_selectors = retained.selectors.clone().unwrap_or_default();
            if !compatible_selector_restrictions(&old_selectors, &selector) {
                return Err(invalid(&format!(
                    "Source selector restrictions differ for source {}; a broad measurement cannot replace narrower source evidence.",
                    retained.id
                )));
            }
            let stale =
                retained.inventory_status == "stale" || linked_stale(old, &retained.provenance_ids);
            let negative = stale
                || retained
                    .evidence
                    .as_ref()
                    .and_then(|evidence| evidence.observed)
                    == Some(false);
            let incoming_observed = f.evidence.as_ref().and_then(|evidence| evidence.observed);
            let retained_measurement = source_measurement(old, &retained);
            let ordered_source = if stale || negative && retained_measurement.is_none() {
                incoming_source.is_some_and(|(_, start, _)| {
                    parse_time(&old.created_at).is_ok_and(|created| start > created)
                })
            } else {
                retained_measurement
                    .zip(incoming_source)
                    .is_some_and(|(previous, incoming)| incoming > previous)
            };
            let conflicting_source = if negative {
                incoming_observed != Some(false)
            } else {
                retained
                    .evidence
                    .as_ref()
                    .and_then(|evidence| evidence.observed)
                    .zip(incoming_observed)
                    .is_some_and(|(previous, incoming)| previous != incoming)
            };
            if conflicting_source && !ordered_source {
                return Err(invalid(&format!(
                    "Source measurement ordering conflict for source {}: contradictory source evidence cannot replace current evidence without a newer qualifying measurement.",
                    retained.id
                )));
            }
            let preserve_source = !ordered_source
                && (negative || incoming_observed.is_none() || retained_measurement.is_some());
            for key in ["name", "description", "parser_contract", "retained_context"] {
                if prior.get(key).is_some()
                    && (fresh.get(key).is_none_or(Value::is_null)
                        || key == "name"
                        || key == "description")
                {
                    merged[key] = prior[key].clone();
                }
            }
            if fresh.get("parser_contract").is_none_or(Value::is_null)
                && let Some(declared) = prior.pointer("/evidence/declared")
            {
                merged["evidence"]["declared"] = declared.clone();
            }
            if let Some(relationship) = prior.get("relationship").and_then(Value::as_object) {
                if let Some(parent) = relationship
                    .get("parent_repo_id")
                    .filter(|value| !value.is_null())
                {
                    merged["relationship"]["parent_repo_id"] = parent.clone();
                }
                for key in ["datasets", "parsers"] {
                    let mut items = merged["relationship"][key]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    for item in relationship
                        .get(key)
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if !items.contains(item) {
                            items.push(item.clone());
                        }
                    }
                    merged["relationship"][key] = Value::Array(items);
                }
                // Fold current and prior entries through one per-field union so duplicate entries for
                // the same field (on either side) can never keep values or truncation evidence apart.
                let current = f
                    .relationship
                    .as_ref()
                    .map(|r| r.record_kinds.clone())
                    .unwrap_or_default();
                let previous = source(&prior)?
                    .relationship
                    .into_iter()
                    .flat_map(|r| r.record_kinds);
                let mut record_kinds: Vec<_> = Vec::new();
                for entry in current.into_iter().chain(previous) {
                    if let Some(existing) = record_kinds
                        .iter_mut()
                        .find(|existing: &&mut RecordKind| existing.field == entry.field)
                    {
                        for value in entry.values {
                            if !existing.values.contains(&value) {
                                existing.values.push(value);
                            }
                        }
                        existing.truncated |= entry.truncated;
                    } else {
                        record_kinds.push(entry);
                    }
                }
                if !record_kinds.is_empty() {
                    merged["relationship"]["record_kinds"] = serde_json::json!(record_kinds);
                }
            }
            let mut evidence = merged["provenance_ids"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for p in prior["provenance_ids"].as_array().into_iter().flatten() {
                if !evidence.contains(p)
                    && !data
                        .provenance
                        .iter()
                        .any(|value| value["id"] == *p && deployment_provenance(value))
                {
                    evidence.push(p.clone());
                }
            }
            merged["provenance_ids"] = Value::Array(evidence);
            if let Some(selectors) = prior.get("selectors").and_then(Value::as_object) {
                for key in ["event_family", "sourcetype"] {
                    if let Some(v) = selectors.get(key).filter(|value| !value.is_null()) {
                        merged["selectors"][key] = v.clone();
                    }
                }
            }
            if preserve_source {
                // Retained source proof cannot date a new historical marker for omitted fields.
                field_batches.insert(source_id, BTreeSet::new());
                for key in ["evidence", "inventory_status", "provenance_ids"] {
                    merged[key] = prior[key].clone();
                }
            }
            data.sources[index] = merged;
        } else {
            data.sources.push(merged);
        }
    }
    let mut fresh_fields = Vec::new();
    let mut field_ids = BTreeMap::new();
    let mut collision_field_sources = BTreeMap::new();
    let mut incoming_fields = BTreeSet::new();
    let mut matched_fields = BTreeSet::new();
    let mut no_fold_matches = BTreeMap::new();
    let mut folded_fields = BTreeMap::new();
    let mut notices = Vec::new();
    let mut retained_fields: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for prior in &old.data.fields {
        let (source, path, is_tag) = record_field_parts(prior);
        retained_fields
            .entry(field_merge.identity(source, path, is_tag))
            .or_default()
            .push(prior);
    }
    for record in &candidate.data.fields {
        let fresh = field(record)?;
        let (original_source, path, is_tag) = record_field_parts(record);
        let source_id = &source_ids[original_source];
        let identity = field_merge.identity(source_id, path, is_tag);
        let name = || {
            let prefix = if is_tag { "#" } else { "" };
            format!("{prefix}{}", identity.1)
        };
        if field_merge == FieldMerge::Canonical && !incoming_fields.insert(identity) {
            return Err(invalid(&format!(
                "Duplicate incoming field for source {source_id} field {}.",
                name()
            )));
        }

        let mut f = record.clone();
        let colliding_source =
            fresh.source_id != *source_id && retained_sources.contains(fresh.source_id.as_str());
        if colliding_source {
            collision_field_sources.insert(fresh.id.clone(), source_id);
        }
        f["source_id"] = Value::String(source_id.clone());
        let mut colliding_field = false;
        if let Some(prior) = old.data.fields.iter().find(|prior| id(prior) == fresh.id) {
            let (prior_source, prior_path, prior_tag) = record_field_parts(prior);
            if field_merge.identity(prior_source, prior_path, prior_tag) != identity {
                if !colliding_source || prior_source == source_id {
                    return Err(invalid(
                        "A field ID cannot be reused for a different source, path or namespace.",
                    ));
                }
                colliding_field = true;
            }
        }
        if let Some(matches) = retained_fields.get(&identity) {
            let first = matches[0];
            let incoming_measurement = field_measurement(candidate, &fresh);
            let in_batch = field_batches
                .get(source_id)
                .map(|batch| field_merge.batch_contains(batch, path, is_tag));
            let mut replace = matches.len() == 1 || incoming_measurement.is_some();
            let mut unapplied = false;
            let mut newest = first;
            let mut newest_measurement = None;
            for prior in matches {
                let retained = field(prior)?;
                let prior_measurement = field_measurement(old, &retained);
                let key = prior_measurement.map(FieldMeasurement::key);
                if key > newest_measurement {
                    newest = prior;
                    newest_measurement = key;
                }
                match field_update(
                    old,
                    &retained,
                    &fresh,
                    prior_measurement,
                    incoming_measurement,
                    in_batch,
                    field_merge,
                )? {
                    FieldUpdate::Keep => replace = false,
                    FieldUpdate::Unapplied => {
                        replace = false;
                        unapplied = true;
                    }
                    FieldUpdate::Replace => {}
                }
                matched_fields.insert(id(prior));
            }
            if unapplied {
                notices.push(Diagnostic::warning("FIELD_MEASUREMENT_NOT_APPLIED", format!(
                    "Measurement for source {source_id} field {} did not start after the retained negative snapshot boundary {}.",
                    name(), old.created_at
                )));
            }
            if !replace {
                if matches.len() > 1 {
                    no_fold_matches.insert(id(record), matches.as_slice());
                }
                field_ids.insert(fresh.id, id(newest).to_owned());
                continue;
            }

            let survivor = id(first);
            field_ids.insert(fresh.id.clone(), survivor.to_owned());
            f["id"] = Value::String(survivor.into());
            for prior in &matches[1..] {
                folded_fields.insert(id(prior), survivor);
            }
            if in_batch.is_some()
                && incoming_measurement.is_none()
                && fresh
                    .coverage
                    .as_ref()
                    .and_then(|coverage| coverage.observed)
                    == Some(false)
            {
                f["refresh_status"] = Value::String("not-reobserved".into());
            }
            retain_field_metadata(&mut f, matches, &data.provenance);
        } else if colliding_field {
            let final_id = merge_id(
                "env:field",
                |counter| serde_json::to_vec(&("field", &fresh.id, source_id, counter)).unwrap(),
                |id| !used_ids.contains(id),
            );
            used_ids.insert(final_id.clone());
            field_ids.insert(fresh.id, final_id.clone());
            f["id"] = Value::String(final_id);
        }
        fresh_fields.push(f);
    }
    data.fields
        .retain(|prior| !folded_fields.contains_key(id(prior)));
    for value in &mut data.values {
        if let Some(survivor) = value["field_id"]
            .as_str()
            .and_then(|id| folded_fields.get(id))
        {
            value["field_id"] = Value::String((*survivor).into());
        }
    }
    let selected: BTreeSet<_> = source_ids.values().map(String::as_str).collect();
    for prior in &mut data.fields {
        let (source_id, path, is_tag) = record_field_parts(prior);
        if !selected.contains(source_id) {
            continue;
        }
        if let Some(f) = fresh_fields.iter().find(|f| id(f) == id(prior)) {
            *prior = f.clone();
        } else if !matched_fields.contains(id(prior))
            && field_batches
                .get(source_id)
                .is_none_or(|batch| field_merge.batch_contains(batch, path, is_tag))
            && (field_merge == FieldMerge::Historical
                || field_measurement(old, &field(prior)?).is_none_or(|measurement| {
                    omission_measurements
                        .get(source_id)
                        .is_some_and(|batch| *batch > measurement.key())
                }))
        {
            prior["refresh_status"] = Value::String("not-reobserved".into());
        }
    }
    for f in fresh_fields {
        if !data.fields.iter().any(|p| id(p) == id(&f)) {
            data.fields.push(f);
        }
    }
    for p in &candidate.data.provenance {
        if let Some(prior) = data.provenance.iter().find(|x| id(x) == id(p)) {
            if prior != p {
                return Err(invalid(
                    "Conflicting provenance IDs; use a new dated run identity.",
                ));
            }
        } else {
            data.provenance.push(p.clone());
        }
    }
    let mut incoming_value_ids: BTreeSet<_> = candidate
        .data
        .values
        .iter()
        .map(|v| id(v).to_owned())
        .collect();
    for fresh in &candidate.data.values {
        let mut v = fresh.clone();
        let original_field = fresh["field_id"]
            .as_str()
            .expect("validated value field ID");
        if let Some(field) = field_ids.get(original_field) {
            v["field_id"] = Value::String(field.clone());
        }
        let colliding_source = collision_field_sources.get(original_field);
        let aliases = no_fold_matches.get(original_field);
        if (colliding_source.is_some() || aliases.is_some())
            && let Some(prior_value) = old.data.values.iter().find(|p| id(p) == id(fresh))
            && (colliding_source.is_some_and(|source_id| {
                old.data.fields.iter().any(|field| {
                    field["id"] == prior_value["field_id"] && field["source_id"] != **source_id
                })
            }) || aliases.is_some_and(|matches| {
                // A repeated sample can move to the newest alias while preserving its old binding.
                prior_value["value"] == v["value"]
                    && prior_value["field_id"] != v["field_id"]
                    && matches
                        .iter()
                        .any(|field| field["id"] == prior_value["field_id"])
            }))
        {
            let final_field = v["field_id"].as_str().expect("mapped value field ID");
            let final_id = merge_id(
                "env:value",
                |counter| serde_json::to_vec(&("value", id(fresh), final_field, counter)).unwrap(),
                |id| {
                    !incoming_value_ids.contains(id)
                        && (!used_ids.contains(id)
                            || old.data.values.iter().any(|prior| {
                                prior["id"] == id
                                    && prior["field_id"] == final_field
                                    && prior["value"] == fresh["value"]
                            }))
                },
            );
            incoming_value_ids.insert(final_id.clone());
            used_ids.insert(final_id.clone());
            v["id"] = Value::String(final_id);
        }
        if let Some(index) = data.values.iter().position(|p| id(p) == id(&v)) {
            let prior = &data.values[index];
            if prior["field_id"] != v["field_id"] || prior["value"] != v["value"] {
                return Err(invalid(
                    "A value ID cannot be reused for different evidence.",
                ));
            }
            let mut evidence = v["provenance_ids"].as_array().cloned().unwrap_or_default();
            for p in prior["provenance_ids"].as_array().into_iter().flatten() {
                if !evidence.contains(p) {
                    evidence.push(p.clone());
                }
            }
            v["provenance_ids"] = Value::Array(evidence);
            for key in ["meaning", "sanitized"] {
                if let Some(value) = prior.get(key) {
                    v[key] = value.clone();
                }
            }
            data.values[index] = v;
        } else {
            data.values.push(v);
        }
    }
    // Existing operator annotations are intentionally authoritative across refresh.
    for a in &candidate.data.annotations {
        let mut a = a.clone();
        if let Some(fresh) = a["source_id"].as_str()
            && let Some(final_id) = source_ids.get(fresh)
        {
            a["source_id"] = Value::String(final_id.clone());
        }
        if !data
            .annotations
            .iter()
            .any(|p| p["source_id"] == a["source_id"])
        {
            data.annotations.push(a);
        }
    }
    for parent in [old, candidate] {
        let lineage = serde_json::json!({
            "id": format!("prov:snapshot:{}", parent.content_digest),
            "source_type": "manual-curation",
            "canonical_source": "CQLmatic parent snapshot metadata; not a deployment observation",
            "retrieved_at": parent.created_at,
            "redaction_status": "none",
            "coverage_status": "declared-only",
            "source_version": parent.producer_version,
            "source_hash": parent.content_digest,
            "notes": serde_json::to_string(&serde_json::json!({
                "format_version": parent.format_version,
                "profile_id": parent.profile_id,
                "knowledge_revision": parent.knowledge_revision,
                "environment": parent.environment,
                "created_at": parent.created_at
            })).map_err(|_| invalid("Cannot serialize parent metadata."))?
        });
        if let Some(prior) = data.provenance.iter().find(|p| id(p) == id(&lineage)) {
            if prior != &lineage {
                return Err(invalid("Conflicting parent snapshot metadata."));
            }
        } else {
            data.provenance.push(lineage);
        }
    }
    let pack = Pack::new(old.environment.clone(), candidate.created_at.clone(), data)?;
    Ok((pack, notices))
}
