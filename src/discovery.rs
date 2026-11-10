//! Bounded application use case; pure query planning plus an injected search boundary.
use crate::{
    contracts::*,
    environment::{self, Pack, PackData, ParserContract, Selectors},
    language::{ast::quote, plan},
    network::{CANCELLED, JobPolicy, JobReceipt, SearchBackend, run_job},
    sha256_hex,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

const RECORD_KIND_CANDIDATES: [&str; 5] = [
    "#type",
    "#event.module",
    "#event.kind",
    "event.kind",
    "#event_simpleName",
];
const STRATIFIED_KIND_CANDIDATES: [&str; 6] = [
    "#event_simpleName",
    "#type",
    "#event.kind",
    "event.kind",
    "#event.module",
    "#event.dataset",
];
const MAX_FIELDS_PER_SOURCE: usize = 4000;
const POPULATION_CHUNK: usize = 20;
const KIND_FACET_LIMIT: usize = 50;
const RARE_KIND_EVENTS: u64 = 50_000;
const RARE_KIND_LIMIT: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ContractBinding {
    pub source: Scope,
    pub contract: ParserContract,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FieldSelection {
    pub source: Scope,
    pub fields: Vec<String>,
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum DiscoveryDepth {
    #[default]
    Full,
    SourcesOnly,
}
impl DiscoveryDepth {
    fn is_full(&self) -> bool {
        *self == Self::Full
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DiscoverRequest {
    pub schema_version: u32,
    pub environment: EnvironmentId,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub census: bool,
    #[serde(default, skip_serializing_if = "DiscoveryDepth::is_full")]
    pub depth: DiscoveryDepth,
    #[serde(default)]
    pub sources: Vec<Scope>,
    #[serde(default)]
    pub value_fields: Vec<String>,
    #[serde(default)]
    pub kind_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, schemars(range(min = 1, max = 4000)))]
    pub max_population_fields_per_source: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub population_fields: Vec<FieldSelection>,
    #[serde(default)]
    pub contracts: Vec<ContractBinding>,
    #[serde(default = "query_budget")]
    pub max_queries: u32,
    #[serde(default = "time_budget")]
    pub max_seconds: u64,
}
fn query_budget() -> u32 {
    200
}
fn time_budget() -> u64 {
    900
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Probe {
    pub kind: String,
    pub source: Scope,
    pub query: String,
    pub start: String,
    pub end: String,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub field: Option<String>,
}
pub fn probe_id(view: &str, p: &Probe) -> String {
    probe_id_for(view, &p.query, &p.start, &p.end)
}
pub fn probe_id_for(view: &str, query: &str, start: &str, end: &str) -> String {
    let bytes = serde_json::to_vec(&(view, query, start, end)).expect("probe identity serializes");
    format!("p-{}", &sha256_hex(&bytes)[..16])
}
#[derive(Clone, Copy)]
pub enum Origin {
    Api,
    Mcp,
}
impl Origin {
    fn source_type(self) -> &'static str {
        match self {
            Self::Api => "ngsiem-api",
            Self::Mcp => "mcp-observation",
        }
    }
    fn canonical_source(self) -> &'static str {
        match self {
            Self::Api => "Falcon NG-SIEM search API",
            Self::Mcp => "CrowdStrike Falcon MCP falcon_search_ngsiem (agent-relayed)",
        }
    }
    fn id_prefix(self) -> &'static str {
        match self {
            Self::Api => "prov:api:",
            Self::Mcp => "prov:mcp:",
        }
    }
    fn coverage(self) -> &'static str {
        match self {
            Self::Api => "complete",
            Self::Mcp => "observed-only",
        }
    }
    fn notes(self) -> &'static str {
        match self {
            Self::Api => {
                "Bounded source measurements. Sampled fields/values do not establish an exhaustive deployment inventory or exact ranking."
            }
            Self::Mcp => {
                "Agent-relayed bounded source measurements; cqlmatic did not start or stop the job. The saved MCP result carries no completeness attestation. Sampled fields/values do not establish an exhaustive deployment inventory or exact ranking."
            }
        }
    }
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Capture {
    pub probe: Probe,
    pub retrieved_at: String,
    pub receipt: JobReceipt,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct DiscoverReport {
    pub schema_version: u32,
    pub environment: EnvironmentId,
    pub status: String,
    pub active_pack_updated: bool,
    pub captures: Vec<Capture>,
    pub diagnostics: Vec<Diagnostic>,
}
fn fail(msg: &str) -> Diagnostic {
    Diagnostic::error("DISCOVERY_EVIDENCE", msg)
}
pub fn timestamp() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .expect("UTC formats")
}
fn stamp(t: time::OffsetDateTime) -> String {
    t.format(&time::format_description::well_known::Rfc3339)
        .expect("timestamp formats")
}
pub fn validate_request(r: &DiscoverRequest) -> Result<(), Diagnostic> {
    if r.schema_version != 1 {
        return Err(fail("Unsupported discovery request version."));
    }
    environment::validate_identity(&r.environment)?;
    let start = environment::parse_time(&r.start)?;
    let end = environment::parse_time(&r.end)?;
    if !r.start.ends_with('Z')
        || !r.end.ends_with('Z')
        || start >= end
        || end - start > time::Duration::days(7)
    {
        return Err(fail(
            "Use increasing absolute UTC windows of at most seven days.",
        ));
    }
    if r.max_queries == 0 || r.max_queries > 1000 || r.max_seconds == 0 || r.max_seconds > 3600 {
        return Err(fail("Budgets are 1..1000 queries and 1..3600 seconds."));
    }
    if r.max_population_fields_per_source
        .is_some_and(|limit| !(1..=MAX_FIELDS_PER_SOURCE).contains(&limit))
    {
        return Err(fail("Population field limits are 1..4000 per source."));
    }
    if r.depth == DiscoveryDepth::SourcesOnly
        && (!r.value_fields.is_empty()
            || !r.kind_fields.is_empty()
            || !r.population_fields.is_empty()
            || !r.contracts.is_empty()
            || r.max_population_fields_per_source.is_some())
    {
        return Err(fail(
            "Sources-only discovery does not measure fields, values or parser declarations.",
        ));
    }
    if r.sources.is_empty() && !r.census {
        return Err(fail("Explicitly select sources or set census=true."));
    }
    let mut seen = BTreeSet::new();
    for s in &r.sources {
        validate_scope(s)?;
        if !seen.insert((s.repo.clone(), s.type_.clone())) {
            return Err(fail("Duplicate source selectors."));
        }
    }
    let mut value_fields = BTreeSet::new();
    for f in &r.value_fields {
        plan::checked_field(f)?;
        if !value_fields.insert(f) {
            return Err(fail("Duplicate value field selectors."));
        }
    }
    for f in &r.kind_fields {
        plan::checked_field(f)?;
    }
    let mut selected_sources = BTreeSet::new();
    for selection in &r.population_fields {
        validate_scope(&selection.source)?;
        if !r.sources.contains(&selection.source) {
            return Err(fail(
                "Population fields require an explicitly selected source.",
            ));
        }
        if !selected_sources.insert((&selection.source.repo, &selection.source.type_)) {
            return Err(fail("Duplicate population field source selections."));
        }
        let limit = r
            .max_population_fields_per_source
            .unwrap_or(MAX_FIELDS_PER_SOURCE);
        if selection.fields.is_empty() || selection.fields.len() > limit {
            return Err(fail(
                "Population selections must contain 1..4000 fields and fit the configured source cap.",
            ));
        }
        let mut fields = BTreeSet::new();
        for field in &selection.fields {
            plan::checked_field(field)?;
            if !fields.insert(field) {
                return Err(fail("Duplicate population field selectors."));
            }
        }
    }
    for c in &r.contracts {
        validate_scope(&c.source)?;
        if !r.sources.contains(&c.source) {
            return Err(fail(
                "Parser contracts require an explicitly selected source.",
            ));
        }
    }
    Ok(())
}
fn validate_scope(s: &Scope) -> Result<(), Diagnostic> {
    if (s.repo.is_none() && s.type_.is_none())
        || s.repo.as_ref().is_some_and(|value| value.trim().is_empty())
        || s.type_
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
        || s.event.is_some()
    {
        return Err(fail(
            "Discovery sources require a repository or type selector, without an event selector.",
        ));
    }
    if let Some(r) = &s.repo {
        plan::checked_text(r)?;
    }
    if let Some(t) = &s.type_ {
        plan::checked_text(t)?;
    }
    Ok(())
}
fn scope(s: &Scope) -> Result<String, Diagnostic> {
    Ok(plan::lower(&Plan {
        source: s.clone(),
        filters: vec![],
        steps: vec![],
    })?
    .render())
}
fn probe(
    r: &DiscoverRequest,
    s: &Scope,
    kind: &str,
    suffix: &str,
    sample: bool,
    fields: Vec<String>,
    field: Option<String>,
) -> Result<Probe, Diagnostic> {
    let start = environment::parse_time(&r.start)?;
    let end = environment::parse_time(&r.end)?;
    let query = if kind == "census" {
        suffix.to_string()
    } else {
        format!("{} | {suffix}", scope(s)?)
    };
    let parsed = crate::language::parser::parse(&query).map_err(|e| e.diagnostic)?;
    let checked = crate::language::validate::validate(&parsed, &query);
    if checked.status != Acceptance::Accepted {
        return Err(fail(
            "An internal discovery query did not pass the bundled language profile.",
        ));
    }
    Ok(Probe {
        kind: kind.into(),
        source: s.clone(),
        query,
        start: stamp(if sample {
            start.max(end - time::Duration::minutes(15))
        } else {
            start
        }),
        end: r.end.clone(),
        fields,
        field,
    })
}
pub fn initial_plan(r: &DiscoverRequest) -> Result<Value, Diagnostic> {
    validate_request(r)?;
    let mut probes = Vec::new();
    if r.census {
        probes.push(census_probe(r)?);
    }
    for s in &r.sources {
        probes.extend(base_probes(r, s)?);
    }
    let mut result = json!({"schema_version":1,"environment":r.environment,"initial_probes":probes,"deferred_stages":["base measurements for sources found by explicit census","population chunks of at most 20 fields from fieldset","tag namespace checks","record-kind facets over the full request window","bounded value samples requested by value_fields"],"max_queries":r.max_queries,"max_seconds":r.max_seconds,"is_live":false,"executes_searches":false});
    if r.depth == DiscoveryDepth::SourcesOnly {
        result["depth"] = json!(r.depth);
        result["deferred_stages"] =
            json!(["source volume checks for sources found by explicit census"]);
    }
    if let Some(limit) = r.max_population_fields_per_source {
        result["max_population_fields_per_source"] = json!(limit);
        result["deferred_stages"] = json!([
            "base measurements for sources found by explicit census",
            "tag namespace checks",
            "record-kind facets over the full request window",
            "up to ten literal-filtered rare-kind fieldsets over the full request window",
            "capped population fields: requested, newly discovered, then sampled",
            "bounded value samples requested by value_fields"
        ]);
    }
    if !r.population_fields.is_empty() {
        result["population_fields"] = json!(r.population_fields);
        let stage = if r.max_population_fields_per_source.is_some() {
            4
        } else {
            1
        };
        result["deferred_stages"][stage] = json!(
            "source-bound explicit population batches; sampled fields use the sample window and unsampled fields use the full request window"
        );
    }
    Ok(result)
}
fn census_probe(r: &DiscoverRequest) -> Result<Probe, Diagnostic> {
    probe(
        r,
        &Scope::default(),
        "census",
        "groupBy([#repo, #type, @sourcetype], function=count(as=events), limit=max)",
        false,
        vec![],
        None,
    )
}
fn census_sources(rows: &[Value], sources: &mut Vec<Scope>) -> Result<(), Diagnostic> {
    for row in rows {
        let s = Scope {
            repo: row
                .get("#repo")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            type_: row
                .get("#type")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_string),
            event: None,
        };
        validate_scope(&s)?;
        if !sources.contains(&s) {
            sources.push(s);
        }
    }
    Ok(())
}
fn fieldset_fields(rows: &[Value]) -> Result<Vec<String>, Diagnostic> {
    let mut fields = Vec::new();
    for row in rows {
        let name = ["field", "fieldname", "name", "_field"]
            .iter()
            .find_map(|k| row.get(*k).and_then(Value::as_str))
            .ok_or_else(|| fail("Unrecognized fieldset result shape."))?;
        plan::checked_field(name)?;
        fields.push(name.to_string());
    }
    fields.sort();
    fields.dedup();
    if fields.len() > MAX_FIELDS_PER_SOURCE {
        return Err(fail("Field budget exceeded; narrow the selected source."));
    }
    Ok(fields)
}
fn field_selection<'a>(r: &'a DiscoverRequest, s: &Scope) -> Option<&'a [String]> {
    r.population_fields
        .iter()
        .find(|selection| selection.source == *s)
        .map(|selection| selection.fields.as_slice())
}
fn contract_field<'a>(contract: &'a ParserContract, path: &str, tag: bool) -> Option<&'a str> {
    contract
        .declared_fields
        .iter()
        .find(|field| {
            field.strip_prefix('#').unwrap_or(field) == path
                && (field.starts_with('#') || contract.fields_to_tag.contains(field)) == tag
        })
        .map(String::as_str)
}
#[derive(Clone, Copy)]
enum PopulationWindow {
    Sample,
    Full,
}
fn population_probes_in(
    r: &DiscoverRequest,
    s: &Scope,
    fields: &[String],
    window: PopulationWindow,
) -> Result<Vec<Probe>, Diagnostic> {
    fields
        .chunks(POPULATION_CHUNK)
        .map(|chunk| {
            let args = chunk
                .iter()
                .enumerate()
                .map(|(i, f)| format!("count(field={}, as=c{i})", quote(f)))
                .collect::<Vec<_>>()
                .join(", ");
            probe(
                r,
                s,
                "population",
                &format!("stats([count(as=total), {args}])"),
                matches!(window, PopulationWindow::Sample),
                chunk.to_vec(),
                None,
            )
        })
        .collect()
}
fn record_kind_candidates(r: &DiscoverRequest) -> Vec<&str> {
    let mut seen = BTreeSet::new();
    if r.max_population_fields_per_source.is_some() {
        return r
            .kind_fields
            .iter()
            .map(String::as_str)
            .chain(STRATIFIED_KIND_CANDIDATES)
            .filter(|field| seen.insert(*field))
            .collect();
    }
    RECORD_KIND_CANDIDATES
        .into_iter()
        .chain(r.kind_fields.iter().map(String::as_str))
        .filter(|field| seen.insert(*field))
        .collect()
}
fn tag_probes(r: &DiscoverRequest, s: &Scope) -> Result<Vec<Probe>, Diagnostic> {
    let mut names = [
        "event_simpleName",
        "event.dataset",
        "event.module",
        "event.outcome",
        "ecs.version",
        "Cps.version",
        "Vendor",
        "type",
        "event.kind",
    ]
    .into_iter()
    .map(str::to_string)
    .collect::<Vec<_>>();
    for field in record_kind_candidates(r) {
        if let Some(tag) = field.strip_prefix('#')
            && !names.iter().any(|name| name == tag)
        {
            names.push(tag.into());
        }
    }
    names
        .into_iter()
        .map(|name| {
            probe(
                r,
                s,
                "tag-detect",
                &format!("{}=* | count(as=events)", quote(&format!("#{name}"))),
                true,
                vec![],
                Some(name),
            )
        })
        .collect()
}
fn record_kind_probes(
    r: &DiscoverRequest,
    s: &Scope,
    fields: &[String],
    tags: &BTreeMap<String, u64>,
) -> Result<Vec<Probe>, Diagnostic> {
    record_kind_candidates(r)
        .into_iter()
        .filter(|field| {
            r.max_population_fields_per_source.is_none() || *field != "#event.dataset"
        })
        .filter(|field| {
            fields.iter().any(|name| name == field)
                || field
                    .strip_prefix('#')
                    .and_then(|tag| tags.get(tag))
                    .is_some_and(|count| *count > 0)
        })
        .map(|field| {
            probe(
                r,
                s,
                "record-kind-facet",
                &format!(
                    "groupBy([{}], function=count(as=events), limit=max) | sort(events, order=desc, limit=50)",
                    quote(field)
                ),
                false,
                vec![],
                Some(field.into()),
            )
        })
        .collect()
}
fn value_probes(r: &DiscoverRequest, s: &Scope) -> Result<Vec<Probe>, Diagnostic> {
    r.value_fields
        .iter()
        .map(|f| {
            probe(
                r,
                s,
                "value-facet",
                &format!(
                    "groupBy([{}], function=count(as=events), limit=25) | sort(events, order=desc, limit=25)",
                    quote(f)
                ),
                true,
                vec![],
                Some(f.clone()),
            )
        })
        .collect()
}

