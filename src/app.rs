//! Application use cases: explicit context in, structured assessments out.
use crate::{
    contracts::*,
    environment::{self, Observation, Pack, observation},
    inventory::{FieldObservation, QueryObservation, UnverifiedFieldLead},
    language::{self, ast::*},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
pub struct GroundingEvidence<'a> {
    pub environment: &'a EnvironmentId,
    pub active_pack: Option<&'a Pack>,
    pub query_observations: &'a [QueryObservation],
}

pub fn generate(request: GenerateRequest, pack: Option<&Pack>) -> Analysis {
    generate_with_evidence(request, pack, None)
}

pub fn generate_with_evidence(
    request: GenerateRequest,
    pack: Option<&Pack>,
    evidence: Option<GroundingEvidence<'_>>,
) -> Analysis {
    let mut result = Analysis::failed(
        request.id.clone(),
        Diagnostic::error("INTERNAL", "Analysis did not complete."),
    );
    result.grounding.policy = (&request.context).into();
    if request.schema_version != 1 {
        result.diagnostics = vec![Diagnostic::error(
            "REQUEST_VERSION",
            "Only schema_version=1 is supported.",
        )];
        return result;
    }
    let (path, plan) = match &request.input {
        GenerateInput::Plan { plan } => ("plan", Ok(plan.clone())),
        GenerateInput::Recipe { request } => ("recipe", Ok(language::plan::recipe(request))),
        GenerateInput::English { text, source } => {
            ("english", language::english::interpret(text, source))
        }
    };
    result.interpretation.path = path.into();
    result.diagnostics.clear();
    let plan = match plan {
        Ok(p) => p,
        Err(d) => {
            result.outcome = if d.is_unsupported() {
                Outcome::Unsupported
            } else {
                Outcome::NeedsContext
            };
            result.diagnostics.push(d);
            return result;
        }
    };
    result.interpretation.plan = Some(plan.clone());
    let pipeline = match language::plan::lower(&plan) {
        Ok(q) => q,
        Err(d) => {
            result.outcome = if d.is_unsupported() {
                Outcome::Unsupported
            } else {
                Outcome::Invalid
            };
            result.validation.status = if result.outcome == Outcome::Unsupported {
                Acceptance::Unknown
            } else {
                Acceptance::Rejected
            };
            result.diagnostics.push(d);
            return result;
        }
    };
    let query = pipeline.render();
    let parsed = match language::parser::parse(&query) {
        Ok(q) => q,
        Err(e) => {
            result.outcome = if e.unsupported {
                Outcome::Unsupported
            } else {
                Outcome::Invalid
            };
            result.validation.status = if e.unsupported {
                Acceptance::Unknown
            } else {
                Acceptance::Rejected
            };
            result.diagnostics.push(e.diagnostic);
            return result;
        }
    };
    finish(
        result,
        &parsed,
        &query,
        &request.context,
        pack,
        evidence,
        true,
    )
}
pub fn check(request: CheckRequest, pack: Option<&Pack>) -> Analysis {
    check_with_evidence(request, pack, None)
}

pub fn check_with_evidence(
    request: CheckRequest,
    pack: Option<&Pack>,
    evidence: Option<GroundingEvidence<'_>>,
) -> Analysis {
    let mut result = Analysis::failed(
        request.id,
        Diagnostic::error("INTERNAL", "Analysis did not complete."),
    );
    result.grounding.policy = (&request.context).into();
    if request.schema_version != 1 {
        result.diagnostics = vec![Diagnostic::error(
            "REQUEST_VERSION",
            "Only schema_version=1 is supported.",
        )];
        return result;
    }
    result.diagnostics.clear();
    result.interpretation.path = "check".into();
    match language::parser::parse(&request.query) {
        Ok(parsed) => finish(
            result,
            &parsed,
            &request.query,
            &request.context,
            pack,
            evidence,
            false,
        ),
        Err(e) => {
            result.outcome = if e.unsupported {
                Outcome::Unsupported
            } else {
                Outcome::Invalid
            };
            result.validation.status = if e.unsupported {
                Acceptance::Unknown
            } else {
                Acceptance::Rejected
            };
            result.diagnostics.push(e.diagnostic);
            result
        }
    }
}

fn inventory_requirements(query: &str) -> Result<Vec<Requirement>, Diagnostic> {
    let pipeline = language::parser::parse(query).map_err(|error| error.diagnostic)?;
    let report = language::validate::validate(&pipeline, query);
    if let Some(error) = report
        .diagnostics
        .into_iter()
        .find(|d| d.severity == Severity::Error)
    {
        return Err(error);
    }
    Ok(requirements(
        &pipeline,
        None,
        &Context::default(),
        &[],
        &mut Grounding::default(),
    ))
}

pub fn unverified_leads(query: &str) -> Result<Vec<UnverifiedFieldLead>, Diagnostic> {
    let mut leads = BTreeMap::new();
    for r in inventory_requirements(query)? {
        if !r.is_direct_deployment_input()
            || r.uncertain_source
            || r.target.is_some()
            || (r.scope.repo.is_none() && r.scope.type_.is_none())
        {
            continue;
        }
        let source = Scope {
            event: None,
            ..r.scope
        };
        let key = (source.repo.clone(), source.type_.clone(), r.field.clone());
        leads.insert(
            key,
            UnverifiedFieldLead {
                source,
                field: r.field,
            },
        );
        if leads.len() > 1000 {
            return Err(Diagnostic::error(
                "CQL_LEAD_BUDGET",
                "Query input fields exceed the 1000-lead inventory limit.",
            ));
        }
    }
    Ok(leads.into_values().collect())
}

pub fn uncredited_observation_fields(
    query: &str,
    fields: &[FieldObservation],
) -> Result<Vec<String>, Diagnostic> {
    let positive: BTreeSet<_> = fields
        .iter()
        .filter(|field| field.positive_rows > 0)
        .map(|field| field.field.as_str())
        .collect();
    let missing: BTreeSet<_> = inventory_requirements(query)?
        .into_iter()
        .filter(|r| {
            r.is_direct_deployment_input()
                && !r.array
                && r.target.is_none()
                && !positive.contains(r.field.as_str())
        })
        .map(|r| r.field)
        .collect();
    Ok(missing.into_iter().collect())
}

fn is_builtin_field(field: &str) -> bool {
    matches!(
        field,
        "#repo" | "#type" | "@timestamp" | "@rawstring" | "@timestamp.nanos"
    )
}

fn finish(
    mut result: Analysis,
    pipeline: &Pipeline,
    query: &str,
    context: &Context,
    pack: Option<&Pack>,
    evidence: Option<GroundingEvidence<'_>>,
    emit_query: bool,
) -> Analysis {
    let report = language::validate::validate(pipeline, query);
    result.validation.status = report.status.clone();
    result.diagnostics = report.diagnostics;
    result.grounding = ground(pipeline, context, pack, evidence);
    result.outcome = match report.status {
        Acceptance::Rejected => Outcome::Invalid,
        Acceptance::Unknown => Outcome::Unsupported,
        Acceptance::Accepted => {
            if !result.grounding.missing.is_empty()
                || !result.grounding.conflicting.is_empty()
                || !result.grounding.stale.is_empty()
            {
                Outcome::NeedsContext
            } else {
                Outcome::Ready
            }
        }
    };
    if result.outcome == Outcome::NeedsContext {
        result.diagnostics.push(Diagnostic::error("CQL_CONTEXT_REQUIRED","Resolve the missing, conflicting or stale grounding requirements; grounding.basis explains the evidence. Language acceptance remains separate."));
    }
    for (key, evidence) in &result.grounding.basis {
        if evidence
            .split(" | ")
            .any(|basis| basis.starts_with("caller-attested query observation;"))
        {
            result.diagnostics.push(Diagnostic::warning(
                "CQL_QUERY_OBSERVATION",
                format!("{key}: {evidence}; exact query/result/target/window binding is caller-attested, not independent execution attestation."),
            ));
        }
        if evidence
            .split(" | ")
            .any(|basis| basis.starts_with("observed with accepted partial coverage;"))
        {
            result.diagnostics.push(Diagnostic::warning(
                "CQL_PARTIAL_COVERAGE",
                format!(
                    "{key}: {evidence}; positive observations do not establish complete coverage."
                ),
            ));
        }
        if result.outcome == Outcome::Ready
            && !context.require_observed
            && !key.ends_with(":@record-kind")
            && evidence.split(" | ").any(|basis| {
                !matches!(
                    basis,
                    "query-produced" | "language-built-in" | "observed in a bounded sample"
                ) && !basis.starts_with("observed with accepted partial coverage;")
                    && !basis.starts_with("caller-attested query observation;")
            })
        {
            result.diagnostics.push(Diagnostic::warning(
                "CQL_GROUNDING_LENIENT",
                format!("{key}: {evidence}; ready relies on lenient grounding because require_observed is false."),
            ));
        }
        if key.ends_with(":@record-kind")
            && (evidence.starts_with("multiple record kinds observed (")
                || evidence.starts_with("record-kind facet truncated ("))
        {
            let message = format!("{key}: {evidence}");
            result.diagnostics.push(if !context.require_observed {
                let mut notice = Diagnostic::error("CQL_RECORD_KIND_NOTICE", message);
                notice.severity = Severity::Info;
                notice
            } else if context.accept_mixed_record_kinds {
                Diagnostic::warning("CQL_RECORD_KIND_AMBIGUOUS", message)
            } else {
                Diagnostic::error("CQL_RECORD_KIND_AMBIGUOUS", message)
            });
        }
    }
    if result.outcome == Outcome::Ready && emit_query {
        result.query = Some(query.into());
    }
    result
}

