//! Application use cases: explicit context in, structured assessments out.
use crate::{
    contracts::*,
    environment::{self, Observation, Pack, observation},
    inventory::{FieldObservation, InventoryHint, QueryObservation, UnverifiedFieldLead},
    language::{self, ast::*},
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy)]
pub struct GroundingEvidence<'a> {
    pub environment: &'a EnvironmentId,
    pub active_pack: Option<&'a Pack>,
    pub query_observations: &'a [QueryObservation],
    /// Searches a client hook passed to `env capture`, reported as their own kind of basis.
    pub captured_observations: &'a [QueryObservation],
    pub capabilities: Option<crate::capability::CapabilityEvidence<'a>>,
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

fn target_requirements(
    pipeline: &Pipeline,
    selected_environment: Option<&EnvironmentId>,
    evidence: Option<crate::capability::CapabilityEvidence<'_>>,
) -> Vec<Diagnostic> {
    use crate::capability::{self, CaseId, ResourceBindings};
    struct Notices<'a> {
        selected_view: Option<&'a str>,
        evidence: Option<capability::CapabilityEvidence<'a>>,
        remote: bool,
        qualified: BTreeMap<CaseId, Option<capability::QualifiedCapability>>,
        diagnostics: BTreeMap<String, Diagnostic>,
    }
    fn note(code: &str, message: String) -> Diagnostic {
        let mut diagnostic = Diagnostic::warning(code, message);
        diagnostic.severity = Severity::Info;
        diagnostic
    }
    fn observed(message: String) -> Diagnostic {
        note("CQL_TARGET_OBSERVED", message)
    }
    impl Notices<'_> {
        fn notice(&mut self, key: String, diagnostic: Diagnostic) {
            // One observed form cannot waive an unqualified sibling's prerequisite.
            if self.diagnostics.get(&key).is_none_or(|previous| {
                previous.code == "CQL_TARGET_OBSERVED" && diagnostic.code != "CQL_TARGET_OBSERVED"
            }) {
                self.diagnostics.insert(key, diagnostic);
            }
        }
        fn call(&mut self, call: &Call, target: Option<&str>, scalar: bool) {
            let Some(resolved) = crate::catalog::resolve(call) else {
                return;
            };
            let spec = resolved.function;
            let case = match spec.name {
                "array:union" => Some(CaseId::ArrayUnion),
                "array:intersection" => Some(CaseId::ArrayIntersection),
                "array:reduceRow" => Some(CaseId::ArrayReduceRow),
                "array:reduceColumn" => Some(CaseId::ArrayReduceColumn),
                _ => None,
            }
            .filter(|case| {
                !self.remote && resolved.diagnostics.is_empty() && case.matches_call(call)
            });
            let qualified = case
                .and_then(|case| {
                    self.evidence
                        .filter(|e| target.is_none_or(|t| t == e.environment.view))
                        .and_then(|e| {
                            // Eligibility precedes the cache: remote and unqualified calls cannot inherit it.
                            self.qualified
                                .entry(case)
                                .or_insert_with(|| {
                                    capability::qualify(e, case, ResourceBindings::None {})
                                        .ok()
                                        .flatten()
                                })
                                .as_ref()
                        })
                })
                .cloned();
            for requirement in spec
                .requirements()
                .iter()
                .filter(|r| r.applies(&resolved.arguments))
            {
                let key = format!("{}:{}:{target:?}", spec.name, requirement.note);
                let diagnostic = if let Some(qualified) = &qualified {
                    observed(format!(
                        "{}: this form was observed available at {}; evidence expires at {}. Target and execution context are caller-attested.",
                        spec.name, qualified.observed_at, qualified.expires_at
                    ))
                } else {
                    let remediation = if case.is_none() {
                        "Verify the prerequisite on the selected target; no automated capability case covers this form."
                    } else if target.is_some_and(|target| self.selected_view != Some(target)) {
                        "Verify the prerequisite on the queried view, or analyze it with capability evidence from a matching selected target."
                    } else {
                        "Run the optional capability check for this form or verify the prerequisite."
                    };
                    note(
                        "CQL_TARGET_REQUIREMENT",
                        format!(
                            "{} requires this prerequisite: {} Availability has not been confirmed for the selected target. {remediation}",
                            spec.name, requirement.note
                        ),
                    )
                };
                self.notice(key, diagnostic);
            }
            if let Some(entry) = crate::catalog::scope_entry(spec.name) {
                let form_conditions = entry["forms"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(
                        |form| match (spec.name, form["id"].as_str().unwrap_or("")) {
                            ("wildcard", "expression") => scalar,
                            ("rename", "field_pairs") => {
                                matches!(resolved.arguments.get("field"), Some(Value::Array(_)))
                            }
                            ("cidr", "lookup_subnets") => resolved.arguments.contains_key("file"),
                            ("worldMap", "ip_input") => resolved.arguments.contains_key("ip"),
                            _ => false,
                        },
                    )
                    .flat_map(|form| form["conditions"].as_array().into_iter().flatten());
                for condition in entry["conditions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .chain(form_conditions)
                {
                    let kind = condition["kind"].as_str().unwrap_or("");
                    let value = condition["value"].as_str().unwrap_or("");
                    if !matches!(kind, "lifecycle" | "minimum_version" | "resource")
                        || kind == "resource" && !spec.requirements().is_empty()
                    {
                        continue;
                    }
                    let key = format!("{}:{kind}:{value}:{target:?}", spec.name);
                    let diagnostic = if kind == "minimum_version" {
                        if let Some(qualified) = &qualified {
                            observed(format!(
                                "{}: this form was observed available at {}; evidence expires at {}.",
                                spec.name, qualified.observed_at, qualified.expires_at
                            ))
                        } else {
                            note(
                                "CQL_TARGET_REQUIREMENT",
                                format!(
                                    "{} requires target support documented from version {value}. Availability has not been confirmed for the selected target.",
                                    spec.name
                                ),
                            )
                        }
                    } else if kind == "lifecycle" {
                        Diagnostic::warning(
                            "CQL_FUNCTION_LIFECYCLE",
                            format!(
                                "{} is {value}; {}",
                                spec.name,
                                condition["reason"]
                                    .as_str()
                                    .unwrap_or("verify the documented usage restrictions")
                            ),
                        )
                    } else {
                        note(
                            "CQL_TARGET_REQUIREMENT",
                            format!(
                                "{} requires {value}; verify the existing resource and access on the selected target.",
                                spec.name
                            ),
                        )
                    };
                    self.notice(key, diagnostic);
                }
            }
            let nested_target = resolved
                .arguments
                .get("view")
                .and_then(|v| v.text())
                .or(target);
            let enclosing_evidence = self.evidence;
            let enclosing_remote = self.remote;
            if spec.name == "remoteTable" {
                self.evidence = None;
                self.remote = true;
            }
            for argument in &call.args {
                self.value(&argument.value, nested_target);
            }
            self.evidence = enclosing_evidence;
            self.remote = enclosing_remote;
        }
        fn value(&mut self, value: &Value, target: Option<&str>) {
            match value {
                Value::Call(call) => self.call(call, target, true),
                Value::Array(values) => {
                    for value in values {
                        self.value(value, target);
                    }
                }
                Value::Query { pipeline, .. } => self.pipeline(pipeline, target),
                Value::Binary { left, right, .. } => {
                    self.value(left, target);
                    self.value(right, target);
                }
                Value::Unary { value, .. } => self.value(value, target),
                _ => {}
            }
        }
        fn filter(&mut self, filter: &Filter, target: Option<&str>) {
            match filter {
                Filter::Call(call) => self.call(call, target, false),
                Filter::And(filters) | Filter::Or(filters) => {
                    for filter in filters {
                        self.filter(filter, target);
                    }
                }
                Filter::Not(filter) => self.filter(filter, target),
                _ => {}
            }
        }
        fn pipeline(&mut self, pipeline: &Pipeline, target: Option<&str>) {
            let semantic_steps: Vec<_> = pipeline.steps.iter().map(Step::semantic).collect();
            for step in semantic_steps.iter() {
                let step = step.as_ref();
                match step {
                    Step::Call(call) => self.call(call, target, false),
                    Step::Assign { value, .. } => self.value(value, target),
                    Step::Filter(filter) => self.filter(filter, target),
                    Step::Case(branches) => {
                        for branch in branches {
                            self.pipeline(branch, target);
                        }
                    }
                    Step::Match { arms, .. } => {
                        for (_, branch) in arms {
                            self.pipeline(branch, target);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut notices = Notices {
        selected_view: selected_environment.map(|environment| environment.view.as_str()),
        evidence,
        remote: false,
        qualified: BTreeMap::new(),
        diagnostics: BTreeMap::new(),
    };
    notices.pipeline(pipeline, None);
    notices.diagnostics.into_values().collect()
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
    )
    .0)
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

/// Suggestions retain exact input scopes; they are never observation evidence.
pub fn inventory_hints(query: &str) -> Result<Vec<InventoryHint>, Diagnostic> {
    let leads = unverified_leads(query)?;
    let pipeline = language::parser::parse(query).map_err(|error| error.diagnostic)?;
    let mut hints: BTreeMap<_, BTreeSet<String>> = BTreeMap::new();
    let mut flow = Flow::default();
    constrain(&mut flow, &pipeline.steps);
    if !flow.uncertain_source && (flow.scope.repo.is_some() || flow.scope.type_.is_some()) {
        hints.insert((flow.scope.repo, flow.scope.type_), BTreeSet::new());
    }
    for lead in leads {
        hints
            .entry((lead.source.repo, lead.source.type_))
            .or_default()
            .insert(lead.field);
    }
    Ok(hints
        .into_iter()
        .map(|((repo, type_), fields)| InventoryHint {
            source: Scope {
                repo,
                type_,
                event: None,
            },
            fields: fields.into_iter().collect(),
        })
        .collect())
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
    let (dropped, unmapped);
    (result.grounding, dropped, unmapped) = ground(pipeline, context, pack, evidence);
    let capability_evidence = evidence.and_then(|e| e.capabilities).filter(|e| {
        report.status == Acceptance::Accepted
            && evidence.is_some_and(|outer| outer.environment == e.environment)
            && context.execution_context_id.as_deref() == e.execution_context_id
            && context
                .environment
                .as_ref()
                .is_none_or(|target| target == e.environment)
            && pack.is_none_or(|pack| &pack.environment == e.environment)
            && result.grounding.conflicting.is_empty()
    });
    result.diagnostics.extend(target_requirements(
        pipeline,
        context
            .environment
            .as_ref()
            .or(pack.map(|pack| &pack.environment))
            .or(evidence.map(|evidence| evidence.environment)),
        capability_evidence,
    ));
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
            .any(|basis| basis.starts_with("captured search observation;"))
        {
            result.diagnostics.push(Diagnostic::warning(
                "CQL_CAPTURED_OBSERVATION",
                format!("{key}: {evidence}; the search and its result were passed on by a client hook, not independent execution attestation."),
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
    for field in dropped {
        if !report.dropped.contains(&field) {
            result.diagnostics.push(Diagnostic::warning(
                "CQL_DROPPED_FIELD",
                format!(
                    "'{field}' is not available here; an earlier step removed it. Keep it before that step, use the new name after rename, or recompute it before reading it."
                ),
            ));
        }
    }
    result.diagnostics.extend(unmapped);
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
    removed: bool,
    unexported_by: Option<Exporter>,
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
    in_selector: bool,
    query: Option<String>,
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
    unwritten_removals: BTreeSet<String>,
    removing: BTreeSet<String>,
    split_unknown: BTreeSet<(String, bool)>,
    unverified_fields: BTreeSet<String>,
    unknown_enrichment: bool,
    unknown_prefixes: BTreeSet<String>,
    export: Option<Export>,
}
#[derive(Clone)]
enum Export {
    Correlate {
        labels: BTreeSet<String>,
        mandatory_labels: BTreeSet<String>,
        include_all_labels: BTreeSet<String>,
        branch_available: BTreeMap<String, BTreeSet<String>>,
        outputs: BTreeSet<String>,
        removed: BTreeSet<String>,
        removed_after: BTreeSet<String>,
    },
    Query(String),
}
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Exporter {
    QueryExport(String),
    QueryPipeline(String),
    Correlate,
}
fn removed_matches(removed: &BTreeSet<String>, field: &str) -> bool {
    removed
        .iter()
        .any(|removed| removed == field || removed.contains("[]") && array_element(field, removed))
}
impl Flow {
    fn unexported_by(&self, field: &str) -> Option<Exporter> {
        match self.export.as_ref()? {
            Export::Query(name) => Some(Exporter::QueryPipeline(name.clone())),
            Export::Correlate {
                labels,
                mandatory_labels,
                include_all_labels,
                branch_available,
                outputs,
                removed,
                removed_after,
            } => {
                let removed_post = removed_matches(removed_after, field);
                let removed_in_branch = !outputs.contains(field) && removed_matches(removed, field);
                let missing = !removed_post && (removed_in_branch || !outputs.contains(field));
                missing.then(|| match field.split_once('.') {
                    Some((label, sub)) if labels.contains(label) => {
                        let excluded_in_branch = removed_in_branch
                            || !mandatory_labels.contains(label)
                            || include_all_labels.contains(label)
                            || crate::catalog::CORRELATE_BUILTINS.contains(&sub)
                            || branch_available
                                .get(label)
                                .is_some_and(|cols| !cols.contains(sub));
                        if excluded_in_branch {
                            Exporter::QueryPipeline(label.into())
                        } else {
                            Exporter::QueryExport(label.into())
                        }
                    }
                    _ => Exporter::Correlate,
                })
            }
        }
    }
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
        if self.derived.contains(field) || self.arrays.iter().any(|a| array_element(field, a)) {
            return false;
        }
        self.conditional.contains(field)
            || self
                .conditional_arrays
                .iter()
                .any(|a| array_element(field, a))
    }
    fn produces(&self, field: &str) -> bool {
        !self.conditional(field)
            && (self.derived.contains(field) || self.arrays.iter().any(|a| array_element(field, a)))
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
    fn remove(&mut self, field: String) {
        self.removing.insert(field.clone());
        if let Some(Export::Correlate { removed_after, .. }) = &mut self.export {
            removed_after.insert(field.clone());
        }
        self.removed.insert(field);
    }
    fn removed_in(&self, removed: &BTreeSet<String>, field: &str) -> bool {
        removed.iter().any(|removed| {
            removed == field || removed.contains("[]") && array_element(field, removed)
        }) && !self.produces(field)
    }
    fn absent(&self, field: &str) -> bool {
        self.removed_in(&self.removed, field)
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
        !self.arrays.iter().any(|known| array_covers(known, array))
            && (self.removed.iter().any(|removed| {
                removed == array || removed.contains("[]") && array_covers(removed, array)
            }) || !self.unknown_namespace(array)
                && !self
                    .split_unknown
                    .iter()
                    .any(|(prefix, strip)| *strip || path_contains(prefix, array))
                && !self
                    .arrays
                    .iter()
                    .chain(&self.conditional_arrays)
                    .any(|a| array_covers(a, array) || array_covers(array, a))
                && self.available.as_ref().is_some_and(|fields| {
                    !fields.iter().any(|f| indexed_member(f, array).is_some())
                }))
    }
    fn array_produces(&self, array: &str) -> bool {
        if self.arrays.iter().any(|a| array_covers(a, array)) {
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
        self.arrays.iter().any(|a| array_covers(a, member))
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
pub(crate) fn path_contains(parent: &str, field: &str) -> bool {
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

pub(crate) fn array_covers(array: &str, field: &str) -> bool {
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
    state.export = None;
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
    let strip = args.get("strip").and_then(|v| v.boolean()) == Some(true);
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
        state.remove(field.clone());
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
) -> (Vec<Requirement>, bool) {
    let mut out = GroundingScan {
        requirements: Vec::new(),
        record_kind_context: (!packs.is_empty()).then(|| (context, packs.to_vec(), grounding)),
        first_pipeline: true,
        skip_next_kind_check: false,
        in_selector: false,
        query: None,
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
    (out.requirements, out.in_selector)
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
            removed: false,
            unexported_by: unavailable.then(|| state.unexported_by(&mapped)).flatten(),
        });
        return;
    }
    let array = field.ends_with("[]");
    let unavailable = state.absent(field);
    out.requirements.push(Requirement {
        target: state.field_target.clone(),
        array: false,
        scope: state.scope.clone(),
        field: field.into(),
        derived: state.produces(field)
            || array && state.array_produces(field)
            || element_written(&state.derived, field),
        conditional: state.conditional(field)
            || array && state.array_conditional(field)
            || element_written(&state.conditional, field),
        origin: state.origin,
        uncertain_source: state.uncertain_source || state.unverified(field),
        relaxed_type: state.relaxed_type,
        relaxed_event: state.relaxed_event,
        unavailable,
        unexported_by: unavailable.then(|| state.unexported_by(field)).flatten(),
        removed: state.removed_in(&state.removed, field)
            && state.removed_in(&state.unwritten_removals, field),
    });
}
fn element_written(fields: &BTreeSet<String>, array: &str) -> bool {
    array.ends_with("[]") && fields.iter().any(|field| array_element(field, array))
}
fn add_array(state: &Flow, array: &str, out: &mut GroundingScan<'_>) {
    let field = local_field(state, array);
    let unavailable = state.array_absent(&field);
    out.requirements.push(Requirement {
        target: state.field_target.clone(),
        array: true,
        scope: state.scope.clone(),
        derived: state.array_produces(&field) || element_written(&state.derived, &field),
        conditional: state.array_conditional(&field) || element_written(&state.conditional, &field),
        origin: state.origin,
        uncertain_source: state.uncertain_source || state.unverified(&field),
        relaxed_type: state.relaxed_type,
        relaxed_event: state.relaxed_event,
        unavailable,
        removed: false,
        unexported_by: unavailable.then(|| state.unexported_by(&field)).flatten(),
        field,
    });
}

fn add_unknown(state: &Flow, field: &str, out: &mut GroundingScan<'_>) {
    out.requirements.push(Requirement {
        target: state.field_target.clone(),
        array: false,
        scope: state.scope.clone(),
        field: field.into(),
        derived: false,
        conditional: false,
        origin: state.origin,
        uncertain_source: true,
        relaxed_type: state.relaxed_type,
        relaxed_event: state.relaxed_event,
        unavailable: false,
        removed: false,
        unexported_by: None,
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

fn step_aggregates(step: &Step) -> bool {
    match step {
        Step::Call(call) => crate::catalog::function(&call.name)
            .is_some_and(|function| function.effect == crate::catalog::Effect::Aggregate),
        Step::Assign { value, .. } => crate::catalog::assigned_aggregate(value).is_some(),
        _ => false,
    }
}

fn correlation_input_fields(
    state: &Flow,
    args: &BTreeMap<String, &Value>,
    out: &mut GroundingScan<'_>,
) {
    if let Some(globals) = args.get("globalConstraints") {
        for field in fields_of(globals) {
            add(state, &field, out);
        }
    }
    if let Some(dimension) = args.get("sequenceBy") {
        for field in fields_of(dimension) {
            add(state, &field, out);
        }
    } else {
        add(state, "@timestamp", out);
    }
}

fn correlation_inputs(
    pipeline: &Pipeline,
    mut state: Flow,
    args: &BTreeMap<String, &Value>,
    out: &mut GroundingScan<'_>,
) -> Option<Flow> {
    // Trace only paths that have not aggregated. The ordinary scan records all query reads.
    let mut trace = GroundingScan {
        requirements: Vec::new(),
        record_kind_context: None,
        first_pipeline: false,
        skip_next_kind_check: true,
        in_selector: false,
        query: None,
    };
    let mut start = 0;
    let semantic_steps: Vec<_> = pipeline.steps.iter().map(Step::semantic).collect();
    for (index, step) in semantic_steps.iter().enumerate() {
        let step = step.as_ref();
        if !step_aggregates(step) && !matches!(step, Step::Case(_) | Step::Match { .. }) {
            continue;
        }
        state = scan_pipeline(
            &Pipeline {
                advisories: vec![],
                steps: pipeline.steps[start..index].to_vec(),
            },
            state,
            &mut trace,
        );
        trace.requirements.clear();
        if step_aggregates(step) {
            state.export = out.query.clone().map(Export::Query);
            correlation_input_fields(&state, args, out);
            return None;
        }
        let pending = match step {
            Step::Case(branches) => branches
                .iter()
                .filter_map(|branch| correlation_inputs(branch, state.clone(), args, out))
                .collect::<Vec<_>>(),
            Step::Match { field, arms } => arms
                .iter()
                .filter_map(|(pattern, branch)| {
                    let incoming = scan_pipeline(
                        &Pipeline {
                            advisories: vec![],
                            steps: vec![Step::Match {
                                field: field.clone(),
                                arms: vec![(pattern.clone(), Pipeline::default())],
                            }],
                        },
                        state.clone(),
                        &mut trace,
                    );
                    trace.requirements.clear();
                    correlation_inputs(branch, incoming, args, out)
                })
                .collect::<Vec<_>>(),
            _ => unreachable!("only conditional steps remain"),
        };
        if pending.is_empty() {
            return None;
        }
        state = merge_branches(&state, pending);
        start = index + 1;
    }
    Some(scan_pipeline(
        &Pipeline {
            advisories: vec![],
            steps: pipeline.steps[start..].to_vec(),
        },
        state,
        &mut trace,
    ))
}

fn scan_call(c: &Call, state: &Flow, out: &mut GroundingScan<'_>, negated: bool) -> Option<Flow> {
    let resolved = crate::catalog::resolve(c)?;
    out.in_selector |= resolved.function.name == "in"
        && resolved.arguments.get("field").is_some_and(|value| {
            fields_of(value)
                .iter()
                .any(|field| matches!(field.as_str(), "#repo" | "#type"))
        });
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
                out.query = Some(name.clone());
                let export = Some(Export::Query(name.clone()));
                if *aggregation {
                    if let Some(mut input) =
                        correlation_inputs(pipeline, state.clone(), &resolved.arguments, out)
                    {
                        input.export = export.clone();
                        correlation_input_fields(&input, &resolved.arguments, out);
                    }
                } else {
                    out.skip_next_kind_check = true;
                    let mut input = scan_pipeline(pipeline, state.clone(), out);
                    input.export = export;
                    correlation_input_fields(&input, &resolved.arguments, out);
                    contexts.insert(name.clone(), input);
                    continue;
                }
                out.skip_next_kind_check = true;
                let mut local = scan_pipeline(pipeline, state.clone(), out);
                local.export = export;
                contexts.insert(name.clone(), local);
            }
        }
        out.query = None;
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
        let (mut outputs, unknown_prefixes) =
            crate::catalog::correlation_outputs(&resolved.arguments, &queries, &columns);
        let removed: BTreeSet<String> = contexts
            .iter()
            .flat_map(|(name, flow)| flow.removed.iter().map(move |f| format!("{name}.{f}")))
            .collect();
        outputs.retain(|f| {
            f.split_once('.').map_or_else(
                || !removed_matches(&removed, f),
                |(name, sub)| {
                    contexts
                        .get(name)
                        .map_or_else(|| !removed_matches(&removed, f), |flow| !flow.absent(sub))
                },
            )
        });
        let mandatory_labels: BTreeSet<String> = queries
            .iter()
            .filter(|(_, aggregation)| !*aggregation)
            .filter_map(|(value, _)| match value {
                Value::Query {
                    label: Some(name), ..
                } => Some(name.clone()),
                _ => None,
            })
            .collect();
        let include_all_labels: BTreeSet<String> = queries
            .iter()
            .filter(|(_, aggregation)| !*aggregation)
            .filter_map(|(value, _)| match value {
                Value::Query {
                    label: Some(name),
                    attributes,
                    ..
                } if attributes.iter().any(|a| {
                    a.name.as_deref() == Some("include")
                        && crate::catalog::query_all_fields(&a.value)
                }) =>
                {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        let branch_available: BTreeMap<String, BTreeSet<String>> = contexts
            .iter()
            .filter_map(|(name, flow)| {
                flow.available
                    .as_ref()
                    .map(|cols| (name.clone(), cols.clone()))
            })
            .collect();
        return Some(Flow {
            target: state.target.clone(),
            field_target: state.target.clone(),
            derived: outputs.clone(),
            available: Some(outputs.clone()),
            removed: removed.clone(),
            unknown_prefixes,
            tables: state.tables.clone(),
            export: Some(Export::Correlate {
                labels: contexts.into_keys().collect(),
                mandatory_labels,
                include_all_labels,
                branch_available,
                outputs,
                removed,
                removed_after: BTreeSet::new(),
            }),
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
        if resolved.function.name == "remoteTable" {
            let external = Flow {
                origin: Origin::External,
                ..Flow::default()
            };
            return Some(if let Value::Query { pipeline, .. } = value {
                scan_pipeline(pipeline, external, out)
            } else {
                external
            });
        }
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
        {
            for name in resolved.resource_names() {
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
        if negated
            && matches!(resolved.function.name, "match" | "matchAsArray")
            && name == "include"
        {
            continue;
        }
        if name == "source" && resolved.function.name == "getField" {
            continue;
        }
        if name == "query" && foreign.is_some() {
            continue;
        }
        if resolved.function.name.starts_with("time:")
            && name == "timezoneField"
            && resolved.arguments.contains_key("timezone")
        {
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
                if !(matches!(resolved.function.name, "defineTable" | "remoteTable")
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
                if resolved.function.name == "array:append"
                    && name == "array"
                    && state.array_absent(&field)
                {
                    continue;
                }
                if resolved.function.name != "default" || !state.absent(&field) {
                    if resolved.function.name == "split" && name == "field" {
                        add_array(state, &format!("{field}[]"), out);
                    } else if name == "array"
                        && (field.ends_with("[]") || resolved.function.name.starts_with("array:"))
                    {
                        add_array(state, &format!("{}[]", field.trim_end_matches("[]")), out);
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
                | crate::catalog::ArgumentScope::FilterQuery
                | crate::catalog::ArgumentScope::Aggregate => scan_queries(value, state, out),
                crate::catalog::ArgumentScope::Inline
                | crate::catalog::ArgumentScope::ElementAggregate => {
                    let mut local = state.clone();
                    if let Some(array) = resolved.arguments.get("array").and_then(|v| v.text()) {
                        let array_name = if resolved.function.argument_kind("array") == Some("z") {
                            format!("{}[]", array.trim_end_matches("[]"))
                        } else {
                            array.into()
                        };
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
                                array: local_field(state, &array_name),
                                structured,
                            },
                        );
                    }
                    let exported = match value {
                        Value::Query { pipeline, .. } => Some(scan_pipeline(pipeline, local, out)),
                        Value::Call(call) => Some(scan_pipeline(
                            &Pipeline {
                                advisories: vec![],
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
    if matches!(resolved.function.name, "join" | "match" | "matchAsArray") {
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
        Value::Word(name)
            if crate::catalog::function(name).is_some_and(|f| {
                f.effect == crate::catalog::Effect::Aggregate && f.required.is_empty()
            }) =>
        {
            scan_call(&Call::new(name, vec![]), state, out, false);
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
    let candidate_removed: BTreeSet<_> = branches
        .iter()
        .flat_map(|branch| branch.removed.iter().cloned())
        .collect();
    merged.removed = candidate_removed
        .into_iter()
        .filter(|field| branches.iter().all(|branch| branch.absent(field)))
        .collect();
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
    let semantic_steps: Vec<_> = steps.iter().map(Step::semantic).collect();
    for (index, step) in semantic_steps.iter().enumerate() {
        let step = step.as_ref();
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
    let semantic_steps: Vec<_> = p.steps.iter().map(Step::semantic).collect();
    for (index, step) in semantic_steps.iter().enumerate() {
        let step = step.as_ref();
        if let Step::Filter(filter) = step
            && (!pure_filter(filter)
                || index == 0
                || !matches!(&p.steps[index - 1], Step::Filter(filter) if pure_filter(filter)))
        {
            constrain(&mut state, &p.steps[index..]);
        }
        let removed = state.removed.clone();
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
                if let Some(call) = crate::catalog::aliased_call(field, value) {
                    let definite = call.name.eq_ignore_ascii_case("formatTime")
                        && state.definite.contains(field);
                    state = scan_pipeline(
                        &Pipeline {
                            advisories: vec![],
                            steps: vec![Step::Call(call)],
                        },
                        state,
                        out,
                    );
                    if definite {
                        write(&mut state, field.clone(), true);
                    }
                    continue;
                }
                expression_fields(value, &state, out);
                if crate::catalog::assigned_aggregate(value).is_some() {
                    project(&mut state, BTreeSet::new());
                    state.derived.clear();
                    state.conditional.clear();
                    state.definite.clear();
                }
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
            Step::Link { field, .. } => {
                let mut linked = state.clone();
                linked.export = out.query.clone().map(Export::Query);
                add(&linked, field, out);
            }
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
                        if spec.name == "createEvents" {
                            let fields = ["@rawstring", "@timestamp", "@timestamp.nanos"]
                                .into_iter()
                                .map(str::to_owned)
                                .collect::<BTreeSet<_>>();
                            state.available = Some(fields.clone());
                            state.definite = fields.clone();
                            state.derived = fields;
                        }
                    }
                    if spec.name == "correlate"
                        && let Some(exported) = &exported
                    {
                        state = exported.clone();
                    }
                    if matches!(spec.name, "defineTable" | "remoteTable")
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
                            if spec.name == "defineTable" {
                                exported.derived = fields;
                            }
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
                    if spec.name != "split"
                        && let Some(writes) = crate::catalog::output_writes(c)
                        && let Some(prefix) = writes.dynamic_prefix
                    {
                        state.unknown_prefixes.insert(prefix.clone());
                        if !writes.preserves_existing {
                            state.conditional.extend(
                                state
                                    .derived
                                    .iter()
                                    .filter(|field| field.starts_with(&prefix))
                                    .cloned(),
                            );
                            state.derived.retain(|field| !field.starts_with(&prefix));
                            state.definite.retain(|field| !field.starts_with(&prefix));
                            state.conditional_arrays.extend(
                                state
                                    .arrays
                                    .iter()
                                    .filter(|field| field.starts_with(&prefix))
                                    .cloned(),
                            );
                            state.arrays.retain(|field| !field.starts_with(&prefix));
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
                            resolved.arguments.get("strict").and_then(|v| v.boolean())
                                != Some(false),
                            resolved.arguments.get("repeat").and_then(|v| v.boolean())
                                == Some(true),
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
                                    state.remove(old.into());
                                    if let Some(available) = &mut state.available {
                                        available.remove(old);
                                    }
                                }
                                if old != new {
                                    write(&mut state, new.into(), definite);
                                }
                            }
                        }
                        "setField" => {
                            if let Some(Value::String(field)) = resolved.arguments.get("target") {
                                write(&mut state, field.clone(), false);
                            }
                        }
                        "lowercase" => {
                            let rename = resolved
                                .arguments
                                .get("include")
                                .and_then(|v| v.text())
                                .is_some_and(|v| matches!(v, "fields" | "both"));
                            if let Some(value) = resolved.arguments.get("field") {
                                for old in fields_of(value) {
                                    let field = if rename {
                                        old.to_lowercase()
                                    } else {
                                        old.clone()
                                    };
                                    if field != old {
                                        state.derived.remove(&old);
                                        state.definite.remove(&old);
                                        state.conditional.insert(old.clone());
                                        state.removed.remove(&old);
                                        state.unverified_fields.insert(old.clone());
                                    }
                                    write(&mut state, field, false);
                                }
                            }
                        }
                        "array:drop" | "array:rename" => {
                            if let Some(array) =
                                resolved.arguments.get("array").and_then(|v| v.text())
                            {
                                if spec.name == "array:rename"
                                    && resolved.arguments.get("asArray").and_then(|v| v.text())
                                        == Some(array)
                                {
                                    continue;
                                }
                                let prefix = array.trim_end_matches("[]");
                                state.remove(array.into());
                                state.derived.retain(|field| !path_contains(prefix, field));
                                state
                                    .conditional
                                    .retain(|field| !path_contains(prefix, field));
                                state.definite.retain(|field| !path_contains(prefix, field));
                                state.arrays.retain(|field| !path_contains(prefix, field));
                                state
                                    .conditional_arrays
                                    .retain(|field| !path_contains(prefix, field));
                                if let Some(fields) = &mut state.available {
                                    fields.retain(|field| !path_contains(prefix, field));
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
                                    state.remove(field.clone());
                                    if let Some(available) = &mut state.available {
                                        available.remove(&field);
                                    }
                                }
                            }
                        }
                        _ if spec.resolved_effect(&resolved.arguments)
                            == crate::catalog::Effect::Aggregate =>
                        {
                            if let Some((outputs, retains)) = crate::catalog::aggregate_outputs(c) {
                                if spec.name == "appendAggregation" {
                                    let mut summary = state.clone();
                                    project(&mut summary, outputs.clone());
                                    summary.arrays.extend(
                                        outputs
                                            .iter()
                                            .filter(|field| field.contains("[]"))
                                            .cloned(),
                                    );
                                    summary.derived = outputs;
                                    summary.conditional.clear();
                                    summary.definite.clear();
                                    state = merge_branches(&state, vec![state.clone(), summary]);
                                } else if retains {
                                    for field in outputs {
                                        write(&mut state, field, false);
                                    }
                                } else {
                                    project(&mut state, outputs.clone());
                                    state.arrays.extend(
                                        outputs
                                            .iter()
                                            .filter(|field| field.contains("[]"))
                                            .cloned(),
                                    );
                                    state.derived = outputs;
                                    state.conditional.clear();
                                    state.definite.clear();
                                    state.removed.clear();
                                }
                            } else {
                                state.derived.clear();
                                state.conditional.clear();
                                state.definite.clear();
                                state.arrays.clear();
                                state.conditional_arrays.clear();
                                state.available = None;
                                state.unknown_enrichment = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if !adds_no_fields(step) {
            state.unwritten_removals.clear();
        }
        let removing = std::mem::take(&mut state.removing);
        state
            .unwritten_removals
            .extend(state.removed.difference(&removed).cloned().chain(removing));
    }
    state
}

fn adds_no_fields(step: &Step) -> bool {
    match step {
        Step::Filter(filter) => filter_adds_no_fields(filter),
        Step::Call(call) => {
            transparent_kind_call(call)
                || crate::catalog::resolve(call)
                    .is_some_and(|r| r.diagnostics.is_empty() && r.function.name == "drop")
        }
        _ => false,
    }
}

fn filter_adds_no_fields(filter: &Filter) -> bool {
    match filter {
        Filter::Compare {
            value: Pattern::Regex { pattern, .. },
            ..
        }
        | Filter::Text(Pattern::Regex { pattern, .. }) => {
            language::validate::regex_captures(pattern).is_empty()
        }
        Filter::Compare { .. } | Filter::Text(_) | Filter::Bool(_) => true,
        Filter::Call(call) => transparent_kind_call(call),
        Filter::And(filters) | Filter::Or(filters) => filters.iter().all(filter_adds_no_fields),
        Filter::Not(filter) => filter_adds_no_fields(filter),
    }
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
        "in" if resolved
            .arguments
            .get("ignoreCase")
            .and_then(|v| v.boolean())
            != Some(true) =>
        {
            let values = resolved.arguments.get("values")?.string_array()?;
            (values.len() == 1 && !values[0].contains('*')).then(|| values[0].clone())
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
                    && resolved.arguments.get("strict").and_then(|v| v.boolean()) == Some(false)
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
    let semantic_steps: Vec<_> = pipeline.steps.iter().map(Step::semantic).collect();
    for step in semantic_steps.iter() {
        let step = step.as_ref();
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
    let semantic_steps: Vec<_> = pipeline.steps.iter().map(Step::semantic).collect();
    for step in semantic_steps.iter() {
        let step = step.as_ref();
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
    let semantic_steps: Vec<_> = pipeline.steps.iter().map(Step::semantic).collect();
    for (index, step) in semantic_steps.iter().enumerate() {
        let step = step.as_ref();
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
) -> (Grounding, BTreeSet<String>, Vec<Diagnostic>) {
    let mut g = Grounding {
        policy: context.into(),
        ..Grounding::default()
    };
    if let Some(e) = &context.environment
        && let Err(diagnostic) = environment::validate_identity(e)
    {
        g.conflicting.push(diagnostic.message);
    }
    if let Some(p) = pack {
        g.pack_id = Some(p.content_digest.clone());
        if p.validate().is_err() {
            g.conflicting
                .push("Pack integrity or version validation failed.".into());
            return (g, BTreeSet::new(), Vec::new());
        }
        if context
            .environment
            .as_ref()
            .is_some_and(|e| e != &p.environment)
        {
            g.conflicting
                .push("Cloud, tenant or searchable target conflicts with selected pack.".into());
            return (g, BTreeSet::new(), Vec::new());
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
            return (g, BTreeSet::new(), Vec::new());
        }
        if evidence.query_observations.len() > 1000
            || evidence.captured_observations.len() > 1000
            || evidence
                .query_observations
                .iter()
                .chain(evidence.captured_observations)
                .any(|observation| {
                    observation.validate().is_err()
                        || &observation.environment != evidence.environment
                })
        {
            g.conflicting
                .push("Query observation integrity, identity or budget validation failed.".into());
            return (g, BTreeSet::new(), Vec::new());
        }
        if let Some(active) = evidence.active_pack {
            if active.validate().is_err() || &active.environment != evidence.environment {
                g.conflicting
                    .push("Active pack integrity or environment validation failed.".into());
                return (g, BTreeSet::new(), Vec::new());
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
    let mut decoded = Vec::<DecodedPack>::new();
    for p in pack.into_iter().chain(evidence.and_then(|e| e.active_pack)) {
        if !decoded.iter().any(|d| std::ptr::eq(d.pack, p)) {
            decoded.push(DecodedPack::new(p));
        }
    }
    let decoded_of = |p: &Pack| {
        decoded
            .iter()
            .find(|d| std::ptr::eq(d.pack, p))
            .expect("every pack is decoded")
    };
    let evidence_packs: Vec<_> = pack
        .into_iter()
        .chain(evidence.and_then(|e| e.active_pack))
        .map(decoded_of)
        .collect();
    let mut query_observed = false;
    let (reads, in_selector) = requirements(
        pipeline,
        primary_target.clone(),
        context,
        &kind_packs,
        &mut g,
    );
    let lenient = !context.require_observed;
    let compare = lenient && !in_selector;
    let pack_sources: Vec<_> = pack
        .iter()
        .flat_map(|p| &p.data.sources)
        .filter_map(|v| environment::source(v).ok())
        .collect();
    let mapped: Vec<_> = pack_sources
        .iter()
        .filter(|source| source.selectors.is_some())
        .collect();
    let mut records = BTreeMap::<String, Vec<(String, environment::LegacyField)>>::new();
    for field in pack
        .filter(|_| compare)
        .iter()
        .flat_map(|p| &p.data.fields)
        .filter_map(|v| environment::field(v).ok())
    {
        let tag = field.is_tag == Some(true) || field.path.starts_with('#');
        records
            .entry(field.source_id.clone())
            .or_default()
            .push((map_path(&field.path, tag), field));
    }
    let recorded = |value: &str, repo: bool| {
        mapped
            .iter()
            .filter_map(|s| s.selectors.as_ref())
            .any(|s| selector(s, repo) == Some(value))
    };
    let unrecorded = |repo: bool| {
        mapped
            .iter()
            .filter_map(|s| s.selectors.as_ref())
            .any(|s| selector(s, repo).is_none())
    };
    let view = primary_target.as_deref();
    let mut dropped = BTreeSet::new();
    let mut unexported = BTreeMap::<Exporter, BTreeSet<String>>::new();
    let mut unmapped_selections = BTreeSet::new();
    let mut unmapped_fields = BTreeMap::<String, BTreeSet<String>>::new();
    let mut measured_absent = BTreeMap::<(String, Option<String>), BTreeSet<String>>::new();
    for r in reads {
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
            } else if r.removed {
                dropped.insert(r.field.clone());
            }
            if lenient && let Some(source) = r.unexported_by {
                unexported.entry(source).or_default().insert(r.field);
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
        let mut selected = Vec::new();
        if pack.is_some() && r.origin == Origin::Deployment {
            selected = pack_sources
                .iter()
                .filter(|source| {
                    r.scope.repo.is_none() && r.scope.type_.is_none()
                        || source_overlaps_scope(source, &r.scope)
                })
                .collect();
            let view_repo = r
                .scope
                .repo
                .as_deref()
                .is_some_and(|repo| Some(repo) == view && !recorded(repo, true));
            if view_repo || selected.is_empty() && !mapped.is_empty() {
                unmapped_selections.insert((r.scope.repo.clone(), r.scope.type_.clone()));
                selected.clear();
            }
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
        let mut winning_namespace = None;
        if let Some(evidence) = evidence {
            match query_observation_basis(
                &r,
                &evidence_packs,
                evidence,
                !lenient,
                &mut winning_namespace,
            ) {
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
        let decoded_p = decoded_of(p);
        let mut matches = decoded_p
            .fields
            .iter()
            .filter(|f| {
                field_matches_requirement(f, &r)
                    && (!r.array || f.is_array == Some(true) || f.data_type == "array")
            })
            .filter_map(|f| {
                decoded_p
                    .sources
                    .iter()
                    .find(|s| s.id == f.source_id && source_matches_requirement(s, &r))
                    .map(|source| {
                        (
                            f,
                            source
                                .selectors
                                .as_ref()
                                .expect("matching source has selectors"),
                            source,
                        )
                    })
            })
            .collect::<Vec<_>>();
        let sole_match = match matches.as_slice() {
            [candidate] => Some(*candidate),
            _ => None,
        };
        let mut ambiguity = None;
        if !lenient {
            matches.retain(|(_, selectors, _)| {
                !event_family_mismatch(selectors, &r)
                    && winning_namespace.is_none_or(|winner| winner == field_namespace(selectors))
            });
            if matches.len() > 1 {
                let namespaces = matches
                    .iter()
                    .map(|(_, selectors, _)| field_namespace(selectors))
                    .collect();
                match resolve_field_namespace(&namespaces, &r.scope) {
                    Ok(Some(winner)) => {
                        matches.retain(|(_, selectors, _)| winner == field_namespace(selectors))
                    }
                    Err(basis) => ambiguity = Some(basis),
                    Ok(None) => {}
                }
            }
        }
        let candidate = match matches.as_slice() {
            [candidate] => Some((*candidate, true)),
            // A nonmatching namespace can still report an unambiguous negative.
            [] => sole_match.map(|candidate| (candidate, false)),
            _ => None,
        };
        if let Some(((f, _, source), eligible)) = candidate {
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
            } else if observed && !eligible {
                let active_has_winner = winning_namespace.is_some_and(|winner| {
                    evidence
                        .and_then(|e| e.active_pack)
                        .filter(|active| active.content_digest != p.content_digest)
                        .map(decoded_of)
                        .is_some_and(|active| {
                            active.sources.iter().any(|source| {
                                source.selectors.as_ref().map(field_namespace) == Some(winner)
                                    && active.fields.iter().any(|field| {
                                        field.source_id == source.id
                                            && field_matches_requirement(field, &r)
                                    })
                            })
                        })
                });
                if active_has_winner {
                    "The selected pack measures this field at a different scope. See the field record at the query's scope in the inventory's active pack.".into()
                } else {
                    "query-supplied; not measured in selected pack".into()
                }
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
                } else if !observed || !eligible {
                    g.missing.push(key);
                }
            }
        } else {
            record_basis(
                &mut g,
                key.clone(),
                if matches.len() > 1 {
                    ambiguity.unwrap_or_else(|| AMBIGUOUS_FIELD.into())
                } else {
                    "query-supplied; not measured in selected pack".into()
                },
            );
            if context.require_observed {
                g.missing.push(key);
            }
        }
        if compare
            && !selected.is_empty()
            && !r.field.starts_with('@')
            && !selected.iter().any(|s| records_kind_field(s, &r.field))
        {
            let read = map_path(&r.field, r.field.starts_with('#'));
            let mut unlisted = Vec::new();
            let mut absent = BTreeMap::<Option<String>, Vec<_>>::new();
            let has_field = selected.iter().any(|s| {
                match map_gap(records.get(&s.id).into_iter().flatten(), &read) {
                    Some(MapGap::Unlisted) => unlisted.push(*s),
                    Some(MapGap::NotObserved(window)) => absent.entry(window).or_default().push(*s),
                    None => return true,
                }
                false
            });
            if !has_field {
                if !unlisted.is_empty() {
                    unmapped_fields
                        .entry(labels(unlisted))
                        .or_default()
                        .insert(r.field.clone());
                }
                for (window, sources) in absent {
                    measured_absent
                        .entry((labels(sources), window))
                        .or_default()
                        .insert(r.field.clone());
                }
            }
        }
    }
    let mut warnings = unexported
        .into_iter()
        .map(|(source, fields)| {
            let listed = fields.iter().cloned().collect::<Vec<_>>().join(", ");
            let verb = if fields.len() == 1 { "is" } else { "are" };
            let message = match source {
                Exporter::QueryExport(label) => {
                    let prefix = format!("{label}.");
                    let example = fields
                        .iter()
                        .map(|f| f.strip_prefix(&prefix).unwrap_or(f.as_str()))
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!(
                        "{listed} {verb} not exported by correlate query {label}. Add missing fields to {label}'s include: [...] clause after the closing brace (for example, {label}: {{ ... }} include: [{example}]), or keep them in {label}'s pipeline."
                    )
                }
                Exporter::QueryPipeline(label) => format!(
                    "{listed} {verb} not exported by correlate query {label}. Keep or compute {} inside {label}'s pipeline.",
                    if fields.len() == 1 { "it" } else { "them" },
                ),
                Exporter::Correlate => format!(
                    "{listed} {verb} not exported by correlate, which names the fields of each query QUERY.FIELD. Prefix {} with the query label (QUERY.FIELD) and, for a mandatory query, list non-metadata fields in its include: [...] clause after the closing brace.",
                    if fields.len() == 1 { "it" } else { "each field" },
                ),
            };
            Diagnostic::warning("CQL_UNEXPORTED_FIELD", message)
        })
        .collect::<Vec<_>>();
    let mut unmapped = Vec::new();
    for (repo, type_) in unmapped_selections {
        let lacking: Vec<_> = [(repo.as_deref(), true), (type_.as_deref(), false)]
            .into_iter()
            .filter_map(|(value, is_repo)| {
                let value = value?;
                (!recorded(value, is_repo)
                    && (!unrecorded(is_repo) || is_repo && Some(value) == view))
                    .then_some((value, is_repo))
            })
            .collect();
        let named = |is_repo: bool| if is_repo { "repo" } else { "type" };
        let head = if lacking.is_empty() {
            format!(
                "{} matches no mapped source.",
                selector_text(repo.as_deref(), type_.as_deref())
            )
        } else {
            format!(
                "{} {} no mapped source.",
                lacking
                    .iter()
                    .map(|(value, is_repo)| format!("#{}={value}", named(*is_repo)))
                    .collect::<Vec<_>>()
                    .join(" and "),
                if lacking.len() == 1 {
                    "matches"
                } else {
                    "match"
                }
            )
        };
        let views: String = lacking
            .iter()
            .filter(|(value, _)| Some(*value) == view)
            .map(|(value, is_repo)| {
                format!(
                    " {value} is the target view's name, not a mapped {}.",
                    if *is_repo { "repository" } else { "type" }
                )
            })
            .collect();
        let kept = |is_repo: bool| !lacking.iter().any(|(_, lacks)| *lacks == is_repo);
        let sharing: Vec<_> = mapped
            .iter()
            .copied()
            .filter(|s| {
                s.selectors.as_ref().is_some_and(|s| {
                    (repo.is_some() && kept(true) && s.repo == repo)
                        || (type_.is_some() && kept(false) && s.type_ == type_)
                })
            })
            .collect();
        let listed = if !sharing.is_empty() {
            format!(
                " Mapped sources with the same #repo or #type: {}.",
                labels(sharing)
            )
        } else if !mapped.is_empty() {
            format!(" Mapped selectors: {}.", labels(mapped.iter().copied()))
        } else {
            String::new()
        };
        unmapped.push(Diagnostic::warning(
            "CQL_UNMAPPED_SOURCE",
            format!("{head}{views}{listed}"),
        ));
    }
    let names = |fields: &BTreeSet<String>| fields.iter().cloned().collect::<Vec<_>>().join(", ");
    for (sources, fields) in unmapped_fields {
        unmapped.push(Diagnostic::warning(
            "CQL_UNMAPPED_FIELD",
            format!(
                "{} {} not observed, documented or declared in the map for {sources}.",
                names(&fields),
                if fields.len() == 1 { "is" } else { "are" },
            ),
        ));
    }
    for ((sources, window), fields) in measured_absent {
        unmapped.push(Diagnostic::warning(
            "CQL_UNMAPPED_FIELD",
            format!(
                "{} {} measured in the map for {sources} and not observed{}.",
                names(&fields),
                if fields.len() == 1 { "was" } else { "were" },
                window
                    .map(|window| format!(" during {window}"))
                    .unwrap_or_default()
            ),
        ));
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
    warnings.extend(unmapped);
    (g, dropped, warnings)
}

fn listed(field: &environment::LegacyField) -> bool {
    field.evidence_kind != "observed"
        || field
            .coverage
            .as_ref()
            .is_none_or(|c| c.observed != Some(false) || c.declared == Some(true))
}

fn records_kind_field(source: &environment::LegacySource, field: &str) -> bool {
    source.relationship.as_ref().is_some_and(|relationship| {
        field == "#event.dataset" && !relationship.datasets.is_empty()
            || relationship
                .record_kinds
                .iter()
                .any(|kind| kind.field == field)
    })
}

fn selector(selectors: &environment::Selectors, repo: bool) -> Option<&str> {
    if repo {
        selectors.repo.as_deref()
    } else {
        selectors.type_.as_deref()
    }
}

fn selector_text(repo: Option<&str>, type_: Option<&str>) -> String {
    [
        repo.map(|repo| format!("#repo={repo}")),
        type_.map(|type_| format!("#type={type_}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

fn labels<'a>(sources: impl IntoIterator<Item = &'a environment::LegacySource>) -> String {
    sources
        .into_iter()
        .map(|source| {
            source
                .selectors
                .as_ref()
                .map(|s| selector_text(s.repo.as_deref(), s.type_.as_deref()))
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| source.name.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("; ")
}

fn map_path(path: &str, tag: bool) -> String {
    let mut out = String::from(if tag { "#" } else { "" });
    let mut rest = path.trim_start_matches('#');
    while let Some((head, tail)) = rest.split_once('[') {
        out.push_str(head);
        out.push('[');
        rest = match tail.split_once(']') {
            Some((index, after)) if index.bytes().all(|b| b.is_ascii_digit()) => {
                out.push(']');
                after
            }
            _ => tail,
        };
    }
    out.push_str(rest);
    out
}

enum MapGap {
    Unlisted,
    NotObserved(Option<String>),
}

fn map_gap<'a>(
    records: impl Iterator<Item = &'a (String, environment::LegacyField)> + Clone,
    read: &str,
) -> Option<MapGap> {
    let exact: Vec<_> = records
        .clone()
        .filter(|(path, _)| path == read || read.ends_with("[]") && path_contains(read, path))
        .map(|(_, field)| field)
        .collect();
    if exact.iter().any(|field| listed(field)) {
        None
    } else if !exact.is_empty() {
        let window = exact.iter().find_map(|f| f.observation_window.clone());
        Some(MapGap::NotObserved(window))
    } else if records.into_iter().any(|(path, field)| {
        field.evidence_kind == "documented"
            && matches!(field.data_type.as_str(), "object" | "array")
            && path_contains(path, read)
    }) {
        None
    } else {
        Some(MapGap::Unlisted)
    }
}

const AMBIGUOUS_FIELD: &str = "multiple source-specific observations; not interchangeable";

type FieldNamespace<'a> = (
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
    Option<&'a str>,
);

fn field_namespace(selectors: &environment::Selectors) -> FieldNamespace<'_> {
    (
        selectors.repo.as_deref(),
        selectors.type_.as_deref(),
        selectors.event_family.as_deref(),
        selectors.sourcetype.as_deref(),
    )
}

fn exact_namespace((repo, kind, event, sourcetype): FieldNamespace<'_>, scope: &Scope) -> bool {
    repo == scope.repo.as_deref()
        && kind == scope.type_.as_deref()
        && event.is_none()
        && (sourcetype.is_none() || sourcetype == kind)
}

fn resolve_field_namespace<'a>(
    namespaces: &BTreeSet<FieldNamespace<'a>>,
    scope: &Scope,
) -> Result<Option<FieldNamespace<'a>>, String> {
    if namespaces.len() < 2 {
        return Ok(None);
    }
    let mut exact = namespaces
        .iter()
        .copied()
        .filter(|&namespace| exact_namespace(namespace, scope));
    let multiple_exact = match (exact.next(), exact.next()) {
        (Some(winner), None) => return Ok(Some(winner)),
        (Some(_), Some(_)) => true,
        _ => false,
    };
    let &(repo, kind, event, _) = namespaces.first().expect("competing namespaces");
    let next_action = if namespaces
        .iter()
        .all(|&(r, k, e, _)| (r, k, e) == (repo, kind, event))
    {
        "Select the intended snapshot, or measure the field at the query's own scope."
    } else if multiple_exact {
        "Select the snapshot you intend, or update the other."
    } else {
        "Narrow the query to one of these selectors, or measure the field at the query's own scope."
    };
    let labels = namespaces
        .iter()
        .map(|&(repo, kind, event, sourcetype)| {
            let label = [
                ("#repo", repo),
                ("#type", kind),
                ("#event_simpleName", event),
                ("@sourcetype", sourcetype),
            ]
            .into_iter()
            .filter_map(|(field, value)| value.map(|value| format!("{field}={}", quote(value))))
            .collect::<Vec<_>>()
            .join(" ");
            if label.is_empty() {
                "unscoped".into()
            } else {
                label
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(format!("{AMBIGUOUS_FIELD} ({labels}). {next_action}"))
}

fn event_family_mismatch(selectors: &environment::Selectors, r: &Requirement) -> bool {
    selectors
        .event_family
        .as_ref()
        .zip(r.scope.event.as_ref())
        .is_some_and(|(family, event)| family != event)
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

struct DecodedPack<'a> {
    pack: &'a Pack,
    sources: Vec<environment::LegacySource>,
    fields: Vec<environment::LegacyField>,
}

impl<'a> DecodedPack<'a> {
    fn new(pack: &'a Pack) -> Self {
        Self {
            pack,
            sources: pack
                .data
                .sources
                .iter()
                .filter_map(|value| environment::source(value).ok())
                .collect(),
            fields: pack
                .data
                .fields
                .iter()
                .filter_map(|value| environment::field(value).ok())
                .collect(),
        }
    }
}

fn query_observation_basis<'a>(
    r: &Requirement,
    packs: &[&'a DecodedPack<'_>],
    evidence: GroundingEvidence<'_>,
    strict: bool,
    winning_namespace: &mut Option<FieldNamespace<'a>>,
) -> Result<Option<String>, String> {
    let mut namespaces = BTreeSet::new();
    let mut contradictions = Vec::new();
    let mut active_contradiction = false;
    for decoded in packs {
        let pack = decoded.pack;
        let sources = decoded
            .sources
            .iter()
            .filter(|source| source_overlaps_scope(source, &r.scope))
            .collect::<Vec<_>>();
        let fields = decoded
            .fields
            .iter()
            .filter(|field| {
                field_matches_requirement(field, r)
                    && sources.iter().any(|source| source.id == field.source_id)
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
                && (!strict || !event_family_mismatch(selectors, r))
            {
                namespaces.insert(field_namespace(selectors));
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
    if strict {
        *winning_namespace = resolve_field_namespace(&namespaces, &r.scope)?;
    } else if namespaces.len() > 1 {
        return Err(AMBIGUOUS_FIELD.into());
    }
    let source_id = crate::discovery::source_id(&r.scope);
    let mut matching = false;
    for observation in evidence
        .query_observations
        .iter()
        .chain(evidence.captured_observations)
        .filter(|observation| {
            observation.source_id == source_id
                && r.target
                    .as_ref()
                    .is_some_and(|target| target == &observation.environment.view)
                && !r.array
        })
    {
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
                "{}; id={}; positive_rows={}; retrieved_at={}; observation_window={}/{}",
                if observation.caller_assertion
                    == crate::inventory::QueryResultAssertion::ClientHookCapture
                {
                    "captured search observation"
                } else {
                    "caller-attested query observation"
                },
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
    fn target_notices_replay_retained_capture_once_and_reject_corruption() {
        use crate::capability::{CapabilityEvidence, CapabilityRun, CaseId, QUALIFICATION_CALLS};
        use serde_json::json;
        let environment = EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "synthetic".into(),
        };
        let start = "2026-01-01T00:00:00Z";
        let settled = "2026-01-01T00:00:01Z";
        let host = serde_json::to_vec(&json!({"schema_version":1,"environment":environment,
            "session_id":"synthetic-session","verified_at":start,
            "mechanism":"provider-enforced-search-lifetime","max_search_seconds":1,
            "capability_reference":"Synthetic reference","verification_evidence":"Synthetic fixture"})).unwrap();
        let mut run = CapabilityRun::prepare(
            environment.clone(),
            "synthetic-auth".into(),
            "synthetic-session".into(),
            vec![CaseId::ArrayUnion],
            &host,
            1,
            10,
            start,
            0,
        )
        .unwrap();
        let reservation = run
            .dispatch(
                CaseId::ArrayUnion,
                "synthetic-auth",
                "synthetic-session",
                start,
            )
            .unwrap()
            .reservation
            .clone();
        let raw = br#"{"result":[{"out[0]":1,"out[1]":2,"out[2]":3}]}"#;
        let report = serde_json::to_vec(&json!({"schema_version":1,"environment":environment,
            "session_id":"synthetic-session","attempt_id":reservation.id,"arguments":reservation.arguments,
            "termination":"completed","recorded_at":settled,"result_sha256":crate::sha256_hex(raw),
            "evidence":"Synthetic terminal report"})).unwrap();
        run.submit(
            &reservation.id,
            "synthetic-auth",
            Some(raw),
            &report,
            settled,
            settled,
        )
        .unwrap();
        let pipeline = language::parser::parse(
            "array:union(items,as=out)|array:union(out,as=again)|array:union(again,as=last)",
        )
        .unwrap();
        for valid in [true, false] {
            if !valid {
                run.attempts[0].capture.as_mut().unwrap().sha256 = "0".repeat(64);
            }
            QUALIFICATION_CALLS.with(|calls| calls.set(0));
            let diagnostics = target_requirements(
                &pipeline,
                Some(&environment),
                Some(CapabilityEvidence {
                    runs: std::slice::from_ref(&run),
                    environment: &environment,
                    execution_context_id: Some("synthetic-auth"),
                    now: settled,
                }),
            );
            assert_eq!(QUALIFICATION_CALLS.with(|calls| calls.get()), 1);
            assert_eq!(
                diagnostics.iter().any(|d| d.code == "CQL_TARGET_OBSERVED"),
                valid
            );
            assert_eq!(
                diagnostics
                    .iter()
                    .any(|d| d.code == "CQL_TARGET_REQUIREMENT"),
                !valid
            );
        }
    }

    #[test]
    fn target_notices_qualify_each_eligible_case_once_including_absence_and_errors() {
        use crate::capability::{CapabilityEvidence, QUALIFICATION_CALLS};
        let environment = EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: "synthetic".into(),
        };
        let calls = "array:union(items,as=out) | array:intersection(items,as=out) | array:reduceRow(items,var=x,function=sum(x),as=out) | array:reduceColumn(items,var=x,function=sum(x),as=out)";
        let pipeline = language::parser::parse(&format!("{calls} | {calls}")).unwrap();
        for now in ["2026-01-01T00:00:00Z", "invalid-time"] {
            QUALIFICATION_CALLS.with(|calls| calls.set(0));
            let diagnostics = target_requirements(
                &pipeline,
                Some(&environment),
                Some(CapabilityEvidence {
                    runs: &[],
                    environment: &environment,
                    execution_context_id: Some("synthetic-auth"),
                    now,
                }),
            );
            assert_eq!(QUALIFICATION_CALLS.with(|calls| calls.get()), 4);
            assert!(
                diagnostics
                    .iter()
                    .any(|d| d.code == "CQL_TARGET_REQUIREMENT")
            );
            assert!(diagnostics.iter().all(|d| d.code != "CQL_TARGET_OBSERVED"));
        }
        QUALIFICATION_CALLS.with(|calls| calls.set(0));
        let pipeline = language::parser::parse(
            "math:sqrt(value,as=root) | array:union(items) | join(query={array:union(items,as=out)},field=host,view=other)",
        ).unwrap();
        target_requirements(
            &pipeline,
            Some(&environment),
            Some(CapabilityEvidence {
                runs: &[],
                environment: &environment,
                execution_context_id: Some("synthetic-auth"),
                now: "2026-01-01T00:00:00Z",
            }),
        );
        assert_eq!(QUALIFICATION_CALLS.with(|calls| calls.get()), 0);
    }

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
    fn inventory_hints_keep_source_only_and_reject_uncertain_selectors() {
        let hints = inventory_hints("#repo=unseen | count()").unwrap();
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].source.repo.as_deref(), Some("unseen"));
        assert!(hints[0].fields.is_empty());
        assert!(inventory_hints("#repo=* | count()").unwrap().is_empty());
        assert!(
            inventory_hints("(#repo=a OR #repo=b) | count()")
                .unwrap()
                .is_empty()
        );
        let hints = inventory_hints("#repo=unseen | user=* | count()").unwrap();
        assert_eq!(hints[0].fields, ["user"]);
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
