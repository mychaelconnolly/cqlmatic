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
pub const KNOWLEDGE_REVISION: &str = "2158f99fc529e234d5fd6fd4d4af899d7d83a848";
const COMPATIBLE_PACKS: &[(u32, &str, &str)] = &[
    (1, "cqlmatic-ngsiem-core-v1", KNOWLEDGE_REVISION),
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

/// Replace only fully measured selected sources. Quiet fields retain their original evidence.
pub fn merge(old: &Pack, candidate: &Pack) -> Result<Pack, Diagnostic> {
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
    let mut replaced = BTreeMap::new();
    let mut field_batches = BTreeMap::new();
    for fresh in &candidate.data.sources {
        let f = source(fresh)?;
        let selector = f.selectors.clone().unwrap_or_default();
        if !matches!(f.inventory_status.as_str(), "reconciled" | "observed-only") {
            return Err(invalid(
                "Only completed source measurements can replace a source.",
            ));
        }
        let previous = data.sources.iter().position(|s| {
            source(s)
                .ok()
                .and_then(|s| s.selectors)
                .is_some_and(|s| s.repo == selector.repo && s.type_ == selector.type_)
        });
        let mut merged = fresh.clone();
        let source_id = previous
            .map(|index| id(&data.sources[index]).to_string())
            .unwrap_or_else(|| f.id.clone());
        if let Some(fields) = f.evidence.as_ref().and_then(|e| e.measured_fields.as_ref()) {
            field_batches.insert(source_id, fields.iter().cloned().collect::<BTreeSet<_>>());
        } else if f
            .evidence
            .as_ref()
            .is_some_and(|e| e.discovery_depth.as_deref() == Some("sources-only"))
        {
            field_batches.insert(source_id, BTreeSet::new());
        }
        if let Some(index) = previous {
            let prior = data.sources[index].clone();
            let old_id = id(&prior).to_string();
            let new_id = f.id.clone();
            replaced.insert(old_id.clone(), new_id.clone());
            merged["id"] = Value::String(old_id.clone());
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
                if let Some(parent) = relationship.get("parent_repo_id") {
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
                if !evidence.contains(p) {
                    evidence.push(p.clone());
                }
            }
            merged["provenance_ids"] = Value::Array(evidence);
            if let Some(selectors) = prior.get("selectors").and_then(Value::as_object) {
                for key in ["event_family", "sourcetype"] {
                    if let Some(v) = selectors.get(key) {
                        merged["selectors"][key] = v.clone();
                    }
                }
            }
            data.sources[index] = merged;
        } else {
            data.sources.push(merged);
        }
    }
    let mut fresh_fields = candidate.data.fields.clone();
    let mut field_ids = BTreeMap::new();
    for f in &mut fresh_fields {
        let fresh = field(f)?;
        let source_id = replaced
            .iter()
            .find(|(_, new)| **new == fresh.source_id)
            .map(|(old, _)| old.clone())
            .unwrap_or(fresh.source_id.clone());
        f["source_id"] = Value::String(source_id.clone());
        if let Some(prior) = data.fields.iter().find(|p| {
            p["source_id"] == source_id
                && p["path"] == fresh.path
                && p.get("is_tag").and_then(Value::as_bool).unwrap_or(false)
                    == fresh.is_tag.unwrap_or(false)
        }) {
            field_ids.insert(fresh.id.clone(), id(prior).to_string());
            if let Some(batch) = field_batches.get(&source_id) {
                let name = if fresh.is_tag == Some(true) {
                    format!("#{}", fresh.path)
                } else {
                    fresh.path.clone()
                };
                if !batch.contains(&name) {
                    *f = prior.clone();
                    continue;
                }
                let observed = fresh
                    .coverage
                    .as_ref()
                    .and_then(|coverage| coverage.observed)
                    .unwrap_or(fresh.evidence_kind == "observed");
                if !observed {
                    f["refresh_status"] = Value::String("not-reobserved".into());
                }
            }
            f["id"] = Value::String(id(prior).into());
            for key in ["schema_variant", "retained_context"] {
                if prior.get(key).is_some() {
                    f[key] = prior[key].clone();
                }
            }
            if let Some(description) = prior
                .get("description")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                && prior.get("retained_context").is_none()
            {
                let dates = prior["provenance_ids"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|pid| data.provenance.iter().find(|p| p["id"] == *pid))
                    .filter_map(|p| p["retrieved_at"].as_str())
                    .map(str::to_string)
                    .collect::<BTreeSet<_>>();
                f["retained_context"] =
                    serde_json::json!({"text":description,"evidence_retrieved_at":dates});
            }
            if fresh.data_type == "unknown"
                && prior["data_type"].as_str().is_some_and(|s| s != "unknown")
            {
                f["data_type"] = prior["data_type"].clone();
            }
            if f["is_array"].is_null() && prior.get("is_array").is_some() {
                f["is_array"] = prior["is_array"].clone();
            }
            let mut evidence = f["provenance_ids"].as_array().cloned().unwrap_or_default();
            for p in prior["provenance_ids"].as_array().into_iter().flatten() {
                if !evidence.contains(p) {
                    evidence.push(p.clone());
                }
            }
            f["provenance_ids"] = Value::Array(evidence);
            if let Some(declared) = prior
                .pointer("/coverage/declared")
                .filter(|_| f.pointer("/coverage/declared").is_none_or(Value::is_null))
            {
                if f.get("coverage").is_none() {
                    f["coverage"] = serde_json::json!({});
                }
                f["coverage"]["declared"] = declared.clone();
            }
        }
    }
    let selected: BTreeSet<String> = candidate
        .data
        .sources
        .iter()
        .map(|s| {
            let fresh = id(s);
            replaced
                .iter()
                .find(|(_, new)| new.as_str() == fresh)
                .map(|(old, _)| old.clone())
                .unwrap_or_else(|| fresh.into())
        })
        .collect();
    for prior in &mut data.fields {
        if selected.contains(prior["source_id"].as_str().unwrap_or("")) {
            if let Some(f) = fresh_fields.iter().find(|f| id(f) == id(prior)) {
                *prior = f.clone();
            } else {
                let source_id = prior["source_id"].as_str().unwrap_or("");
                let tag = prior
                    .get("is_tag")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let path = prior["path"].as_str().unwrap_or("");
                let name = if tag {
                    format!("#{path}")
                } else {
                    path.to_string()
                };
                if field_batches
                    .get(source_id)
                    .is_none_or(|batch| batch.contains(&name))
                {
                    prior["refresh_status"] = Value::String("not-reobserved".into());
                }
            }
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
    for v in &candidate.data.values {
        let mut v = v.clone();
        if let Some(field) = v["field_id"].as_str().and_then(|f| field_ids.get(f)) {
            v["field_id"] = Value::String(field.clone());
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
            && let Some((old, _)) = replaced.iter().find(|(_, new)| new.as_str() == fresh)
        {
            a["source_id"] = Value::String(old.clone());
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
    Pack::new(old.environment.clone(), candidate.created_at.clone(), data)
}