#[derive(Clone)]
struct Requirement {
    target: Option<String>,
    array: bool,
    scope: Scope,
    field: String,
    derived: bool,
    conditional: bool,
    origin: Origin,
    uncertain_source: bool,
    relaxed_type: bool,
    relaxed_event: bool,
    unavailable: bool,
}
impl Requirement {
    fn is_direct_deployment_input(&self) -> bool {
        self.origin == Origin::Deployment
            && !self.derived
            && !self.conditional
            && !self.unavailable
            && !is_builtin_field(&self.field)
    }
}
struct GroundingScan<'a> {
    requirements: Vec<Requirement>,
    record_kind_context: Option<(&'a Context, Vec<&'a Pack>, &'a mut Grounding)>,
    first_pipeline: bool,
    skip_next_kind_check: bool,
}
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Origin {
    #[default]
    Deployment,
    Synthetic,
    External,
    Mixed,
}
#[derive(Clone)]
struct ElementBinding {
    array: String,
    structured: bool,
}
#[derive(Clone, Default)]
struct Flow {
    // Ambient execution target can differ from a named table's field provenance.
    target: Option<String>,
    field_target: Option<String>,
    scope: Scope,
    derived: BTreeSet<String>,
    definite: BTreeSet<String>,
    conditional: BTreeSet<String>,
    arrays: BTreeSet<String>,
    conditional_arrays: BTreeSet<String>,
    origin: Origin,
    uncertain_source: bool,
    relaxed_type: bool,
    relaxed_event: bool,
    tables: BTreeMap<String, Flow>,
    locals: BTreeMap<String, ElementBinding>,
    available: Option<BTreeSet<String>>,
    removed: BTreeSet<String>,
    split_unknown: BTreeSet<(String, bool)>,
    unverified_fields: BTreeSet<String>,
    unknown_enrichment: bool,
    unknown_prefixes: BTreeSet<String>,
}
impl Flow {
    fn known_columns(&self) -> Option<&BTreeSet<String>> {
        (self.arrays.is_empty()
            && self.conditional_arrays.is_empty()
            && self.split_unknown.is_empty()
            && self.unknown_prefixes.is_empty()
            && !self.unknown_enrichment)
            .then_some(())?;
        self.available.as_ref()
    }
    fn conditional(&self, field: &str) -> bool {
        if self.origin == Origin::Synthetic
            || self.derived.contains(field)
            || self.arrays.iter().any(|a| array_element(field, a))
        {
            return false;
        }
        self.conditional.contains(field)
            || self
                .conditional_arrays
                .iter()
                .any(|a| array_element(field, a))
    }
    fn produces(&self, field: &str) -> bool {
        self.origin == Origin::Synthetic
            || !self.conditional(field)
                && (self.derived.contains(field)
                    || self.arrays.iter().any(|a| array_element(field, a)))
    }
    fn unverified(&self, field: &str) -> bool {
        (self.unverified_fields.contains(field)
            || self.unknown_enrichment
            || self
                .unknown_prefixes
                .iter()
                .any(|prefix| field.starts_with(prefix))
            || self.split_unknown.iter().any(|(prefix, strip)| {
                *strip
                    || field
                        .strip_prefix(prefix)
                        .is_some_and(|tail| tail.starts_with('.') || tail.starts_with('['))
            }))
            && !self.produces(field)
    }
    fn absent(&self, field: &str) -> bool {
        self.removed.contains(field)
            || !self.unverified(field)
                && self.available.as_ref().is_some_and(|fields| {
                    !fields.contains(field)
                        && !self
                            .arrays
                            .iter()
                            .chain(&self.conditional_arrays)
                            .any(|a| array_element(field, a))
                })
    }
    fn array_absent(&self, array: &str) -> bool {
        !self.unknown_namespace(array)
            && !self
                .split_unknown
                .iter()
                .any(|(prefix, strip)| *strip || path_contains(prefix, array))
            && !self
                .arrays
                .iter()
                .chain(&self.conditional_arrays)
                .any(|a| array_covers(a, array) || array_covers(array, a))
            && self
                .available
                .as_ref()
                .is_some_and(|fields| !fields.iter().any(|f| indexed_member(f, array).is_some()))
    }
    fn array_produces(&self, array: &str) -> bool {
        if self.origin == Origin::Synthetic || self.arrays.iter().any(|a| array_covers(a, array)) {
            return true;
        }
        !self.unknown_namespace(array)
            && self.available.as_ref().is_some_and(|fields| {
                let members = fields
                    .iter()
                    .filter(|f| indexed_member(f, array).is_some())
                    .collect::<Vec<_>>();
                (!members.is_empty() || self.arrays.iter().any(|a| array_covers(array, a)))
                    && members.iter().all(|f| self.produces(f))
                    && !self
                        .conditional_arrays
                        .iter()
                        .any(|a| array_covers(array, a) || array_covers(a, array))
            })
    }
    fn member_produces(&self, member: &str) -> bool {
        self.origin == Origin::Synthetic
            || self.arrays.iter().any(|a| array_covers(a, member))
            || !self.unknown_namespace(member)
                && self.available.as_ref().is_some_and(|fields| {
                    let members = fields
                        .iter()
                        .filter(|f| indexed_member(f, member) == Some(""))
                        .collect::<Vec<_>>();
                    !members.is_empty() && members.iter().all(|f| self.produces(f))
                })
    }
    fn member_conditional(&self, member: &str) -> bool {
        !self.member_produces(member)
            && self
                .conditional_arrays
                .iter()
                .any(|a| array_covers(a, member))
    }
    fn unknown_namespace(&self, field: &str) -> bool {
        self.unknown_prefixes
            .iter()
            .any(|prefix| field.starts_with(prefix))
    }
    fn array_conditional(&self, array: &str) -> bool {
        !self.array_produces(array)
            && self
                .conditional_arrays
                .iter()
                .any(|a| array_covers(a, array) || array_covers(array, a))
    }
}
fn path_contains(parent: &str, field: &str) -> bool {
    field
        .strip_prefix(parent)
        .is_some_and(|tail| tail.is_empty() || tail.starts_with('.') || tail.starts_with('['))
}
// Match only [] dimensions; fixed parent and member indices remain literal.
fn indexed_member<'a>(field: &'a str, pattern: &str) -> Option<&'a str> {
    let mut rest = field;
    let mut pattern = pattern;
    let mut indexed = false;
    while let Some((prefix, suffix)) = pattern.split_once("[]") {
        rest = rest.strip_prefix(prefix)?.strip_prefix('[')?;
        let (index, tail) = rest.split_once(']')?;
        if index.is_empty() || !index.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        rest = tail;
        pattern = suffix;
        indexed = true;
    }
    let tail = rest.strip_prefix(pattern)?;
    (indexed && (tail.is_empty() || tail.starts_with('.') || tail.starts_with('['))).then_some(tail)
}

fn array_covers(array: &str, field: &str) -> bool {
    if array.ends_with("[]") {
        path_contains(array, field) || indexed_member(field, array).is_some()
    } else {
        array == field || indexed_member(field, array) == Some("")
    }
}

fn fields_of(v: &Value) -> Vec<String> {
    crate::catalog::fields(v).into_iter().collect()
}

fn independent(state: &Flow) -> Flow {
    Flow {
        target: state.target.clone(),
        field_target: state.target.clone(),
        tables: state.tables.clone(),
        ..Flow::default()
    }
}
fn query_context(state: &Flow, args: &BTreeMap<String, &Value>) -> Flow {
    let mut context = independent(state);
    if let Some(view) = args.get("view").and_then(|v| v.text()) {
        context.target = Some(view.into());
        context.field_target = Some(view.into());
    }
    context
}
fn define(state: &mut Flow, field: String) {
    state.conditional.remove(&field);
    state.unverified_fields.remove(&field);
    state.removed.remove(&field);
    if let Some(available) = &mut state.available {
        available.insert(field.clone());
    }
    state.derived.insert(field);
}
fn write(state: &mut Flow, field: String, definite: bool) {
    if definite {
        state.definite.insert(field.clone());
    } else {
        state.definite.remove(&field);
    }
    if definite || state.produces(&field) || state.absent(&field) {
        define(state, field);
    } else {
        state.derived.remove(&field);
        state.removed.remove(&field);
        if let Some(available) = &mut state.available {
            available.insert(field.clone());
        }
        state.conditional.insert(field);
    }
}
fn write_array(state: &mut Flow, array: String) {
    state.definite.retain(|field| !array_element(field, &array));
    let removed = state
        .removed
        .iter()
        .filter(|field| array_element(field, &array))
        .cloned()
        .collect::<Vec<_>>();
    for field in removed {
        define(state, field);
    }
    let produced = state.array_produces(&array) || state.array_absent(&array);
    state.arrays.remove(&array);
    state.conditional_arrays.remove(&array);
    if produced {
        state.arrays.insert(array);
    } else {
        state.conditional_arrays.insert(array);
    }
}
fn definite_value(value: &Value, state: &Flow) -> bool {
    match value {
        Value::Word(field) => {
            !state.locals.iter().any(|(binding, local)| {
                field == binding || local.structured && path_contains(binding, field)
            }) && state.definite.contains(field)
        }
        Value::String(_) | Value::Bool(_) => true,
        Value::Number(number) => number.parse::<serde_json::Number>().is_ok(),
        Value::Unary { op, value } if matches!(op.as_str(), "+" | "-") => {
            matches!(value.as_ref(), Value::Number(number) if number.parse::<serde_json::Number>().is_ok())
        }
        Value::Unary { op, value } if op == "!" => {
            matches!(value.as_ref(), Value::Bool(_))
        }
        _ => false,
    }
}
fn project(state: &mut Flow, fields: BTreeSet<String>) {
    let unverified = fields
        .iter()
        .filter(|field| state.unverified(field))
        .cloned()
        .collect();
    let derived = fields
        .iter()
        .filter(|field| state.produces(field))
        .cloned()
        .collect();
    let conditional = fields
        .iter()
        .filter(|field| state.conditional(field))
        .cloned()
        .collect();
    state.derived = derived;
    state.conditional = conditional;
    state.arrays.clear();
    state.conditional_arrays.clear();
    state.definite.retain(|field| fields.contains(field));
    state.available = Some(fields);
    state.split_unknown.clear();
    state.unknown_enrichment = false;
    state.unknown_prefixes.clear();
    state.unverified_fields = unverified;
}
fn split_members(state: &Flow, prefix: &str) -> BTreeSet<String> {
    let root = format!("{prefix}[]");
    state
        .available
        .iter()
        .flatten()
        .chain(&state.derived)
        .chain(&state.conditional)
        .filter(|f| array_element(f, &root))
        .cloned()
        .collect()
}
fn apply_split(state: &mut Flow, args: &BTreeMap<String, &Value>) {
    let prefix = args
        .get("field")
        .and_then(|v| v.text())
        .unwrap_or("_events");
    let strip = args.get("strip").copied() == Some(&Value::Bool(true));
    let root = format!("{prefix}[]");
    let members = split_members(state, prefix)
        .into_iter()
        .filter(|field| {
            !strip
                || field
                    .strip_prefix(prefix)
                    .and_then(|tail| tail.split_once(']'))
                    .is_some_and(|(_, tail)| !tail.is_empty())
        })
        .collect::<BTreeSet<_>>();
    let known = state.available.is_some()
        && !state
            .arrays
            .iter()
            .chain(&state.conditional_arrays)
            .any(|a| array_covers(&root, a) || array_covers(a, &root));
    if strip && known && members.is_empty() {
        return;
    }
    let outputs = members
        .iter()
        .filter_map(|field| {
            field
                .strip_prefix(prefix)
                .and_then(|s| s.strip_prefix('['))
                .and_then(|s| s.split_once(']'))
                .map(|(_, tail)| {
                    if strip && !tail.is_empty() {
                        tail.strip_prefix('.').unwrap_or(tail).to_string()
                    } else {
                        format!("{prefix}{tail}")
                    }
                })
        })
        .collect::<BTreeSet<_>>();
    for field in &members {
        state.derived.remove(field);
        state.conditional.remove(field);
        state.definite.remove(field);
        state.removed.insert(field.clone());
        if let Some(fields) = &mut state.available {
            fields.remove(field);
        }
    }
    state.arrays.retain(|a| !array_covers(&root, a));
    state.conditional_arrays.retain(|a| !array_covers(&root, a));
    if !known {
        state.split_unknown.insert((prefix.into(), strip));
        let affected = |field: &str| {
            strip
                || field
                    .strip_prefix(prefix)
                    .is_some_and(|tail| tail.starts_with('.') || tail.starts_with('['))
        };
        state.derived.retain(|f| !affected(f));
        state.definite.retain(|f| !affected(f));
        let arrays = state
            .arrays
            .iter()
            .filter(|a| affected(a))
            .cloned()
            .collect::<Vec<_>>();
        for array in arrays {
            state.arrays.remove(&array);
            state.conditional_arrays.insert(array);
        }
    }
    if !strip {
        write(state, prefix.into(), false);
    }
    for field in outputs {
        write(state, field, false);
    }
    write(state, "_index".into(), false);
}
fn requirements(
    p: &Pipeline,
    target: Option<String>,
    context: &Context,
    packs: &[&Pack],
    grounding: &mut Grounding,
) -> Vec<Requirement> {
    let mut out = GroundingScan {
        requirements: Vec::new(),
        record_kind_context: (!packs.is_empty()).then(|| (context, packs.to_vec(), grounding)),
        first_pipeline: true,
        skip_next_kind_check: false,
    };
    scan_pipeline(
        p,
        Flow {
            target: target.clone(),
            field_target: target,
            ..Flow::default()
        },
        &mut out,
    );
    out.requirements
}
fn array_element(field: &str, array: &str) -> bool {
    indexed_member(field, array).is_some_and(|tail| array.ends_with("[]") || tail.is_empty())
}

