//! Pure pack contracts, validation, grounding and evidence-preserving merge.
use crate::{
    contracts::{Diagnostic, EnvironmentId, PROFILE, falcon_cid},
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
/// Sort RFC 3339 timestamps by UTC instant without truncating fractional seconds.
pub fn exact_time_key(s: &str) -> Result<(i64, bool, &str), Diagnostic> {
    let time = parse_time(s)?;
    let offset_len = if s.ends_with(['Z', 'z']) { 1 } else { 6 };
    let fraction = s[19..s.len() - offset_len]
        .trim_start_matches('.')
        .trim_end_matches('0');
    // The parser represents a leap second at the end of the previous Unix second.
    Ok((time.unix_timestamp(), &s[17..19] == "60", fraction))
}

pub fn normalize_request_time(s: &str) -> Result<String, Diagnostic> {
    let separator = s.as_bytes().get(10);
    if !matches!(separator, Some(b'T' | b't' | b' ')) {
        return Err(Diagnostic::error(
            "PACK_TIME",
            "Use T, t or a space between the date and time in an RFC 3339 timestamp with an explicit offset.",
        ));
    }
    let time = parse_time(s)?;
    if separator == Some(&b'T') && s.ends_with('Z') {
        return Ok(s.into());
    }
    let utc = time
        .checked_to_offset(time::UtcOffset::UTC)
        .and_then(|utc| {
            utc.format(&time::format_description::well_known::Rfc3339)
                .ok()
        })
        .ok_or_else(|| {
            Diagnostic::error("PACK_TIME", "Timestamp is outside the UTC RFC 3339 range.")
        })?;
    // An RFC 3339 offset changes the date, hour and minute. Keep the original seconds and
    // fractional digits, including any trailing zeros that the formatter would remove.
    let offset_len = if s.ends_with(['Z', 'z']) { 1 } else { 6 };
    Ok(format!("{}{}Z", &utc[..17], &s[17..s.len() - offset_len]))
}

pub fn validate_identity(e: &EnvironmentId) -> Result<(), Diagnostic> {
    let cid = falcon_cid(&e.tenant).is_some();
    let client = e
        .tenant
        .strip_prefix("api-client:")
        .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()));
    if !cid && !client {
        return Err(Diagnostic::error(
            "ENV_TENANT",
            "Supply a Falcon CID of 32 hexadecimal digits with an optional two-digit hexadecimal checksum suffix (-XX), or an api-client:<SHA-256> fingerprint.",
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

pub fn normalize_identity(mut identity: EnvironmentId) -> Result<EnvironmentId, Diagnostic> {
    validate_identity(&identity)?;
    if let Some(cid) = falcon_cid(&identity.tenant) {
        identity.tenant = cid.to_ascii_lowercase();
    }
    Ok(identity)
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

type MeasurementKey = (
    time::OffsetDateTime,
    time::OffsetDateTime,
    time::OffsetDateTime,
);

fn measurement_key(
    pack: &Pack,
    created: time::OffsetDateTime,
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
    source_measurement_at(pack, source, parse_time(&pack.created_at).ok()?)
}

fn source_measurement_at(
    pack: &Pack,
    source: &LegacySource,
    created: time::OffsetDateTime,
) -> Option<MeasurementKey> {
    let evidence = source.evidence.as_ref()?;
    evidence.observed?;
    measurement_key(
        pack,
        created,
        &source.provenance_ids,
        evidence.observation_window.as_deref()?,
    )
}

pub(crate) fn field_measurement(pack: &Pack, field: &LegacyField) -> Option<FieldMeasurement> {
    field_measurement_at(pack, field, parse_time(&pack.created_at).ok()?)
}

fn field_measurement_at(
    pack: &Pack,
    field: &LegacyField,
    created: time::OffsetDateTime,
) -> Option<FieldMeasurement> {
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
        created,
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
    validate_import_pair(old, candidate)?;
    if old.content_digest == candidate.content_digest {
        return Ok(old.clone());
    }
    let mut data = old.data.clone();
    for (collection, retained, imported, key) in [
        ("sources", &mut data.sources, &candidate.data.sources, "id"),
        ("fields", &mut data.fields, &candidate.data.fields, "id"),
        ("values", &mut data.values, &candidate.data.values, "id"),
        (
            "provenance",
            &mut data.provenance,
            &candidate.data.provenance,
            "id",
        ),
        (
            "annotations",
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
                    return Err(invalid(&format!(
                        "Imported {collection} {key} {} conflicts with retained evidence. Submit import-pack with mode: \"update\" to update matching records.",
                        record[key],
                    )));
                }
            } else {
                if collection == "sources"
                    && let Some(selectors) = selector_key(record)
                    && retained
                        .iter()
                        .any(|prior| selector_key(prior) == Some(selectors))
                {
                    return Err(invalid(&format!(
                        "Imported sources have duplicate source selectors (repo: {:?}, type: {:?}). Submit import-pack with mode: \"update\" to update matching records.",
                        selectors.0, selectors.1,
                    )));
                }
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

fn validate_import_pair(old: &Pack, candidate: &Pack) -> Result<(), Diagnostic> {
    old.validate()?;
    candidate.validate()?;
    if old.environment != candidate.environment {
        return Err(Diagnostic::error(
            "PACK_ENVIRONMENT_MISMATCH",
            "Cloud, tenant and searchable target must all match.",
        ));
    }
    Ok(())
}

#[derive(Default, Debug, Serialize)]
pub struct ImportCounts {
    pub sources_updated: usize,
    pub sources_added: usize,
    pub sources_unchanged: usize,
    pub fields_updated: usize,
    pub fields_added: usize,
    pub fields_unchanged: usize,
    pub values_updated: usize,
    pub values_added: usize,
    pub annotations_added: usize,
    pub reidentified: Reidentified,
}

#[derive(Default, Debug, Serialize)]
pub struct Reidentified {
    pub sources: usize,
    pub fields: usize,
    pub values: usize,
    pub provenance: usize,
}

impl ImportCounts {
    pub fn appended(previous: Option<&Pack>, pack: &Pack, input: &Pack) -> Self {
        let old = previous.map(|p| &p.data);
        let old_sources = old.map_or(0, |data| data.sources.len());
        let old_fields = old.map_or(0, |data| data.fields.len());
        Self {
            sources_added: pack.data.sources.len() - old_sources,
            sources_unchanged: input.data.sources.len() - (pack.data.sources.len() - old_sources),
            fields_added: pack.data.fields.len() - old_fields,
            fields_unchanged: input.data.fields.len() - (pack.data.fields.len() - old_fields),
            values_added: pack.data.values.len() - old.map_or(0, |data| data.values.len()),
            annotations_added: pack.data.annotations.len()
                - old.map_or(0, |data| data.annotations.len()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Serialize)]
pub struct NotAppliedRecord {
    pub collection: &'static str,
    pub id: String,
    pub reason: &'static str,
}

#[derive(Debug)]
pub struct ImportUpdateResult {
    pub pack: Pack,
    pub counts: ImportCounts,
    pub not_applied: Vec<NotAppliedRecord>,
}

fn selector_key(record: &Value) -> Option<(Option<&str>, Option<&str>)> {
    let selectors = record["selectors"].as_object()?;
    Some((
        selectors.get("repo").and_then(Value::as_str),
        selectors.get("type").and_then(Value::as_str),
    ))
}

fn scoped(record: &Value) -> bool {
    selector_key(record).is_some_and(|(repo, ty)| repo.is_some() || ty.is_some())
}

pub fn validate_update_input(pack: &Pack) -> Result<(), Diagnostic> {
    let mut identities = BTreeMap::new();
    for record in &pack.data.fields {
        let (source, path, tag) = record_field_parts(record);
        if let Some(previous) = identities.insert(field_identity(source, path, tag), id(record)) {
            return Err(invalid(&format!(
                "Duplicate incoming fields {:?} and {previous:?} for source {source:?}.",
                id(record)
            )));
        }
    }
    Ok(())
}

fn record_indices(records: &[Value]) -> BTreeMap<&str, usize> {
    records
        .iter()
        .enumerate()
        .map(|(i, v)| (id(v), i))
        .collect()
}

fn imported_id(
    collection: &str,
    prefix: &str,
    record: &Value,
    used: &mut BTreeSet<String>,
    count: &mut usize,
    identical: impl Fn(&str) -> bool,
) -> String {
    for counter in 0u64.. {
        let bytes = serde_json::to_vec(&(collection, id(record), record, counter))
            .expect("validated JSON records are serializable");
        let hash = sha256_hex(&bytes);
        let id = format!("{prefix}:import:{}", &hash[..16]);
        if used.insert(id.clone()) || identical(&id) {
            *count += 1;
            return id;
        }
    }
    unreachable!("finite pack record IDs cannot exhaust the import ID space")
}

fn discovery_proof_class(record: &Value) -> Option<&'static str> {
    let class = match record["canonical_source"].as_str()? {
        "CrowdStrike Falcon MCP falcon_search_ngsiem (agent-relayed)" => "prov:mcp",
        "Falcon NG-SIEM search API" => "prov:api",
        _ => return None,
    };
    let tail = id(record).strip_prefix(class)?.strip_prefix(':')?;
    let hash = |s: &str| {
        s.len() == 16
            && s.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let valid = match tail.split(':').collect::<Vec<_>>().as_slice() {
        ["import", digest] => hash(digest),
        [clock, digest, kind, index] => {
            let bytes = clock.as_bytes();
            bytes.len() >= 16
                && bytes[..8].iter().all(u8::is_ascii_digit)
                && matches!(bytes[8], b'T' | b't')
                && bytes[9..15].iter().all(u8::is_ascii_digit)
                && (bytes[15..].iter().all(u8::is_ascii_digit)
                    || matches!(bytes.last(), Some(b'Z' | b'z'))
                        && bytes[15..bytes.len() - 1].iter().all(u8::is_ascii_digit))
                && hash(digest)
                && !kind.is_empty()
                && !index.is_empty()
                && index.bytes().all(|b| b.is_ascii_digit())
        }
        _ => false,
    };
    valid.then_some(class)
}

fn value_identity<'a>(field: &'a str, record: &'a Value) -> (&'a str, &'a str, String) {
    (
        field,
        record["value_kind"].as_str().unwrap(),
        record["value"].to_string(),
    )
}

struct ImportIds<'a> {
    sources: BTreeMap<&'a str, String>,
    fields: BTreeMap<&'a str, String>,
    provenance: BTreeMap<&'a str, String>,
    field_matches: BTreeMap<&'a str, Vec<usize>>,
    retained_ids: BTreeSet<&'a str>,
    used: BTreeSet<String>,
    reidentified: Reidentified,
}

fn import_ids<'a>(old: &'a PackData, fresh: &'a PackData) -> Result<ImportIds<'a>, Diagnostic> {
    let source_indices = record_indices(&old.sources);
    let selector_indices: BTreeMap<_, _> = old
        .sources
        .iter()
        .filter_map(|v| Some((selector_key(v)?, id(v))))
        .collect();
    let retained_ids: BTreeSet<_> = [&old.sources, &old.fields, &old.values, &old.provenance]
        .into_iter()
        .flatten()
        .map(id)
        .collect();
    // Reserve incoming IDs too so a generated ID cannot take an unmatched record's ID.
    let mut used = retained_ids
        .iter()
        .copied()
        .chain(
            [
                &fresh.sources,
                &fresh.fields,
                &fresh.values,
                &fresh.provenance,
            ]
            .into_iter()
            .flatten()
            .map(id),
        )
        .map(str::to_owned)
        .collect();
    let mut counts = Reidentified::default();
    let mut sources = BTreeMap::new();
    let mut claimed_sources = BTreeSet::new();
    for record in &fresh.sources {
        let original = id(record);
        let same_id = source_indices.get(original).map(|&i| &old.sources[i]);
        let key = selector_key(record);
        let matched = match key {
            Some(key) => selector_indices.get(&key).copied(),
            None => same_id.filter(|v| selector_key(v).is_none()).map(id),
        };
        if let Some(prior) = same_id {
            let conflict = if key.is_none() && scoped(prior) {
                Some(id(prior))
            } else {
                matched.filter(|target| scoped(record) && !scoped(prior) && *target != original)
            };
            if let Some(target) = conflict {
                return Err(invalid(&format!(
                    "Incoming source {original:?} has an ambiguous identity with retained source {target:?}."
                )));
            }
        }
        let target = if let Some(target) = matched {
            target.to_owned()
        } else if retained_ids.contains(original) {
            let prefix = original
                .splitn(3, ':')
                .take(2)
                .collect::<Vec<_>>()
                .join(":");
            imported_id(
                "sources",
                &prefix,
                record,
                &mut used,
                &mut counts.sources,
                |candidate| {
                    key.is_none()
                        && !claimed_sources.contains(candidate)
                        && source_indices
                            .get(candidate)
                            .is_some_and(|&i| selector_key(&old.sources[i]).is_none())
                },
            )
        } else {
            original.to_owned()
        };
        claimed_sources.insert(target.clone());
        sources.insert(original, target);
    }

    let mut identities = BTreeMap::<_, Vec<usize>>::new();
    for (index, record) in old.fields.iter().enumerate() {
        let (source, path, tag) = record_field_parts(record);
        identities
            .entry(field_identity(source, path, tag))
            .or_default()
            .push(index);
    }
    let mut fields = BTreeMap::new();
    let mut field_matches = BTreeMap::new();
    for record in &fresh.fields {
        let original = id(record);
        let (source, path, tag) = record_field_parts(record);
        let identity = field_identity(&sources[source], path, tag);
        let target = if let Some(matches) = identities.get(&identity) {
            field_matches.insert(original, matches.clone());
            id(&old.fields[matches[0]]).to_owned()
        } else if retained_ids.contains(original) {
            imported_id(
                "fields",
                "env:field",
                record,
                &mut used,
                &mut counts.fields,
                |_| false,
            )
        } else {
            original.to_owned()
        };
        fields.insert(original, target);
    }

    let proofs = record_indices(&old.provenance);
    let mut provenance = BTreeMap::new();
    for record in &fresh.provenance {
        let original = id(record);
        let target = if proofs
            .get(original)
            .is_some_and(|&i| old.provenance[i] != *record)
        {
            imported_id(
                "provenance",
                discovery_proof_class(record).unwrap_or("prov"),
                record,
                &mut used,
                &mut counts.provenance,
                |id| {
                    proofs.get(id).is_some_and(|&index| {
                        let mut rewritten = record.clone();
                        rewritten["id"] = Value::String(id.into());
                        old.provenance[index] == rewritten
                    })
                },
            )
        } else {
            original.to_owned()
        };
        provenance.insert(original, target);
    }

    Ok(ImportIds {
        sources,
        fields,
        provenance,
        field_matches,
        retained_ids,
        used,
        reidentified: counts,
    })
}

fn match_value_ids<'a>(
    retained_values: &[Value],
    incoming_values: &'a [Value],
    fields: &BTreeMap<&'a str, String>,
    retained_ids: &BTreeSet<&str>,
    used: &mut BTreeSet<String>,
    reidentified_values: &mut usize,
) -> BTreeMap<&'a str, String> {
    let value_indices = record_indices(retained_values);
    let mut value_ids = BTreeMap::new();
    let mut claimed = BTreeSet::new();

    // Pass 1: reserve exact-ID matches first so later records cannot overwrite them.
    for record in incoming_values {
        let original = id(record);
        let key = value_identity(
            fields[record["field_id"].as_str().unwrap()].as_str(),
            record,
        );
        if value_indices.get(original).is_some_and(|&i| {
            value_identity(
                retained_values[i]["field_id"].as_str().unwrap(),
                &retained_values[i],
            ) == key
        }) {
            claimed.insert(original.to_owned());
            value_ids.insert(original, original.to_owned());
        }
    }

    // Pass 2: unique key fallback only when one unclaimed retained value matches one
    // remaining incoming value, keeping assignment order-independent.
    let mut retained_by_key = BTreeMap::new();
    for record in retained_values {
        let rid = id(record);
        if claimed.contains(rid) {
            continue;
        }
        let key = value_identity(record["field_id"].as_str().unwrap(), record);
        retained_by_key
            .entry(key)
            .and_modify(|slot| *slot = None)
            .or_insert(Some(rid));
    }
    let mut incoming_by_key = BTreeMap::<_, usize>::new();
    for record in incoming_values {
        if value_ids.contains_key(id(record)) {
            continue;
        }
        let key = value_identity(
            fields[record["field_id"].as_str().unwrap()].as_str(),
            record,
        );
        *incoming_by_key.entry(key).or_default() += 1;
    }
    for record in incoming_values {
        let original = id(record);
        if value_ids.contains_key(original) {
            continue;
        }
        let key = value_identity(
            fields[record["field_id"].as_str().unwrap()].as_str(),
            record,
        );
        if incoming_by_key.get(&key) == Some(&1)
            && let Some(Some(target)) = retained_by_key.get(&key)
        {
            claimed.insert((*target).to_owned());
            value_ids.insert(original, (*target).to_owned());
        }
    }

    // Pass 3: add remaining values, reusing a prior collision re-ID on identical replay.
    for record in incoming_values {
        let original = id(record);
        if value_ids.contains_key(original) {
            continue;
        }
        let key = value_identity(
            fields[record["field_id"].as_str().unwrap()].as_str(),
            record,
        );
        let target = if retained_ids.contains(original) {
            imported_id(
                "values",
                "env:value",
                record,
                used,
                reidentified_values,
                |candidate| {
                    !claimed.contains(candidate)
                        && value_indices.get(candidate).is_some_and(|&i| {
                            value_identity(
                                retained_values[i]["field_id"].as_str().unwrap(),
                                &retained_values[i],
                            ) == key
                        })
                },
            )
        } else {
            original.to_owned()
        };
        if value_indices.contains_key(target.as_str()) {
            claimed.insert(target.clone());
        }
        value_ids.insert(original, target);
    }

    value_ids
}

fn rewrite_proof(record: &mut Value, ids: &BTreeMap<&str, String>) {
    for proof in record["provenance_ids"].as_array_mut().unwrap() {
        *proof = Value::String(ids[proof.as_str().unwrap()].clone());
    }
}

fn discovery_sources(data: &PackData) -> BTreeSet<&str> {
    let proofs: BTreeSet<_> = data
        .provenance
        .iter()
        .filter(|v| discovery_proof_class(v).is_some())
        .map(id)
        .collect();
    let has_discovery = |v: &Value| {
        v["provenance_ids"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| proofs.contains(p.as_str().unwrap()))
    };
    let field_sources: BTreeMap<_, _> = data
        .fields
        .iter()
        .map(|v| (id(v), v["source_id"].as_str().unwrap()))
        .collect();
    data.sources
        .iter()
        .filter(|v| has_discovery(v))
        .map(id)
        .chain(
            data.fields
                .iter()
                .filter(|v| has_discovery(v))
                .map(|v| v["source_id"].as_str().unwrap()),
        )
        .chain(
            data.values
                .iter()
                .filter(|v| has_discovery(v))
                .map(|v| field_sources[v["field_id"].as_str().unwrap()]),
        )
        .collect()
}

#[derive(Clone, Copy)]
enum UpdatedRecord {
    Source,
    Field,
}

impl UpdatedRecord {
    fn collection(self) -> &'static str {
        match self {
            Self::Source => "sources",
            Self::Field => "fields",
        }
    }
}

struct UpdateObservation {
    key: Option<MeasurementKey>,
    observed: Option<bool>,
    stale: bool,
}

fn update_observation(
    pack: &Pack,
    kind: UpdatedRecord,
    record: &Value,
    created: time::OffsetDateTime,
) -> Result<UpdateObservation, Diagnostic> {
    Ok(match kind {
        UpdatedRecord::Source => {
            let record = source(record)?;
            let stale =
                record.inventory_status == "stale" || linked_stale(pack, &record.provenance_ids);
            let observed = record.evidence.as_ref().and_then(|e| e.observed);
            let key = if !stale
                && matches!(
                    record.inventory_status.as_str(),
                    "observed-only" | "reconciled"
                ) {
                source_measurement_at(pack, &record, created)
            } else {
                None
            };
            UpdateObservation {
                key,
                observed,
                stale,
            }
        }
        UpdatedRecord::Field => {
            let record = field(record)?;
            let stale =
                record.refresh_status.is_some() || linked_stale(pack, &record.provenance_ids);
            let key = field_measurement_at(pack, &record, created).map(FieldMeasurement::key);
            let observed = record.coverage.and_then(|c| c.observed);
            UpdateObservation {
                key,
                observed,
                stale,
            }
        }
    })
}

/// The measurement key exposed to existing pack readers (`source_measurement` / `field_measurement`).
fn pack_reader_measurement(
    pack: &Pack,
    kind: UpdatedRecord,
    record: &Value,
    created: time::OffsetDateTime,
) -> Result<Option<MeasurementKey>, Diagnostic> {
    Ok(match kind {
        UpdatedRecord::Source => source_measurement_at(pack, &source(record)?, created),
        UpdatedRecord::Field => {
            field_measurement_at(pack, &field(record)?, created).map(FieldMeasurement::key)
        }
    })
}

/// Latest qualifying own-clock deployment retrieval linked by an applied value observation.
fn value_qualifying_retrieved_at(
    pack: &Pack,
    created: time::OffsetDateTime,
    all_proofs: &[String],
    linked_proofs: &BTreeSet<&str>,
) -> Option<time::OffsetDateTime> {
    if linked_proofs.is_empty() || linked_stale(pack, all_proofs) {
        return None;
    }
    pack.data
        .provenance
        .iter()
        .filter(|value| {
            deployment_provenance(value)
                && linked_proofs.contains(id(value))
                && matches!(
                    value["coverage_status"].as_str(),
                    Some("complete" | "observed-only" | "reconciled")
                )
        })
        .filter_map(|value| {
            let (start, end) = value["observation_window"].as_str()?.split_once('/')?;
            let start = parse_time(start).ok()?;
            let end = parse_time(end).ok()?;
            if start >= end {
                return None;
            }
            let retrieved = parse_time(value["retrieved_at"].as_str()?).ok()?;
            (end <= retrieved && retrieved <= created).then_some(retrieved)
        })
        .max()
}

fn strip_post_clock_incoming_proofs(
    fresh: &mut Value,
    original: &Value,
    post_clock_incoming: &BTreeSet<&str>,
    ids: &BTreeMap<&str, String>,
) {
    if post_clock_incoming.is_empty() {
        return;
    }
    let allowed: BTreeSet<&str> = original["provenance_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap())
        .filter(|p| !post_clock_incoming.contains(*p))
        .map(|p| ids[p].as_str())
        .collect();
    let incoming_all: BTreeSet<&str> = original["provenance_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| ids[p.as_str().unwrap()].as_str())
        .collect();
    fresh["provenance_ids"].as_array_mut().unwrap().retain(|p| {
        let pid = p.as_str().unwrap();
        !incoming_all.contains(pid) || allowed.contains(pid)
    });
}

fn keep_observation(
    incoming: &UpdateObservation,
    retained: &UpdateObservation,
    old_clock: time::OffsetDateTime,
) -> Option<&'static str> {
    let Some(key) = incoming.key else {
        return Some("unqualified-incoming");
    };
    if let Some(previous) = retained.key {
        return match key.cmp(&previous) {
            std::cmp::Ordering::Greater => None,
            std::cmp::Ordering::Less => Some("older-observation"),
            std::cmp::Ordering::Equal => Some("tied-observation"),
        };
    }
    if retained.stale || retained.observed == Some(false) {
        return (key.1 <= old_clock).then_some("unorderable-against-negative");
    }
    (retained.observed == Some(true) && incoming.observed == Some(false))
        .then_some("unorderable-contradiction")
}

fn copy_key(to: &mut Value, from: &Value, key: &str) {
    let to = to.as_object_mut().unwrap();
    if let Some(value) = from.get(key) {
        to.insert(key.to_owned(), value.clone());
    } else {
        to.remove(key);
    }
}

fn set_parent(record: &mut Value, parent: &Value) {
    if !parent.is_null() {
        record["relationship"]["parent_repo_id"] = parent.clone();
    } else if let Some(relationship) = record["relationship"].as_object_mut() {
        relationship.remove("parent_repo_id");
        if relationship.is_empty() {
            record.as_object_mut().unwrap().remove("relationship");
        }
    }
}

fn union_proof(record: &mut Value, incoming: impl IntoIterator<Item = Value>) {
    let ids = record["provenance_ids"].as_array_mut().unwrap();
    for value in incoming {
        if !ids.contains(&value) {
            ids.push(value);
        }
    }
}

fn retained_proof<'a>(
    record: &'a Value,
    deployment: &'a BTreeSet<&str>,
) -> impl Iterator<Item = Value> + 'a {
    record["provenance_ids"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|value| !deployment.contains(value.as_str().unwrap()))
        .cloned()
}

const SELECTOR_KEPT: &str = "discovery-evidence-selector-kept";

fn not_applied(
    entries: &mut Vec<NotAppliedRecord>,
    collection: &'static str,
    id: &str,
    reason: &'static str,
) {
    entries.push(NotAppliedRecord {
        collection,
        id: id.to_owned(),
        reason,
    });
}

type AddedObservation<'a> = (
    UpdatedRecord,
    &'a Value,
    &'a str,
    Option<MeasurementKey>,
    Option<MeasurementKey>,
    bool,
);

struct UpdateClockInputs<'a> {
    applied: &'a BTreeMap<&'a str, MeasurementKey>,
    folded: &'a BTreeMap<&'a str, &'a str>,
    additions: &'a [AddedObservation<'a>],
    value_clocks: &'a [(&'a str, time::OffsetDateTime)],
}