struct KindFacet {
    field: String,
    values: Vec<(String, u64)>,
    truncated: bool,
}
impl KindFacet {
    fn metadata(&self) -> Value {
        json!({"field":self.field,"values":self.values.iter().map(|(value,_)|value).collect::<Vec<_>>(),"truncated":self.truncated})
    }
}
fn literal_filter(field: &str, value: &str) -> Result<String, Diagnostic> {
    let query = plan::lower(&Plan {
        source: Scope::default(),
        filters: vec![Predicate::Eq {
            field: field.into(),
            value: Scalar::Text(value.into()),
        }],
        steps: vec![],
    })?
    .render();
    let parsed = crate::language::parser::parse(&query).map_err(|e| e.diagnostic)?;
    if crate::language::validate::validate(&parsed, &query).status != Acceptance::Accepted {
        return Err(fail("Facet value cannot be expressed as a CQL literal."));
    }
    Ok(query)
}
fn kind_facet(rows: &[Value], field: &str, limit: Option<usize>) -> Result<KindFacet, Diagnostic> {
    if limit.is_some_and(|limit| rows.len() > limit) || rows.len() >= 10_000 {
        return Err(fail("Facet exceeded its result-row limit."));
    }
    let mut values = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let count = number(row.get("events"))?;
        let value = row
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| fail("Facet row requires a string discriminator value."))?;
        literal_filter(field, value)
            .map_err(|_| fail("Facet value cannot be expressed as a CQL literal."))?;
        if !seen.insert(value) {
            return Err(fail("Facet contains duplicate discriminator values."));
        }
        values.push((value.into(), count));
    }
    Ok(KindFacet {
        field: field.into(),
        values,
        truncated: limit.is_some_and(|limit| rows.len() == limit),
    })
}
fn population_counts(
    probe: &Probe,
    rows: &[Value],
    population: &mut BTreeMap<String, (u64, u64)>,
) -> Result<(), Diagnostic> {
    if rows.len() != 1 {
        return Err(fail("Population query requires exactly one result row."));
    }
    let total = number(rows[0].get("total"))?;
    if total == 0 {
        return Err(fail(
            "Population sample is empty; prior schema was retained.",
        ));
    }
    for (i, field) in probe.fields.iter().enumerate() {
        let count = number(rows[0].get(format!("c{i}").as_str()))?;
        if count > total {
            return Err(fail("Field count exceeds its matching denominator."));
        }
        population.insert(field.clone(), (count, total));
    }
    Ok(())
}
fn check_measurement(probe: &Probe, rows: &[Value]) -> Result<(), Diagnostic> {
    match probe.kind.as_str() {
        "volume" | "tag-detect" => count_row(rows).map(|_| ()),
        "dataset-facet" => kind_facet(rows, "#event.dataset", None).map(|_| ()),
        "fieldset" | "kind-fieldset" => fieldset_fields(rows).map(|_| ()),
        "record-kind-facet" => kind_facet(
            rows,
            probe.field.as_deref().expect("kind probe has a field"),
            Some(KIND_FACET_LIMIT),
        )
        .map(|_| ()),
        "population" => population_counts(probe, rows, &mut BTreeMap::new()),
        _ => Ok(()),
    }
}
fn unanswered(
    r: &DiscoverRequest,
    probes: &[Probe],
    answered: &BTreeMap<String, Vec<Value>>,
) -> Vec<Probe> {
    probes
        .iter()
        .filter(|p| !answered.contains_key(&probe_id(&r.environment.view, p)))
        .cloned()
        .collect()
}
#[derive(Default)]
struct SourceMeasurements {
    fields: Vec<String>,
    fieldset_seen: BTreeSet<String>,
    full_fieldset_seen: BTreeSet<String>,
    population: BTreeMap<String, (u64, u64)>,
    tags: BTreeMap<String, u64>,
    record_kinds: Vec<Value>,
    full_fields: BTreeSet<String>,
    kind_fields: BTreeSet<String>,
}