fn local_binding<'a>(state: &'a Flow, field: &str) -> Option<(&'a String, &'a ElementBinding)> {
    state
        .locals
        .iter()
        .filter(|(binding, local)| {
            field == binding.as_str() || local.structured && path_contains(binding, field)
        })
        .max_by_key(|(binding, _)| binding.len())
}
fn local_field(state: &Flow, field: &str) -> String {
    local_binding(state, field)
        .map(|(binding, local)| format!("{}{}", local.array, &field[binding.len()..]))
        .unwrap_or_else(|| field.into())
}
fn add(state: &Flow, field: &str, out: &mut GroundingScan<'_>) {
    if let Some((binding, local)) = local_binding(state, field) {
        let array = &local.array;
        let suffix = &field[binding.len()..];
        let mapped = format!("{array}{suffix}");
        let unavailable = state.array_absent(array)
            || !suffix.is_empty()
                && !state.unknown_namespace(&mapped)
                && state.available.as_ref().is_some_and(|fields| {
                    !state
                        .arrays
                        .iter()
                        .chain(&state.conditional_arrays)
                        .any(|a| array_covers(a, &mapped))
                        && !fields
                            .iter()
                            .any(|f| indexed_member(f, &mapped) == Some(""))
                });
        out.requirements.push(Requirement {
            target: state.field_target.clone(),
            array: suffix.is_empty(),
            scope: state.scope.clone(),
            field: mapped.clone(),
            derived: if suffix.is_empty() {
                state.array_produces(array)
            } else {
                state.member_produces(&mapped)
            },
            conditional: if suffix.is_empty() {
                state.array_conditional(array)
            } else {
                state.member_conditional(&mapped)
            },
            origin: state.origin,
            uncertain_source: state.uncertain_source
                || state.unverified(field)
                || state.unverified(&mapped),
            relaxed_type: state.relaxed_type,
            relaxed_event: state.relaxed_event,
            unavailable,
        });
        return;
    }
    out.requirements.push(Requirement {
        target: state.field_target.clone(),
        array: false,
        scope: state.scope.clone(),
        field: field.into(),
        derived: state.produces(field),
        conditional: state.conditional(field),
        origin: state.origin,
        uncertain_source: state.uncertain_source || state.unverified(field),
        relaxed_type: state.relaxed_type,
        relaxed_event: state.relaxed_event,
        unavailable: state.absent(field),
    });
}
fn add_array(state: &Flow, array: &str, out: &mut GroundingScan<'_>) {
    let field = local_field(state, array);
    out.requirements.push(Requirement {
        target: state.field_target.clone(),
        array: true,
        scope: state.scope.clone(),
        derived: state.array_produces(&field),
        conditional: state.array_conditional(&field),
        origin: state.origin,
        uncertain_source: state.uncertain_source || state.unverified(&field),
        relaxed_type: state.relaxed_type,
        relaxed_event: state.relaxed_event,
        unavailable: state.array_absent(&field),
        field,
    });
}

