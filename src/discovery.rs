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
/// Tag namespaces every field run counts in its tag batch, before any tagged kind candidate.
const TAG_CANDIDATES: [&str; 9] = [
    "#event_simpleName",
    "#event.dataset",
    "#event.module",
    "#event.outcome",
    "#ecs.version",
    "#Cps.version",
    "#Vendor",
    "#type",
    "#event.kind",
];
const MAX_FIELDS_PER_SOURCE: usize = 4000;
const MAX_SOURCES: usize = 100;
const MAX_QUERIES: u32 = 1000;
const POPULATION_CHUNK: usize = 20;
const KIND_FACET_LIMIT: usize = 50;
const RARE_KIND_EVENTS: u64 = 50_000;
const RARE_KIND_LIMIT: usize = 10;
/// Every selected source answered its volume search with zero events.
pub const ALL_QUIET_SOURCES: &str =
    "Every selected source had zero events in the measurement window; no pack was activated.";
/// A source with events listed no fields, even over the full window when that was eligible.
pub const EMPTY_FIELD_LISTING: &str = "A source with events in the measurement window returned an empty field listing; no pack was activated.";

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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
/// The type of a source whose selector lowers `#type` to literal text equality, so that every
/// event the source holds has that type. A type lowered to an anchored regular expression keeps
/// its own facet.
fn literal_type(s: &Scope) -> Result<Option<&str>, Diagnostic> {
    let Some(value) = s.type_.as_deref() else {
        return Ok(None);
    };
    let lowered = scope(&Scope {
        type_: Some(value.into()),
        ..Scope::default()
    })?;
    Ok((lowered == format!("#type = {}", quote(value))).then_some(value))
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
    let query = if kind == "census" {
        checked_query(suffix.to_string())?
    } else {
        scoped_query(s, suffix)?
    };
    Ok(Probe {
        kind: kind.into(),
        source: s.clone(),
        query,
        start: window(r, sample)?.0,
        end: r.end.clone(),
        fields,
        field,
    })
}
/// The concrete instants of the sample window (the final 15 minutes of the request window) or of
/// the full request window. They are equal when the request window is 15 minutes or shorter.
fn window(r: &DiscoverRequest, sample: bool) -> Result<Window, Diagnostic> {
    let start = environment::parse_time(&r.start)?;
    let end = environment::parse_time(&r.end)?;
    Ok((
        stamp(if sample {
            start.max(end - time::Duration::minutes(15))
        } else {
            start
        }),
        r.end.clone(),
    ))
}
fn checked_query(query: String) -> Result<String, Diagnostic> {
    let parsed = crate::language::parser::parse(&query).map_err(|e| e.diagnostic)?;
    let checked = crate::language::validate::validate(&parsed, &query);
    if checked.status != Acceptance::Accepted {
        return Err(fail(
            "An internal discovery query did not pass the bundled language profile.",
        ));
    }
    Ok(query)
}
fn scoped_query(s: &Scope, suffix: &str) -> Result<String, Diagnostic> {
    checked_query(format!("{} | {suffix}", scope(s)?))
}
fn facet_suffix(field: &str, limit: usize) -> String {
    format!(
        "groupBy([{}], function=count(as=events), limit=max) | sort(events, order=desc, limit={limit})",
        quote(field)
    )
}
/// The query an agent may run itself to see which types in a quiet source's repository have
/// events: the repository selector alone, grouped by type. Discovery reserves nothing for it and
/// never reads its answer. A source without both a repository and a type has none: the same
/// repository alone would repeat the measured zero, and a type alone names no repository.
pub fn quiet_census_query(s: &Scope) -> Result<Option<String>, Diagnostic> {
    if s.repo.is_none() || s.type_.is_none() {
        return Ok(None);
    }
    let repository = Scope {
        repo: s.repo.clone(),
        type_: None,
        event: None,
    };
    scoped_query(&repository, &facet_suffix("#type", 25)).map(Some)
}
/// Refuse more selected sources than one discovery run measures.
pub fn check_source_count(r: &DiscoverRequest) -> Result<(), Diagnostic> {
    if r.sources.len() > MAX_SOURCES {
        return Err(fail(
            "More than 100 sources require an explicitly narrowed discovery run.",
        ));
    }
    Ok(())
}
/// Admission of a new run against one reading of the clock: the window has ended, the pack it
/// would extend was not created later than now, and the query limit covers every selected source
/// completing live. Activation still requires a candidate created after its predecessor.
pub fn admit(
    r: &DiscoverRequest,
    predecessor_created_at: Option<&str>,
    now: &str,
) -> Result<(), Diagnostic> {
    let clock = environment::parse_time(now)?;
    if environment::parse_time(&r.end)? > clock {
        return Err(fail(&format!(
            "The discovery window ends at {}, after the current time {now}. Choose an end at or before the current time.",
            r.end
        )));
    }
    if let Some(created_at) = predecessor_created_at
        && environment::parse_time(created_at)? > clock
    {
        return Err(fail(&format!(
            "The active pack was created at {created_at}, after the current time {now}, so no new discovery pack can follow it yet. Prepare discovery after that time."
        )));
    }
    if let Some(expected) = expected_searches(r)
        && expected.minimum_all_live > MAX_QUERIES
    {
        return Err(fail(&format!(
            "The {} searches needed when every selected source has events exceed the {MAX_QUERIES}-query ceiling. Select fewer sources or population fields.",
            expected.minimum_all_live
        )));
    }
    if let Some(expected) = expected_searches(r)
        && r.max_queries < expected.minimum_all_live
    {
        return Err(fail(&format!(
            "The query limit {} is below the {} searches needed when every selected source has events. Set max_queries to at least {}.",
            r.max_queries, expected.minimum_all_live, expected.minimum_all_live
        )));
    }
    Ok(())
}
pub fn initial_plan(r: &DiscoverRequest) -> Result<Value, Diagnostic> {
    validate_request(r)?;
    check_source_count(r)?;
    // The census answers first, then every source's volume, matching execution.
    let probes = if r.census {
        vec![census_probe(r)?]
    } else {
        r.sources
            .iter()
            .map(|s| volume_probe(r, s))
            .collect::<Result<Vec<_>, _>>()?
    };
    let listings =
        "dataset facet and fieldset for every source with events, after every source volume";
    let mut result = json!({"schema_version":1,"environment":r.environment,"initial_probes":probes,"deferred_stages":["base measurements for sources found by explicit census",listings,TAG_STAGE,"population chunks of at most 20 fields from fieldset, reusing tag batch counts over the same window","record-kind facets over the full request window","bounded value samples requested by value_fields"],"max_queries":r.max_queries,"max_seconds":r.max_seconds,"is_live":false,"executes_searches":false});
    if r.depth == DiscoveryDepth::SourcesOnly {
        result["depth"] = json!(r.depth);
        result["deferred_stages"] =
            json!(["source volume checks for sources found by explicit census"]);
    }
    if let Some(limit) = r.max_population_fields_per_source {
        result["max_population_fields_per_source"] = json!(limit);
        result["deferred_stages"] = json!([
            "base measurements for sources found by explicit census",
            listings,
            TAG_STAGE,
            "record-kind facets over the full request window",
            "up to ten literal-filtered rare-kind fieldsets over the full request window",
            "capped population fields: requested, newly discovered, then sampled",
            "bounded value samples requested by value_fields"
        ]);
    }
    if !r.population_fields.is_empty() {
        result["population_fields"] = json!(r.population_fields);
        let stage = if r.max_population_fields_per_source.is_some() {
            5
        } else {
            3
        };
        result["deferred_stages"][stage] = json!(
            "source-bound explicit population batches; sampled fields use the sample window and unsampled fields use the full request window"
        );
    }
    if r.census && !r.sources.is_empty() {
        result["deferred_stages"]
            .as_array_mut()
            .expect("stages are an array")
            .insert(
                0,
                json!("volume checks for requested sources after the census"),
            );
    }
    if let Some(expected) = expected_searches(r) {
        // The query limit always applies; below the high figure a run can exhaust it.
        result["searches"] = json!({"expected_low":expected.low,"minimum_all_live":expected.minimum_all_live,"expected_high":expected.high,"limit":r.max_queries,"below_expected_high":r.max_queries < expected.high});
    }
    if r.depth.is_full() {
        result["deferred_stages"].as_array_mut().expect("stages are an array").push(json!("one source-bound full-window fieldset fallback when positive source volume has an empty recent sample; affected population, tag and value probes use that full window"));
    }
    Ok(result)
}
const TAG_STAGE: &str = "tag batches of at most 20 tag namespace counts over the sample window, or the full window after a fieldset fallback";
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
fn fieldset_name(row: &Value) -> Option<&str> {
    ["field", "fieldname", "name", "_field"]
        .into_iter()
        .find_map(|key| row.get(key).and_then(Value::as_str))
}