fn protect_update_clock(
    old: &Pack,
    candidate: &Pack,
    data: &mut PackData,
    inputs: &UpdateClockInputs<'_>,
    skipped: &mut Vec<NotAppliedRecord>,
) -> Result<(time::OffsetDateTime, BTreeSet<String>), Diagnostic> {
    let retained = &old.data;
    let old_clock = parse_time(&old.created_at)?;
    let mut children = BTreeMap::<_, Vec<_>>::new();
    for record in &data.sources[retained.sources.len()..] {
        if let Some(parent) = record["relationship"]["parent_repo_id"].as_str() {
            children.entry(parent).or_default().push(id(record));
        }
    }
    let compute_clock = |blocked: &BTreeSet<String>| {
        inputs
            .applied
            .values()
            .map(|key| key.2)
            .chain(
                inputs
                    .additions
                    .iter()
                    .filter_map(|(_, _, target, key, _, _)| {
                        key.filter(|_| !blocked.contains(*target)).map(|key| key.2)
                    }),
            )
            .chain(
                inputs
                    .value_clocks
                    .iter()
                    .filter(|(field_target, _)| !blocked.contains(*field_target))
                    .map(|(_, retrieved_at)| *retrieved_at),
            )
            .fold(old_clock, std::cmp::max)
    };
    let mut date_blocked = BTreeSet::<String>::new();
    let mut clock = compute_clock(&date_blocked);
    loop {
        let mut next_blocked = BTreeSet::<String>::new();
        for &(kind, record, target, key, raw_key, stripped) in inputs.additions {
            if stripped {
                continue;
            }
            if update_observation(candidate, kind, record, clock)?.key != key
                || pack_reader_measurement(candidate, kind, record, clock)? != raw_key
            {
                next_blocked.insert(target.to_owned());
            }
        }
        // Omit dependent new records as well, so refusing a date cannot leave dangling references.
        let mut queue: Vec<_> = next_blocked.iter().cloned().collect();
        while let Some(parent) = queue.pop() {
            for &child in children.get(parent.as_str()).into_iter().flatten() {
                if next_blocked.insert(child.to_owned()) {
                    queue.push(child.to_owned());
                }
            }
        }
        for record in &data.fields[retained.fields.len()..] {
            if next_blocked.contains(record["source_id"].as_str().unwrap()) {
                next_blocked.insert(id(record).to_owned());
            }
        }
        let next_clock = compute_clock(&next_blocked);
        date_blocked = next_blocked;
        if next_clock == clock {
            break;
        }
        clock = next_clock;
    }
    for (kind, record, target, _, _, _) in inputs.additions {
        if date_blocked.contains(*target) {
            not_applied(skipped, kind.collection(), id(record), "date-inconsistent");
        }
    }
    for (prior, record) in retained.sources.iter().zip(&mut data.sources) {
        if record["relationship"]["parent_repo_id"]
            .as_str()
            .is_some_and(|p| date_blocked.contains(p))
        {
            set_parent(record, &prior["relationship"]["parent_repo_id"]);
        }
    }
    if clock > old_clock {
        for (kind, records) in [
            (UpdatedRecord::Source, &retained.sources),
            (UpdatedRecord::Field, &retained.fields),
        ] {
            for record in records {
                if inputs.applied.contains_key(id(record)) || inputs.folded.contains_key(id(record))
                {
                    continue;
                }
                let before_obs = update_observation(old, kind, record, old_clock)?.key;
                let after_obs = update_observation(old, kind, record, clock)?.key;
                let before_raw = pack_reader_measurement(old, kind, record, old_clock)?;
                let after_raw = pack_reader_measurement(old, kind, record, clock)?;
                if after_obs != before_obs || after_raw != before_raw {
                    let (_, _, retrieved_at) = after_obs
                        .filter(|_| after_obs != before_obs)
                        .or(after_raw)
                        .expect("a changed measurement key under a later clock is present");
                    let retrieved_at = retrieved_at
                        .format(&time::format_description::well_known::Rfc3339)
                        .map_err(|_| invalid("Cannot represent the retained retrieval time."))?;
                    return Err(invalid(&format!(
                        "Retained {} record {:?} has retrieved_at {retrieved_at} after its pack created_at {}; an update would newly qualify or strengthen it.",
                        kind.collection(),
                        id(record),
                        old.created_at,
                    )));
                }
            }
        }
    }
    Ok((clock, date_blocked))
}