fn add_unknown(state: &Flow, field: &str, out: &mut GroundingScan<'_>) {
    out.requirements.push(Requirement {
        target: state.field_target.clone(),
        array: false,
        scope: state.scope.clone(),
        field: field.into(),
        derived: state.origin == Origin::Synthetic,
        conditional: false,
        origin: state.origin,
        uncertain_source: true,
        relaxed_type: state.relaxed_type,
        relaxed_event: state.relaxed_event,
        unavailable: false,
    });
}
fn expression_fields(v: &Value, state: &Flow, out: &mut GroundingScan<'_>) {
    match v {
        Value::Word(s) => add(state, s, out),
        Value::Binary { left, right, .. } => {
            expression_fields(left, state, out);
            expression_fields(right, state, out);
        }
        Value::Unary { value, .. } => expression_fields(value, state, out),
        Value::Call(c) => {
            scan_call(c, state, out, false);
        }
        Value::Array(xs) => {
            for x in xs {
                expression_fields(x, state, out);
            }
        }
        Value::Query { pipeline, .. } => {
            scan_pipeline(pipeline, independent(state), out);
        }
        _ => {}
    }
}
fn scan_call(c: &Call, state: &Flow, out: &mut GroundingScan<'_>, negated: bool) -> Option<Flow> {
    let resolved = crate::catalog::resolve(c)?;
    if !resolved.diagnostics.is_empty() {
        return None;
    }
    if resolved.function.name == "correlate" {
        let aggregations = resolved
            .arguments
            .get("aggregation")
            .and_then(|v| {
                if let Value::Array(xs) = v {
                    Some(xs.as_slice())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let queries = resolved
            .queries
            .iter()
            .map(|v| (*v, false))
            .chain(aggregations.iter().map(|v| (v, true)))
            .collect::<Vec<_>>();
        let mut contexts = BTreeMap::new();
        for (value, aggregation) in &queries {
            if let Value::Query {
                label: Some(name),
                pipeline,
                ..
            } = value
            {
                // Correlation aggregation queries are scanned in pieces for field flow.
                check_record_kinds(pipeline, state, out, true);
                let boundary = if *aggregation {
                    pipeline.steps.iter().position(|step| matches!(step, Step::Call(c) if crate::catalog::function(&c.name).is_some_and(|f| f.effect == crate::catalog::Effect::Aggregate))).unwrap_or(pipeline.steps.len())
                } else {
                    pipeline.steps.len()
                };
                let prefix = Pipeline {
                    steps: pipeline.steps[..boundary].to_vec(),
                };
                out.skip_next_kind_check = true;
                let mut local = scan_pipeline(&prefix, state.clone(), out);
                if let Some(globals) = resolved.arguments.get("globalConstraints") {
                    for field in fields_of(globals) {
                        add(&local, &field, out);
                    }
                }
                if let Some(dimension) = resolved.arguments.get("sequenceBy") {
                    for field in fields_of(dimension) {
                        add(&local, &field, out);
                    }
                } else {
                    add(&local, "@timestamp", out);
                }
                out.skip_next_kind_check = true;
                local = scan_pipeline(
                    &Pipeline {
                        steps: pipeline.steps[boundary..].to_vec(),
                    },
                    local,
                    out,
                );
                contexts.insert(name.clone(), local);
            }
        }
        for (value, aggregation) in &queries {
            let Value::Query {
                label: Some(name),
                pipeline,
                attributes,
            } = value
            else {
                continue;
            };
            let Some(local) = contexts.get(name) else {
                continue;
            };
            if !aggregation {
                for attr in attributes.iter().filter(|a| {
                    a.name.as_deref() == Some("include")
                        && !crate::catalog::query_all_fields(&a.value)
                }) {
                    for field in fields_of(&attr.value) {
                        add(local, &field, out);
                    }
                }
            }
            for (_, target, _) in crate::catalog::correlation_links(pipeline) {
                if let Some((other, key)) = target.split_once('.')
                    && let Some(foreign) = contexts.get(other)
                {
                    add(foreign, key, out);
                }
            }
        }
        let columns = contexts
            .iter()
            .map(|(name, flow)| (name.clone(), flow.known_columns().cloned()))
            .collect();
        let (outputs, unknown_prefixes) =
            crate::catalog::correlation_outputs(&resolved.arguments, &queries, &columns);
        return Some(Flow {
            target: state.target.clone(),
            field_target: state.target.clone(),
            derived: outputs.clone(),
            available: Some(outputs),
            unknown_prefixes,
            tables: state.tables.clone(),
            ..Flow::default()
        });
    }
    for field in resolved.function.default_read_fields(&resolved.arguments) {
        if field.starts_with('<') {
            add_unknown(state, field, out);
        } else if resolved.function.name == "split" {
            add_array(state, field, out);
        } else {
            add(state, field, out);
        }
    }
    if resolved.function.name == "split" {
        let prefix = resolved
            .arguments
            .get("field")
            .and_then(|v| v.text())
            .unwrap_or("_events");
        for field in split_members(state, prefix) {
            add(state, &field, out);
        }
    }
    if resolved.function.name == "eval" {
        let mut local = state.clone();
        for (name, value) in &resolved.ordered {
            expression_fields(value, &local, out);
            let definite = definite_value(value, &local) || local.definite.contains(name);
            write(&mut local, name.clone(), definite);
        }
        return None;
    }
    if resolved.function.name == "getField"
        && let Some(source) = resolved.arguments.get("source")
    {
        if let Value::String(field) = source {
            add(state, field, out);
        } else {
            expression_fields(source, state, out);
            add_unknown(state, "<runtime-field-name>", out);
        }
    }
    let foreign = resolved.arguments.get("query").and_then(|value| {
        if let Value::Query { pipeline, .. } = value {
            Some(scan_pipeline(
                pipeline,
                query_context(state, &resolved.arguments),
                out,
            ))
        } else {
            None
        }
    });
    let table = resolved
        .arguments
        .get("file")
        .and_then(|value| value.text())
        .and_then(|name| state.tables.get(name));
    if resolved.function.name == "readFile" {
        let mut sources = Vec::new();
        let requested = resolved
            .arguments
            .get("include")
            .map(|v| fields_of(v).into_iter().collect::<BTreeSet<_>>())
            .unwrap_or_default();
        let mut provided = BTreeSet::new();
        if let Some(files) = resolved.arguments.get("file") {
            for name in fields_of(files) {
                let mut source = state.tables.get(&name).cloned().unwrap_or_else(|| Flow {
                    origin: Origin::External,
                    field_target: state.target.clone(),
                    ..Flow::default()
                });
                if source.origin == Origin::External {
                    add_unknown(&source, &format!("<resource:{name}>"), out);
                }
                if let Some(include) = resolved.arguments.get("include") {
                    let fields: BTreeSet<_> = fields_of(include)
                        .into_iter()
                        .filter(|field| !source.absent(field))
                        .collect();
                    provided.extend(fields.iter().cloned());
                    for field in &fields {
                        add(&source, field, out);
                    }
                    project(&mut source, fields);
                }
                sources.push(source);
            }
        }
        let mut source = merge_branches(&Flow::default(), sources);
        source.target = state.target.clone();
        source.tables = state.tables.clone();
        for missing in requested.difference(&provided) {
            add(&source, missing, out);
        }
        return Some(source);
    }
    for value in &resolved.queries {
        if let Value::Query { pipeline, .. } = value {
            scan_pipeline(pipeline, independent(state), out);
        }
    }
    for (name, value) in &resolved.arguments {
        if negated && resolved.function.name == "match" && name == "include" {
            continue;
        }
        if name == "source" && resolved.function.name == "getField" {
            continue;
        }
        if name == "query" && foreign.is_some() {
            continue;
        }
        let role = resolved.function.argument_role(name);
        if role == crate::catalog::ArgumentRole::ForeignField {
            let external = Flow {
                origin: Origin::External,
                ..Flow::default()
            };
            let context = foreign.as_ref().or(table).unwrap_or(&external);
            for field in fields_of(value) {
                if !(resolved.function.name == "defineTable"
                    && name == "include"
                    && crate::catalog::all_columns(value))
                {
                    add(context, &field, out);
                }
            }
        } else if role == crate::catalog::ArgumentRole::Renames {
            for (old, _) in crate::catalog::rename_bindings(&resolved.arguments) {
                add(state, old, out);
            }
        } else if role == crate::catalog::ArgumentRole::InputField {
            for field in fields_of(value) {
                if resolved.function.name != "default" || !state.absent(&field) {
                    if resolved.function.name == "split" && name == "field" {
                        add_array(state, &format!("{field}[]"), out);
                    } else if name == "array" && field.ends_with("[]") {
                        add_array(state, &field, out);
                    } else {
                        add(state, &field, out);
                    }
                }
            }
        } else if role == crate::catalog::ArgumentRole::Expression {
            expression_fields(value, state, out);
        } else {
            match resolved.function.argument_scope(name) {
                crate::catalog::ArgumentScope::Independent => {
                    scan_queries(value, &independent(state), out);
                }
                crate::catalog::ArgumentScope::Continuing
                | crate::catalog::ArgumentScope::FilterQuery => scan_queries(value, state, out),
                crate::catalog::ArgumentScope::Inline => {
                    let mut local = state.clone();
                    if let Some(array) = resolved.arguments.get("array").and_then(|v| v.text()) {
                        let binding = resolved
                            .arguments
                            .get("var")
                            .and_then(|v| v.text())
                            .unwrap_or_else(|| array.trim_end_matches("[]"));
                        let structured = resolved.function.name.starts_with("objectArray:");
                        let shadows = |field: &str| {
                            field == binding || structured && path_contains(binding, field)
                        };
                        local.definite.retain(|field| !shadows(field));
                        local.locals.retain(|name, _| !shadows(name));
                        local.locals.insert(
                            binding.into(),
                            ElementBinding {
                                array: local_field(state, array),
                                structured,
                            },
                        );
                    }
                    let exported = match value {
                        Value::Query { pipeline, .. } => Some(scan_pipeline(pipeline, local, out)),
                        Value::Call(call) => Some(scan_pipeline(
                            &Pipeline {
                                steps: vec![Step::Call(call.clone())],
                            },
                            local,
                            out,
                        )),
                        _ => None,
                    };
                    if matches!(resolved.function.name, "array:eval" | "objectArray:eval")
                        && let (Some(exported), Some(output)) = (
                            exported,
                            resolved
                                .arguments
                                .get("asArray")
                                .or_else(|| resolved.arguments.get("array"))
                                .and_then(|v| v.text())
                                .and_then(|s| s.strip_suffix("[]")),
                        )
                    {
                        add(&exported, output, out);
                    }
                }
                _ => scan_nested_calls(value, state, out),
            }
        }
    }
    if matches!(resolved.function.name, "join" | "match") {
        let foreign_name = if resolved.function.name == "join" {
            "key"
        } else {
            "column"
        };
        if !resolved.arguments.contains_key(foreign_name)
            && let Some(fields) = resolved.arguments.get("field")
        {
            let external = Flow {
                origin: Origin::External,
                ..Flow::default()
            };
            for field in fields_of(fields) {
                add(foreign.as_ref().or(table).unwrap_or(&external), &field, out);
            }
        }
    }
    if resolved.function.name == "match" && !negated && !resolved.arguments.contains_key("include")
    {
        let external = Flow {
            origin: Origin::External,
            ..Flow::default()
        };
        let context = table.unwrap_or(&external);
        if let Some(fields) =
            crate::catalog::imported_fields("match", &resolved.arguments, context.known_columns())
        {
            for field in fields {
                add(context, &field, out);
            }
        } else {
            add_unknown(context, "<lookup-output-columns>", out);
        }
    }
    foreign
}
fn scan_nested_calls(value: &Value, state: &Flow, out: &mut GroundingScan<'_>) {
    match value {
        Value::Call(c) => {
            scan_call(c, state, out, false);
        }
        Value::Array(xs) => {
            for x in xs {
                scan_nested_calls(x, state, out);
            }
        }
        _ => {}
    }
}
fn scan_queries(value: &Value, state: &Flow, out: &mut GroundingScan<'_>) {
    match value {
        Value::Query { pipeline, .. } => {
            scan_pipeline(pipeline, state.clone(), out);
        }
        Value::Array(xs) => {
            for x in xs {
                scan_queries(x, state, out);
            }
        }
        Value::Call(c) => {
            scan_call(c, state, out, false);
        }
        _ => {}
    }
}
fn selectors(filter: &Filter, found: &mut [BTreeSet<String>; 3]) {
    match filter {
        Filter::Compare {
            field,
            op,
            value: Pattern::Text(text),
        } if op == "=" && !text.contains('*') => {
            let index = match field.as_str() {
                "#repo" => 0,
                "#type" => 1,
                "#event_simpleName" => 2,
                _ => return,
            };
            found[index].insert(text.clone());
        }
        Filter::And(xs) => {
            for x in xs {
                selectors(x, found);
            }
        }
        _ => {}
    }
}
fn pure_filter(filter: &Filter) -> bool {
    match filter {
        Filter::Call(_) => false,
        Filter::Compare {
            value: Pattern::Regex { pattern, .. },
            ..
        }
        | Filter::Text(Pattern::Regex { pattern, .. }) => !pattern.contains("(?<"),
        Filter::And(xs) | Filter::Or(xs) => xs.iter().all(pure_filter),
        Filter::Not(x) => pure_filter(x),
        _ => true,
    }
}
fn apply_captures(
    state: &mut Flow,
    info: language::validate::RegexAnalysis,
    strict: bool,
    repeat: bool,
) {
    for capture in info.captures {
        let definite = strict && !repeat && info.guaranteed.contains(&capture);
        write(state, capture, definite);
    }
}
fn scan_filter(filter: &Filter, state: &Flow, out: &mut GroundingScan<'_>, negated: bool) {
    match filter {
        Filter::Compare { field, .. } => add(state, field, out),
        Filter::And(xs) | Filter::Or(xs) => {
            for x in xs {
                scan_filter(x, state, out, negated);
            }
        }
        Filter::Not(x) => scan_filter(x, state, out, true),
        Filter::Call(c) => {
            scan_call(c, state, out, negated);
        }
        _ => {}
    }
}
fn merge_branches(incoming: &Flow, branches: Vec<Flow>) -> Flow {
    let mut merged = incoming.clone();
    if let Some(first) = branches.first() {
        merged.derived = first.derived.clone();
        merged.origin = if branches.iter().all(|b| b.origin == first.origin) {
            first.origin
        } else {
            Origin::Mixed
        };
        merged.field_target = if branches
            .iter()
            .all(|b| b.field_target == first.field_target)
        {
            first.field_target.clone()
        } else {
            None
        };
        merged.scope = if branches.iter().all(|b| b.scope == first.scope) {
            first.scope.clone()
        } else {
            Scope::default()
        };
        merged.uncertain_source = branches.iter().any(|b| b.uncertain_source)
            || branches
                .iter()
                .any(|b| b.scope != first.scope || b.field_target != first.field_target);
        merged.relaxed_type = branches.iter().any(|b| b.relaxed_type);
        merged.relaxed_event = branches.iter().any(|b| b.relaxed_event);
        for branch in &branches {
            merged
                .derived
                .retain(|field| branch.derived.contains(field));
            merged
                .conditional
                .extend(branch.conditional.iter().cloned());
        }
        for branch in &branches {
            merged
                .conditional
                .extend(branch.derived.difference(&merged.derived).cloned());
        }
        let candidates: BTreeSet<_> = branches
            .iter()
            .flat_map(|branch| branch.derived.iter().cloned())
            .collect();
        for field in candidates {
            if branches
                .iter()
                .all(|branch| branch.derived.contains(&field) || branch.absent(&field))
            {
                merged.derived.insert(field);
            }
        }
    }
    merged
        .conditional
        .retain(|field| !merged.derived.contains(field));
    merged.definite = branches
        .first()
        .map(|branch| branch.definite.clone())
        .unwrap_or_default();
    for branch in &branches {
        merged
            .definite
            .retain(|field| branch.definite.contains(field));
    }
    merged.arrays.clear();
    merged.conditional_arrays.clear();
    let arrays: BTreeSet<_> = branches
        .iter()
        .flat_map(|b| b.arrays.iter().chain(&b.conditional_arrays).cloned())
        .collect();
    for array in arrays {
        if branches
            .iter()
            .all(|b| b.array_produces(&array) || b.array_absent(&array))
        {
            merged.arrays.insert(array);
        } else {
            merged.conditional_arrays.insert(array);
        }
    }
    merged.unknown_enrichment = branches.iter().any(|b| b.unknown_enrichment);
    merged.unknown_prefixes = branches
        .iter()
        .flat_map(|b| b.unknown_prefixes.iter().cloned())
        .collect();
    merged.unverified_fields = branches
        .iter()
        .flat_map(|b| b.unverified_fields.iter().cloned())
        .collect();
    merged.split_unknown = branches
        .iter()
        .flat_map(|b| b.split_unknown.iter().cloned())
        .collect();
    merged.available = branches_available(&branches);
    merged.removed = branches
        .first()
        .map(|branch| branch.removed.clone())
        .unwrap_or_default();
    for branch in &branches {
        merged
            .removed
            .retain(|field| branch.removed.contains(field));
    }
    merged
}
fn branches_available(branches: &[Flow]) -> Option<BTreeSet<String>> {
    let mut fields = branches.first()?.available.clone()?;
    for branch in branches {
        fields.extend(branch.available.as_ref()?.iter().cloned());
    }
    Some(fields)
}
fn mentions_source(filter: &Filter) -> bool {
    match filter {
        Filter::Compare { field, .. } => {
            matches!(field.as_str(), "#repo" | "#type" | "#event_simpleName")
        }
        Filter::And(xs) | Filter::Or(xs) => xs.iter().any(mentions_source),
        Filter::Not(x) => mentions_source(x),
        _ => false,
    }
}
fn positive_discriminators(filter: &Filter) -> bool {
    match filter {
        Filter::Compare { field, op, value } if mentions_source(filter) => {
            field != "#repo"
                && matches!(value, Pattern::Text(text) if op == "=" && (text == "*" || !text.contains('*')))
        }
        Filter::And(xs) | Filter::Or(xs) => xs.iter().all(positive_discriminators),
        Filter::Not(_) => !mentions_source(filter),
        _ => true,
    }
}
fn mentions_field(filter: &Filter, selected: &str) -> bool {
    match filter {
        Filter::Compare { field, .. } => field == selected,
        Filter::And(xs) | Filter::Or(xs) => xs.iter().any(|x| mentions_field(x, selected)),
        Filter::Not(x) => mentions_field(x, selected),
        _ => false,
    }
}
fn relaxed_selector(filter: &Filter, selected: &str) -> bool {
    match filter {
        Filter::Compare { field, op, value } => {
            field == selected && op == "=" && matches!(value, Pattern::Text(text) if text == "*")
        }
        Filter::And(xs) => xs.iter().any(|x| relaxed_selector(x, selected)),
        Filter::Or(_) => mentions_field(filter, selected),
        _ => false,
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceAnchor {
    Unresolved,
    Repository,
}
fn unresolved_selector(filter: &Filter, anchor: SourceAnchor) -> bool {
    match filter {
        Filter::Compare { field, op, value }
            if matches!(field.as_str(), "#repo" | "#type" | "#event_simpleName") =>
        {
            !matches!(value, Pattern::Text(text) if op == "=" && !text.contains('*'))
                && !(anchor == SourceAnchor::Repository && positive_discriminators(filter))
        }
        Filter::And(xs) => xs.iter().any(|x| unresolved_selector(x, anchor)),
        Filter::Or(_) => {
            mentions_source(filter)
                && !(anchor == SourceAnchor::Repository && positive_discriminators(filter))
        }
        Filter::Not(_) => mentions_source(filter),
        _ => false,
    }
}
fn constrain(state: &mut Flow, steps: &[Step]) {
    let mut found: [BTreeSet<String>; 3] = Default::default();
    let mut filters = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        match step {
            Step::Filter(filter) if index == 0 || pure_filter(filter) => {
                selectors(filter, &mut found);
                filters.push(filter);
                if !pure_filter(filter) {
                    break;
                }
            }
            _ => break,
        }
    }
    for (slot, mut values) in [
        &mut state.scope.repo,
        &mut state.scope.type_,
        &mut state.scope.event,
    ]
    .into_iter()
    .zip(found)
    {
        if let Some(existing) = slot.as_ref() {
            values.insert(existing.clone());
        }
        if values.len() == 1 {
            *slot = values.into_iter().next();
        } else if values.len() > 1 {
            *slot = None;
            state.uncertain_source = true;
        }
    }
    let anchor = if state.origin == Origin::Deployment
        && state.scope.repo.is_some()
        && !state.uncertain_source
    {
        SourceAnchor::Repository
    } else {
        SourceAnchor::Unresolved
    };
    for filter in filters {
        let unresolved = unresolved_selector(filter, anchor);
        // Relaxing an event family does not relax a source's type namespace.
        // Keep each restriction so one discriminator cannot broaden the other.
        state.relaxed_type |= !unresolved && relaxed_selector(filter, "#type");
        state.relaxed_event |= !unresolved && relaxed_selector(filter, "#event_simpleName");
        state.uncertain_source |= unresolved;
    }
}
fn check_record_kinds(p: &Pipeline, state: &Flow, out: &mut GroundingScan<'_>, nested: bool) {
    if state.scope == Scope::default()
        && state.origin == Origin::Deployment
        && !state.uncertain_source
        && let Some((context, packs, grounding)) = &mut out.record_kind_context
        && packs
            .iter()
            .any(|pack| state.target.as_deref() == Some(pack.environment.view.as_str()))
    {
        record_kind_gaps(p, context, packs, grounding, nested);
    }
}

fn scan_pipeline(p: &Pipeline, mut state: Flow, out: &mut GroundingScan<'_>) -> Flow {
    let root = out.first_pipeline;
    out.first_pipeline = false;
    if !std::mem::take(&mut out.skip_next_kind_check) && (root || state.scope == Scope::default()) {
        check_record_kinds(p, &state, out, !root);
    }
    for (index, step) in p.steps.iter().enumerate() {
        if let Step::Filter(filter) = step
            && (!pure_filter(filter)
                || index == 0
                || !matches!(&p.steps[index - 1], Step::Filter(filter) if pure_filter(filter)))
        {
            constrain(&mut state, &p.steps[index..]);
        }
        match step {
            Step::Filter(f) => {
                scan_filter(f, &state, out, false);
                for pattern in language::validate::positive_regexes(f) {
                    if let Ok(info) = language::validate::regex_analysis(pattern) {
                        apply_captures(&mut state, info, true, false);
                    }
                }
            }
            Step::Assign { field, value } => {
                expression_fields(value, &state, out);
                let definite = definite_value(value, &state) || state.definite.contains(field);
                write(&mut state, field.clone(), definite);
            }
            Step::Case(branches) => {
                let results = branches
                    .iter()
                    .map(|p| scan_pipeline(p, state.clone(), out))
                    .collect();
                state = merge_branches(&state, results);
            }
            Step::Match { field, arms } => {
                add(&state, field, out);
                let results = arms
                    .iter()
                    .map(|(pattern, p)| {
                        let mut branch = state.clone();
                        if !matches!(pattern, Pattern::Any) {
                            constrain(
                                &mut branch,
                                &[Step::Filter(Filter::Compare {
                                    field: field.clone(),
                                    op: "=".into(),
                                    value: pattern.clone(),
                                })],
                            );
                        }
                        if let Pattern::Regex { pattern, .. } = pattern
                            && let Ok(info) = language::validate::regex_analysis(pattern)
                        {
                            apply_captures(&mut branch, info, true, false);
                        }
                        scan_pipeline(p, branch, out)
                    })
                    .collect();
                state = merge_branches(&state, results);
            }
            Step::Link { field, .. } => add(&state, field, out),
            Step::Call(c) => {
                let exported = scan_call(c, &state, out, false);
                if let Some(resolved) = crate::catalog::resolve(c) {
                    let spec = resolved.function;
                    if spec.resolved_effect(&resolved.arguments) == crate::catalog::Effect::Source {
                        let tables = state.tables.clone();
                        let target = state.target.clone();
                        state = exported.clone().unwrap_or_else(|| Flow {
                            target: target.clone(),
                            field_target: target,
                            origin: if spec.name == "createEvents" {
                                Origin::Synthetic
                            } else {
                                Origin::External
                            },
                            ..Flow::default()
                        });
                        state.tables = tables;
                    }
                    if spec.name == "correlate"
                        && let Some(exported) = &exported
                    {
                        state = exported.clone();
                    }
                    if spec.name == "defineTable"
                        && let (Some(name), Some(mut exported)) = (
                            resolved.arguments.get("name").and_then(|v| v.text()),
                            exported,
                        )
                    {
                        if let Some(include) = resolved.arguments.get("include")
                            && !crate::catalog::all_columns(include)
                        {
                            let fields: BTreeSet<_> = fields_of(include)
                                .into_iter()
                                .filter(|field| !exported.absent(field))
                                .collect();
                            project(&mut exported, fields.clone());
                            exported.derived = fields;
                        }
                        exported.tables.clear();
                        state.tables.insert(name.into(), exported);
                    }
                    if matches!(spec.name, "join" | "match") {
                        let columns = resolved
                            .arguments
                            .get("file")
                            .and_then(|v| v.text())
                            .and_then(|name| state.tables.get(name))
                            .and_then(Flow::known_columns);
                        let include = crate::catalog::imported_fields(
                            spec.name,
                            &resolved.arguments,
                            columns,
                        );
                        if let Some(include) = include {
                            for field in include {
                                write(&mut state, field, false);
                            }
                        } else if spec.name == "match" {
                            state.unknown_enrichment = true;
                            state.removed.clear();
                            state.conditional.extend(std::mem::take(&mut state.derived));
                            state
                                .conditional_arrays
                                .extend(std::mem::take(&mut state.arrays));
                            state.definite.clear();
                        }
                    }
                    if let Some((outputs, members)) = spec.named_outputs(&resolved.arguments) {
                        for field in outputs {
                            write(&mut state, field, false);
                        }
                        for member in members {
                            write_array(&mut state, member);
                        }
                    }
                    if spec.name == "eval" {
                        for (field, value) in &resolved.ordered {
                            let definite =
                                definite_value(value, &state) || state.definite.contains(field);
                            write(&mut state, field.clone(), definite);
                        }
                    }
                    if let Some(output) = spec.default_output(&resolved.arguments) {
                        if spec.name == "splitString" && !resolved.arguments.contains_key("index") {
                            write_array(&mut state, format!("{output}[]"));
                        } else {
                            write(&mut state, output, false);
                        }
                    }
                    if matches!(
                        spec.name,
                        "array:eval" | "array:filter" | "objectArray:eval"
                    ) && let Some(array) = resolved
                        .arguments
                        .get("asArray")
                        .or_else(|| resolved.arguments.get("array"))
                        .and_then(|v| v.text())
                    {
                        write_array(&mut state, array.into());
                    }
                    if spec.name == "regex"
                        && let Some(pattern) =
                            resolved.arguments.get("regex").and_then(|v| v.text())
                        && let Ok(info) = language::validate::regex_analysis(pattern)
                    {
                        apply_captures(
                            &mut state,
                            info,
                            resolved.arguments.get("strict").copied() != Some(&Value::Bool(false)),
                            resolved.arguments.get("repeat").copied() == Some(&Value::Bool(true)),
                        );
                    }
                    match spec.name {
                        "split" => apply_split(&mut state, &resolved.arguments),
                        "default" => {
                            if let Some(fields) = resolved.arguments.get("field") {
                                for field in fields_of(fields) {
                                    write(&mut state, field, true);
                                }
                            }
                        }
                        "rename" => {
                            for (old, new) in crate::catalog::rename_bindings(&resolved.arguments) {
                                let definite = state.definite.contains(old);
                                if old != new {
                                    state.definite.remove(old);
                                    state.derived.remove(old);
                                    state.conditional.remove(old);
                                    state.removed.insert(old.into());
                                    if let Some(available) = &mut state.available {
                                        available.remove(old);
                                    }
                                }
                                if old != new {
                                    write(&mut state, new.into(), definite);
                                }
                            }
                        }
                        "select" | "table" => {
                            if let Some(fields) = resolved.arguments.get("fields") {
                                project(&mut state, fields_of(fields).into_iter().collect());
                            }
                        }
                        "drop" => {
                            if let Some(fields) = resolved.arguments.get("fields") {
                                for field in fields_of(fields) {
                                    state.definite.remove(&field);
                                    state.derived.remove(&field);
                                    state.conditional.remove(&field);
                                    state.removed.insert(field.clone());
                                    if let Some(available) = &mut state.available {
                                        available.remove(&field);
                                    }
                                }
                            }
                        }
                        "groupBy" | "count" | "stats" | "sum" | "avg" | "min" | "max"
                        | "collect" | "selectFromMax" | "selectFromMin" | "selectLast" | "top"
                        | "fieldset" | "bucket" | "timeChart" | "session" | "series" => {
                            if let Some((outputs, retains)) = crate::catalog::aggregate_outputs(c) {
                                if retains {
                                    for field in outputs {
                                        write(&mut state, field, false);
                                    }
                                } else {
                                    project(&mut state, outputs.clone());
                                    state.derived = outputs;
                                    state.conditional.clear();
                                    state.definite.clear();
                                    state.removed.clear();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    state
}

fn record_basis(grounding: &mut Grounding, key: String, evidence: String) {
    let entry = grounding.basis.entry(key).or_default();
    if entry.is_empty() {
        *entry = evidence;
    } else if !entry.split(" | ").any(|item| item == evidence) {
        entry.push_str(" | ");
        entry.push_str(&evidence);
    }
}

fn kind_filter_value(filter: &Filter, field: &str) -> Option<String> {
    match filter {
        Filter::Compare {
            field: candidate,
            op,
            value,
        } if candidate == field && op == "=" => match value {
            Pattern::Text(text) if !text.contains('*') => Some(text.clone()),
            _ => None,
        },
        Filter::And(filters) => {
            let values = filters
                .iter()
                .filter_map(|filter| kind_filter_value(filter, field))
                .collect::<BTreeSet<_>>();
            (values.len() == 1)
                .then(|| values.into_iter().next())
                .flatten()
        }
        Filter::Or(filters) => {
            let values = filters
                .iter()
                .map(|filter| kind_filter_value(filter, field))
                .collect::<Option<BTreeSet<_>>>()?;
            (values.len() == 1)
                .then(|| values.into_iter().next())
                .flatten()
        }
        Filter::Call(call) => positive_kind_call(call, field),
        _ => None,
    }
}

fn positive_kind_call(call: &Call, field: &str) -> Option<String> {
    let resolved = crate::catalog::resolve(call)?;
    if !resolved.diagnostics.is_empty()
        || call_assigns_fields(call)
        || !resolved
            .arguments
            .get("field")
            .is_some_and(|value| fields_of(value) == [field])
    {
        return None;
    }
    match resolved.function.name {
        "in" if resolved.arguments.get("ignoreCase") != Some(&&Value::Bool(true)) => {
            match resolved.arguments.get("values") {
                Some(Value::Array(values)) if values.len() == 1 => match &values[0] {
                    Value::Word(text) | Value::String(text) if !text.contains('*') => {
                        Some(text.clone())
                    }
                    _ => None,
                },
                _ => None,
            }
        }
        _ => None,
    }
}

fn call_assigns_fields(call: &Call) -> bool {
    let Some(resolved) = crate::catalog::resolve(call) else {
        return true;
    };
    !resolved.diagnostics.is_empty()
        || resolved.function.name == "regex"
            && resolved
                .arguments
                .get("regex")
                .and_then(|value| value.text())
                .is_some_and(|pattern| !language::validate::regex_captures(pattern).is_empty())
        || resolved.ordered.iter().any(|(name, _)| {
            resolved.function.argument_role(name) == crate::catalog::ArgumentRole::Output
                || matches!(name.as_str(), "output" | "include")
                || name == "strict"
                    && resolved.arguments.get("strict") == Some(&&Value::Bool(false))
        })
}

fn transparent_kind_call(call: &Call) -> bool {
    crate::catalog::resolve(call).is_some_and(|resolved| {
        matches!(resolved.function.name, "in" | "regex" | "wildcard") && !call_assigns_fields(call)
    })
}

fn transparent_kind_filter(filter: &Filter) -> bool {
    match filter {
        Filter::Compare { .. } => !language::validate::positive_regexes(filter)
            .iter()
            .any(|pattern| !language::validate::regex_captures(pattern).is_empty()),
        Filter::Call(call) => transparent_kind_call(call),
        Filter::And(filters) | Filter::Or(filters) => filters.iter().all(transparent_kind_filter),
        Filter::Not(filter) => transparent_kind_filter(filter),
        _ => false,
    }
}

fn leading_kind_filter(pipeline: &Pipeline, field: &str) -> bool {
    for step in &pipeline.steps {
        match step {
            Step::Filter(filter) => {
                if !transparent_kind_filter(filter) {
                    return false;
                }
                if kind_filter_value(filter, field).is_some() {
                    return true;
                }
            }
            Step::Call(call) => {
                if !transparent_kind_call(call) {
                    return false;
                }
                if positive_kind_call(call, field).is_some() {
                    return true;
                }
            }
            _ => break,
        }
    }
    false
}

fn uses_record_kind(pipeline: &Pipeline, field: &str, scope: &Scope) -> bool {
    if field == "#event_simpleName" && scope.event.is_some() {
        return true;
    }
    for step in &pipeline.steps {
        match step {
            Step::Filter(filter) if transparent_kind_filter(filter) => {
                if kind_filter_value(filter, field).is_some() {
                    return true;
                }
            }
            Step::Call(call) => {
                let Some(resolved) = crate::catalog::resolve(call) else {
                    return false;
                };
                if resolved.diagnostics.is_empty()
                    && matches!(resolved.function.name, "groupBy" | "top")
                    && resolved
                        .arguments
                        .get("field")
                        .is_some_and(|keys| fields_of(keys).iter().any(|key| key == field))
                {
                    return true;
                }
                if !transparent_kind_call(call) {
                    return false;
                }
                if positive_kind_call(call, field).is_some() {
                    return true;
                }
            }
            Step::Match {
                field: matched,
                arms,
            } if matched == field
                && !arms.is_empty()
                && arms.iter().all(|(pattern, _)| {
                    matches!(pattern, Pattern::Text(value) if !value.contains(['*', '?']))
                }) =>
            {
                return true;
            }
            Step::Case(branches)
                if !branches.is_empty()
                    && branches
                        .iter()
                        .all(|branch| leading_kind_filter(branch, field)) =>
            {
                return true;
            }
            _ => return false,
        }
    }
    false
}

fn record_kind_gaps(
    pipeline: &Pipeline,
    context: &Context,
    packs: &[&Pack],
    g: &mut Grounding,
    nested: bool,
) {
    let mut state = Flow::default();
    for (index, step) in pipeline.steps.iter().enumerate() {
        match step {
            Step::Filter(_) => constrain(&mut state, &pipeline.steps[index..]),
            Step::Call(call)
                if crate::catalog::resolve(call).is_some_and(|resolved| {
                    resolved.function.resolved_effect(&resolved.arguments)
                        == crate::catalog::Effect::Filter
                        && matches!(resolved.function.name, "in" | "regex")
                }) => {}
            _ => break,
        }
    }
    if state.uncertain_source || (state.scope.repo.is_none() && state.scope.type_.is_none()) {
        return;
    }
    let mut kinds = BTreeMap::<String, (BTreeSet<String>, BTreeSet<String>, bool)>::new();
    for source in packs
        .iter()
        .flat_map(|pack| &pack.data.sources)
        .filter_map(|value| environment::source(value).ok())
        .filter(|source| source_overlaps_scope(source, &state.scope))
    {
        if let Some(type_) = source.selectors.as_ref().and_then(|s| s.type_.as_ref()) {
            let (values, ids, _) = kinds.entry("#type".into()).or_default();
            values.insert(type_.clone());
            ids.insert(source.id.clone());
        }
        if let Some(relationship) = source.relationship {
            for dataset in relationship.datasets {
                let (values, ids, _) = kinds.entry("#event.dataset".into()).or_default();
                values.insert(dataset);
                ids.insert(source.id.clone());
            }
            for kind in relationship.record_kinds {
                let (values, ids, truncated) = kinds.entry(kind.field).or_default();
                *truncated |= kind.truncated;
                ids.insert(source.id.clone());
                for value in kind.values {
                    values.insert(value);
                }
            }
        }
    }
    for (field, (values, ids, truncated)) in kinds {
        if (!truncated && values.len() < 2) || uses_record_kind(pipeline, &field, &state.scope) {
            continue;
        }
        let visible = values.into_iter().collect::<Vec<_>>().join(", ");
        let guidance = if !context.require_observed
            && field == "#event.dataset"
            && state.scope.event.is_some()
        {
            "the exact #event_simpleName selector does not establish #event.dataset; this query can include these kinds; add an exact #event.dataset filter or group by #event.dataset when you need separate results"
        } else if !context.require_observed {
            "this query can include these kinds; filter or group by the kind field when you need separate results"
        } else if field == "#event.dataset" && state.scope.event.is_some() {
            "the exact #event_simpleName selector does not establish #event.dataset; add an exact #event.dataset filter, group by #event.dataset, or explicitly accept mixed record kinds with accept_mixed_record_kinds"
        } else {
            "constrain one, group by it, or set accept_mixed_record_kinds"
        };
        let mut message = if truncated {
            format!(
                "record-kind facet truncated ({field}: {visible}); other kinds may exist; {guidance}"
            )
        } else {
            format!("multiple record kinds observed ({field}: {visible}); {guidance}")
        };
        if nested {
            let scope = [
                state
                    .scope
                    .repo
                    .as_ref()
                    .map(|repo| format!("#repo={repo}")),
                state
                    .scope
                    .type_
                    .as_ref()
                    .map(|type_| format!("#type={type_}")),
                state
                    .scope
                    .event
                    .as_ref()
                    .map(|event| format!("#event_simpleName={event}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
            message.push_str(&format!("; nested query scope {scope}"));
        }
        for id in ids {
            let key = format!("{id}:@record-kind");
            record_basis(g, key.clone(), message.clone());
            if context.require_observed && !context.accept_mixed_record_kinds {
                g.missing.push(format!("{key}: {message}"));
            }
        }
    }
}

fn ground(
    pipeline: &Pipeline,
    context: &Context,
    pack: Option<&Pack>,
    evidence: Option<GroundingEvidence<'_>>,
) -> Grounding {
    let mut g = Grounding {
        policy: context.into(),
        ..Grounding::default()
    };
    if let Some(e) = &context.environment
        && environment::validate_identity(e).is_err()
    {
        g.conflicting
            .push("Invalid explicit environment identity.".into());
    }
    if let Some(p) = pack {
        g.pack_id = Some(p.content_digest.clone());
        if p.validate().is_err() {
            g.conflicting
                .push("Pack integrity or version validation failed.".into());
            return g;
        }
        if context
            .environment
            .as_ref()
            .is_some_and(|e| e != &p.environment)
        {
            g.conflicting
                .push("Cloud, tenant or searchable target conflicts with selected pack.".into());
            return g;
        }
    }
    let mut kind_packs = pack.into_iter().collect::<Vec<_>>();
    if let Some(evidence) = evidence {
        if environment::validate_identity(evidence.environment).is_err()
            || context
                .environment
                .as_ref()
                .is_some_and(|environment| environment != evidence.environment)
            || pack.is_some_and(|pack| &pack.environment != evidence.environment)
        {
            g.conflicting
                .push("Query evidence conflicts with the selected environment identity.".into());
            return g;
        }
        if evidence.query_observations.len() > 1000
            || evidence.query_observations.iter().any(|observation| {
                observation.validate().is_err() || &observation.environment != evidence.environment
            })
        {
            g.conflicting
                .push("Query observation integrity, identity or budget validation failed.".into());
            return g;
        }
        if let Some(active) = evidence.active_pack {
            if active.validate().is_err() || &active.environment != evidence.environment {
                g.conflicting
                    .push("Active pack integrity or environment validation failed.".into());
                return g;
            }
            if !kind_packs
                .iter()
                .any(|pack| pack.content_digest == active.content_digest)
            {
                kind_packs.push(active);
            }
        }
    }
    let primary_target = pack
        .map(|p| &p.environment)
        .or(context.environment.as_ref())
        .or(evidence.map(|evidence| evidence.environment))
        .map(|e| e.view.clone());
    let mut query_observed = false;
    for r in requirements(
        pipeline,
        primary_target.clone(),
        context,
        &kind_packs,
        &mut g,
    ) {
        let key = format!(
            "{}{}:{}:{}",
            r.target
                .as_ref()
                .filter(|t| Some(*t) != primary_target.as_ref())
                .map(|t| format!("view={t}:"))
                .unwrap_or_default(),
            r.scope.repo.as_deref().unwrap_or("*"),
            r.scope.type_.as_deref().unwrap_or("*"),
            r.field
        );
        if r.unavailable {
            record_basis(
                &mut g,
                key.clone(),
                "not exported by preceding query".into(),
            );
            if context.require_observed {
                g.missing
                    .push(format!("{key}: not exported by preceding query."));
            }
            continue;
        }
        if r.derived && !r.conditional {
            record_basis(&mut g, key, "query-produced".into());
            continue;
        }
        if let Some(target) = &r.target
            && pack
                .map(|p| &p.environment)
                .or(context.environment.as_ref())
                .or(evidence.map(|evidence| evidence.environment))
                .is_none_or(|e| &e.view != target)
        {
            record_basis(
                &mut g,
                key.clone(),
                "searchable target has no matching environment evidence".into(),
            );
            if context.require_observed {
                g.missing.push(format!(
                    "{key}: subquery target requires separate evidence."
                ));
            }
            continue;
        }
        if matches!(r.origin, Origin::External | Origin::Mixed) || r.uncertain_source {
            let basis = if r.uncertain_source {
                "source or field selection unresolved; unverified"
            } else if r.origin == Origin::Mixed {
                "mixed-source; unverified"
            } else {
                "external-source; unverified"
            };
            record_basis(&mut g, key.clone(), basis.into());
            if context.require_observed {
                g.missing
                    .push(format!("{key}: source fields require separate evidence."));
            }
            continue;
        }
        if r.conditional && !context.require_observed {
            record_basis(
                &mut g,
                key,
                "query-produced; conditional; incoming field unverified".into(),
            );
            continue;
        }
        if is_builtin_field(&r.field) {
            record_basis(&mut g, key, "language-built-in".into());
            continue;
        }
        if let Some(evidence) = evidence {
            match query_observation_basis(&r, pack, evidence) {
                Ok(Some(basis)) => {
                    query_observed = true;
                    record_basis(&mut g, key, basis);
                    continue;
                }
                Err(basis) => {
                    record_basis(&mut g, key.clone(), basis);
                    if context.require_observed {
                        g.missing.push(key);
                    }
                    continue;
                }
                Ok(None) => {}
            }
        }
        let Some(p) = pack else {
            record_basis(
                &mut g,
                key.clone(),
                if context.confirmed_fields.contains(&r.field) {
                    "caller-confirmed"
                } else {
                    "query-supplied; deployment unverified"
                }
                .into(),
            );
            if context.require_observed && evidence.is_some() {
                g.missing.push(key);
            }
            continue;
        };
        let sources = p
            .data
            .sources
            .iter()
            .filter_map(|v| environment::source(v).ok())
            .filter(|source| source_matches_requirement(source, &r))
            .collect::<Vec<_>>();
        let matches = p
            .data
            .fields
            .iter()
            .filter_map(|v| environment::field(v).ok())
            .filter(|f| {
                sources.iter().any(|s| s.id == f.source_id)
                    && field_matches_requirement(f, &r)
                    && (!r.array || f.is_array == Some(true) || f.data_type == "array")
            })
            .collect::<Vec<_>>();
        if matches.len() == 1 {
            let f = &matches[0];
            let source = sources
                .iter()
                .find(|s| s.id == f.source_id)
                .expect("matched field belongs to a selected source");
            let historical = f.refresh_status.as_deref() == Some("not-reobserved");
            let field_observed = f
                .coverage
                .as_ref()
                .and_then(|c| c.observed)
                .unwrap_or(f.evidence_kind == "observed");
            let source_observed = matches!(
                source.inventory_status.as_str(),
                "inventoried" | "observed-only" | "reconciled"
            ) && source.evidence.as_ref().and_then(|e| e.observed)
                != Some(false);
            let field_evidence = observation(p, &f.provenance_ids);
            let source_evidence = observation(p, &source.provenance_ids);
            let stale = historical
                || source.inventory_status == "stale"
                || field_evidence == Observation::Stale
                || source_evidence == Observation::Stale;
            let partial_applicable =
                context.accept_partial_coverage && !stale && field_observed && source_observed;
            let field_partial = if partial_applicable && field_evidence == Observation::Missing {
                partial_observation(p, &f.provenance_ids)
            } else {
                None
            };
            let source_partial = if partial_applicable && source_evidence == Observation::Missing {
                partial_observation(p, &source.provenance_ids)
            } else {
                None
            };
            let accepted_partial = partial_applicable
                && (field_evidence == Observation::Qualified || field_partial.is_some())
                && (source_evidence == Observation::Qualified
                    || (source.evidence.as_ref().and_then(|e| e.observed) == Some(true)
                        && source_partial.is_some()))
                && (field_evidence != Observation::Qualified
                    || source_evidence != Observation::Qualified);
            let observed = field_observed
                && source_observed
                && ((field_evidence == Observation::Qualified
                    && source_evidence == Observation::Qualified)
                    || accepted_partial);
            let evidence = if historical {
                "historical; not re-observed".into()
            } else if source.inventory_status == "stale" {
                "source marked stale".into()
            } else if stale {
                "observation evidence marked stale".into()
            } else if accepted_partial {
                let mut details = Vec::new();
                if field_evidence != Observation::Qualified {
                    details.push(format!(
                        "field {}",
                        field_partial.expect("accepted field coverage")
                    ));
                }
                if source_evidence != Observation::Qualified {
                    details.push(format!(
                        "source {}",
                        source_partial.expect("accepted source coverage")
                    ));
                }
                format!(
                    "observed with accepted partial coverage; {}",
                    details.join("; ")
                )
            } else if observed {
                "observed in a bounded sample".into()
            } else if !source_observed {
                "source has no current observed evidence".into()
            } else if f.evidence_kind == "documented"
                && f.coverage.as_ref().is_none_or(|c| c.observed.is_none())
            {
                "listed field namespace; population not measured".into()
            } else if !field_observed {
                "not observed in the selected sample".into()
            } else {
                let missing = match (
                    field_evidence == Observation::Qualified,
                    source_evidence == Observation::Qualified,
                ) {
                    (false, true) => "field",
                    (true, false) => "source",
                    _ => "field and source",
                };
                format!(
                    "no qualifying deployment observation provenance for {missing}; requires source_type ngsiem-api, mcp-observation, or console-export and coverage_status complete, observed-only, or reconciled"
                )
            };
            record_basis(&mut g, key.clone(), evidence);
            if context.require_observed {
                if stale {
                    g.stale.push(key);
                } else if !observed {
                    g.missing.push(key);
                }
            }
        } else {
            record_basis(
                &mut g,
                key.clone(),
                if matches.len() > 1 {
                    "multiple source-specific observations; not interchangeable"
                } else {
                    "query-supplied; not measured in selected pack"
                }
                .into(),
            );
            if context.require_observed {
                g.missing.push(key);
            }
        }
    }
    if pack.is_none() && context.require_observed && !query_observed {
        g.missing.push(
            if evidence.is_some() {
                "A measured environment pack or qualifying query observation is required."
            } else {
                "A measured environment pack is required."
            }
            .into(),
        );
    }
    g.missing.sort();
    g.missing.dedup();
    g.stale.sort();
    g.stale.dedup();
    g
}

fn source_matches_requirement(source: &environment::LegacySource, r: &Requirement) -> bool {
    source.selectors.as_ref().is_some_and(|selectors| {
        r.scope
            .repo
            .as_ref()
            .is_none_or(|repo| selectors.repo.as_ref() == Some(repo))
            && r.scope
                .type_
                .as_ref()
                .is_none_or(|kind| selectors.type_.as_ref() == Some(kind))
            && (!r.relaxed_type
                || ((r.scope.type_.is_some() || selectors.type_.is_none())
                    && selectors.event_family.is_none()
                    && selectors.sourcetype.is_none()))
            && (!r.relaxed_event || selectors.event_family.is_none())
    })
}

fn source_overlaps_scope(source: &environment::LegacySource, scope: &Scope) -> bool {
    // Broader source measurements and kind facets still apply to narrowed queries.
    source.selectors.as_ref().is_some_and(|selectors| {
        selectors
            .repo
            .as_ref()
            .zip(scope.repo.as_ref())
            .is_none_or(|(stored, query)| stored == query)
            && selectors
                .type_
                .as_ref()
                .zip(scope.type_.as_ref())
                .is_none_or(|(stored, query)| stored == query)
    })
}

fn field_matches_requirement(field: &environment::LegacyField, r: &Requirement) -> bool {
    field.path.trim_start_matches('#') == r.field.trim_start_matches('#')
        && field.is_tag == Some(r.field.starts_with('#'))
}

fn query_observation_basis(
    r: &Requirement,
    pack: Option<&Pack>,
    evidence: GroundingEvidence<'_>,
) -> Result<Option<String>, String> {
    let mut namespaces = BTreeSet::new();
    let mut contradictions = Vec::new();
    let mut active_contradiction = false;
    for pack in pack.into_iter().chain(evidence.active_pack) {
        let sources = pack
            .data
            .sources
            .iter()
            .filter_map(|value| environment::source(value).ok())
            .filter(|source| source_overlaps_scope(source, &r.scope))
            .collect::<Vec<_>>();
        let fields = pack
            .data
            .fields
            .iter()
            .filter_map(|value| environment::field(value).ok())
            .filter(|field| {
                sources.iter().any(|source| source.id == field.source_id)
                    && field_matches_requirement(field, r)
            })
            .collect::<Vec<_>>();
        let mut provenance_ids = BTreeSet::new();
        let mut windows = Vec::new();
        let mut contradicted = false;
        let mut snapshot_bound = false;
        for source in &sources {
            let source_fields = fields
                .iter()
                .filter(|field| field.source_id == source.id)
                .collect::<Vec<_>>();
            if !source_fields.is_empty()
                && source_matches_requirement(source, r)
                && let Some(selectors) = &source.selectors
            {
                namespaces.insert((
                    selectors.repo.clone(),
                    selectors.type_.clone(),
                    selectors.event_family.clone(),
                    selectors.sourcetype.clone(),
                ));
            }
            let source_evidence = observation(pack, &source.provenance_ids);
            let source_negative = source.inventory_status == "stale"
                || source
                    .evidence
                    .as_ref()
                    .and_then(|evidence| evidence.observed)
                    == Some(false)
                || source_evidence == Observation::Stale;
            let negative_fields = source_fields
                .into_iter()
                .filter(|field| {
                    field.refresh_status.as_deref() == Some("not-reobserved")
                        || field
                            .coverage
                            .as_ref()
                            .and_then(|coverage| coverage.observed)
                            == Some(false)
                        || observation(pack, &field.provenance_ids) == Observation::Stale
                })
                .collect::<Vec<_>>();
            let measured_absent = !fields.iter().any(|field| field.source_id == source.id)
                && source.evidence.as_ref().is_some_and(|evidence| {
                    evidence
                        .measured_fields
                        .as_ref()
                        .is_some_and(|fields| fields.iter().any(|field| field == &r.field))
                });
            if source_negative || !negative_fields.is_empty() || measured_absent {
                contradicted = true;
                if source_negative || measured_absent {
                    snapshot_bound |= source.inventory_status == "stale"
                        || environment::linked_stale(pack, &source.provenance_ids)
                        || environment::source_measurement(pack, source).is_none();
                    provenance_ids.extend(source.provenance_ids.iter().cloned());
                    windows.extend(
                        source
                            .evidence
                            .as_ref()
                            .and_then(|evidence| evidence.observation_window.clone()),
                    );
                }
                for field in negative_fields {
                    snapshot_bound |= field.refresh_status.as_deref() == Some("not-reobserved")
                        || environment::linked_stale(pack, &field.provenance_ids)
                        || environment::field_measurement(pack, field).is_none();
                    provenance_ids.extend(field.provenance_ids.iter().cloned());
                    windows.extend(field.observation_window.clone());
                }
            }
        }
        if contradicted {
            active_contradiction |= evidence
                .active_pack
                .is_some_and(|active| active.content_digest == pack.content_digest);
            contradictions.push((pack, provenance_ids, windows, snapshot_bound));
        }
    }
    if namespaces.len() > 1 {
        return Err("multiple source-specific observations; not interchangeable".into());
    }
    let source_id = crate::discovery::source_id(&r.scope);
    let mut matching = false;
    for observation in evidence.query_observations.iter().filter(|observation| {
        observation.source_id == source_id
            && r.target
                .as_ref()
                .is_some_and(|target| target == &observation.environment.view)
            && !r.array
    }) {
        let Some(field) = observation
            .fields
            .iter()
            .find(|field| field.field == r.field)
        else {
            continue;
        };
        matching = true;
        if contradictions
            .iter()
            .all(|(pack, ids, windows, snapshot_bound)| {
                measurement_precedes_observation(pack, ids, windows, *snapshot_bound, observation)
            })
        {
            return Ok(Some(format!(
                "caller-attested query observation; id={}; positive_rows={}; retrieved_at={}; observation_window={}/{}",
                observation.id,
                field.positive_rows,
                observation.retrieved_at,
                observation.start,
                observation.end
            )));
        }
    }
    if active_contradiction || (matching && !contradictions.is_empty()) {
        Err("stale, not-reobserved or nonobserved pack measurement remains authoritative; query observation is absent, older or cannot be ordered after that measurement".into())
    } else {
        Ok(None)
    }
}

fn measurement_precedes_observation(
    pack: &Pack,
    ids: &BTreeSet<String>,
    windows: &[String],
    snapshot_bound: bool,
    observation: &QueryObservation,
) -> bool {
    let Ok(start) = environment::parse_time(&observation.start) else {
        return false;
    };
    let Ok(created) = environment::parse_time(&pack.created_at) else {
        return false;
    };
    if snapshot_bound && created >= start {
        return false;
    }
    let window_before = |window: &str, before| {
        window.split_once('/').is_some_and(|(from, to)| {
            matches!((environment::parse_time(from), environment::parse_time(to)), (Ok(from), Ok(to)) if from < to && to <= before)
        })
    };
    if windows.iter().any(|window| !window_before(window, start)) {
        return false;
    }
    let mut measured = false;
    for provenance in &pack.data.provenance {
        if !provenance["id"].as_str().is_some_and(|id| ids.contains(id))
            || !matches!(
                provenance["source_type"].as_str(),
                Some("ngsiem-api" | "mcp-observation" | "console-export")
            )
        {
            continue;
        }
        measured = true;
        let Some(retrieved) = provenance["retrieved_at"]
            .as_str()
            .and_then(|retrieved| environment::parse_time(retrieved).ok())
        else {
            return false;
        };
        if retrieved > created
            || retrieved >= start
            || provenance["observation_window"]
                .as_str()
                .is_some_and(|window| !window_before(window, retrieved))
        {
            return false;
        }
    }
    measured
}

fn partial_observation(pack: &Pack, ids: &[String]) -> Option<String> {
    pack.data.provenance.iter().find_map(|evidence| {
        (evidence["id"]
            .as_str()
            .is_some_and(|id| ids.iter().any(|candidate| candidate == id))
            && matches!(
                evidence["source_type"].as_str(),
                Some("ngsiem-api" | "mcp-observation" | "console-export")
            )
            && evidence["coverage_status"] == "partial")
            .then(|| {
                format!(
                    "retrieved_at={} observation_window={}",
                    evidence["retrieved_at"].as_str().unwrap_or("not recorded"),
                    evidence["observation_window"]
                        .as_str()
                        .unwrap_or("not recorded")
                )
            })
    })
}

pub fn recipe_catalog() -> serde_json::Value {
    serde_json::json!([
        {"id":"events","use":"Filter and project events; explicit earliest row cap"},
        {"id":"count-events","use":"Count matching events"},
        {"id":"count-by","use":"Count events per key combination"},
        {"id":"distinct","use":"Explicit exact or estimated cardinality"},
        {"id":"top-values","use":"Rank grouped values, with execution-cap warning"},
        {"id":"threshold","use":"Filter groups by an event-count threshold"},
        {"id":"pivot","use":"Literal entity pivot, including literal asterisks"},
        {"id":"join-pivot","use":"Explicit join keys, match count and row caps"},
        {"id":"sequence","use":"Named, linked filter queries within an explicit duration"}
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uncredited_fields_use_positive_exact_names_without_the_quick_lead_cap() {
        let fields = [
            FieldObservation {
                field: "#action".into(),
                positive_rows: 1,
            },
            FieldObservation {
                field: "user".into(),
                positive_rows: 0,
            },
        ];
        let query =
            "#repo=sample action=* | user=* | sort(user) | select([#action, user, @timestamp])";
        assert_eq!(
            uncredited_observation_fields(query, &fields).unwrap(),
            ["action", "user"]
        );

        let names = (0..1001)
            .map(|i| format!("field_{i:04}"))
            .collect::<Vec<_>>();
        let wide_query = format!("#repo=sample | select([{}])", names.join(", "));
        assert_eq!(
            unverified_leads(&wide_query).unwrap_err().code,
            "CQL_LEAD_BUDGET"
        );
        assert_eq!(
            uncredited_observation_fields(&wide_query, &[]).unwrap(),
            names
        );
    }

    #[test]
    fn exact_asterisk_is_not_lowered_to_a_glob() {
        let r = GenerateRequest {
            schema_version: 1,
            id: None,
            input: GenerateInput::Plan {
                plan: Plan {
                    source: Scope::default(),
                    filters: vec![Predicate::Eq {
                        field: "name".into(),
                        value: Scalar::Text("a*b".into()),
                    }],
                    steps: vec![],
                },
            },
            context: Context::default(),
        };
        let a = generate(r, None);
        assert_eq!(a.outcome, Outcome::Ready, "{:?}", a.diagnostics);
        let q = a.query.unwrap();
        assert!(q.contains("\\A") && q.contains("\\*") && q.contains("\\z"));
        assert!(!q.contains("name = \"a*b\""));
    }
    #[test]
    fn accepted_language_is_not_execution_proof() {
        let a = check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: "count()".into(),
                context: Context::default(),
            },
            None,
        );
        assert_eq!(a.validation.status, Acceptance::Accepted);
        assert_eq!(a.validation.execution, "not-run");
    }
    #[test]
    fn unresolved_english_never_emits_query() {
        let a = generate(
            GenerateRequest {
                schema_version: 1,
                id: None,
                input: GenerateInput::English {
                    text: "show events without admins".into(),
                    source: Scope::default(),
                },
                context: Context::default(),
            },
            None,
        );
        assert_ne!(a.outcome, Outcome::Ready);
        assert!(a.query.is_none());
    }
}