fn fieldset_fields(rows: &[Value]) -> Result<Vec<String>, Diagnostic> {
    let mut fields = Vec::new();
    for row in rows {
        let name = fieldset_name(row).ok_or_else(|| fail("Unrecognized fieldset result shape."))?;
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
/// Field counts and their denominators, by namespaced field.
type Counts = BTreeMap<String, (u64, u64)>;
/// Concrete start and end instants.
type Window = (String, String);
#[derive(Clone, Copy)]
enum PopulationWindow {
    Sample,
    Full,
}
/// Field counts in chunks of at most 20 columns, each chunk numbering its aliases from `c0` in
/// `fields` order. Population searches and tag batches share this query shape.
fn population_probes_in(
    r: &DiscoverRequest,
    s: &Scope,
    kind: &str,
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
                kind,
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
/// The tag batch: every tag candidate, then each further tagged kind candidate, counted over the
/// sample window, or over the full window after a fieldset fallback.
fn tag_probes(r: &DiscoverRequest, s: &Scope, full_window: bool) -> Result<Vec<Probe>, Diagnostic> {
    let mut fields = TAG_CANDIDATES.map(String::from).to_vec();
    for field in record_kind_candidates(r) {
        if field.starts_with('#') && !fields.iter().any(|name| name == field) {
            fields.push(field.into());
        }
    }
    population_probes_in(r, s, "tag-batch", &fields, sample_window(full_window))
}
fn sample_window(full_window: bool) -> PopulationWindow {
    if full_window {
        PopulationWindow::Full
    } else {
        PopulationWindow::Sample
    }
}
/// Each tag candidate's count and denominator once every tag batch chunk is answered.
fn tag_counts(
    r: &DiscoverRequest,
    probes: &[Probe],
    answered: &BTreeMap<String, Vec<Value>>,
) -> Result<Option<Counts>, Diagnostic> {
    let mut counts = BTreeMap::new();
    for p in probes {
        let Some(rows) = answered.get(&probe_id(&r.environment.view, p)) else {
            return Ok(None);
        };
        population_counts(p, rows, &mut counts)?;
    }
    Ok(Some(counts))
}
/// Record-kind candidates the run measures: listed by the source or positive in its tag batch. A
/// capped run takes its dataset facet from the base measurement.
fn eligible_kinds<'a>(r: &'a DiscoverRequest, fields: &[String], tags: &Counts) -> Vec<&'a str> {
    record_kind_candidates(r)
        .into_iter()
        .filter(|field| r.max_population_fields_per_source.is_none() || *field != "#event.dataset")
        .filter(|field| {
            fields.iter().any(|name| name == field)
                || tags.get(*field).is_some_and(|(count, _)| *count > 0)
        })
        .collect()
}
fn kind_probe(r: &DiscoverRequest, s: &Scope, field: &str) -> Result<Probe, Diagnostic> {
    probe(
        r,
        s,
        "record-kind-facet",
        &facet_suffix(field, KIND_FACET_LIMIT),
        false,
        vec![],
        Some(field.into()),
    )
}
/// A record-kind facet that the base measurements over the request window already answer
/// exactly, with no search of its own: the single type of a literal type selector, counted by
/// the source volume, and a Full `#event.dataset` facet whose complete dataset listing has fewer
/// rows than the facet limit, ordered as the facet search orders it.
fn base_facet(
    r: &DiscoverRequest,
    s: &Scope,
    field: &str,
    answered: &BTreeMap<String, Vec<Value>>,
) -> Result<Option<KindFacet>, Diagnostic> {
    let id = |p: &Probe| probe_id(&r.environment.view, p);
    if field == "#type"
        && let Some(value) = literal_type(s)?
    {
        // Every facet follows the answered volume of its source.
        let events = count_row(&answered[&id(&volume_probe(r, s)?)])?;
        return Ok(Some(KindFacet {
            field: field.into(),
            values: vec![(value.into(), events)],
            truncated: false,
        }));
    }
    if field == "#event.dataset" && r.max_population_fields_per_source.is_none() {
        let rows = &answered[&id(&base_probes(r, s)?[1])];
        if rows.len() < KIND_FACET_LIMIT {
            let mut facet = kind_facet(rows, field, None)?;
            facet
                .values
                .sort_by(|(a, n), (b, m)| m.cmp(n).then_with(|| a.cmp(b)));
            return Ok(Some(facet));
        }
    }
    Ok(None)
}
fn record_kind_probes(
    r: &DiscoverRequest,
    s: &Scope,
    fields: &[String],
    tags: &Counts,
    answered: &BTreeMap<String, Vec<Value>>,
) -> Result<Vec<Probe>, Diagnostic> {
    let mut probes = Vec::new();
    for field in eligible_kinds(r, fields, tags) {
        if base_facet(r, s, field, answered)?.is_none() {
            probes.push(kind_probe(r, s, field)?);
        }
    }
    Ok(probes)
}
/// Every measured record-kind facet in candidate order, from the base measurements or from the
/// answer to its own search.
fn kind_facets(
    r: &DiscoverRequest,
    s: &Scope,
    fields: &[String],
    tags: &Counts,
    answered: &BTreeMap<String, Vec<Value>>,
) -> Result<Vec<KindFacet>, Diagnostic> {
    eligible_kinds(r, fields, tags)
        .into_iter()
        .map(|field| match base_facet(r, s, field, answered)? {
            Some(facet) => Ok(facet),
            None => kind_facet(
                &answered[&probe_id(&r.environment.view, &kind_probe(r, s, field)?)],
                field,
                Some(KIND_FACET_LIMIT),
            ),
        })
        .collect()
}
fn value_probes(
    r: &DiscoverRequest,
    s: &Scope,
    full_window: bool,
) -> Result<Vec<Probe>, Diagnostic> {
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
                !full_window,
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
pub(crate) fn check_measurement(probe: &Probe, rows: &[Value]) -> Result<(), Diagnostic> {
    match probe.kind.as_str() {
        "volume" | "tag-detect" => count_row(rows).map(|_| ()),
        "tag-batch" => population_counts(probe, rows, &mut BTreeMap::new()),
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
    /// Tag batch counts by namespaced field, and the concrete window they cover.
    tags: Counts,
    tag_window: Option<Window>,
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

/// The capture that proves each field of `source`, its presence and, for a population count, its
/// rate. Without a selection (inventory work proofs) a tag batch column carries no rate; the
/// chosen captures do not depend on it.
pub(crate) type CaptureOrderingKey = (
    time::OffsetDateTime,
    time::OffsetDateTime,
    time::OffsetDateTime,
);

pub(crate) fn source_uses_tag_batch_policy(captures: &[Capture], source: &Scope) -> bool {
    captures
        .iter()
        .any(|capture| &capture.probe.source == source && capture.probe.kind == "tag-batch")
}

pub(crate) fn capture_ordering_key(capture: &Capture) -> Result<CaptureOrderingKey, Diagnostic> {
    capture_key(capture)
}

#[cfg(test)]
pub(crate) fn field_captures(
    captures: &[Capture],
    source: &Scope,
) -> Result<FieldCaptures, Diagnostic> {
    let current_plan = source_uses_tag_batch_policy(captures, source);
    field_captures_for(captures, source, None, current_plan)
}

pub(crate) fn field_captures_with_policy(
    captures: &[Capture],
    source: &Scope,
    current_plan: bool,
) -> Result<FieldCaptures, Diagnostic> {
    field_captures_for(captures, source, None, current_plan)
}

fn field_captures_for(
    captures: &[Capture],
    source: &Scope,
    selected: Option<&BTreeSet<String>>,
    current_plan: bool,
) -> Result<FieldCaptures, Diagnostic> {
    let counted: BTreeSet<&str> = captures
        .iter()
        .filter(|capture| &capture.probe.source == source && capture.probe.kind == "population")
        .flat_map(|capture| capture.probe.fields.iter().map(String::as_str))
        .collect();
    // A literal type selector's volume stands for the `#type` facet the run did not search for,
    // where that facet was measured: listed by the source or positive in its tag batch.
    let type_from_volume = current_plan
        && literal_type(source)?.is_some()
        && captures
            .iter()
            .filter(|capture| &capture.probe.source == source)
            .map(|capture| {
                Ok(match capture.probe.kind.as_str() {
                    "fieldset" => capture
                        .receipt
                        .events
                        .iter()
                        .any(|row| fieldset_name(row) == Some("#type")),
                    "tag-batch" => {
                        let mut counts = BTreeMap::new();
                        population_counts(&capture.probe, &capture.receipt.events, &mut counts)?;
                        counts.get("#type").is_some_and(|(count, _)| *count > 0)
                    }
                    _ => false,
                })
            })
            .collect::<Result<Vec<_>, Diagnostic>>()?
            .contains(&true);
    let mut seen_by_key: BTreeMap<(String, CaptureOrderingKey), (bool, Option<f64>)> =
        BTreeMap::new();
    let mut result = FieldCaptures::new();
    for (index, capture) in captures
        .iter()
        .enumerate()
        .filter(|(_, capture)| &capture.probe.source == source)
    {
        let probe = &capture.probe;
        let rows = &capture.receipt.events;
        let values = match probe.kind.as_str() {
            "population" | "tag-batch" => {
                let mut counts = BTreeMap::new();
                population_counts(probe, rows, &mut counts)?;
                counts
                    .into_iter()
                    .map(|(field, (count, total))| {
                        // A tag batch column is a population rate only for a selected field that
                        // no residual population search counted over another window.
                        let rate = probe.kind == "population"
                            || selected.is_some_and(|selected| selected.contains(&field))
                                && !counted.contains(field.as_str());
                        (field, count > 0, rate.then(|| count as f64 / total as f64))
                    })
                    .collect::<Vec<_>>()
            }
            "volume" if type_from_volume => vec![("#type".into(), count_row(rows)? > 0, None)],
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
            // Check equal-time contradictions per (field, concrete window, retrieval instant)
            // before winner reduction so an intervening narrower capture cannot mask a tie.
            if let Some((seen_observed, seen_rate)) = seen_by_key.get_mut(&(field.clone(), key)) {
                if observed != *seen_observed || rate.zip(*seen_rate).is_some_and(|(a, b)| a != b) {
                    return Err(fail(
                        "Tied field measurements contain contradictory presence or population rates.",
                    ));
                }
                if seen_rate.is_none() {
                    *seen_rate = rate;
                }
            } else {
                seen_by_key.insert((field.clone(), key), (observed, rate));
            }
            if let Some(&(previous, previous_observed, previous_rate)) = result.get(&field) {
                let previous_key = capture_key(&captures[previous])?;
                let keep = if current_plan {
                    keeps_previous(
                        (&captures[previous], previous_key, previous_observed),
                        (capture, key, observed),
                    )
                } else {
                    key < previous_key
                        || key == previous_key && (rate.is_none() || previous_rate.is_some())
                };
                if keep {
                    continue;
                }
            }
            result.insert(field, (index, observed, rate));
        }
    }
    Ok(result)
}

type Candidate<'a> = (
    &'a Capture,
    (
        time::OffsetDateTime,
        time::OffsetDateTime,
        time::OffsetDateTime,
    ),
    bool,
);

/// Current-plan precedence between two non-contradictory-at-tie measurements of one field.
fn keeps_previous(previous: Candidate, new: Candidate) -> bool {
    let (
        (p, (p_end, p_start, p_retrieved), p_observed),
        (n, (n_end, n_start, n_retrieved), n_observed),
    ) = (previous, new);
    let counts = |c: &Capture| matches!(c.probe.kind.as_str(), "population" | "tag-batch");
    if (p_end, p_start) == (n_end, n_start) {
        if p_retrieved == n_retrieved {
            // Prefer a count over presence alone.
            return counts(p) || !counts(n);
        }
        // Within one window a later count replaces the earlier measurement; a later presence-only
        // facet does so only when it contradicts it, which also drops the earlier rate.
        let new_later = n_retrieved > p_retrieved;
        let later = if new_later { n } else { p };
        let later_wins = counts(later) || p_observed != n_observed;
        return later_wins != new_later;
    }
    let new_inside = p_start <= n_start && n_end <= p_end;
    let previous_inside = n_start <= p_start && p_end <= n_end;
    // A selected population count over its window outranks a tag batch column over a narrower one.
    if p.probe.kind == "population" && n.probe.kind == "tag-batch" && new_inside {
        return true;
    }
    if n.probe.kind == "population" && p.probe.kind == "tag-batch" && previous_inside {
        return false;
    }
    // A zero in a narrower window never hides presence seen over the wider one.
    if new_inside && p_observed && !n_observed {
        return true;
    }
    if previous_inside && n_observed && !p_observed {
        return false;
    }
    (n_end, n_start) < (p_end, p_start)
}

/// Each logically selected field with its concrete population window: the sample window for a
/// field the sample listing named, the full window otherwise.
fn selected_windows(
    r: &DiscoverRequest,
    measured: &SourceMeasurements,
) -> Result<Vec<(String, Window, PopulationWindow)>, Diagnostic> {
    let (sample, full) = (window(r, true)?, window(r, false)?);
    Ok(measured
        .fields
        .iter()
        .filter(|field| !measured.full_fields.contains(*field))
        .map(|field| (field.clone(), sample.clone(), PopulationWindow::Sample))
        .chain(
            measured
                .full_fields
                .iter()
                .map(|field| (field.clone(), full.clone(), PopulationWindow::Full)),
        )
        .collect())
}

/// The tag batch count of a selected field, when the batch counted that exact namespaced field
/// over that exact window. The batch shares the source selector and view by construction.
fn reused_count(
    measured: &SourceMeasurements,
    field: &str,
    concrete: &Window,
) -> Option<(u64, u64)> {
    (measured.tag_window.as_ref() == Some(concrete))
        .then(|| measured.tags.get(field).copied())
        .flatten()
}

/// Residual population searches for the logically selected fields that the tag batch did not
/// already count, grouped by concrete window before chunking, so equal sample and full windows
/// share one search and an empty residual takes none.
fn selected_population_probes(
    r: &DiscoverRequest,
    s: &Scope,
    measured: &SourceMeasurements,
) -> Result<Vec<Probe>, Diagnostic> {
    let mut buckets: Vec<(Window, PopulationWindow, Vec<String>)> = Vec::new();
    for (field, concrete, nominal) in selected_windows(r, measured)? {
        if reused_count(measured, &field, &concrete).is_some() {
            continue;
        }
        match buckets
            .iter_mut()
            .find(|(window, _, _)| *window == concrete)
        {
            Some((_, _, fields)) => fields.push(field),
            None => buckets.push((concrete, nominal, vec![field])),
        }
    }
    let mut probes = Vec::new();
    for (_, nominal, fields) in buckets {
        probes.extend(population_probes_in(r, s, "population", &fields, nominal)?);
    }
    Ok(probes)
}

/// Add the selected fields' reused tag batch counts to their population measurements.
fn reuse_tag_counts(
    r: &DiscoverRequest,
    measured: &mut SourceMeasurements,
) -> Result<(), Diagnostic> {
    for (field, concrete, _) in selected_windows(r, measured)? {
        if let Some(count) = reused_count(measured, &field, &concrete) {
            measured.population.insert(field, count);
        }
    }
    Ok(())
}

fn sampled_measurements(
    r: &DiscoverRequest,
    s: &Scope,
    fields: Vec<String>,
    full_window: bool,
) -> SourceMeasurements {
    let fieldset_seen = fields.iter().cloned().collect::<BTreeSet<_>>();
    let selected = field_selection(r, s).unwrap_or(&fields).to_vec();
    let full_fields = selected
        .iter()
        .filter(|field| full_window || !fieldset_seen.contains(*field))
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
    Complete(Box<SourceMeasurements>),
}

// Both relay planning and execution follow this dependency graph:
// base -> tag batch -> kind facets -> rare-kind fieldsets -> selected population -> values.
fn stratified_source(
    r: &DiscoverRequest,
    s: &Scope,
    sampled: &[String],
    full_window: bool,
    dataset_rows: &[Value],
    answered: &BTreeMap<String, Vec<Value>>,
) -> Result<SourceProgress, Diagnostic> {
    let limit = r
        .max_population_fields_per_source
        .expect("stratified discovery has a population cap");
    let tags = tag_probes(r, s, full_window)?;
    let Some(tag_counts) = tag_counts(r, &tags, answered)? else {
        return Ok(SourceProgress::Pending(unanswered(r, &tags, answered)));
    };
    let mut measured = SourceMeasurements {
        tags: tag_counts,
        tag_window: Some(window(r, !full_window)?),
        ..SourceMeasurements::default()
    };
    let kinds = record_kind_probes(r, s, sampled, &measured.tags, answered)?;
    let pending = unanswered(r, &kinds, answered);
    if !pending.is_empty() {
        return Ok(SourceProgress::Pending(pending));
    }
    let mut facets = kind_facets(r, s, sampled, &measured.tags, answered)?
        .into_iter()
        .map(|facet| (facet.field.clone(), facet))
        .collect::<BTreeMap<_, _>>();
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
    if full_window {
        rare_fields.extend(sampled.iter().cloned());
    }
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
    let sampled = sampled
        .iter()
        .filter(|_| !full_window)
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut observed = sampled
        .union(&rare_fields)
        .cloned()
        .collect::<BTreeSet<_>>();
    observed.extend(measured.kind_fields.iter().cloned());
    observed.extend(
        measured
            .tags
            .iter()
            .filter(|(_, (count, _))| *count > 0)
            .map(|(field, _)| field.clone()),
    );
    let mandatory = validate_mandatory_population(r, s, &observed)?;
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
    reuse_tag_counts(r, &mut measured)?;
    Ok(SourceProgress::Complete(Box::new(measured)))
}
/// Search counts stated before the first answer, counted with the builders execution uses. A
/// quiet source costs its volume search alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedSearches {
    /// The fewest searches of a run that activates: one source live on its cheapest path, every
    /// other source quiet.
    pub low: u32,
    /// The fewest searches with which every selected source completes live; `prepare-discovery`
    /// refuses a smaller query limit.
    pub minimum_all_live: u32,
    /// The most searches any run of the request can take, whatever the limit.
    pub high: u32,
}
/// A census names its own sources, so it has no figures.
pub fn expected_searches(r: &DiscoverRequest) -> Option<ExpectedSearches> {
    if r.census || r.sources.is_empty() {
        return None;
    }
    let (mut cheapest, mut all_live, mut high) = (usize::MAX, 0, 0);
    for s in &r.sources {
        let (low, most) = live_source_searches(r, s).ok()?;
        cheapest = cheapest.min(low);
        all_live += low;
        high += most;
    }
    let count = |searches: usize| u32::try_from(searches).unwrap_or(u32::MAX);
    Some(ExpectedSearches {
        low: count(r.sources.len() - 1 + cheapest),
        minimum_all_live: count(all_live),
        high: count(high),
    })
}
/// The cheapest and the most expensive successful path of one source with events. Every branch
/// is counted on one path: a fieldset fallback puts every field on the full window, a facet is
/// counted only when a successful run can issue it, and rare-kind listings only when a
/// discriminator can legally hold several values.
fn live_source_searches(r: &DiscoverRequest, s: &Scope) -> Result<(usize, usize), Diagnostic> {
    if r.depth == DiscoveryDepth::SourcesOnly {
        return Ok((1, 1));
    }
    let tags = tag_probes(r, s, false)?;
    let fixed = base_probes(r, s)?.len() + tags.len() + r.value_fields.len();
    let tagged: BTreeSet<&str> = tags
        .iter()
        .flat_map(|p| p.fields.iter().map(String::as_str))
        .collect();
    // Over a window longer than the sample window, a listed field and an unlisted one are counted
    // over different windows, and an empty sample listing takes the full-window fallback.
    let split = window(r, true)? != window(r, false)?;
    let chunks = |fields: usize| fields.div_ceil(POPULATION_CHUNK);
    let singleton = literal_type(s)?.is_some();
    let capped = r.max_population_fields_per_source;
    let selection = field_selection(r, s);
    // Candidates whose facet is a search of its own whenever it is measured.
    let facets: Vec<&str> = record_kind_candidates(r)
        .into_iter()
        .filter(|field| capped.is_none() || *field != "#event.dataset")
        .filter(|field| !(singleton && *field == "#type"))
        .collect();
    // A capped run fails when an observed requested kind or value field is left out.
    let mandatory: BTreeSet<&str> = r
        .kind_fields
        .iter()
        .chain(&r.value_fields)
        .map(String::as_str)
        .collect();
    let legal = |field: &str| match (capped, selection) {
        (Some(_), Some(selected)) => {
            !mandatory.contains(field) || selected.iter().any(|name| name == field)
        }
        _ => true,
    };
    let most_facets = match (capped, selection) {
        (Some(cap), None) => {
            let required = facets.iter().filter(|f| mandatory.contains(**f)).count();
            facets.len() - required + required.min(cap)
        }
        _ => facets.iter().filter(|field| legal(field)).count(),
    };
    let rare = if capped.is_some()
        && record_kind_candidates(r)
            .into_iter()
            .any(|field| !(singleton && field == "#type") && legal(field))
    {
        RARE_KIND_LIMIT
    } else {
        0
    };
    let (fewest, most) = match selection {
        // The listing decides the fields: one tag listed alone costs nothing more, and the
        // fallback listing puts the most fields on one window at one more search.
        None => (
            0,
            usize::from(split) + chunks(capped.unwrap_or(MAX_FIELDS_PER_SOURCE)),
        ),
        Some(selected) => {
            let (tags, plain): (Vec<&String>, Vec<&String>) = selected
                .iter()
                .partition(|field| tagged.contains(field.as_str()));
            if split {
                // A tag listed in the sample window reuses its batch count; an unlisted one is
                // counted again over the full window, and so is a plain field.
                let fallback = 1 + chunks(plain.len());
                let most = (0..=plain.len())
                    .map(|listed| chunks(listed) + chunks(plain.len() - listed + tags.len()))
                    .max()
                    .unwrap_or(0);
                // A listed kind candidate takes its facet; left unlisted it is counted over the
                // full window instead. A capped run's dataset facet and a Full one with a short
                // dataset listing come from the base measurement.
                let kinds = tags
                    .iter()
                    .filter(|field| {
                        facets.contains(&field.as_str()) && field.as_str() != "#event.dataset"
                    })
                    .count();
                let plain_kinds = plain
                    .iter()
                    .filter(|field| facets.contains(&field.as_str()))
                    .count();
                let others = plain.len() - plain_kinds;
                let fewest = (0..=others)
                    .flat_map(|listed| {
                        (0..=kinds).map(move |faceted| {
                            faceted
                                + chunks(listed)
                                + chunks(others - listed + plain_kinds + kinds - faceted)
                        })
                    })
                    .min()
                    .unwrap_or(0);
                (fewest.min(fallback), most.max(fallback))
            } else {
                (chunks(plain.len()), chunks(plain.len()))
            }
        }
    };
    Ok((fixed + fewest, fixed + most_facets + rare + most))
}
/// The figures of a checkpoint of the earlier plan, which counted each tag candidate in its own
/// search. The earlier graph is not executed.
pub(crate) fn earlier_plan_searches(r: &DiscoverRequest) -> Option<(u32, Option<u32>)> {
    if r.census {
        return None;
    }
    let (mut low, mut high) = (0, Some(0));
    for s in &r.sources {
        if r.depth == DiscoveryDepth::SourcesOnly {
            low += 1;
            high = high.map(|high| high + 1);
            continue;
        }
        let tag_searches = tag_probes(r, s, false)
            .ok()?
            .iter()
            .map(|p| p.fields.len())
            .sum::<usize>();
        let fixed = base_probes(r, s).ok()?.len() + tag_searches + r.value_fields.len();
        // Fields are measured in chunks, over the sample window, the whole window or both.
        let selected = field_selection(r, s).map(|fields| fields.len().max(1));
        low += fixed + selected.unwrap_or(1).div_ceil(POPULATION_CHUNK);
        high = match (high, selected.or(r.max_population_fields_per_source)) {
            (Some(high), Some(fields)) => {
                // A capped run takes its dataset facet from the base measurement.
                let kinds = record_kind_candidates(r)
                    .into_iter()
                    .filter(|field| {
                        r.max_population_fields_per_source.is_none() || *field != "#event.dataset"
                    })
                    .count();
                // One more search when the sample window lists no fields, then every kind
                // facet and rare-kind field listing.
                Some(
                    high + fixed
                        + 1
                        + kinds
                        + RARE_KIND_LIMIT
                        + fields.div_ceil(POPULATION_CHUNK)
                        + 1,
                )
            }
            _ => None,
        };
    }
    let count = |searches: usize| u32::try_from(searches).unwrap_or(u32::MAX);
    Some((count(low), high.map(count)))
}

/// List the reachable, unanswered measurements using the same builders as execution.
pub fn frontier(
    r: &DiscoverRequest,
    answered: &BTreeMap<String, Vec<Value>>,
) -> Result<Vec<Probe>, Diagnostic> {
    frontier_with_policy(r, answered, true)
}

/// Sources whose accepted volume answer is zero, in request order. Only the exact volume probe
/// of each source, over the request window, counts.
pub fn quiet_sources(
    r: &DiscoverRequest,
    answered: &BTreeMap<String, Vec<Value>>,
) -> Result<Vec<Scope>, Diagnostic> {
    let mut sources = r.sources.clone();
    if r.census {
        let Some(rows) = answered.get(&probe_id(&r.environment.view, &census_probe(r)?)) else {
            return Ok(vec![]);
        };
        census_sources(rows, &mut sources)?;
    }
    let mut quiet = Vec::new();
    for s in sources {
        if let Some(rows) = answered.get(&probe_id(&r.environment.view, &volume_probe(r, &s)?))
            && count_row(rows)? == 0
        {
            quiet.push(s);
        }
    }
    Ok(quiet)
}

pub(crate) fn frontier_with_policy(
    r: &DiscoverRequest,
    answered: &BTreeMap<String, Vec<Value>>,
    sparse_fieldset_fallback: bool,
) -> Result<Vec<Probe>, Diagnostic> {
    validate_request(r)?;
    let id = |p: &Probe| probe_id(&r.environment.view, p);
    let mut sources = r.sources.clone();
    if r.census {
        let p = census_probe(r)?;
        let Some(rows) = answered.get(&id(&p)) else {
            return Ok(vec![p]);
        };
        census_sources(rows, &mut sources)?;
    }
    if sources.len() > 100 {
        return Err(fail(
            "More than 100 sources require an explicitly narrowed discovery run.",
        ));
    }
    // Every source's volume comes first, so a quiet source costs one search and nothing more.
    let mut pending = Vec::new();
    let mut live = Vec::new();
    for s in &sources {
        validate_scope(s)?;
        let p = volume_probe(r, s)?;
        match answered.get(&id(&p)) {
            Some(rows) if count_row(rows)? == 0 => {}
            Some(_) => live.push(s.clone()),
            None => pending.push(p),
        }
    }
    if !pending.is_empty() {
        return Ok(pending);
    }
    if live.is_empty() {
        return Err(fail(ALL_QUIET_SOURCES));
    }
    if r.depth == DiscoveryDepth::SourcesOnly {
        return Ok(pending);
    }
    // Then every live source's listings, so an empty one fails before dependent searches.
    let mut listed = Vec::new();
    for s in &live {
        let base = base_probes(r, s)?;
        if let Some(rows) = answered.get(&id(&base[1])) {
            kind_facet(rows, "#event.dataset", None)?;
        }
        pending.extend(unanswered(r, &base[1..], answered));
        let Some(mut fields) = answered
            .get(&id(&base[2]))
            .map(|rows| fieldset_fields(rows))
            .transpose()?
        else {
            continue;
        };
        let mut full_window = false;
        if sparse_fieldset_fallback && fields.is_empty() {
            let fallback = sparse_fieldset_probe(r, s)?;
            let Some(rows) = answered.get(&id(&fallback)) else {
                pending.push(fallback);
                continue;
            };
            fields = fieldset_fields(rows)?;
            full_window = true;
        }
        if fields.is_empty() {
            return Err(fail(EMPTY_FIELD_LISTING));
        }
        let mut known_observed: BTreeSet<String> = fields.iter().cloned().collect();
        if let Some(rows) = answered.get(&id(&base[1]))
            && record_kind_candidates(r).contains(&"#event.dataset")
            && kind_facet(rows, "#event.dataset", None)?
                .values
                .iter()
                .any(|(_, count)| *count > 0)
        {
            known_observed.insert("#event.dataset".into());
        }
        validate_mandatory_population(r, s, &known_observed)?;
        listed.push((s, base, fields, full_window));
    }
    if !pending.is_empty() {
        return Ok(pending);
    }
    for (s, base, fields, full_window) in listed {
        if r.max_population_fields_per_source.is_some() {
            match stratified_source(
                r,
                s,
                &fields,
                full_window,
                &answered[&id(&base[1])],
                answered,
            )? {
                SourceProgress::Pending(probes) => pending.extend(probes),
                SourceProgress::Complete(_) => {
                    pending.extend(unanswered(r, &value_probes(r, s, full_window)?, answered));
                }
            }
            continue;
        }
        // The tag batch answers before population, so no tag column is counted twice.
        let tags = tag_probes(r, s, full_window)?;
        let Some(tag_counts) = tag_counts(r, &tags, answered)? else {
            pending.extend(unanswered(r, &tags, answered));
            continue;
        };
        let mut measured = sampled_measurements(r, s, fields.clone(), full_window);
        measured.tags = tag_counts;
        measured.tag_window = Some(window(r, !full_window)?);
        let pop_probes = selected_population_probes(r, s, &measured)?;
        let mut dependent = pop_probes.clone();
        dependent.extend(record_kind_probes(r, s, &fields, &measured.tags, answered)?);
        if unanswered(r, &pop_probes, answered).is_empty() {
            dependent.extend(value_probes(r, s, full_window)?);
        }
        pending.extend(unanswered(r, &dependent, answered));
    }
    Ok(pending)
}

fn validate_mandatory_population(
    r: &DiscoverRequest,
    s: &Scope,
    observed: &BTreeSet<String>,
) -> Result<BTreeSet<String>, Diagnostic> {
    let Some(limit) = r.max_population_fields_per_source else {
        return Ok(BTreeSet::new());
    };
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
    Ok(mandatory)
}

fn sparse_fieldset_probe(r: &DiscoverRequest, s: &Scope) -> Result<Probe, Diagnostic> {
    if environment::parse_time(&r.end)? - environment::parse_time(&r.start)?
        <= time::Duration::minutes(15)
    {
        return Err(fail(EMPTY_FIELD_LISTING));
    }
    probe(r, s, "fieldset", "fieldset()", false, vec![], None)
}
fn volume_probe(r: &DiscoverRequest, s: &Scope) -> Result<Probe, Diagnostic> {
    probe(r, s, "volume", "count(as=events)", false, vec![], None)
}
fn base_probes(r: &DiscoverRequest, s: &Scope) -> Result<Vec<Probe>, Diagnostic> {
    if r.depth == DiscoveryDepth::SourcesOnly {
        return Ok(vec![volume_probe(r, s)?]);
    }
    Ok(vec![
        volume_probe(r, s)?,
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
    sparse_fieldset_fallback: bool,
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
        let capture = self.report.captures.push_mut(Capture {
            probe,
            retrieved_at: receipt
                .job_id
                .as_deref()
                .and_then(|job| self.backend.observed_at(job))
                .unwrap_or_else(timestamp),
            receipt,
        });
        let receipt = &capture.receipt;
        self.backend.checkpoint(
            &self.request.environment.view,
            receipt,
            &capture.retrieved_at,
        )?;
        if receipt.completion == "complete" && receipt.cleanup_complete() {
            check_measurement(&capture.probe, &receipt.events)?;
            capture_key(capture)?;
            Ok(receipt.events.clone())
        } else {
            Err(receipt
                .diagnostic
                .clone()
                .unwrap_or_else(|| fail("Incomplete search evidence or unconfirmed search cleanup; no further measurement or pack activation is allowed.")))
        }
    }
    fn measure_source(
        &mut self,
        source: &Scope,
        fields: Vec<String>,
        full_window: bool,
        mut answered: BTreeMap<String, Vec<Value>>,
    ) -> Result<SourceMeasurements, Diagnostic> {
        if self.request.max_population_fields_per_source.is_some() {
            let dataset_probe = &base_probes(self.request, source)?[1];
            let dataset_rows =
                answered[&probe_id(&self.request.environment.view, dataset_probe)].clone();
            loop {
                match stratified_source(
                    self.request,
                    source,
                    &fields,
                    full_window,
                    &dataset_rows,
                    &answered,
                )? {
                    SourceProgress::Complete(measured) => return Ok(*measured),
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
        let mut measured = sampled_measurements(self.request, source, fields, full_window);
        for p in tag_probes(self.request, source, full_window)? {
            let rows = self.execute(p.clone())?;
            population_counts(&p, &rows, &mut measured.tags)?;
        }
        measured.tag_window = Some(window(self.request, !full_window)?);
        for p in selected_population_probes(self.request, source, &measured)? {
            let rows = self.execute(p.clone())?;
            population_counts(&p, &rows, &mut measured.population)?;
        }
        reuse_tag_counts(self.request, &mut measured)?;
        let sampled = measured.fieldset_seen.iter().cloned().collect::<Vec<_>>();
        for p in record_kind_probes(self.request, source, &sampled, &measured.tags, &answered)? {
            let id = probe_id(&self.request.environment.view, &p);
            let rows = self.execute(p)?;
            answered.insert(id, rows);
        }
        for facet in kind_facets(self.request, source, &sampled, &measured.tags, &answered)? {
            measured.record_kinds.push(facet.metadata());
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
        // A source whose volume is zero takes no further search and stays out of the pack.
        let (mut live, mut volumes) = (Vec::new(), Vec::new());
        for s in &sources {
            validate_scope(s)?;
            let rows = self.execute(volume_probe(self.request, s)?)?;
            if count_row(&rows)? > 0 {
                live.push(s.clone());
                volumes.push(rows);
            }
        }
        if live.is_empty() {
            return Err(fail(ALL_QUIET_SOURCES));
        }
        let sources = live;
        // Every live source is listed before any source's dependent measurements.
        let mut listings = Vec::new();
        for (s, volume) in sources.iter().zip(volumes) {
            let mut datasets = Vec::new();
            let mut dataset_observed = false;
            let mut fields = Vec::new();
            let mut answered = BTreeMap::new();
            answered.insert(
                probe_id(
                    &self.request.environment.view,
                    &volume_probe(self.request, s)?,
                ),
                volume,
            );
            for p in base_probes(self.request, s)?.into_iter().skip(1) {
                let kind = p.kind.clone();
                let id = probe_id(&self.request.environment.view, &p);
                let rows = self.execute(p)?;
                match kind.as_str() {
                    "dataset-facet" => {
                        let facet = kind_facet(&rows, "#event.dataset", None)?;
                        dataset_observed = facet.values.iter().any(|(_, count)| *count > 0);
                        datasets = facet.values.into_iter().map(|(value, _)| value).collect();
                    }
                    "fieldset" => {
                        fields = fieldset_fields(&rows)?;
                    }
                    _ => unreachable!(),
                }
                answered.insert(id, rows);
            }
            let mut full_window = false;
            if self.sparse_fieldset_fallback
                && fields.is_empty()
                && self.request.depth != DiscoveryDepth::SourcesOnly
            {
                let p = sparse_fieldset_probe(self.request, s)?;
                let id = probe_id(&self.request.environment.view, &p);
                let rows = self.execute(p)?;
                fields = fieldset_fields(&rows)?;
                answered.insert(id, rows);
                full_window = true;
            }
            if fields.is_empty() && self.request.depth != DiscoveryDepth::SourcesOnly {
                return Err(fail(EMPTY_FIELD_LISTING));
            }
            if self.request.depth != DiscoveryDepth::SourcesOnly {
                let mut known_observed: BTreeSet<String> = fields.iter().cloned().collect();
                if dataset_observed
                    && record_kind_candidates(self.request).contains(&"#event.dataset")
                {
                    known_observed.insert("#event.dataset".into());
                }
                validate_mandatory_population(self.request, s, &known_observed)?;
            }
            listings.push((datasets, fields, full_window, answered));
        }
        let mut data = PackData::default();
        for (s, (datasets, fields, full_window, answered)) in sources.into_iter().zip(listings) {
            let SourceMeasurements {
                fields,
                fieldset_seen,
                population,
                record_kinds,
                ..
            } = if self.request.depth == DiscoveryDepth::SourcesOnly {
                sampled_measurements(self.request, &s, vec![], false)
            } else {
                self.measure_source(&s, fields, full_window, answered)?
            };
            let sid = source_id(&s);
            let source_window = format!("{}/{}", self.request.start, self.request.end);
            let selected = population.keys().cloned().collect();
            let current_plan = source_uses_tag_batch_policy(&self.report.captures, &s);
            let measurements =
                field_captures_for(&self.report.captures, &s, Some(&selected), current_plan)?;
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
            let mut source = json!({"id":sid,"kind":if s.repo.is_some(){"repo"}else{"source"},"name":s.type_.as_ref().or(s.repo.as_ref()).unwrap(),"inventory_status":"observed-only","selectors":{"repo":s.repo,"type":s.type_},"relationship":{"datasets":datasets},"evidence":{"declared":contract.is_some(),"observed":true,"observation_window":source_window},"provenance_ids":[source_pid]});
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
                                    .any(|row| fieldset_name(row) == Some(original.as_str())))
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
            for p in value_probes(self.request, &s, full_window)? {
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
    execute_with_policy(request, backend, origin, remaining, true)
}

pub(crate) fn execute_with_policy(
    request: &DiscoverRequest,
    backend: &mut impl SearchBackend,
    origin: Origin,
    remaining: Duration,
    sparse_fieldset_fallback: bool,
) -> (Option<Pack>, DiscoverReport) {
    let mut run = Run {
        backend,
        request,
        deadline: Instant::now() + remaining,
        origin,
        sparse_fieldset_fallback,
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
    fn capture(
        kind: &str,
        start: &str,
        fields: &[&str],
        field: Option<&str>,
        row: Value,
    ) -> Capture {
        let probe = Probe {
            kind: kind.into(),
            source: Scope {
                repo: Some("synthetic".into()),
                ..Scope::default()
            },
            query: format!("{kind} {start}"),
            start: start.into(),
            end: "2026-01-01T01:00:00Z".into(),
            fields: fields.iter().map(|field| field.to_string()).collect(),
            field: field.map(str::to_string),
        };
        Capture {
            receipt: JobReceipt {
                query: probe.query.clone(),
                start: probe.start.clone(),
                end: probe.end.clone(),
                job_id: Some("job".into()),
                completion: "complete".into(),
                cleanup: "confirmed".into(),
                polls: 1,
                events: vec![row],
                metadata: None,
                issues: BTreeMap::new(),
                diagnostic: None,
            },
            probe,
            retrieved_at: "2026-02-01T00:00:00Z".into(),
        }
    }
    #[test]
    fn earlier_plan_receipts_keep_their_ordering() {
        let source = Scope {
            repo: Some("synthetic".into()),
            ..Scope::default()
        };
        let full = capture(
            "population",
            "2026-01-01T00:00:00Z",
            &["#Vendor"],
            None,
            json!({"total":10,"c0":4}),
        );
        // An earlier-plan tag check over the recent window still decides presence.
        let detect = capture(
            "tag-detect",
            "2026-01-01T00:45:00Z",
            &[],
            Some("Vendor"),
            json!({"events":0}),
        );
        let earlier = field_captures(&[full.clone(), detect], &source).unwrap();
        assert_eq!(earlier["#Vendor"], (1, false, None));
        // The current plan's tag batch over the same recent window does not override the
        // full-window population count.
        let batch = capture(
            "tag-batch",
            "2026-01-01T00:45:00Z",
            &["#Vendor"],
            None,
            json!({"total":3,"c0":0}),
        );
        let current = field_captures(&[full, batch], &source).unwrap();
        assert_eq!(current["#Vendor"], (0, true, Some(0.4)));
    }
    #[test]
    fn missing_count_is_not_zero() {
        assert!(count_row(&[]).is_err());
        assert_eq!(count_row(&[json!({"events":"0"})]).unwrap(), 0);
    }
}