/// Update an inventory snapshot without attaching an observation to a different search scope.
pub fn import_update(
    old: &Pack,
    candidate: &Pack,
    discovery_measured_sources: &BTreeSet<String>,
) -> Result<ImportUpdateResult, Diagnostic> {
    validate_import_pair(old, candidate)?;
    validate_update_input(candidate)?;
    let retained = &old.data;
    let incoming = &candidate.data;
    let mut ids = import_ids(retained, incoming)?;
    let old_clock = parse_time(&old.created_at)?;
    let input_clock = parse_time(&candidate.created_at)?;
    let post_clock_incoming: BTreeSet<_> = incoming
        .provenance
        .iter()
        .filter(|v| {
            deployment_provenance(v)
                && v["retrieved_at"]
                    .as_str()
                    .and_then(|s| parse_time(s).ok())
                    .is_some_and(|retrieved| retrieved > input_clock)
        })
        .map(id)
        .collect();
    let mut data = retained.clone();
    let mut skipped = Vec::new();
    let mut applied = BTreeMap::new();
    let mut additions = Vec::new();
    let deployment: BTreeSet<_> = retained
        .provenance
        .iter()
        .filter(|v| deployment_provenance(v))
        .map(id)
        .collect();
    let old_sources = record_indices(&retained.sources);
    let mut measured = discovery_sources(retained);
    measured.extend(
        discovery_sources(incoming)
            .into_iter()
            .map(|id| ids.sources[id].as_str()),
    );
    let mut restricted = BTreeSet::new();

    for record in &incoming.sources {
        let original = id(record);
        let target = ids.sources[original].as_str();
        let mut fresh = record.clone();
        fresh["id"] = Value::String(target.into());
        if let Some(parent) = record["relationship"]["parent_repo_id"].as_str() {
            set_parent(&mut fresh, &Value::String(ids.sources[parent].clone()));
        }
        rewrite_proof(&mut fresh, &ids.provenance);
        let kind = UpdatedRecord::Source;
        let Some(&index) = old_sources.get(target) else {
            let measurement = update_observation(candidate, kind, record, input_clock)?;
            let raw_key = pack_reader_measurement(candidate, kind, record, input_clock)?;
            let stripped = measurement.key.is_some() || raw_key.is_some();
            if stripped {
                strip_post_clock_incoming_proofs(
                    &mut fresh,
                    record,
                    &post_clock_incoming,
                    &ids.provenance,
                );
            }
            data.sources.push(fresh);
            additions.push((kind, record, target, measurement.key, raw_key, stripped));
            continue;
        };

        let prior = &retained.sources[index];
        let previous = source(prior)?;
        let old_selector = previous.selectors.unwrap_or_default();
        let new_selector = source(record)?.selectors.unwrap_or_default();
        let scope = crate::contracts::Scope {
            repo: old_selector.repo.clone(),
            type_: old_selector.type_.clone(),
            event: None,
        };
        let discovery_measured = measured.contains(target)
            || discovery_measured_sources.contains(&crate::discovery::source_id(&scope))
            || previous
                .evidence
                .as_ref()
                .is_some_and(|e| e.measured_fields.is_some() || e.discovery_depth.is_some());
        let metadata = &mut data.sources[index];
        for key in ["name", "description"] {
            if !fresh[key].is_null() {
                copy_key(metadata, &fresh, key);
            }
        }
        if discovery_measured && !compatible_selector_restrictions(&old_selector, &new_selector) {
            restricted.insert(target);
            not_applied(&mut skipped, "sources", original, SELECTOR_KEPT);
            continue;
        }
        if !discovery_measured {
            copy_key(metadata, &fresh, "selectors");
        }
        if !fresh["relationship"]["parent_repo_id"].is_null() {
            let parent = fresh["relationship"]["parent_repo_id"].clone();
            set_parent(metadata, &parent);
        } else if !metadata["relationship"]["parent_repo_id"].is_null() {
            set_parent(&mut fresh, &metadata["relationship"]["parent_repo_id"]);
        }
        for key in ["name", "description", "selectors", "kind"] {
            copy_key(&mut fresh, metadata, key);
        }
        union_proof(&mut fresh, retained_proof(prior, &deployment));
        if fresh == *metadata {
            continue;
        }
        let measurement = update_observation(candidate, kind, record, input_clock)?;
        let previous = update_observation(old, kind, prior, old_clock)?;
        if let Some(reason) = keep_observation(&measurement, &previous, old_clock) {
            not_applied(&mut skipped, "sources", original, reason);
        } else {
            strip_post_clock_incoming_proofs(
                &mut fresh,
                record,
                &post_clock_incoming,
                &ids.provenance,
            );
            *metadata = fresh;
            applied.insert(target, measurement.key.unwrap());
        }
    }

    let mut folded = BTreeMap::new();
    for record in &incoming.fields {
        let original = id(record);
        let target = ids.fields[original].as_str();
        let source_id = ids.sources[record["source_id"].as_str().unwrap()].as_str();
        let mut fresh = record.clone();
        fresh["id"] = Value::String(target.into());
        fresh["source_id"] = Value::String(source_id.into());
        rewrite_proof(&mut fresh, &ids.provenance);
        let matches = ids.field_matches.get(original);
        if restricted.contains(source_id) {
            if matches.is_none_or(|indices| retained.fields[indices[0]] != fresh) {
                not_applied(&mut skipped, "fields", original, SELECTOR_KEPT);
            }
            continue;
        }
        let kind = UpdatedRecord::Field;
        let Some(matches) = matches else {
            let measurement = update_observation(candidate, kind, record, input_clock)?;
            let raw_key = pack_reader_measurement(candidate, kind, record, input_clock)?;
            let stripped = measurement.key.is_some() || raw_key.is_some();
            if stripped {
                strip_post_clock_incoming_proofs(
                    &mut fresh,
                    record,
                    &post_clock_incoming,
                    &ids.provenance,
                );
            }
            data.fields.push(fresh);
            additions.push((kind, record, target, measurement.key, raw_key, stripped));
            continue;
        };
        let first = matches[0];
        let metadata = &mut data.fields[first];
        for key in ["description", "schema_variant"] {
            if !fresh[key].is_null() {
                copy_key(metadata, &fresh, key);
            }
            copy_key(&mut fresh, metadata, key);
        }
        if metadata["data_type"] == "unknown" && fresh["data_type"] != "array" {
            copy_key(metadata, &fresh, "data_type");
        }
        if fresh["data_type"] == "unknown" && metadata["data_type"] != "array" {
            copy_key(&mut fresh, metadata, "data_type");
        }
        union_proof(
            &mut fresh,
            retained_proof(&retained.fields[first], &deployment),
        );
        if fresh == *metadata {
            continue;
        }
        let measurement = update_observation(candidate, kind, record, input_clock)?;
        let mut reason = None;
        for &index in matches {
            let previous = update_observation(old, kind, &retained.fields[index], old_clock)?;
            reason = keep_observation(&measurement, &previous, old_clock);
            if reason.is_some() {
                break;
            }
        }
        if let Some(reason) = reason {
            not_applied(&mut skipped, "fields", original, reason);
            continue;
        }
        for &index in &matches[1..] {
            let prior = &retained.fields[index];
            union_proof(&mut fresh, retained_proof(prior, &deployment));
        }
        for &index in &matches[1..] {
            folded.insert(id(&retained.fields[index]), target);
        }
        fresh.as_object_mut().unwrap().remove("refresh_status");
        strip_post_clock_incoming_proofs(&mut fresh, record, &post_clock_incoming, &ids.provenance);
        *metadata = fresh;
        applied.insert(target, measurement.key.unwrap());
    }

    // Rewrite retained value field links for folded fields before indexing and matching values.
    for record in &mut data.values {
        if let Some(&survivor) = folded.get(record["field_id"].as_str().unwrap()) {
            record["field_id"] = Value::String(survivor.into());
        }
    }
    let value_ids = match_value_ids(
        &data.values,
        &incoming.values,
        &ids.fields,
        &ids.retained_ids,
        &mut ids.used,
        &mut ids.reidentified.values,
    );
    let old_values: BTreeMap<String, usize> = data
        .values
        .iter()
        .enumerate()
        .map(|(i, v)| (id(v).to_owned(), i))
        .collect();
    let incoming_field_indices = record_indices(&incoming.fields);
    let mut value_clocks = Vec::new();
    for record in &incoming.values {
        let original = id(record);
        let field = record["field_id"].as_str().unwrap();
        let field_target = ids.fields[field].as_str();
        let field_record = &incoming.fields[incoming_field_indices[field]];
        let source = ids.sources[field_record["source_id"].as_str().unwrap()].as_str();
        if restricted.contains(source) {
            continue;
        }
        let target = value_ids[original].as_str();
        let all_proofs: Vec<String> = record["provenance_ids"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p.as_str().unwrap().to_owned())
            .collect();
        let newly_linked: BTreeSet<&str> = if let Some(&i) = old_values.get(target) {
            let existing: BTreeSet<&str> = data.values[i]["provenance_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap())
                .collect();
            record["provenance_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap())
                .filter(|p| !existing.contains(ids.provenance[*p].as_str()))
                .collect()
        } else {
            record["provenance_ids"]
                .as_array()
                .unwrap()
                .iter()
                .map(|p| p.as_str().unwrap())
                .collect()
        };
        if let Some(retrieved_at) =
            value_qualifying_retrieved_at(candidate, input_clock, &all_proofs, &newly_linked)
        {
            value_clocks.push((field_target, retrieved_at));
        }
    }

    let clock_inputs = UpdateClockInputs {
        applied: &applied,
        folded: &folded,
        additions: &additions,
        value_clocks: &value_clocks,
    };
    let (clock, date_blocked) =
        protect_update_clock(old, candidate, &mut data, &clock_inputs, &mut skipped)?;

    for record in &incoming.values {
        let original = id(record);
        let target = value_ids[original].as_str();
        let field = record["field_id"].as_str().unwrap();
        let field_record = &incoming.fields[incoming_field_indices[field]];
        let source = ids.sources[field_record["source_id"].as_str().unwrap()].as_str();
        let mut fresh = record.clone();
        fresh["id"] = Value::String(target.into());
        fresh["field_id"] = Value::String(ids.fields[field].clone());
        rewrite_proof(&mut fresh, &ids.provenance);
        let index = old_values.get(target).copied();
        let reason = if restricted.contains(source) {
            Some(SELECTOR_KEPT)
        } else {
            date_blocked
                .contains(ids.fields[field].as_str())
                .then_some("date-inconsistent")
        };
        if let Some(reason) = reason {
            if index.is_none_or(|i| data.values[i] != fresh) {
                not_applied(&mut skipped, "values", original, reason);
            }
        } else if let Some(i) = index {
            for key in ["meaning", "sanitized"] {
                if !fresh[key].is_null() {
                    copy_key(&mut data.values[i], &fresh, key);
                }
            }
            union_proof(
                &mut data.values[i],
                fresh["provenance_ids"].as_array().unwrap().iter().cloned(),
            );
        } else {
            data.values.push(fresh);
        }
    }

    let old_annotations: BTreeMap<_, _> = retained
        .annotations
        .iter()
        .map(|v| (v["source_id"].as_str().unwrap(), v))
        .collect();
    for record in &incoming.annotations {
        let original = record["source_id"].as_str().unwrap();
        let target = ids.sources[original].as_str();
        let mut fresh = record.clone();
        fresh["source_id"] = Value::String(target.into());
        let prior = old_annotations.get(target);
        if prior.is_some_and(|v| **v == fresh) {
            continue;
        }
        if restricted.contains(target) {
            not_applied(&mut skipped, "annotations", original, SELECTOR_KEPT);
        } else if date_blocked.contains(target) {
            not_applied(&mut skipped, "annotations", original, "date-inconsistent");
        } else if prior.is_some() {
            not_applied(
                &mut skipped,
                "annotations",
                original,
                "retained-annotation-kept",
            );
        } else {
            data.annotations.push(fresh);
        }
    }

    let mut counts = ImportCounts {
        reidentified: ids.reidentified,
        sources_added: data.sources[retained.sources.len()..]
            .iter()
            .filter(|v| !date_blocked.contains(id(v)))
            .count(),
        fields_added: data.fields[retained.fields.len()..]
            .iter()
            .filter(|v| !date_blocked.contains(id(v)))
            .count(),
        values_updated: retained
            .values
            .iter()
            .zip(&data.values)
            .filter(|(a, b)| a != b)
            .count(),
        values_added: data.values.len() - retained.values.len(),
        annotations_added: data.annotations.len() - retained.annotations.len(),
        ..ImportCounts::default()
    };
    for (incoming, indices, before, after, updated, unchanged) in [
        (
            &ids.sources,
            &old_sources,
            &retained.sources,
            &data.sources,
            &mut counts.sources_updated,
            &mut counts.sources_unchanged,
        ),
        (
            &ids.fields,
            &record_indices(&retained.fields),
            &retained.fields,
            &data.fields,
            &mut counts.fields_updated,
            &mut counts.fields_unchanged,
        ),
    ] {
        for target in incoming.values() {
            if let Some(&index) = indices.get(target.as_str()) {
                if before[index] == after[index] {
                    *unchanged += 1;
                } else {
                    *updated += 1;
                }
            }
        }
    }
    data.sources.retain(|v| !date_blocked.contains(id(v)));
    data.fields
        .retain(|v| !folded.contains_key(id(v)) && !date_blocked.contains(id(v)));
    let referenced: BTreeSet<_> = [&data.sources, &data.fields, &data.values]
        .into_iter()
        .flatten()
        .flat_map(|v| v["provenance_ids"].as_array().unwrap())
        .map(|p| p.as_str().unwrap())
        .collect();
    let old_proofs = record_indices(&retained.provenance);
    for record in &incoming.provenance {
        let target = ids.provenance[id(record)].as_str();
        if referenced.contains(target) && !old_proofs.contains_key(target) {
            let mut proof = record.clone();
            proof["id"] = Value::String(target.into());
            data.provenance.push(proof);
        }
    }

    let pack = if data == old.data {
        old.clone()
    } else {
        let created_at = if clock == old_clock {
            old.created_at.clone()
        } else {
            clock
                .format(&time::format_description::well_known::Rfc3339)
                .map_err(|_| invalid("Cannot represent the cumulative pack creation time."))?
        };
        Pack::new(old.environment.clone(), created_at, data)?
    };
    Ok(ImportUpdateResult {
        pack,
        counts,
        not_applied: skipped,
    })
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