fn capture_key(
    capture: &Capture,
) -> Result<
    (
        time::OffsetDateTime,
        time::OffsetDateTime,
        time::OffsetDateTime,
    ),
    Diagnostic,
> {
    let start = environment::parse_time(&capture.probe.start)?;
    let end = environment::parse_time(&capture.probe.end)?;
    let retrieved = environment::parse_time(&capture.retrieved_at)?;
    if start >= end || end > retrieved {
        return Err(fail(
            "Measurement retrieval must follow its increasing observation window.",
        ));
    }
    Ok((end, start, retrieved))
}

type FieldCaptures = BTreeMap<String, (usize, bool, Option<f64>)>;

fn field_captures(captures: &[Capture], source: &Scope) -> Result<FieldCaptures, Diagnostic> {
    let mut result = FieldCaptures::new();
    for (index, capture) in captures
        .iter()
        .enumerate()
        .filter(|(_, capture)| &capture.probe.source == source)
    {
        let probe = &capture.probe;
        let rows = &capture.receipt.events;
        let values = match probe.kind.as_str() {
            "population" => {
                let mut counts = BTreeMap::new();
                population_counts(probe, rows, &mut counts)?;
                counts
                    .into_iter()
                    .map(|(field, (count, total))| {
                        (field, count > 0, Some(count as f64 / total as f64))
                    })
                    .collect::<Vec<_>>()
            }
            "tag-detect" => vec![(
                format!(
                    "#{}",
                    probe.field.as_deref().expect("tag probe has a field")
                ),
                count_row(rows)? > 0,
                None,
            )],
            "dataset-facet" | "record-kind-facet" => {
                let field = if probe.kind == "dataset-facet" {
                    "#event.dataset"
                } else {
                    probe.field.as_deref().expect("kind probe has a field")
                };
                let facet = kind_facet(
                    rows,
                    field,
                    if probe.kind == "dataset-facet" {
                        None
                    } else {
                        Some(KIND_FACET_LIMIT)
                    },
                )?;
                vec![(
                    field.into(),
                    facet.values.iter().any(|(_, count)| *count > 0),
                    None,
                )]
            }
            _ => continue,
        };
        let key = capture_key(capture)?;
        for (field, observed, rate) in values {
            if let Some((previous, previous_observed, previous_rate)) = result.get(&field) {
                let previous_key = capture_key(&captures[*previous])?;
                if key == previous_key
                    && (observed != *previous_observed
                        || rate.zip(*previous_rate).is_some_and(|(a, b)| a != b))
                {
                    return Err(fail(
                        "Tied field measurements contain contradictory presence or population rates.",
                    ));
                }
                if key < previous_key
                    || key == previous_key && (rate.is_none() || previous_rate.is_some())
                {
                    continue;
                }
            }
            result.insert(field, (index, observed, rate));
        }
    }
    Ok(result)
}

fn selected_population_probes(
    r: &DiscoverRequest,
    s: &Scope,
    measured: &SourceMeasurements,
) -> Result<Vec<Probe>, Diagnostic> {
    let sampled = measured
        .fields
        .iter()
        .filter(|field| !measured.full_fields.contains(*field))
        .cloned()
        .collect::<Vec<_>>();
    let full = measured.full_fields.iter().cloned().collect::<Vec<_>>();
    let mut probes = population_probes_in(r, s, &sampled, PopulationWindow::Sample)?;
    probes.extend(population_probes_in(r, s, &full, PopulationWindow::Full)?);
    Ok(probes)
}

fn sampled_measurements(r: &DiscoverRequest, s: &Scope, fields: Vec<String>) -> SourceMeasurements {
    let fieldset_seen = fields.iter().cloned().collect::<BTreeSet<_>>();
    let selected = field_selection(r, s).unwrap_or(&fields).to_vec();
    let full_fields = selected
        .iter()
        .filter(|field| !fieldset_seen.contains(*field))
        .cloned()
        .collect();
    SourceMeasurements {
        fields: selected,
        fieldset_seen,
        full_fields,
        ..SourceMeasurements::default()
    }
}
enum SourceProgress {
    Pending(Vec<Probe>),
    Complete(SourceMeasurements),
}

// Both relay planning and execution follow this dependency graph:
// base -> tags -> kind facets -> rare-kind fieldsets -> selected population -> values.
fn stratified_source(
    r: &DiscoverRequest,
    s: &Scope,
    sampled: &[String],
    dataset_rows: &[Value],
    answered: &BTreeMap<String, Vec<Value>>,
) -> Result<SourceProgress, Diagnostic> {
    let limit = r
        .max_population_fields_per_source
        .expect("stratified discovery has a population cap");
    let tags = tag_probes(r, s)?;
    let pending = unanswered(r, &tags, answered);
    if !pending.is_empty() {
        return Ok(SourceProgress::Pending(pending));
    }
    let mut measured = SourceMeasurements::default();
    for p in tags {
        measured.tags.insert(
            p.field.clone().expect("tag probe has a name"),
            count_row(&answered[&probe_id(&r.environment.view, &p)])?,
        );
    }
    let kinds = record_kind_probes(r, s, sampled, &measured.tags)?;
    let pending = unanswered(r, &kinds, answered);
    if !pending.is_empty() {
        return Ok(SourceProgress::Pending(pending));
    }
    let mut facets = BTreeMap::new();
    for p in kinds {
        let field = p.field.as_deref().expect("kind probe has a field");
        facets.insert(
            field.to_string(),
            kind_facet(
                &answered[&probe_id(&r.environment.view, &p)],
                field,
                Some(KIND_FACET_LIMIT),
            )?,
        );
    }
    facets.insert(
        "#event.dataset".into(),
        kind_facet(dataset_rows, "#event.dataset", None)?,
    );
    let mut rare_probes = Vec::new();
    if let Some(facet) = record_kind_candidates(r)
        .into_iter()
        .filter_map(|field| facets.get(field))
        .find(|facet| !facet.truncated && facet.values.len() > 1)
    {
        let mut values = facet
            .values
            .iter()
            .filter(|(_, count)| (1..=RARE_KIND_EVENTS).contains(count))
            .collect::<Vec<_>>();
        values.sort_by(|(a, n), (b, m)| n.cmp(m).then_with(|| a.cmp(b)));
        for (value, _) in values.into_iter().take(RARE_KIND_LIMIT) {
            rare_probes.push(probe(
                r,
                s,
                "kind-fieldset",
                &format!("{} | fieldset()", literal_filter(&facet.field, value)?),
                false,
                vec![],
                Some(facet.field.clone()),
            )?);
        }
    }
    let pending = unanswered(r, &rare_probes, answered);
    if !pending.is_empty() {
        return Ok(SourceProgress::Pending(pending));
    }
    let mut rare_fields = BTreeSet::new();
    for p in rare_probes {
        rare_fields.extend(fieldset_fields(
            &answered[&probe_id(&r.environment.view, &p)],
        )?);
    }
    for field in record_kind_candidates(r) {
        if let Some(facet) = facets.get(field) {
            if facet.values.iter().any(|(_, count)| *count > 0) {
                measured.kind_fields.insert(field.into());
            }
            if !facet.values.is_empty() {
                measured.record_kinds.push(facet.metadata());
            }
        }
    }
    let sampled = sampled.iter().cloned().collect::<BTreeSet<_>>();
    let mut observed = sampled
        .union(&rare_fields)
        .cloned()
        .collect::<BTreeSet<_>>();
    observed.extend(measured.kind_fields.iter().cloned());
    observed.extend(
        measured
            .tags
            .iter()
            .filter(|(_, count)| **count > 0)
            .map(|(field, _)| format!("#{field}")),
    );
    let mandatory = r
        .kind_fields
        .iter()
        .chain(&r.value_fields)
        .filter(|field| observed.contains(*field))
        .cloned()
        .collect::<BTreeSet<_>>();
    if field_selection(r, s).is_none() && mandatory.len() > limit {
        return Err(fail(
            "Population cap cannot include all observed requested kind/value fields.",
        ));
    }
    if let Some(fields) = field_selection(r, s)
        && mandatory.iter().any(|field| !fields.contains(field))
    {
        return Err(fail(
            "Explicit population selections must include observed requested kind/value fields.",
        ));
    }
    let newly_seen = rare_fields.difference(&sampled).cloned().collect();
    let mut selected = BTreeSet::new();
    measured.fields = match field_selection(r, s) {
        Some(fields) => fields.to_vec(),
        None => [mandatory, newly_seen, sampled.clone()]
            .into_iter()
            .flatten()
            .filter(|field| selected.insert(field.clone()))
            .take(limit)
            .collect(),
    };
    measured.fieldset_seen = sampled.union(&rare_fields).cloned().collect();
    measured.full_fieldset_seen = rare_fields.difference(&sampled).cloned().collect();
    measured.full_fields = measured
        .fields
        .iter()
        .filter(|field| !sampled.contains(*field))
        .cloned()
        .collect();
    let population = selected_population_probes(r, s, &measured)?;
    let pending = unanswered(r, &population, answered);
    if !pending.is_empty() {
        return Ok(SourceProgress::Pending(pending));
    }
    for p in population {
        population_counts(
            &p,
            &answered[&probe_id(&r.environment.view, &p)],
            &mut measured.population,
        )?;
    }
    Ok(SourceProgress::Complete(measured))
}
/// List the reachable, unanswered measurements using the same builders as execution.
pub fn frontier(
    r: &DiscoverRequest,
    answered: &BTreeMap<String, Vec<Value>>,
) -> Result<Vec<Probe>, Diagnostic> {
    validate_request(r)?;
    let mut sources = r.sources.clone();
    let mut pending = Vec::new();
    if r.census {
        let p = census_probe(r)?;
        if let Some(rows) = answered.get(&probe_id(&r.environment.view, &p)) {
            census_sources(rows, &mut sources)?;
        } else {
            pending.push(p);
        }
    }
    if sources.len() > 100 {
        return Err(fail(
            "More than 100 sources require an explicitly narrowed discovery run.",
        ));
    }
    for s in sources {
        validate_scope(&s)?;
        let base = base_probes(r, &s)?;
        if r.depth == DiscoveryDepth::SourcesOnly {
            for p in base {
                if let Some(rows) = answered.get(&probe_id(&r.environment.view, &p)) {
                    if count_row(rows)? == 0 {
                        return Err(fail(
                            "Quiet source measurements cannot establish observed presence.",
                        ));
                    }
                } else {
                    pending.push(p);
                }
            }
            continue;
        }
        if let Some(rows) = answered.get(&probe_id(&r.environment.view, &base[1])) {
            kind_facet(rows, "#event.dataset", None)?;
        }
        for p in &base {
            if !answered.contains_key(&probe_id(&r.environment.view, p)) {
                pending.push(p.clone());
            }
        }
        let Some(fields) = answered
            .get(&probe_id(&r.environment.view, &base[2]))
            .map(|rows| fieldset_fields(rows))
            .transpose()?
        else {
            continue;
        };
        if r.max_population_fields_per_source.is_some() {
            if base
                .iter()
                .any(|p| !answered.contains_key(&probe_id(&r.environment.view, p)))
            {
                continue;
            }
            if count_row(&answered[&probe_id(&r.environment.view, &base[0])])? == 0
                || fields.is_empty()
            {
                return Err(fail(
                    "Quiet or empty measurement windows cannot replace an existing schema.",
                ));
            }
            match stratified_source(
                r,
                &s,
                &fields,
                &answered[&probe_id(&r.environment.view, &base[1])],
                answered,
            )? {
                SourceProgress::Pending(probes) => pending.extend(probes),
                SourceProgress::Complete(_) => {
                    pending.extend(unanswered(r, &value_probes(r, &s)?, answered));
                }
            }
            continue;
        }
        let measured = sampled_measurements(r, &s, fields.clone());
        let mut dependent = selected_population_probes(r, &s, &measured)?;
        let tag_probes = tag_probes(r, &s)?;
        dependent.extend(tag_probes.iter().cloned());
        dependent.extend(record_kind_probes(r, &s, &fields, &BTreeMap::new())?);
        for p in &dependent {
            if !answered.contains_key(&probe_id(&r.environment.view, p)) {
                pending.push(p.clone());
            }
        }
        if dependent
            .iter()
            .all(|p| answered.contains_key(&probe_id(&r.environment.view, p)))
        {
            let mut tags = BTreeMap::new();
            for p in &tag_probes {
                let rows = &answered[&probe_id(&r.environment.view, p)];
                tags.insert(
                    p.field.clone().expect("tag probe has a name"),
                    count_row(rows)?,
                );
            }
            let mut final_probes = record_kind_probes(r, &s, &fields, &tags)?;
            final_probes.extend(value_probes(r, &s)?);
            for p in final_probes {
                if !answered.contains_key(&probe_id(&r.environment.view, &p)) {
                    pending.push(p);
                }
            }
        }
    }
    Ok(pending)
}
fn base_probes(r: &DiscoverRequest, s: &Scope) -> Result<Vec<Probe>, Diagnostic> {
    if r.depth == DiscoveryDepth::SourcesOnly {
        return Ok(vec![probe(
            r,
            s,
            "volume",
            "count(as=events)",
            false,
            vec![],
            None,
        )?]);
    }
    Ok(vec![
        probe(r, s, "volume", "count(as=events)", false, vec![], None)?,
        probe(
            r,
            s,
            "dataset-facet",
            "groupBy([#event.dataset], function=count(as=events), limit=max)",
            false,
            vec![],
            None,
        )?,
        probe(r, s, "fieldset", "fieldset()", true, vec![], None)?,
    ])
}
struct Run<'a, B> {
    backend: &'a mut B,
    request: &'a DiscoverRequest,
    deadline: Instant,
    origin: Origin,
    report: DiscoverReport,
}
impl<B: SearchBackend> Run<'_, B> {
    fn provenance(&self, index: usize) -> Value {
        let capture = &self.report.captures[index];
        let source = source_id(&capture.probe.source);
        json!({
            "id":format!("{}{}:{}:{}:{index}", self.origin.id_prefix(), compact(&capture.retrieved_at), source.trim_start_matches("env:repo:"), capture.probe.kind),
            "source_type":self.origin.source_type(), "canonical_source":self.origin.canonical_source(),
            "retrieved_at":capture.retrieved_at,"redaction_status":"none","coverage_status":self.origin.coverage(),
            "observation_window":format!("{}/{}",capture.probe.start,capture.probe.end),"notes":self.origin.notes()
        })
    }

    fn execute(&mut self, probe: Probe) -> Result<Vec<Value>, Diagnostic> {
        if self.report.captures.len() >= self.request.max_queries as usize
            || Instant::now() >= self.deadline
        {
            return Err(fail(
                "Discovery budget exhausted; the working pack must remain unchanged.",
            ));
        }
        if CANCELLED.load(Ordering::Relaxed) {
            return Err(Diagnostic::error("CANCELLED", "Discovery cancelled."));
        }
        let timeout = self
            .deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(180));
        let receipt = run_job(
            self.backend,
            &self.request.environment.view,
            &probe.query,
            &probe.start,
            &probe.end,
            &JobPolicy {
                timeout,
                ..JobPolicy::default()
            },
        );
        let result = if receipt.completion == "complete" && receipt.cleanup_complete() {
            check_measurement(&probe, &receipt.events).map(|()| receipt.events.clone())
        } else {
            Err(receipt
                .diagnostic
                .clone()
                .unwrap_or_else(|| fail("Incomplete search evidence or unconfirmed search cleanup; no further measurement or pack activation is allowed.")))
        };
        self.report.captures.push(Capture {
            probe,
            retrieved_at: receipt
                .job_id
                .as_deref()
                .and_then(|job| self.backend.observed_at(job))
                .unwrap_or_else(timestamp),
            receipt,
        });
        result
    }
    fn measure_source(
        &mut self,
        source: &Scope,
        fields: Vec<String>,
        mut answered: BTreeMap<String, Vec<Value>>,
    ) -> Result<SourceMeasurements, Diagnostic> {
        if self.request.max_population_fields_per_source.is_some() {
            let dataset_probe = &base_probes(self.request, source)?[1];
            let dataset_rows =
                answered[&probe_id(&self.request.environment.view, dataset_probe)].clone();
            loop {
                match stratified_source(self.request, source, &fields, &dataset_rows, &answered)? {
                    SourceProgress::Complete(measured) => return Ok(measured),
                    SourceProgress::Pending(probes) => {
                        for p in probes {
                            let id = probe_id(&self.request.environment.view, &p);
                            let rows = self.execute(p)?;
                            answered.insert(id, rows);
                        }
                    }
                }
            }
        }
        let mut measured = sampled_measurements(self.request, source, fields);
        for p in selected_population_probes(self.request, source, &measured)? {
            let rows = self.execute(p.clone())?;
            population_counts(&p, &rows, &mut measured.population)?;
        }
        for p in tag_probes(self.request, source)? {
            let name = p.field.clone().expect("tag probe has a name");
            let rows = self.execute(p)?;
            measured.tags.insert(name, count_row(&rows)?);
        }
        let sampled = measured.fieldset_seen.iter().cloned().collect::<Vec<_>>();
        for p in record_kind_probes(self.request, source, &sampled, &measured.tags)? {
            let field = p.field.clone().expect("kind probe has a field");
            let rows = self.execute(p)?;
            measured
                .record_kinds
                .push(kind_facet(&rows, &field, Some(KIND_FACET_LIMIT))?.metadata());
        }
        Ok(measured)
    }
    fn measure(&mut self) -> Result<Pack, Diagnostic> {
        let mut sources = self.request.sources.clone();
        if self.request.census {
            let rows = self.execute(census_probe(self.request)?)?;
            census_sources(&rows, &mut sources)?;
        }
        if sources.is_empty() {
            return Err(fail(
                "Census returned no sources; no empty replacement was activated.",
            ));
        }
        if sources.len() > 100 {
            return Err(fail(
                "More than 100 sources require an explicitly narrowed discovery run.",
            ));
        }
        let mut data = PackData::default();
        for s in sources {
            validate_scope(&s)?;
            let mut counts = BTreeMap::new();
            let mut datasets = Vec::new();
            let mut fields = Vec::new();
            let mut answered = BTreeMap::new();
            for p in base_probes(self.request, &s)? {
                let kind = p.kind.clone();
                let id = probe_id(&self.request.environment.view, &p);
                let rows = self.execute(p)?;
                match kind.as_str() {
                    "volume" => {
                        counts.insert(kind, count_row(&rows)?);
                    }
                    "dataset-facet" => {
                        datasets = kind_facet(&rows, "#event.dataset", None)?
                            .values
                            .into_iter()
                            .map(|(value, _)| value)
                            .collect();
                    }
                    "fieldset" => {
                        fields = fieldset_fields(&rows)?;
                    }
                    _ => unreachable!(),
                }
                answered.insert(id, rows);
            }
            if counts.values().any(|n| *n == 0)
                || fields.is_empty() && self.request.depth != DiscoveryDepth::SourcesOnly
            {
                return Err(fail(
                    "Quiet or empty measurement windows cannot replace an existing schema.",
                ));
            }
            let SourceMeasurements {
                fields,
                fieldset_seen,
                population,
                record_kinds,
                ..
            } = if self.request.depth == DiscoveryDepth::SourcesOnly {
                sampled_measurements(self.request, &s, vec![])
            } else {
                self.measure_source(&s, fields, answered)?
            };
            let sid = source_id(&s);
            let full_window = format!("{}/{}", self.request.start, self.request.end);
            let measurements = field_captures(&self.report.captures, &s)?;
            let volume = self
                .report
                .captures
                .iter()
                .position(|capture| capture.probe.source == s && capture.probe.kind == "volume")
                .expect("completed source has a volume capture");
            let source_provenance = self.provenance(volume);
            let source_pid = source_provenance["id"].clone();
            for (index, _) in self
                .report
                .captures
                .iter()
                .enumerate()
                .filter(|(_, capture)| capture.probe.source == s)
            {
                data.provenance.push(self.provenance(index));
            }
            let contract = self
                .request
                .contracts
                .iter()
                .find(|c| c.source == s)
                .map(|c| c.contract.clone());
            let contract_pid = contract.as_ref().map(|c| {
                let digest = sha256_hex(&serde_json::to_vec(c).unwrap());
                format!(
                    "prov:contract:{}:{}",
                    compact(&c.retrieved_at),
                    &digest[..16]
                )
            });
            if let (Some(c), Some(cpid)) = (&contract, &contract_pid)
                && !data.provenance.iter().any(|p| p["id"] == *cpid)
            {
                data.provenance.push(json!({"id":cpid,"source_type":"manual-curation","canonical_source":"Explicit parser contract supplied by the operator","retrieved_at":c.retrieved_at,"source_hash":c.source_hash,"redaction_status":"none","coverage_status":"declared-only","notes":"Declaration evidence retains its original date; search observations do not re-review it."}));
            }
            let mut source = json!({"id":sid,"kind":if s.repo.is_some(){"repo"}else{"source"},"name":s.type_.as_ref().or(s.repo.as_ref()).unwrap(),"inventory_status":"observed-only","selectors":{"repo":s.repo,"type":s.type_},"relationship":{"datasets":datasets},"evidence":{"declared":contract.is_some(),"observed":true,"observation_window":full_window},"provenance_ids":[source_pid]});
            if self.request.depth == DiscoveryDepth::SourcesOnly {
                source["evidence"]["discovery_depth"] = json!("sources-only");
            }
            if self.request.depth != DiscoveryDepth::SourcesOnly {
                source["evidence"]["measured_fields"] =
                    json!(population.keys().collect::<Vec<_>>());
            }
            if !record_kinds.is_empty() {
                source["relationship"]["record_kinds"] = json!(record_kinds);
            }
            if let Some(c) = &contract {
                source["parser_contract"] = serde_json::to_value(c).unwrap();
                source["inventory_status"] = json!("reconciled");
                source["provenance_ids"]
                    .as_array_mut()
                    .unwrap()
                    .push(json!(contract_pid));
            }
            data.sources.push(source);
            let mut namespace_fields: BTreeSet<(String, bool)> = fields
                .iter()
                .chain(&fieldset_seen)
                .chain(measurements.keys())
                .map(|f| {
                    (
                        f.strip_prefix('#').unwrap_or(f).to_string(),
                        f.starts_with('#'),
                    )
                })
                .collect();
            if let Some(c) = &contract {
                for f in &c.declared_fields {
                    namespace_fields.insert((
                        f.strip_prefix('#').unwrap_or(f).into(),
                        c.fields_to_tag.contains(f) || f.starts_with('#'),
                    ));
                }
            }
            for (path, is_tag) in namespace_fields {
                let original = if is_tag {
                    format!("#{path}")
                } else {
                    path.clone()
                };
                let measurement = measurements.get(&original);
                let observed = measurement.map(|(_, observed, _)| *observed);
                let declaration = contract
                    .as_ref()
                    .and_then(|c| contract_field(c, &path, is_tag));
                let declared = contract.as_ref().map(|_| declaration.is_some());
                let rate = measurement.and_then(|(_, _, rate)| *rate);
                let seen = fieldset_seen.contains(&original);
                let capture = measurement.map(|(index, _, _)| *index).or_else(|| {
                    self.report
                        .captures
                        .iter()
                        .enumerate()
                        .find_map(|(index, capture)| {
                            (capture.probe.source == s
                                && matches!(
                                    capture.probe.kind.as_str(),
                                    "fieldset" | "kind-fieldset"
                                )
                                && capture
                                    .receipt
                                    .events
                                    .iter()
                                    .any(|row| row["field"].as_str() == Some(&original)))
                            .then_some(index)
                        })
                });
                let field_window = capture.map(|index| {
                    format!(
                        "{}/{}",
                        self.report.captures[index].probe.start,
                        self.report.captures[index].probe.end
                    )
                });
                let is_array = contract.as_ref().zip(declaration).map(|(c, field)| {
                    c.array_fields
                        .iter()
                        .any(|array| array == field || array == &path)
                });
                let mut provenance = Vec::new();
                if let Some(index) = capture {
                    provenance.push(self.provenance(index)["id"].clone());
                }
                if declared == Some(true)
                    && let Some(cpid) = &contract_pid
                {
                    provenance.push(json!(cpid));
                }
                if provenance.is_empty() {
                    return Err(fail(
                        "A field has neither observation nor declaration evidence.",
                    ));
                }
                data.fields.push(json!({"id":field_id(&sid,&path,is_tag),"source_id":sid,"path":path,"data_type":"unknown","is_tag":is_tag,"is_array":is_array,"evidence_kind":if measurement.is_some(){"observed"}else if seen{"documented"}else{"declared"},"observation_window":field_window,"coverage":{"declared":declared,"observed":observed,"rate":rate},"provenance_ids":provenance}));
            }
            for p in value_probes(self.request, &s)? {
                let f = p.field.clone().expect("value probe has a name");
                let tag = f.starts_with('#');
                let path = f.strip_prefix('#').unwrap_or(&f);
                let fid = field_id(&sid, path, tag);
                let rows = self.execute(p)?;
                if rows.is_empty() {
                    continue;
                }
                if !data.fields.iter().any(|v| v["id"] == fid) {
                    return Err(fail(
                        "Requested value field was not observed or declared in this source.",
                    ));
                }
                let proof = self.provenance(self.report.captures.len() - 1);
                let pid = proof["id"].clone();
                data.provenance.push(proof);
                for row in rows {
                    let value = row
                        .get(&f)
                        .filter(|v| !v.is_array() && !v.is_object())
                        .ok_or_else(|| fail("Value facet omitted its typed scalar field."))?;
                    let _ = number(row.get("events"))?;
                    let value_hash = sha256_hex(&serde_json::to_vec(&(&f, value)).unwrap());
                    data.values.push(json!({"id":format!("env:value:{}:{}",sid.trim_start_matches("env:repo:"),&value_hash[..16]),"field_id":fid,"value_kind":"sample","value":value,"vocabulary":"sampled","evidence_kind":"observed","provenance_ids":[pid]}));
                }
            }
        }
        let pack = Pack::new(self.request.environment.clone(), timestamp(), data)?;
        if pack
            .data
            .fields
            .iter()
            .filter_map(|value| environment::field(value).ok())
            .any(|field| {
                field.evidence_kind == "observed"
                    && environment::field_measurement(&pack, &field).is_none()
            })
        {
            return Err(fail(
                "Current field measurements require exact scoped, causal observation provenance.",
            ));
        }
        Ok(pack)
    }
}
pub fn execute(
    request: &DiscoverRequest,
    backend: &mut impl SearchBackend,
) -> (Option<Pack>, DiscoverReport) {
    execute_with_origin(
        request,
        backend,
        Origin::Api,
        Duration::from_secs(request.max_seconds.min(3600)),
    )
}
pub fn execute_with_origin(
    request: &DiscoverRequest,
    backend: &mut impl SearchBackend,
    origin: Origin,
    remaining: Duration,
) -> (Option<Pack>, DiscoverReport) {
    let mut run = Run {
        backend,
        request,
        deadline: Instant::now() + remaining,
        origin,
        report: DiscoverReport {
            schema_version: 1,
            environment: request.environment.clone(),
            status: "error".into(),
            active_pack_updated: false,
            captures: vec![],
            diagnostics: vec![],
        },
    };
    let result = validate_request(request).and_then(|_| run.measure());
    match result {
        Ok(pack) => {
            run.report.status = "complete".into();
            (Some(pack), run.report)
        }
        Err(d) => {
            run.report.diagnostics.push(d);
            (None, run.report)
        }
    }
}
fn number(v: Option<&Value>) -> Result<u64, Diagnostic> {
    v.and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    })
    .ok_or_else(|| {
        fail("Expected an explicit nonnegative integer count; missing rows are not zero.")
    })
}
fn count_row(rows: &[Value]) -> Result<u64, Diagnostic> {
    if rows.len() != 1 {
        return Err(fail("Count measurement requires exactly one explicit row."));
    }
    number(rows[0].get("events").or_else(|| rows[0].get("_count")))
}
fn compact(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect()
}
pub fn source_id(s: &Scope) -> String {
    let selectors = Selectors {
        repo: s.repo.clone(),
        type_: s.type_.clone(),
        ..Selectors::default()
    };
    let h = sha256_hex(&serde_json::to_vec(&selectors).unwrap());
    format!("env:repo:{}", &h[..16])
}
pub fn field_id(source: &str, path: &str, tag: bool) -> String {
    format!(
        "env:field:{}:{}{}",
        source.trim_start_matches("env:repo:"),
        if tag { "#" } else { "" },
        path
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn offline_plan_is_bounded_and_source_values_are_literals() {
        let r = DiscoverRequest {
            schema_version: 1,
            environment: EnvironmentId {
                cloud: Cloud::Us1,
                tenant: "0".repeat(32),
                view: "search-all".into(),
            },
            start: "2026-01-01T00:00:00Z".into(),
            end: "2026-01-01T01:00:00Z".into(),
            census: false,
            depth: DiscoveryDepth::Full,
            sources: vec![Scope {
                repo: Some("literal*repo".into()),
                ..Scope::default()
            }],
            value_fields: vec![],
            kind_fields: vec![],
            max_population_fields_per_source: None,
            population_fields: vec![],
            contracts: vec![],
            max_queries: 200,
            max_seconds: 900,
        };
        let p = initial_plan(&r).unwrap();
        assert_eq!(p["executes_searches"], false);
        assert!(
            p["initial_probes"][0]["query"]
                .as_str()
                .unwrap()
                .contains("\\Aliteral\\*repo\\z")
        );
    }
    #[test]
    fn missing_count_is_not_zero() {
        assert!(count_row(&[]).is_err());
        assert_eq!(count_row(&[json!({"events":"0"})]).unwrap(), 0);
    }
}
