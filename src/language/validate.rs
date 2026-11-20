use super::ast::*;
use crate::{
    catalog::{self, Effect},
    contracts::*,
};
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
};

#[derive(Debug)]
pub struct Report {
    pub status: Acceptance,
    pub diagnostics: Vec<Diagnostic>,
    pub fields: BTreeSet<String>,
    pub dropped: BTreeSet<String>,
}

pub fn validate(pipeline: &Pipeline, query: &str) -> Report {
    let mut checker = Checker {
        query,
        diagnostics: super::parser::lexical_diagnostics(query),
        fields: BTreeSet::new(),
        reads: BTreeSet::new(),
        dropped: BTreeSet::new(),
        projected_input: None,
        nodes: 0,
        bindings: vec![],
        nesting: 0,
        definitions: 0,
        joins: false,
        correlation_seen: false,
        table_columns: BTreeMap::new(),
        correlation_columns: BTreeMap::new(),
        narrowed: false,
        aggregate_input: false,
        after_aggregate: false,
        ordered_input: false,
        aggregate_parents: vec![],
        enclosed_queries: 0,
        stored_input: true,
        time_interval_seen: false,
    };
    checker.pipeline(pipeline, QueryContext::Plain, &mut BTreeSet::new());
    if leading_fixed_time(pipeline) {
        checker.info("CQL_ABSOLUTE_TIME_FILTER", "A top-level filter compares @timestamp with a fixed time, which ties the query to one window. Set the time window with the search request's start and end instead.", None);
    }
    if checker.definitions > 0 && checker.joins {
        checker.emit(
            "CQL_TABLE_JOIN_CONFLICT",
            "defineTable cannot be combined with join, selfJoin or selfJoinFilter.",
            None,
            false,
        );
    }
    if checker.joins && checker.correlation_seen {
        checker.emit(
            "CQL_CORRELATION_JOIN_CONFLICT",
            "correlate cannot be combined with join, selfJoin or selfJoinFilter.",
            None,
            false,
        );
    }
    let status = if checker
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error && !d.is_unsupported())
    {
        Acceptance::Rejected
    } else if checker
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        Acceptance::Unknown
    } else {
        Acceptance::Accepted
    };
    Report {
        status,
        diagnostics: checker.diagnostics,
        fields: checker.fields,
        dropped: checker.dropped,
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryContext {
    Plain,
    Conditional,
    Continuing,
    Mandatory,
    Aggregation,
    Subaggregate,
}

/// What the stages of one query have done so far, for notes about stage order and cost.
#[derive(Default)]
struct StageOrder {
    /// An earlier stage is not a per-event step with listed outputs, so no filter can be
    /// said to belong before it.
    fixed_order: bool,
    /// A stage other than a tag filter or a declaration has run.
    past_tag_filters: bool,
    transformed: bool,
    /// An earlier stage may have removed events.
    filtered: bool,
    /// Leading names of the fields earlier stages wrote, renamed or were given.
    touched: BTreeSet<String>,
    tag_noted: bool,
    filter_noted: bool,
}

impl StageOrder {
    fn notes(&mut self, step: &Step, narrowed: bool) -> Vec<(&'static str, &'static str)> {
        let mut notes = vec![];
        match step {
            Step::Filter(filter) => {
                let mut fields = vec![];
                let comparisons = compared_fields(filter, &mut fields);
                let tags_only = comparisons && fields.iter().all(|field| field.starts_with('#'));
                // Only a whole filter that reads nothing an earlier stage touched, and writes
                // nothing itself, gives the same result when it runs earlier.
                let movable = comparisons
                    && !self.fixed_order
                    && !has_capture_marker(filter)
                    && !fields.iter().any(|field| {
                        // A failing function reports through the @error fields.
                        field.starts_with("@error") || self.touched.contains(leading_name(field))
                    });
                if movable && tags_only && self.past_tag_filters && !self.tag_noted {
                    self.tag_noted = true;
                    notes.push((
                        "CQL_TAG_FILTER_ORDER",
                        "Tag filters select the data that is searched. Put filters on #-fields before other filters and transformations.",
                    ));
                }
                if movable
                    && self.transformed
                    && !self.filter_noted
                    && !fields.iter().any(|field| field.starts_with('#'))
                {
                    self.filter_noted = true;
                    notes.push((
                        "CQL_FILTER_AFTER_TRANSFORM",
                        "This filter reads only fields that the preceding transformations did not write. Filter first, so that the transformations run on fewer events.",
                    ));
                }
                // Once a stage has written a field, a named field may no longer hold the
                // event's own value, which is what a regex with no field searches.
                if narrowed
                    && !self.transformed
                    && !self.fixed_order
                    && self.touched.is_empty()
                    && unfielded_regex(filter)
                {
                    notes.push((
                        "CQL_REGEX_ALL_FIELDS",
                        "A regex with no field searches the fields of each event, which is slower than a regex on one field. If the text is in a known field, field=/regex/ is faster.",
                    ));
                }
                let mut calls = vec![];
                filter_calls(filter, &mut calls);
                if calls.iter().any(|call| case_insensitive_wildcard(call)) {
                    notes.push(WILDCARD_IGNORE_CASE);
                }
                self.past_tag_filters |= !tags_only;
                self.filtered = true;
                self.fixed_order |= !calls.is_empty();
                for pattern in positive_regexes(filter) {
                    for capture in regex_captures(pattern) {
                        self.touched.insert(leading_name(&capture).into());
                    }
                }
            }
            Step::Assign { field, value } => {
                self.past_tag_filters = true;
                self.transformed = true;
                self.touched.insert(leading_name(field).into());
                if let Value::Call(call) = value {
                    self.touch_call(call);
                }
            }
            Step::Call(call) => {
                let resolved = catalog::resolve(call);
                let effect = resolved
                    .as_ref()
                    .map(|resolved| resolved.function.resolved_effect(&resolved.arguments));
                if effect == Some(Effect::Definition) {
                    return notes;
                }
                let name = resolved.as_ref().map(|resolved| resolved.function.name);
                if !self.filtered && name == Some("sort") {
                    notes.push((
                        "CQL_SORT_COST",
                        "sort works in memory and no earlier stage filters the events it sorts. Narrow the events before sorting where the query allows, or sort after an aggregate.",
                    ));
                }
                if !self.filtered && name == Some("split") {
                    notes.push((
                        "CQL_SPLIT_COST",
                        "split copies the other fields of an event for every array element and no earlier stage filters the events. Filter on other fields before the split where the query allows.",
                    ));
                }
                if case_insensitive_wildcard(call) {
                    notes.push(WILDCARD_IGNORE_CASE);
                }
                self.past_tag_filters = true;
                // A function may drop the events it cannot process.
                self.filtered = true;
                if effect == Some(Effect::Transform) {
                    self.transformed = true;
                    self.touch_call(call);
                } else {
                    self.fixed_order = true;
                }
            }
            _ => {
                self.past_tag_filters = true;
                self.filtered = true;
                self.fixed_order = true;
            }
        }
        notes
    }

    /// Records what a call writes and every name it is given, since a transformation may
    /// rename, remove or overwrite its inputs.
    fn touch_call(&mut self, call: &Call) {
        match catalog::output_writes(call) {
            // lowercase can rename fields by locale rules that are not modelled here.
            Some(writes)
                if !call.name.eq_ignore_ascii_case("lowercase")
                    && writes.dynamic_prefix.is_none()
                    && !(writes.fields.is_empty() && writes.required.is_empty()) =>
            {
                for name in writes.fields.iter().chain(&writes.required) {
                    self.touched.insert(leading_name(name).into());
                }
            }
            _ => self.fixed_order = true,
        }
        for argument in &call.args {
            named_values(&argument.value, &mut self.touched);
        }
    }
}

const WILDCARD_IGNORE_CASE: (&str, &str) = (
    "CQL_WILDCARD_IGNORE_CASE",
    "A case-insensitive wildcard search can take up to twice as long. Set ignoreCase=true only when the case varies.",
);

#[derive(Clone, Default)]
struct PipelineState {
    has_input: bool,
    aggregated: bool,
    after_aggregate: bool,
    synthetic: bool,
    replaced: bool,
    extracted: bool,
    ordered: bool,
    narrowed: bool,
    projected: Option<BTreeSet<String>>,
}
impl PipelineState {
    fn merge(branches: &[Self]) -> Self {
        let Some(first) = branches.first() else {
            return Self::default();
        };
        let mut merged = first.clone();
        merged.has_input = branches.iter().any(|b| b.has_input);
        merged.aggregated = branches.iter().any(|b| b.aggregated);
        merged.after_aggregate = branches.iter().any(|b| b.after_aggregate);
        merged.synthetic = branches.iter().any(|b| b.synthetic);
        merged.replaced = branches.iter().any(|b| b.replaced);
        merged.extracted = branches.iter().all(|b| b.extracted);
        merged.ordered = branches.iter().all(|b| b.ordered);
        merged.narrowed = branches.iter().all(|b| b.narrowed);
        for branch in branches {
            if let (Some(left), Some(right)) = (&mut merged.projected, &branch.projected) {
                left.extend(right.iter().cloned());
            } else {
                merged.projected = None;
            }
        }
        merged
    }
}
struct Checker<'a> {
    query: &'a str,
    diagnostics: Vec<Diagnostic>,
    fields: BTreeSet<String>,
    reads: BTreeSet<String>,
    dropped: BTreeSet<String>,
    projected_input: Option<BTreeSet<String>>,
    nodes: usize,
    bindings: Vec<(String, bool)>,
    nesting: usize,
    definitions: usize,
    joins: bool,
    correlation_seen: bool,
    table_columns: BTreeMap<String, Option<BTreeSet<String>>>,
    correlation_columns: BTreeMap<usize, Option<BTreeSet<String>>>,
    narrowed: bool,
    aggregate_input: bool,
    after_aggregate: bool,
    ordered_input: bool,
    aggregate_parents: Vec<&'static str>,
    enclosed_queries: usize,
    stored_input: bool,
    time_interval_seen: bool,
}
impl Checker<'_> {
    fn emit(&mut self, code: &str, message: impl Into<String>, call: Option<&Call>, warning: bool) {
        let mut d = if warning {
            Diagnostic::warning(code, message)
        } else {
            Diagnostic::error(code, message)
        };
        if let Some(c) = call {
            d.span = Some(Box::new(span(self.query, c.start, c.end)));
            d.reference = catalog::function(&c.name).map(|f| f.reference_id.into());
        }
        self.diagnostics.push(d);
    }
    fn info(&mut self, code: &str, message: impl Into<String>, call: Option<&Call>) {
        self.emit(code, message, call, true);
        self.diagnostics.last_mut().expect("just emitted").severity = Severity::Info;
    }
    fn raw_scan_warning(&mut self, call: Option<&Call>) {
        self.emit(
            "CQL_RAW_SCAN_COST",
            "Regex or wildcard scans raw event text without a guaranteed preceding source/tag or field filter. Narrow earlier when possible and review execution cost; the query is unchanged.",
            call,
            true,
        );
    }
    fn free_text(&mut self, call: Option<&Call>) {
        if self.after_aggregate {
            self.emit(
                "CQL_FREE_TEXT_AFTER_AGGREGATE",
                "Free-text filters must precede the first aggregate; use a field comparison after aggregation.",
                call,
                false,
            );
        }
    }
    fn unsupported(
        &mut self,
        code: &str,
        message: impl Into<String>,
        call: Option<&Call>,
        warning: bool,
    ) {
        self.emit(code, message, call, warning);
        self.diagnostics.last_mut().expect("just emitted").class = DiagnosticClass::Coverage;
    }
    fn field(&mut self, f: &str) {
        self.fields.insert(f.into());
        if self.local_binding(f).is_none() {
            self.reads.insert(f.into());
        }
    }
    fn local_binding(&self, field: &str) -> Option<&(String, bool)> {
        let contains = |binding: &str, structured: bool, field: &str| {
            field == binding
                || structured
                    && field
                        .strip_prefix(binding)
                        .is_some_and(|tail| tail.starts_with('.') || tail.starts_with('['))
        };
        self.bindings
            .iter()
            .enumerate()
            .filter(|(index, (binding, structured))| {
                contains(binding, *structured, field)
                    && !self.bindings[index + 1..]
                        .iter()
                        .any(|(later, structured)| contains(later, *structured, binding))
            })
            .map(|(_, binding)| binding)
            .max_by_key(|(binding, _)| binding.len())
    }
    fn projected_reads(
        &mut self,
        previous: BTreeSet<String>,
        available: Option<&BTreeSet<String>>,
        call: Option<&Call>,
    ) {
        let fields = std::mem::replace(&mut self.fields, previous);
        self.fields.extend(fields);
        let reads = std::mem::take(&mut self.reads);
        if let Some(available) = available {
            for field in reads.iter().filter(|field| !available.contains(*field)) {
                self.dropped.insert(field.clone());
                self.emit(
                    "CQL_DROPPED_FIELD",
                    format!("'{field}' is not among the known projected/aggregate outputs."),
                    call,
                    true,
                );
            }
        }
    }
    fn pipeline(&mut self, p: &Pipeline, context: QueryContext, tables: &mut BTreeSet<String>) {
        self.pipeline_state(
            p,
            context,
            tables,
            &mut PipelineState {
                narrowed: context == QueryContext::Continuing && self.narrowed,
                after_aggregate: context == QueryContext::Continuing && self.after_aggregate,
                projected: if context == QueryContext::Continuing {
                    self.projected_input.clone()
                } else {
                    None
                },
                ..PipelineState::default()
            },
        );
    }
    fn branches(
        &mut self,
        branches: Vec<&Pipeline>,
        context: QueryContext,
        tables: &BTreeSet<String>,
        incoming: PipelineState,
    ) -> PipelineState {
        let context = if context == QueryContext::Plain {
            QueryContext::Conditional
        } else {
            context
        };
        let mut outcomes = Vec::new();
        for pipeline in branches {
            for original_step in &pipeline.steps {
                let semantic_step = original_step.semantic();
                let step = semantic_step.as_ref();
                match step {
                    Step::Call(call)
                    | Step::Assign {
                        value: Value::Call(call),
                        ..
                    } => self.branch_function(call),
                    Step::Filter(filter) => self.branch_filter(filter),
                    _ => {}
                }
            }
            let mut branch = incoming.clone();
            self.pipeline_state(pipeline, context, &mut tables.clone(), &mut branch);
            outcomes.push(branch);
        }
        let mut merged = PipelineState::merge(&outcomes);
        merged.ordered &= incoming.ordered;
        merged
    }
    fn branch_function(&mut self, call: &Call) {
        if catalog::function(&call.name).is_some_and(|f| first_aggregate(f.name, f.effect)) {
            self.emit("CQL_BRANCH_FUNCTION", format!("{}() cannot be used inside a case or match branch; aggregate before or after the conditional.", call.name), Some(call), false);
        } else if matches!(call.name.as_str(), "join" | "selfJoin" | "selfJoinFilter") {
            self.emit("CQL_BRANCH_FUNCTION", format!("{}() cannot be used inside a case or match branch; join before or after the conditional.", call.name), Some(call), false);
        }
    }
    fn branch_filter(&mut self, filter: &Filter) {
        match filter {
            Filter::Call(call) => self.branch_function(call),
            Filter::Not(filter) => self.branch_filter(filter),
            Filter::And(filters) | Filter::Or(filters) => {
                for filter in filters {
                    self.branch_filter(filter);
                }
            }
            _ => {}
        }
    }
    fn pipeline_state(
        &mut self,
        p: &Pipeline,
        context: QueryContext,
        tables: &mut BTreeSet<String>,
        state: &mut PipelineState,
    ) {
        self.nesting += 1;
        let enclosing_narrowed = self.narrowed;
        let enclosing_input = (self.aggregate_input, self.ordered_input, self.stored_input);
        let enclosing_after_aggregate = self.after_aggregate;
        let enclosing_reads = std::mem::take(&mut self.reads);
        let enclosing_projection = self.projected_input.take();
        let PipelineState {
            mut has_input,
            mut aggregated,
            mut after_aggregate,
            mut synthetic,
            mut replaced,
            mut extracted,
            mut ordered,
            mut narrowed,
            mut projected,
        } = state.clone();
        let mut mandatory_link = false;
        // Order notes apply to a query's own stages, not to function arguments or branches.
        let mut order = (context == QueryContext::Plain
            && self.enclosed_queries == 0
            && self.aggregate_parents.is_empty())
        .then(StageOrder::default);
        for (index, original_step) in p.steps.iter().enumerate() {
            for (_, advice) in p.advisories.iter().filter(|(stage, _)| *stage == index) {
                if after_aggregate
                    && matches!(
                        advice.code,
                        "CQL_BARE_FUNCTION_NAME" | "CQL_BARE_COMMAND_WORD"
                    )
                {
                    continue;
                }
                if advice.informational {
                    self.info(advice.code, &advice.message, None);
                } else {
                    self.emit(advice.code, &advice.message, None, true);
                }
            }
            let transformed;
            let step = if let Step::Filter(filter) = original_step
                && let Some(call) = filter.negated_transform()
            {
                self.emit(
                    "CQL_NEGATION_NO_EFFECT",
                    "Negation has no effect on this transform; the transform still runs.",
                    Some(call),
                    true,
                );
                transformed = Step::Call(call.clone());
                &transformed
            } else {
                original_step
            };
            self.reads.clear();
            self.projected_input = projected.clone();
            self.narrowed = narrowed;
            self.aggregate_input = aggregated;
            self.after_aggregate = after_aggregate;
            self.ordered_input = ordered;
            self.stored_input = !synthetic && !replaced;
            if context == QueryContext::Mandatory
                && mandatory_link
                && !matches!(step, Step::Link { .. })
            {
                self.emit("CQL_LINK_ORDER", "Only links may follow the first mandatory-query link; keep query logic before its links.", None, false);
            }
            mandatory_link |= matches!(step, Step::Link { .. });
            self.nodes += 1;
            if self.nodes > MAX_NODES {
                self.unsupported(
                    "CQL_UNSUPPORTED_BUDGET",
                    "AST node budget exceeded.",
                    None,
                    false,
                );
                break;
            }
            if matches!(
                context,
                QueryContext::Mandatory | QueryContext::Aggregation | QueryContext::Subaggregate
            ) {
                let call = match step {
                    Step::Call(call)
                    | Step::Assign {
                        value: Value::Call(call),
                        ..
                    } => Some(call),
                    _ => None,
                };
                if let Some(call) = call
                    && let Some(resolved) = catalog::resolve(call)
                    && (matches!(
                        resolved.function.resolved_effect(&resolved.arguments),
                        Effect::Source | Effect::Definition | Effect::Correlation
                    ) || context == QueryContext::Mandatory
                        && resolved.function.resolved_effect(&resolved.arguments)
                            == Effect::Aggregate)
                {
                    self.emit(
                        "CQL_CORRELATION_BRANCH",
                        "Mandatory correlation queries cannot aggregate or replace the stream; aggregation queries cannot replace or nest correlation.",
                        Some(call),
                        false,
                    );
                }
            }
            if aggregated || replaced || synthetic {
                order = None;
            }
            if let Some(order) = &mut order {
                for (code, message) in order.notes(step, narrowed) {
                    let call = if let Step::Call(call) = step {
                        Some(call)
                    } else {
                        None
                    };
                    self.info(code, message, call);
                }
            }
            match step {
                Step::Filter(f) => {
                    let previous = std::mem::take(&mut self.fields);
                    self.filter(f, tables, false);
                    narrowed |= narrowing_filter(f);
                    if let Some(available) = &mut projected {
                        for pattern in positive_regexes(f) {
                            available.extend(regex_captures(pattern));
                        }
                    }
                    if synthetic && !extracted && has_non_raw_comparison(f) {
                        self.emit("CQL_SYNTHETIC_FIELD_BEFORE_PARSE","createEvents emits raw strings. Parse before filtering extracted fields.",None,true);
                    }
                    self.projected_reads(previous, projected.as_ref(), None);
                }
                Step::Assign { field, value } => {
                    self.fields.insert(field.clone());
                    self.output_field(field, None);
                    let previous = std::mem::take(&mut self.fields);
                    if let Value::Call(call) = value {
                        let resolved = catalog::resolve(call);
                        // eval() takes no named parameters; its assigned form stays unclassified.
                        if resolved.as_ref().is_some_and(|resolved| {
                            resolved.function.name != "eval"
                                && resolved.function.argument_kind("as").is_none()
                        }) {
                            self.emit(
                                "CQL_ASSIGN_ALIAS",
                                "Direct assignment requires a function with an as parameter.",
                                Some(call),
                                false,
                            );
                        } else if call
                            .args
                            .iter()
                            .any(|arg| arg.name.as_deref() == Some("as"))
                            || resolved
                                .is_some_and(|resolved| resolved.arguments.contains_key("as"))
                        {
                            self.emit("CQL_ASSIGN_ALIAS", "Direct assignment and an explicit as argument cannot be combined; choose one output name.", Some(call), false);
                        }
                    }
                    if let Some(call) = catalog::aliased_call(field, value) {
                        let args = self.function(&call, tables, false, false, false);
                        self.projected_reads(previous, projected.as_ref(), Some(&call));
                        if let Some(fields) = &mut projected {
                            if let Some(writes) = catalog::output_writes(&call) {
                                fields.extend(writes.fields);
                            }
                            if call.name.eq_ignore_ascii_case("rename") {
                                for (old, new) in catalog::rename_bindings(&args) {
                                    fields.remove(old);
                                    fields.insert(new.into());
                                }
                            }
                        }
                    } else if let Some(call) = catalog::assigned_aggregate(value) {
                        self.function(call, tables, false, false, false);
                        self.projected_reads(previous, projected.as_ref(), Some(call));
                        projected = Some(BTreeSet::from([field.clone()]));
                        aggregated = true;
                        ordered = false;
                        has_input = true;
                    } else {
                        self.scalar_value(value, tables);
                        self.projected_reads(
                            previous,
                            projected.as_ref(),
                            if let Value::Call(call) = value {
                                Some(call)
                            } else {
                                None
                            },
                        );
                    }
                    if catalog::aliased_call(field, value).is_none()
                        && let Some(xs) = &mut projected
                    {
                        xs.insert(field.clone());
                    }
                }
                Step::Case(branches) => {
                    let merged = self.branches(
                        branches.iter().collect(),
                        context,
                        tables,
                        PipelineState {
                            has_input,
                            aggregated,
                            after_aggregate,
                            synthetic,
                            replaced,
                            extracted,
                            ordered,
                            narrowed,
                            projected: projected.clone(),
                        },
                    );
                    aggregated = merged.aggregated;
                    after_aggregate = merged.after_aggregate;
                    synthetic = merged.synthetic;
                    replaced = merged.replaced;
                    extracted = merged.extracted;
                    ordered = merged.ordered;
                    narrowed = merged.narrowed;
                    projected = merged.projected;
                }
                Step::Match { field, arms } => {
                    let previous = std::mem::take(&mut self.fields);
                    self.field(field);
                    if !narrowed
                        && field == "@rawstring"
                        && arms.iter().any(|(pattern, _)| costly_raw_pattern(pattern))
                    {
                        self.raw_scan_warning(None);
                    }
                    for (pattern, _) in arms {
                        self.pattern(pattern);
                    }
                    self.projected_reads(previous, projected.as_ref(), None);
                    let merged = self.branches(
                        arms.iter().map(|(_, p)| p).collect(),
                        context,
                        tables,
                        PipelineState {
                            has_input,
                            aggregated,
                            after_aggregate,
                            synthetic,
                            replaced,
                            extracted,
                            ordered,
                            narrowed,
                            projected: projected.clone(),
                        },
                    );
                    aggregated = merged.aggregated;
                    after_aggregate = merged.after_aggregate;
                    synthetic = merged.synthetic;
                    replaced = merged.replaced;
                    extracted = merged.extracted;
                    ordered = merged.ordered;
                    narrowed = merged.narrowed;
                    projected = merged.projected;
                }
                Step::Link { field, target } => {
                    self.field(field);
                    if context == QueryContext::Aggregation && aggregated {
                        self.emit(
                            "CQL_LINK_AFTER_AGGREGATE",
                            "Aggregation links must precede aggregate functions.",
                            None,
                            false,
                        );
                    }
                    if !matches!(context, QueryContext::Mandatory | QueryContext::Aggregation) {
                        self.emit(
                            "CQL_LINK_PLACEMENT",
                            "Link operators belong inside named correlation branches.",
                            None,
                            false,
                        );
                    }
                    if !target.contains('.') {
                        self.emit(
                            "CQL_LINK_TARGET",
                            "Link target must include a query label and field.",
                            None,
                            false,
                        );
                    }
                }
                Step::Call(c) => {
                    let Some(spec) = catalog::function(&c.name) else {
                        self.unsupported(
                            "CQL_UNSUPPORTED_FUNCTION",
                            format!("Function '{}' is outside profile {PROFILE}.", c.name),
                            Some(c),
                            false,
                        );
                        continue;
                    };
                    if spec.effect == Effect::Definition
                        && spec.name != "setTimeInterval"
                        && (self.nesting != 1 || has_input)
                    {
                        self.emit("CQL_TABLE_PLACEMENT", "defineTable declarations must form a top-level preamble before other operations.", Some(c), false);
                    }
                    let previous = std::mem::take(&mut self.fields);
                    let preamble = self.nesting == 1 && !has_input && self.definitions == 0;
                    if spec.name == "head" && index > 0 && head_narrows_sort(&p.steps[index - 1], c)
                    {
                        self.emit(
                            "CQL_SORT_HEAD",
                            "head after sort can select a different set of rows from the first N in the sorted order. Use sort(..., limit=N) to select the first N rows in that order.",
                            Some(c),
                            true,
                        );
                    }
                    if spec.name == "tail"
                        && projected
                            .as_ref()
                            .is_some_and(|fields| !fields.contains("@timestamp"))
                    {
                        self.emit("CQL_TAIL_WITHOUT_TIMESTAMP", "tail requires @timestamp; the preceding steps removed it, so tail returns no events.", Some(c), true);
                    }
                    let args = self.function(c, tables, false, false, preamble);
                    self.projected_reads(previous, projected.as_ref(), Some(c));
                    let effect = spec.resolved_effect(&args);
                    if spec.name == "correlate" {
                        let resolved = catalog::resolve(c).expect("known function");
                        self.correlation(
                            c,
                            tables,
                            &args,
                            &resolved.queries,
                            &PipelineState {
                                has_input,
                                aggregated,
                                after_aggregate,
                                synthetic,
                                replaced,
                                extracted,
                                ordered,
                                narrowed,
                                projected: projected.clone(),
                            },
                        );
                        if context != QueryContext::Plain {
                            self.emit(
                                "CQL_CORRELATION_PLACEMENT",
                                "Correlation must be at the top level of an independent query.",
                                Some(c),
                                false,
                            );
                        }
                    }
                    if effect == Effect::Source {
                        if has_input {
                            self.emit("CQL_SOURCE_DISCARDS_INPUT","This source replaces the event stream; preceding filters do not constrain its rows.",Some(c),true);
                        }
                        synthetic = c.name.eq_ignore_ascii_case("createEvents");
                        replaced = true;
                        extracted = false;
                        aggregated = false;
                        ordered = false;
                        narrowed = false;
                        projected = None;
                    } else {
                        narrowed |= narrowing_call(c);
                    }
                    if effect == Effect::Correlation && (aggregated || replaced) {
                        self.emit(
                            "CQL_CORRELATION_PLACEMENT",
                            "Correlation cannot follow an aggregate or source replacement.",
                            Some(c),
                            false,
                        );
                    }
                    let incoming_projection = projected.clone();
                    let name = c.name.to_ascii_lowercase();
                    match name.as_str() {
                        "select" => projected = Some(arg_fields(args.get("fields").copied())),
                        "drop" => {
                            if let Some(xs) = &mut projected {
                                for f in arg_fields(args.get("fields").copied()) {
                                    xs.remove(&f);
                                }
                            }
                        }
                        "rename" => {
                            if let Some(available) = &mut projected {
                                for (old, new) in catalog::rename_bindings(&args) {
                                    available.remove(old);
                                    available.insert(new.into());
                                }
                            }
                        }
                        "join" => {
                            self.emit("CQL_JOIN_COST","join scans both sides and caps subquery rows and matches. Verify the requested cardinality and completion.",Some(c),true);
                            if aggregated {
                                self.emit(
                                    "CQL_JOIN_AFTER_AGGREGATE",
                                    "Check that join keys survived the preceding aggregation.",
                                    Some(c),
                                    true,
                                );
                            }
                            if let Some(field) = joined_comparison(&args, &p.steps[index + 1..]) {
                                self.emit("CQL_JOIN_MAX", format!("join keeps one matching subquery row per key unless max is set, so the later comparison of {field} may test only one of several matches."), Some(c), true);
                            }
                            projected = None;
                        }
                        "parsejson" | "kvparse" => {
                            extracted = true;
                            projected = None;
                        }
                        "neighbor" => {
                            if !aggregated {
                                self.emit(
                                    "CQL_NEIGHBOR_ORDER",
                                    "neighbor requires a preceding aggregate; supply an explicit sort/head order for reproducible comparisons.",
                                    Some(c),
                                    false,
                                );
                            }
                            if !ordered {
                                self.emit("CQL_NEIGHBOR_UNORDERED", "The preceding aggregate has no qualified row order. Sort explicitly when the neighbor relationship depends on order.", Some(c), true);
                            }
                        }
                        "definetable" | "remotetable" => {
                            if let Some(n) = args
                                .get("name")
                                .filter(|value| !has_parameter(value))
                                .and_then(|v| v.text())
                                && !tables.insert(n.into())
                            {
                                self.emit(
                                    "CQL_DUPLICATE_TABLE",
                                    "Table names must be unique.",
                                    Some(c),
                                    false,
                                );
                            }
                        }
                        "match" => {
                            let columns = args
                                .get("file")
                                .and_then(|v| v.text())
                                .and_then(|t| self.table_columns.get(t))
                                .and_then(|c| c.as_ref());
                            let includes = catalog::imported_fields(spec.name, &args, columns);
                            if let (Some(available), Some(includes)) = (&mut projected, includes) {
                                available.extend(includes);
                            } else {
                                projected = None;
                            }
                        }
                        "asn" | "iplocation" | "ioc:lookup" | "findtimestamp" => {}
                        "correlate" => {
                            projected = self.correlation_columns.get(&c.start).cloned().flatten()
                        }
                        "splitstring" if !args.contains_key("index") => projected = None,
                        "eval" => {
                            if let Some(available) = &mut projected {
                                available.extend(args.keys().cloned());
                            }
                        }
                        "default" => {
                            if let (Some(available), Some(fields)) =
                                (&mut projected, args.get("field"))
                            {
                                available.extend(catalog::fields(fields));
                            }
                        }
                        "readfile" => {
                            projected = args
                                .get("include")
                                .map(|v| catalog::fields(v))
                                .filter(|fields| !fields.contains("*"));
                            if !args.contains_key("include") {
                                let mut columns = BTreeSet::new();
                                let names = catalog::resolve(c)
                                    .expect("known function")
                                    .resource_names();
                                let known = !names.is_empty()
                                    && names.iter().all(|name| {
                                        if let Some(Some(fields)) = self.table_columns.get(name) {
                                            columns.extend(fields.iter().cloned());
                                            true
                                        } else {
                                            false
                                        }
                                    });
                                projected = known.then_some(columns);
                            }
                        }
                        "regex" => {
                            if let Some(pattern) = args.get("regex").and_then(|v| v.text())
                                && let Some(available) = &mut projected
                            {
                                available.extend(regex_captures(pattern));
                            }
                        }
                        _ => {
                            if let Some(output) = spec.default_output(&args).or_else(|| {
                                args.get("as").and_then(|v| v.text()).map(str::to_string)
                            }) {
                                if let Some(xs) = &mut projected {
                                    xs.insert(output);
                                }
                            } else if effect == Effect::Transform {
                                projected = None;
                            }
                        }
                    }
                    if let Some((outputs, members)) = spec.named_outputs(&args) {
                        if !members.is_empty() {
                            projected = None;
                        } else if let Some(available) = &mut projected {
                            available.extend(outputs);
                        }
                    }
                    if let Some((outputs, retains)) = catalog::aggregate_outputs(c) {
                        projected = if retains {
                            incoming_projection.map(|mut fields| {
                                fields.extend(outputs);
                                fields
                            })
                        } else {
                            Some(outputs)
                        };
                    }
                    if matches!(effect, Effect::Aggregate | Effect::Correlation) {
                        aggregated = true;
                        if !matches!(
                            name.as_str(),
                            "accumulate"
                                | "neighbor"
                                | "partition"
                                | "slidingwindow"
                                | "slidingtimewindow"
                        ) {
                            ordered =
                                matches!(name.as_str(), "head" | "tail" | "sort" | "table" | "top");
                        }
                    }
                    if effect != Effect::Definition {
                        has_input = true;
                    }
                }
            }
            if let Some(function) = match step {
                Step::Call(call)
                | Step::Assign {
                    value: Value::Call(call),
                    ..
                } => catalog::function(&call.name),
                _ => None,
            } {
                after_aggregate |= first_aggregate(function.name, function.effect);
            }
            if !matches!(step,Step::Call(c) if catalog::function(&c.name).is_some_and(|f|f.effect==Effect::Definition))
            {
                has_input = true;
            }
        }
        self.nesting -= 1;
        self.narrowed = enclosing_narrowed;
        (self.aggregate_input, self.ordered_input, self.stored_input) = enclosing_input;
        self.after_aggregate = enclosing_after_aggregate;
        self.reads = enclosing_reads;
        self.projected_input = enclosing_projection;
        *state = PipelineState {
            has_input,
            aggregated,
            after_aggregate,
            synthetic,
            replaced,
            extracted,
            ordered,
            narrowed,
            projected,
        };
    }
    fn filter(&mut self, f: &Filter, tables: &mut BTreeSet<String>, negated: bool) {
        if !self.narrowed
            && match f {
                Filter::Compare { field, value, .. } => {
                    field == "@rawstring" && costly_raw_pattern(value)
                }
                Filter::Text(pattern) => costly_raw_pattern(pattern),
                _ => false,
            }
        {
            self.raw_scan_warning(None);
        }
        match f {
            Filter::Compare { field, op, value } => {
                self.field(field);
                self.pattern(value);
                if matches!(op.as_str(), ">" | ">=" | "<" | "<=")
                    && !matches!(value, Pattern::Parameter(_))
                    && !matches!(value,Pattern::Text(s) if s.parse::<serde_json::Number>().is_ok())
                {
                    self.emit(
                        "CQL_NUMERIC_FILTER",
                        "Numeric filter comparisons require a numeric literal.",
                        None,
                        false,
                    );
                }
            }
            Filter::And(xs) | Filter::Or(xs) => {
                if matches!(f, Filter::Or(_)) && xs.iter().any(has_capture_marker) {
                    self.unsupported(
                        "CQL_UNSUPPORTED_REGEX_FILTER_COMPOSITION",
                        "Regex capture markers inside OR are outside this profile.",
                        None,
                        false,
                    );
                }
                if xs.iter().any(Filter::has_call) {
                    self.emit(
                        "CQL_FUNCTION_FILTER_COMPOSITION",
                        "AND and OR cannot combine function calls; use a pipe for conjunctions or case for alternatives.",
                        None,
                        false,
                    );
                }
                for x in xs {
                    self.filter(x, tables, negated)
                }
            }
            Filter::Not(x) => self.filter(x, tables, true),
            Filter::Call(c) => {
                if let Some(resolved) = catalog::resolve(c)
                    && resolved.function.resolved_effect(&resolved.arguments) != Effect::Filter
                {
                    if resolved.function.effect == Effect::Filter
                        || resolved.function.name == "matchAsArray"
                    {
                        self.unsupported(
                            "CQL_UNSUPPORTED_PREDICATE_EFFECT",
                            "This retaining argument form is not qualified in predicate context.",
                            Some(c),
                            false,
                        );
                    } else {
                        self.emit(
                            "CQL_FILTER_FUNCTION",
                            "This function cannot be used as a boolean filter.",
                            Some(c),
                            false,
                        );
                    }
                }
                self.function(c, tables, false, negated, false);
            }
            Filter::Text(p) => {
                if matches!(p, Pattern::Text(_)) || matches!(p, Pattern::Regex { .. }) {
                    self.free_text(None);
                }
                self.pattern(p);
            }
            Filter::Bool(_) => {}
        }
    }
    fn pattern(&mut self, p: &Pattern) {
        match p{
        Pattern::Regex{pattern,flags}=>self.regex(pattern,flags,None),
        Pattern::Parameter(_)=>self.unsupported("CQL_UNSUPPORTED_PARAMETER","Bind parameters through a semantic plan; unresolved saved-query parameters are outside this profile.",None,false),_=>{}
    }
    }
    fn value(&mut self, v: &Value, tables: &mut BTreeSet<String>) {
        match v {
            Value::Call(c) => {
                self.function(c, tables, false, false, false);
            }
            Value::Array(xs) => {
                for x in xs {
                    self.value(x, tables)
                }
            }
            Value::Query { pipeline, .. } => {
                self.pipeline(pipeline, QueryContext::Plain, &mut tables.clone())
            }
            Value::Binary { left, right, .. } => {
                self.value(left, tables);
                self.value(right, tables);
            }
            Value::Unary { value, .. } => self.value(value, tables),
            Value::Parameter(_) => self.unsupported(
                "CQL_UNSUPPORTED_PARAMETER",
                "An unresolved query parameter cannot be validated by this profile.",
                None,
                false,
            ),
            _ => {}
        }
    }
    fn parameters(&mut self, value: &Value, tables: &mut BTreeSet<String>) {
        match value {
            Value::Parameter(_) => self.value(value, tables),
            Value::Array(values) => {
                for value in values {
                    self.parameters(value, tables);
                }
            }
            Value::Call(call) => {
                for arg in &call.args {
                    self.parameters(&arg.value, tables);
                }
            }
            Value::Binary { left, right, .. } => {
                self.parameters(left, tables);
                self.parameters(right, tables);
            }
            Value::Unary { value, .. } => self.parameters(value, tables),
            _ => {}
        }
    }
    fn mutation(&mut self, field: &str, call: Option<&Call>) {
        if self
            .local_binding(field)
            .is_some_and(|(binding, structured)| *structured && field != binding)
        {
            self.unsupported(
                "CQL_UNSUPPORTED_PSEUDO_WRITE",
                "Structured element members are read-only in this profile.",
                call,
                false,
            );
        }
    }
    fn output_field(&mut self, field: &str, call: Option<&Call>) {
        self.mutation(field, call);
        if super::plan::checked_field(field).is_err() || field.starts_with('#') {
            self.emit("CQL_OUTPUT_FIELD", "Output fields must be nonempty data fields without forbidden control characters; tag fields are immutable.", call, false);
        }
    }
    fn scalar_value(&mut self, v: &Value, tables: &mut BTreeSet<String>) {
        match v {
            Value::Word(field) => self.field(field),
            Value::Call(c) => {
                if let Some(resolved) = catalog::resolve(c) {
                    let retaining_filter = resolved.function.effect == Effect::Filter
                        && resolved.function.resolved_effect(&resolved.arguments) != Effect::Filter;
                    let extracting_regex = resolved.function.name == "regex"
                        && (resolved.arguments.get("repeat").and_then(|v| v.boolean())
                            == Some(true)
                            || resolved
                                .arguments
                                .get("regex")
                                .and_then(|v| v.text())
                                .is_some_and(|pattern| !regex_captures(pattern).is_empty()));
                    let array_result = resolved.function.name == "splitString"
                        && !resolved.arguments.contains_key("index");
                    if retaining_filter || extracting_regex || array_result {
                        self.unsupported("CQL_UNSUPPORTED_SCALAR_EFFECT", "This argument form has unqualified extraction, multiplicity or array effects in a scalar expression.", Some(c), false);
                    }
                }
                if catalog::function(&c.name).is_some_and(|f| !f.scalar_computation()) {
                    self.unsupported("CQL_UNSUPPORTED_SCALAR_FUNCTION", "This function is not qualified as a scalar computation in the current profile.", Some(c), false);
                }
                self.function(c, tables, true, false, false);
            }
            Value::Binary { left, right, .. } => {
                self.scalar_value(left, tables);
                self.scalar_value(right, tables);
            }
            Value::Unary { value, .. } => self.scalar_value(value, tables),
            Value::Array(_) | Value::Query { .. } => self.unsupported(
                "CQL_UNSUPPORTED_SCALAR_VALUE",
                "Array and subquery values require a typed function argument in this profile.",
                None,
                false,
            ),
            _ => self.value(v, tables),
        }
    }
    fn function<'a>(
        &mut self,
        c: &'a Call,
        tables: &mut BTreeSet<String>,
        scalar: bool,
        negated: bool,
        preamble: bool,
    ) -> BTreeMap<String, &'a Value> {
        let Some(resolved) = catalog::resolve(c) else {
            self.unsupported(
                "CQL_UNSUPPORTED_FUNCTION",
                format!("Function '{}' is outside this profile.", c.name),
                Some(c),
                false,
            );
            return BTreeMap::new();
        };
        let spec = resolved.function;
        if spec.name == "readFile" {
            for (_, value) in resolved
                .ordered
                .iter()
                .filter(|(name, _)| name == "file")
                .skip(1)
            {
                if has_parameter(value) {
                    self.parameters(value, tables);
                } else if !argument_type(value, "S") {
                    self.emit(
                        "CQL_ARGUMENT_TYPE",
                        "readFile resource arguments must be file/table names or lists of names.",
                        Some(c),
                        false,
                    );
                }
            }
        }
        for mut diagnostic in resolved.diagnostics {
            diagnostic.span = Some(Box::new(span(self.query, c.start, c.end)));
            diagnostic.reference = Some(spec.reference_id.into());
            self.diagnostics.push(diagnostic);
        }
        let args = resolved.arguments;
        if spec.name == "wildcard" && !args.contains_key("field") {
            self.free_text(Some(c));
        }
        let known_args: BTreeMap<_, _> = args
            .iter()
            .filter(|(_, value)| !has_parameter(value))
            .map(|(name, value)| (name.clone(), *value))
            .collect();
        if matches!(
            spec.name,
            "eventSize" | "eventFieldCount" | "eventInternals"
        ) && (self.aggregate_input || !self.stored_input)
        {
            self.emit(
                "CQL_STORED_EVENT_PLACEMENT",
                "Stored-event inspection must precede aggregation and source replacement.",
                Some(c),
                false,
            );
        }
        if spec.name == "reverseDns"
            && (self.nesting != 1 || !self.aggregate_input || !self.aggregate_parents.is_empty())
        {
            self.emit(
                "CQL_REVERSE_DNS_PLACEMENT",
                "reverseDns requires a preceding aggregate in the top-level result pipeline.",
                Some(c),
                false,
            );
        }
        if spec.name == "parseInt"
            && let Some(value) = known_args.get("radix")
            && value
                .unsigned_integer()
                .is_none_or(|n| !(2..=36).contains(&n))
        {
            self.emit(
                "CQL_ARGUMENT_RANGE",
                "Integer radix must be from 2 through 36.",
                Some(c),
                false,
            );
        }
        if spec.name == "setTimeInterval" {
            if !preamble || self.time_interval_seen {
                self.emit("CQL_TIME_INTERVAL_PLACEMENT", "setTimeInterval must appear once at the beginning of the top-level query, before filters and table declarations.", Some(c), false);
            }
            self.time_interval_seen = true;
            self.emit("CQL_QUERY_TIME_OVERRIDE", "This declaration overrides the API/UI time interval. Authorize and verify its effective start/end before execution.", Some(c), true);
        }
        if matches!(
            spec.name,
            "setTimeInterval" | "join" | "defineTable" | "remoteTable" | "readPersistedAggregation"
        ) {
            for name in ["start", "end"] {
                if let Some(value) = known_args.get(name)
                    && argument_type(value, spec.argument_kind(name).expect("interval argument"))
                    && interval_value(value).is_none()
                {
                    self.unsupported(
                        "CQL_UNSUPPORTED_DURATION",
                        format!("'{name}' is outside the supported now, positive fixed-duration, signed offset, epoch-millisecond integer or offset-qualified RFC3339 forms."),
                        Some(c),
                        false,
                    );
                }
            }
        }
        if matches!(
            spec.name,
            "setTimeInterval" | "join" | "defineTable" | "remoteTable" | "readPersistedAggregation"
        ) && let Some(start) = known_args
            .get("start")
            .and_then(|value| interval_value(value))
        {
            let end = known_args
                .get("end")
                .and_then(|value| interval_value(value))
                .or_else(|| {
                    (spec.name == "setTimeInterval" && !args.contains_key("end"))
                        .then_some(IntervalValue::Relative(0))
                });
            if matches!(
                (start, end),
                (IntervalValue::Relative(start), Some(IntervalValue::Relative(end)))
                    | (IntervalValue::Absolute(start), Some(IntervalValue::Absolute(end)))
                    if start > end
            ) {
                self.emit(
                    "CQL_TIME_INTERVAL_ORDER",
                    "The start of the interval must not follow its end.",
                    Some(c),
                    false,
                );
            }
        }
        if spec.name == "slidingTimeWindow"
            && let Some(value) = known_args.get("span")
            && !value.text().is_some_and(super::plan::duration)
            && !long_literal(value).is_some_and(|n| n > 0)
        {
            self.unsupported(
                "CQL_UNSUPPORTED_DURATION",
                "The sliding time span is outside the supported positive fixed-duration or millisecond-integer forms.",
                Some(c),
                false,
            );
        }
        if spec.name == "remoteTable"
            && matches!(args.get("query"), Some(Value::Query { .. }))
            && !args.contains_key("include")
        {
            self.emit(
                "CQL_ARGUMENT_REQUIRED",
                "The CQL remoteTable form requires include.",
                Some(c),
                false,
            );
        }
        if spec.name == "window" {
            if !self
                .aggregate_parents
                .last()
                .is_some_and(|name| matches!(*name, "bucket" | "timeChart"))
            {
                self.emit(
                    "CQL_WINDOW_PLACEMENT",
                    "window is only supported as the aggregate function of bucket or timeChart.",
                    Some(c),
                    false,
                );
            }
            if args.contains_key("span") == args.contains_key("buckets") {
                self.emit(
                    "CQL_WINDOW_WIDTH",
                    "Specify exactly one of window span or buckets.",
                    Some(c),
                    false,
                );
            }
        }
        if matches!(
            spec.name,
            "accumulate" | "slidingWindow" | "slidingTimeWindow" | "partition"
        ) {
            if !self.aggregate_input {
                self.emit("CQL_SEQUENCE_PLACEMENT", "Sequence functions require a preceding aggregation that establishes the input stream order.", Some(c), false);
            } else if !self.ordered_input {
                self.emit("CQL_SEQUENCE_ORDER", "The preceding aggregate has no statically established row order; verify ordering before sequence analysis.", Some(c), true);
            }
        }
        if matches!(
            spec.name,
            "accumulate" | "slidingWindow" | "slidingTimeWindow"
        ) && let Some(function) = known_args.get("function")
            && !single_aggregate(function)
        {
            self.emit(
                "CQL_SEQUENCE_AGGREGATE_ROWS",
                "This sequence function requires sub-aggregations that output at most one event.",
                Some(c),
                false,
            );
        }
        if spec.name == "array:reduceRow"
            && let Some(function) = known_args.get("function")
            && (!single_aggregate(function)
                || catalog::aggregate_value_outputs(function)
                    .is_none_or(|(fields, _)| fields.len() != 1))
        {
            self.unsupported(
                "CQL_UNSUPPORTED_REDUCER_OUTPUT",
                "Row reduction requires a qualified single-event, single-field aggregate.",
                Some(c),
                false,
            );
        }
        if spec.name == "stats"
            && let Some(value) = known_args.get("function")
        {
            let functions = match *value {
                Value::Array(xs) => xs.as_slice(),
                value => std::slice::from_ref(value),
            };
            if functions.iter().enumerate().any(|(i, function)| {
                contains_sequence(function)
                    && functions
                        .iter()
                        .enumerate()
                        .any(|(j, sibling)| i != j && !single_aggregate(sibling))
            }) {
                self.emit("CQL_SEQUENCE_STATS", "A sequence function cannot share stats with another function that can output multiple events.", Some(c), false);
            }
        }
        if spec.name == "setField" && !args.get("target").is_some_and(|v| has_parameter(v)) {
            match args.get("target") {
                Some(Value::String(field)) => self.output_field(field, Some(c)),
                Some(_) => self.unsupported("CQL_UNSUPPORTED_DYNAMIC_OUTPUT", "A computed output name cannot be proven to preserve immutable tag fields; use a literal data-field target.", Some(c), false),
                None => {}
            }
        }
        if spec.name == "parseUri" && known_args.get("prefix").and_then(|v| v.text()) == Some("") {
            self.emit(
                "CQL_ARGUMENT_VALUE",
                "parseUri prefix cannot be empty.",
                Some(c),
                false,
            );
        }
        if spec.name == "percentile" {
            if known_args.get("accuracy").is_some_and(|value| {
                compare_numeric_literal(value, 0.0).is_some_and(|order| order != Ordering::Greater)
                    || compare_numeric_literal(value, 1.0)
                        .is_some_and(|order| order != Ordering::Less)
                    || numeric_literal(value).is_some_and(|number| number <= 0.0 || number >= 1.0)
            }) {
                self.emit(
                    "CQL_ARGUMENT_RANGE",
                    "Percentile accuracy must be greater than zero and less than one.",
                    Some(c),
                    false,
                );
            }
            self.emit("CQL_APPROXIMATE_PERCENTILE", "Percentiles are estimates over non-negative inputs; verify error tolerance and execution completeness.", Some(c), true);
        }
        if matches!(spec.name, "hashMatch" | "hashRewrite")
            && known_args
                .get("bits")
                .and_then(|v| long_literal(v))
                .is_some_and(|n| n % 8 != 0)
        {
            self.emit(
                "CQL_ARGUMENT_RANGE",
                "Hash bits must be a multiple of eight.",
                Some(c),
                false,
            );
        }
        if matches!(spec.name, "parseCsv" | "parseFixedWidth") {
            if let Some(columns) = args.get("columns") {
                for field in catalog::fields(columns) {
                    self.output_field(&field, Some(c));
                }
            }
            if spec.name == "parseCsv"
                && known_args
                    .get("delimiter")
                    .and_then(|v| v.text())
                    .is_some_and(|value| value.chars().count() != 1)
            {
                self.emit(
                    "CQL_ARGUMENT_VALUE",
                    "CSV delimiter must be one character.",
                    Some(c),
                    false,
                );
            }
            if spec.name == "parseFixedWidth"
                && let (Some(Value::Array(columns)), Some(Value::Array(widths))) =
                    (args.get("columns"), args.get("widths"))
                && columns.len() != widths.len()
            {
                self.emit(
                    "CQL_ARGUMENT_COUNT",
                    "Fixed-width columns and widths must have the same length.",
                    Some(c),
                    false,
                );
            }
        }
        if spec.name == "bitfield:extractFlags"
            && let Some(Value::Array(pairs)) = args.get("output")
        {
            for pair in pairs {
                if let Value::Array(pair) = pair
                    && let Some(field) = pair
                        .get(1)
                        .filter(|value| !has_parameter(value))
                        .and_then(|v| v.text())
                {
                    self.output_field(field, Some(c));
                }
            }
        }
        if spec.name == "lowercase"
            && let Some(value) = args.get("field")
        {
            let rename = known_args
                .get("include")
                .and_then(|v| v.text())
                .is_some_and(|v| matches!(v, "fields" | "both"));
            if rename {
                self.emit("CQL_LOWERCASE_RETENTION", "The reference examples do not establish consistent old-name retention for finite field-name conversion. Verify target behavior before relying on the original names.", Some(c), true);
            }
            for field in catalog::fields(value) {
                if field.contains(['*', '?']) {
                    self.unsupported(
                        "CQL_UNSUPPORTED_DYNAMIC_OUTPUT",
                        "Wildcard mutation cannot establish a fixed set of mutable data fields.",
                        Some(c),
                        false,
                    );
                } else {
                    self.output_field(&if rename { field.to_lowercase() } else { field }, Some(c));
                }
            }
        }
        if !self.narrowed
            && matches!(spec.name, "regex" | "wildcard")
            && raw_scan_field(spec.name, &args)
        {
            self.raw_scan_warning(Some(c));
        }
        let mut query_projection = None;
        if matches!(spec.name, "defineTable" | "remoteTable") {
            if matches!(args.get("include"), Some(Value::Array(xs)) if xs.iter().any(catalog::all_columns))
            {
                self.unsupported("CQL_UNSUPPORTED_ALL_COLUMNS", "Use the scalar * selector for all table columns; wildcard entries inside a list are unsupported.", Some(c), false);
            }
            self.definitions += 1;
            if self.definitions > 10 {
                self.emit(
                    "CQL_TABLE_LIMIT",
                    "A query supports at most ten ad-hoc tables.",
                    Some(c),
                    false,
                );
            }
        }
        if matches!(spec.name, "join" | "selfJoin" | "selfJoinFilter")
            && (self.enclosed_queries > 0 || !self.aggregate_parents.is_empty())
        {
            self.emit(
                "CQL_JOIN_PLACEMENT",
                format!(
                    "{}() must be at the top level of the query or of a join subquery; it cannot be used inside a function argument.",
                    spec.name
                ),
                Some(c),
                false,
            );
        }
        self.joins |= matches!(spec.name, "join" | "selfJoin" | "selfJoinFilter");
        self.correlation_seen |= spec.name == "correlate";
        if matches!(spec.name, "match" | "matchAsArray" | "readFile")
            && c.args.iter().any(|a| a.name.as_deref() == Some("table"))
        {
            for table in args
                .get("file")
                .into_iter()
                .flat_map(|v| catalog::fields(v))
            {
                if !tables.contains(&table) {
                    self.emit(
                        "CQL_TABLE_NOT_DEFINED",
                        format!("Table '{table}' has not been defined."),
                        Some(c),
                        false,
                    );
                }
            }
        }
        if matches!(spec.name, "join" | "match" | "matchAsArray") {
            let other = if spec.name == "join" { "key" } else { "column" };
            if let Some(field) = args.get("field")
                && !has_parameter(field)
                && args.get(other).is_none_or(|value| !has_parameter(value))
            {
                let fields = catalog::fields(field);
                let keys = args.get(other).map(|v| catalog::fields(v));
                // Count positional slots, including repeated field names.
                let arity = |v: &&Value| match v {
                    Value::Array(xs) => xs.len(),
                    _ => 1,
                };
                if fields.is_empty()
                    || args.get(other).is_some_and(|v| arity(v) != arity(field))
                    || keys.is_some_and(|k| k.is_empty())
                {
                    self.emit(
                        "CQL_JOIN_KEY_ARITY",
                        "Matching requires nonempty field/key lists of equal length.",
                        Some(c),
                        false,
                    );
                }
            }
        }
        if spec.name == "match" {
            let glob = args.get("glob").and_then(|v| v.boolean()) == Some(true)
                || known_args.get("mode").and_then(|v| v.text()) == Some("glob");
            if glob
                && ["field", "column"]
                    .iter()
                    .any(|k| matches!(args.get(*k), Some(Value::Array(_))))
            {
                self.emit(
                    "CQL_GLOB_MATCH_KEYS",
                    "Glob matching accepts a single scalar field and column, not arrays.",
                    Some(c),
                    false,
                );
            }
            if args.contains_key("mode") && args.contains_key("glob") {
                self.unsupported(
                    "CQL_UNSUPPORTED_MATCH_MODE",
                    "Combining deprecated glob with mode is unsupported; choose one matching option.",
                    Some(c),
                    false,
                );
            }
            self.emit("CQL_LOOKUP_COMPLETENESS", "Verify lookup/table availability, matching mode, row caps and duplicate cardinality; omitted include imports all non-key columns.", Some(c), true);
        }
        if let Some((outputs, members)) = spec.named_outputs(&known_args) {
            for output in outputs {
                self.output_field(&output, Some(c));
            }
            for member in members {
                self.output_field(&member, Some(c));
            }
            if spec.name == "ioc:lookup"
                && args
                    .get("prefix")
                    .and_then(|v| v.text())
                    .is_some_and(|p| p.contains("[]"))
            {
                self.unsupported(
                    "CQL_UNSUPPORTED_ARRAY_NAME",
                    "Empty-index patterns in an IOC output prefix are unsupported.",
                    Some(c),
                    false,
                );
            }
            if matches!(spec.name, "asn" | "ipLocation" | "ioc:lookup") {
                self.emit("CQL_ENRICHMENT_CAPABILITY", "External enrichment data, updates and target availability are unverified. Outputs can be absent or retain incoming values; acceptance is not match or data-quality proof.", Some(c), true);
            }
        }
        if matches!(spec.name, "join" | "match") && !(spec.name == "match" && negated) {
            let columns = args
                .get("file")
                .and_then(|v| v.text())
                .and_then(|t| self.table_columns.get(t))
                .and_then(|c| c.as_ref());
            let includes = catalog::imported_fields(spec.name, &args, columns);
            if let Some(includes) = includes {
                for field in includes {
                    self.output_field(&field, Some(c));
                }
            }
        }
        for (key, maximum) in match spec.name {
            "join" => &[("limit", 200000)][..],
            "match" => &[("nrows", 500)][..],
            "bucket" | "timeChart" => &[("limit", 500), ("buckets", 1500)][..],
            "neighbor" => &[("distance", 10000)][..],
            _ => &[],
        } {
            if let Some(value) = args.get(*key)
                && !has_parameter(value)
                && value
                    .unsigned_integer()
                    .is_some_and(|n| n == 0 || n > *maximum)
            {
                self.emit(
                    "CQL_ARGUMENT_RANGE",
                    format!(
                        "'{key}' must be in 1..={maximum}, or its explicitly supported sentinel."
                    ),
                    Some(c),
                    false,
                );
            }
        }
        if matches!(spec.name, "bucket" | "timeChart") {
            for key in ["span", "minSpan"] {
                if let Some(value) = known_args.get(key).and_then(|v| v.text())
                    && !(super::plan::duration(value) || key == "span" && value == "auto")
                {
                    self.unsupported("CQL_UNSUPPORTED_BUCKET_SPAN", "Use auto for span or a positive fixed duration. Calendar, anchored and numeric minSpan forms are not qualified.", Some(c), false);
                }
            }
            if let Some(Value::Array(units)) = args.get("unit")
                && !units.iter().any(has_parameter)
                && args
                    .get("function")
                    .is_none_or(|value| !has_parameter(value))
            {
                let functions = match args.get("function") {
                    Some(Value::Array(xs)) => xs.len(),
                    _ => 1,
                };
                if units.len() != 1 && units.len() != functions {
                    self.emit(
                        "CQL_UNIT_ARITY",
                        "unit must contain one conversion or one per aggregate function.",
                        Some(c),
                        false,
                    );
                }
            }
            self.emit("CQL_TIME_BUCKET_LIMIT", "Bucket boundaries depend on the query window and timezone; trailing data can add a bucket. Series and memory caps can discard groups. Inspect complete execution results.", Some(c), true);
        }
        if spec.name == "findTimestamp" {
            self.emit("CQL_TIMESTAMP_DETECTION", "Timestamp detection is bounded and nullable; missing date/year can use the current date/year and timezone/DST can change interpretation. Use fully dated fixtures and verify the resulting epoch value.", Some(c), true);
        }
        if matches!(spec.name, "session" | "series") {
            self.emit("CQL_TEMPORAL_STREAM", "Transactions depend on event timestamps, stream order, key scope and memory limits; partial end sequences can survive. A single synthetic batch does not prove production gap or ordering behavior.", Some(c), true);
        }
        if spec.name == "series"
            && let Some(value) = known_args.get("memlimit").and_then(|v| v.text())
        {
            let boundary = value
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(value.len());
            let multiplier = match value[boundary..].trim() {
                "" | "B" => Some(1u64),
                "KB" => Some(1000),
                "KiB" => Some(1024),
                _ => None,
            };
            let bytes = value[..boundary]
                .parse::<u64>()
                .ok()
                .and_then(|n| multiplier.and_then(|m| n.checked_mul(m)));
            if bytes.is_none_or(|n| n == 0 || n > 1024) {
                self.emit("CQL_ARGUMENT_RANGE", "series memlimit must be between 1 byte and 1KiB using a qualified B/KB/KiB spelling.", Some(c), false);
            }
        }
        if matches!(spec.name, "math:mod" | "math:pow") {
            let name = if spec.name == "math:mod" {
                "divisor"
            } else {
                "exponent"
            };
            if let Some(value) = known_args.get(name) {
                if !numeric_expression(value) {
                    self.emit(
                        "CQL_ARGUMENT_TYPE",
                        format!("Argument '{name}' requires a numeric expression."),
                        Some(c),
                        false,
                    );
                } else if long_literal(value).is_none() {
                    self.emit("CQL_MATH_PARAMETER_CONTRACT", "The parameter table specifies integer, while examples describe decimal or field values; verify numeric conversion on the target.", Some(c), true);
                }
            }
        }
        if matches!(spec.name, "text:editDistance" | "text:editDistanceAsArray") {
            if known_args
                .get("maxDistance")
                .and_then(|v| v.unsigned_integer())
                .is_some_and(|v| !(1..=100).contains(&v))
            {
                self.emit(
                    "CQL_ARGUMENT_RANGE",
                    "maxDistance must be from 1 through 100.",
                    Some(c),
                    false,
                );
            }
            if let Some(Value::Array(references)) = args.get("references")
                && !(1..=100).contains(&references.len())
            {
                self.emit(
                    "CQL_ARGUMENT_RANGE",
                    "references must contain from 1 through 100 expressions.",
                    Some(c),
                    false,
                );
            }
        }
        if spec.name == "count" && args.get("distinct").and_then(|v| v.boolean()) == Some(true) {
            if !args.contains_key("field") {
                self.emit(
                    "CQL_ARGUMENT_REQUIRED",
                    "Distinct counting requires field.",
                    Some(c),
                    false,
                );
            }
            self.emit("CQL_APPROXIMATE_DISTINCT", "count(distinct=true) estimates cardinality. Use explicit exact grouping when exactness is required, and check execution caps.", Some(c), true);
        }
        if spec.name == "groupBy" {
            let empty = matches!(args.get("field"),Some(Value::Array(xs)) if xs.is_empty());
            if empty {
                self.emit(
                    "CQL_EMPTY_GROUP_KEYS",
                    "groupBy([]) returns no groups; use a whole-stream aggregate.",
                    Some(c),
                    true,
                );
            } else if args.get("limit").and_then(|v| v.text()) == Some("max") {
                self.info("CQL_GROUPBY_LIMIT", "limit=max still has group and memory ceilings; check the executed result for a cap hit and verify completeness.", Some(c));
            } else {
                self.emit("CQL_GROUPBY_LIMIT", "groupBy can discard groups before a later sort or row cap. Consider limit=max and verify execution completeness.", Some(c), true);
            }
        }
        if spec.name == "formatTime"
            && known_args
                .get("unit")
                .and_then(|v| v.text())
                .is_some_and(|unit| matches!(unit, "seconds" | "milliseconds"))
        {
            let literal = args.get("field").is_some_and(|value| matches!(value,Value::Number(_)) || matches!(value,Value::Unary{op,value} if matches!(op.as_str(),"-"|"+") && matches!(value.as_ref(),Value::Number(_))));
            if literal {
                if args.get("field").and_then(|v| long_literal(v)).is_none() {
                    self.emit("CQL_TIME_INTEGER_INPUT", "An explicit seconds/milliseconds unit requires an exact signed-long integer timestamp input.", Some(c), false);
                }
            } else {
                self.emit("CQL_TIME_INTEGER_INPUT", "Verify that the runtime timestamp expression is a long integer for the explicit unit.", Some(c), true);
            }
        }
        match spec.name {
            "select" if args.get("fields").is_some_and(|v|catalog::fields(v).iter().any(|field|field.ends_with("[]"))) => self.emit("CQL_ARRAY_PROJECTION_LITERAL", "The profile requires explicit indexed projection fields; a trailing [] does not establish whole-array selection.", Some(c), true),
            "split" => self.emit("CQL_SPLIT_SHAPE", "split defaults to _events, expands indexed fields, adds _index and can rewrite @id. Verify strip/prefix mappings and memory growth; unresolved member shapes are not deployment evidence.", Some(c), true),
            "top" => self.emit("CQL_APPROXIMATE_TOP", "top uses an approximate sketch; verify precision, caps and completeness. It is not an exact-ranking substitute.", Some(c), true),
            "collect" => self.emit("CQL_COLLECT_LIMIT", "collect has distinct-value and memory caps, undefined value order, and can return partial results; inspect execution warnings.", Some(c), true),
            "selectFromMax" | "selectFromMin" => self.emit("CQL_SELECTOR_TIES", "Ties select an unspecified event; output contains only include fields.", Some(c), true),
            "selectLast" => self.emit("CQL_LAST_EVENT_ORDER", "Last-seen values depend on event time and ties; verify the required ordering on the selected target.", Some(c), true),
            _ => {}
        }
        if spec.name == "top" && args.get("error").is_some_and(|v| !argument_type(v, "i")) {
            self.emit("CQL_TOP_ERROR_CONTRACT", "Fractional top.error is accepted within this profile; verify target support and precision semantics.", Some(c), true);
        }
        if spec.name == "top" && args.contains_key("sum") && args.contains_key("max") {
            self.unsupported(
                "CQL_UNSUPPORTED_TOP_RANKING",
                "Simultaneous sum and max ranking is unsupported; choose one ranking mode.",
                Some(c),
                false,
            );
        }
        if !scalar
            && !args.contains_key("as")
            && let Some(output) = spec.default_output(&known_args)
        {
            self.output_field(&output, Some(c));
        }
        if spec.name == "default"
            && let Some(fields) = args.get("field")
        {
            let fills_missing_only = args
                .get("replaceEmpty")
                .is_none_or(|value| !has_parameter(value) && value.boolean() == Some(false));
            for field in catalog::fields(fields) {
                if field.starts_with('#') && fills_missing_only {
                    self.mutation(&field, Some(c));
                } else {
                    self.output_field(&field, Some(c));
                }
            }
        }
        for (name, value) in &args {
            if spec.name == "eval" {
                self.output_field(name, Some(c));
                self.scalar_value(value, tables);
                continue;
            }
            let Some(kind) = spec.argument_kind(name) else {
                continue;
            };
            if matches!(value, Value::Parameter(_)) {
                self.value(value, tables);
                continue;
            }
            if has_parameter(value)
                && (!matches!(kind, "g" | "u" | "e" | "p" | "e[]") || !argument_type(value, kind))
            {
                self.parameters(value, tables);
                if argument_type(value, kind) {
                    continue;
                }
            }
            if kind == "y" && value.text().is_some_and(|s| s.contains('`')) {
                self.unsupported(
                    "CQL_UNSUPPORTED_ARRAY_NAME",
                    "Quoted special array members are outside the supported syntax subset.",
                    Some(c),
                    false,
                );
                continue;
            }
            let boolean_string =
                spec.name == "top" && name == "rest" && matches!(value, Value::Bool(_));
            let sample_number =
                spec.name == "sample" && name == "percentage" && numeric_literal(value).is_some();
            if !argument_type(value, kind) && !boolean_string && !sample_number {
                if kind == "d" && value.text().is_some() {
                    self.unsupported("CQL_UNSUPPORTED_DURATION", "Only positive whole-number fixed durations are supported; other relative-time spellings and explicit zero are unsupported.", Some(c), false);
                    continue;
                }
                if matches!(spec.name, "bucket" | "timeChart")
                    && name == "minSpan"
                    && matches!(value, Value::Number(_))
                {
                    continue;
                }
                let complex = matches!(
                    value,
                    Value::Call(_) | Value::Binary { .. } | Value::Unary { .. }
                ) && !(kind == "f"
                    && matches!(value, Value::Call(call) if call.name.eq_ignore_ascii_case("formatTime")));
                let emit = if complex {
                    Self::unsupported
                } else {
                    Self::emit
                };
                emit(
                    self,
                    if complex {
                        "CQL_UNSUPPORTED_ARGUMENT_EXPRESSION"
                    } else {
                        "CQL_ARGUMENT_TYPE"
                    },
                    format!("Argument '{name}' does not match profile type '{kind}'."),
                    Some(c),
                    false,
                );
                continue;
            }
            if matches!(kind, "Q" | "Q[]") {
                continue;
            }
            if kind == "a" {
                self.unsupported(
                    "CQL_UNSUPPORTED_ARGUMENT_CONTRACT",
                    format!("The element and execution context contract for argument '{name}' is outside this profile."),
                    Some(c),
                    false,
                );
                continue;
            }
            if spec.argument_role(name) == catalog::ArgumentRole::Removal {
                for field in catalog::fields(value) {
                    self.mutation(&field, Some(c));
                }
            }
            if kind == "P" {
                for (old, _) in catalog::rename_bindings(&args) {
                    self.mutation(old, Some(c));
                    self.field(old);
                }
            }
            if matches!(kind, "f" | "F" | "A")
                && spec.argument_role(name) == catalog::ArgumentRole::InputField
                && spec.name != "correlate"
                && !(spec.name.starts_with("time:")
                    && name == "timezoneField"
                    && args.contains_key("timezone"))
            {
                for f in arg_fields(Some(value)) {
                    self.field(&f);
                }
            }
            if matches!(name.as_str(), "as" | "timezoneAs")
                && let Some(field) = value.text()
            {
                self.output_field(field, Some(c));
            }
            if spec.name != "concatArray"
                && matches!(name.as_str(), "prefix" | "headerprefix" | "labelprefix")
                && let Some(prefix) = value.text().filter(|s| !s.is_empty())
            {
                self.output_field(prefix, Some(c));
            }
            if name == "asArray"
                && let Some(array) = value.text().and_then(|s| s.strip_suffix("[]"))
            {
                self.output_field(array, Some(c));
            }
            if (name == "array" || name == "asArray")
                && value
                    .text()
                    .and_then(|s| s.strip_suffix("[]"))
                    .is_some_and(|base| base.contains("[]"))
            {
                self.unsupported(
                    "CQL_UNSUPPORTED_ARRAY_NAME",
                    "Empty-index patterns inside an array-name prefix are unsupported.",
                    Some(c),
                    false,
                );
            }
            if spec.name == "splitString"
                && name == "as"
                && !args.contains_key("index")
                && value.text().is_some_and(|base| base.contains("[]"))
            {
                self.unsupported(
                    "CQL_UNSUPPORTED_ARRAY_NAME",
                    "A split-string output prefix containing empty-index patterns is unsupported.",
                    Some(c),
                    false,
                );
            }
            if kind == "r"
                && let Some(pattern) = value.text()
            {
                let flags = c
                    .args
                    .iter()
                    .find(|a| a.name.as_deref() == Some("flags"))
                    .and_then(|a| a.value.text())
                    .unwrap_or("m");
                self.regex(pattern, flags, Some(c));
            }
            if kind == "g" {
                let element_scope =
                    spec.argument_scope(name) == catalog::ArgumentScope::ElementAggregate;
                let binding = element_scope
                    .then(|| args.get("var").and_then(|v| v.text()))
                    .flatten();
                if let Some(binding) = binding {
                    self.bindings.push((binding.into(), false));
                }
                self.aggregate_parents.push(spec.name);
                self.aggregates(value, c, tables);
                self.aggregate_parents.pop();
                if binding.is_some() {
                    self.bindings.pop();
                }
                continue;
            }
            if kind == "u" {
                let binding = args
                    .get("var")
                    .or_else(|| args.get("array"))
                    .and_then(|v| v.text())
                    .map(|s| s.trim_end_matches("[]").to_string());
                if let Some(binding) = &binding {
                    self.bindings
                        .push((binding.clone(), spec.name.starts_with("objectArray:")));
                }
                self.inline(value, tables);
                if binding.is_some() {
                    self.bindings.pop();
                }
            } else if kind == "q[]" {
                if let Value::Array(xs) = value {
                    for x in xs {
                        if let Value::Query { pipeline, .. } = x {
                            self.filter_query(pipeline, c, tables);
                        }
                    }
                }
            } else if kind == "e[]" {
                if let Value::Array(xs) = value {
                    for item in xs {
                        self.scalar_value(item, tables);
                    }
                }
            } else if matches!(kind, "e" | "p") {
                self.scalar_value(value, tables);
            } else if spec.argument_scope(name) == catalog::ArgumentScope::Continuing {
                if let Value::Query { pipeline, .. } = value {
                    self.filter_query(pipeline, c, tables);
                }
            } else if spec.argument_scope(name) == catalog::ArgumentScope::Independent {
                let outer = std::mem::take(&mut self.bindings);
                let fields = std::mem::take(&mut self.fields);
                let enclosed = std::mem::take(&mut self.enclosed_queries);
                let parents = std::mem::take(&mut self.aggregate_parents);
                if let Value::Query { pipeline, .. } = value {
                    let mut state = PipelineState::default();
                    self.pipeline_state(
                        pipeline,
                        QueryContext::Plain,
                        &mut tables.clone(),
                        &mut state,
                    );
                    if name == "query" {
                        query_projection = state.projected;
                    }
                } else {
                    self.value(value, tables);
                }
                self.bindings = outer;
                self.fields = fields;
                self.enclosed_queries = enclosed;
                self.aggregate_parents = parents;
            } else {
                self.value(value, tables);
            }
        }
        if matches!(spec.name, "defineTable" | "remoteTable")
            && let Some(name) = known_args.get("name").and_then(|v| v.text())
        {
            let columns = if args.get("include").is_some_and(|v| catalog::all_columns(v)) {
                query_projection
            } else {
                args.get("include").map(|v| catalog::fields(v))
            };
            self.table_columns.insert(name.into(), columns);
        }
        if matches!(
            spec.name,
            "array:eval" | "array:filter" | "objectArray:eval" | "objectArray:exists"
        ) {
            if spec.name != "objectArray:exists" {
                if !args.contains_key("asArray")
                    && let Some(output) = args
                        .get("asArray")
                        .or_else(|| args.get("array"))
                        .and_then(|v| v.text())
                        .and_then(|s| s.strip_suffix("[]"))
                {
                    self.output_field(output, Some(c));
                }
                self.emit("CQL_ARRAY_OUTPUT_REUSE", "Use a fresh output-array namespace or verify explicit cleanup. Old indexed fields beyond a gap can survive reuse; no cleanup is inserted.", Some(c), true);
            }
            if let Some(binding) = known_args.get("var").and_then(|v| v.text())
                && (!super::ast::bare_field(binding) || binding.contains(['.', '[', ']', '@', '#']))
            {
                self.emit(
                    "CQL_ARRAY_BINDING",
                    "An explicit array binding must be a simple data-field identifier.",
                    Some(c),
                    false,
                );
            }
            if spec.name == "objectArray:eval"
                && args.get("array").is_none_or(|value| !has_parameter(value))
                && args
                    .get("asArray")
                    .is_none_or(|value| !has_parameter(value))
                && args.get("array").and_then(|v| v.text())
                    == args.get("asArray").and_then(|v| v.text())
            {
                self.emit(
                    "CQL_ARRAY_OUTPUT",
                    "objectArray:eval output array must differ from input.",
                    Some(c),
                    false,
                );
            }
            if matches!(spec.name, "array:eval" | "objectArray:eval")
                && let (Some(output), Some(function)) = (
                    args.get("asArray")
                        .or_else(|| args.get("array"))
                        .and_then(|v| v.text())
                        .and_then(|s| s.strip_suffix("[]")),
                    args.get("function"),
                )
                && !inline_writes(function, output, false)
            {
                self.unsupported("CQL_UNSUPPORTED_INLINE_OUTPUT", format!("The inline function must explicitly write '{output}' on every surviving branch in this profile."),Some(c),false);
            }
        }
        for key in ["limit", "max", "maxPerRoot"] {
            if matches!(spec.argument_kind(key), Some("i" | "l"))
                && !(key == "limit" && matches!(spec.name, "head" | "tail"))
                && let Some(v) = known_args.get(key)
                && v.unsigned_integer() == Some(0)
            {
                self.emit(
                    "CQL_ARGUMENT_RANGE",
                    format!("'{key}' must be positive."),
                    Some(c),
                    false,
                );
            }
        }
        for rule in catalog::parameter_rules(spec.name) {
            let Some(value) = args.get(&rule.name) else {
                continue;
            };
            let kind = spec.argument_kind(&rule.name).unwrap_or("");
            let values = match *value {
                Value::Array(values) => values.as_slice(),
                value => std::slice::from_ref(value),
            };
            if !rule.choices.is_empty()
                && matches!(kind, "s" | "S" | "s[]")
                && !(spec.name == "top" && rule.name == "rest")
            {
                for value in values {
                    if !has_parameter(value)
                        && let Some(choice) = value
                            .text()
                            .map(str::to_owned)
                            .or_else(|| numeric_literal(value).map(|_| value.render()))
                            .or_else(|| matches!(value, Value::Bool(_)).then(|| value.render()))
                        && !(if rule.name == "order" {
                            ["asc", "desc", "ascending", "descending"]
                                .contains(&choice.to_ascii_lowercase().as_str())
                        } else {
                            rule.choices.iter().any(|allowed| {
                                if kind == "b" || rule.choices == ["false", "true"] {
                                    allowed.eq_ignore_ascii_case(&choice)
                                } else {
                                    allowed == &choice
                                }
                            })
                        })
                    {
                        self.emit(
                            "CQL_ARGUMENT_VALUE",
                            format!(
                                "'{}' must be one of {}.",
                                rule.name,
                                rule.choices.join(", ")
                            ),
                            Some(c),
                            false,
                        );
                    }
                }
            }
            if matches!(kind, "i" | "j" | "l" | "n" | "N") {
                let (minimum, maximum, range_message) = match (spec.name, rule.name.as_str()) {
                    ("sample", "percentage") => (
                        Some(0.01),
                        rule.maximum,
                        Some("Sample percentage must be between 0.01 and 100 inclusive."),
                    ),
                    ("percentile", "percentiles") => (
                        Some(0.0),
                        Some(100.0),
                        Some("Percentiles must be between 0 and 100 inclusive."),
                    ),
                    _ => (rule.minimum, rule.maximum, None),
                };
                for value in values {
                    let converted;
                    let value = if matches!(kind, "i" | "j" | "l")
                        && let Value::String(text) = value
                    {
                        converted = Value::Number(text.clone());
                        &converted
                    } else {
                        value
                    };
                    if !has_parameter(value)
                        && (minimum.is_some_and(|minimum| {
                            compare_numeric_literal(value, minimum) == Some(Ordering::Less)
                        }) || maximum.is_some_and(|maximum| {
                            compare_numeric_literal(value, maximum) == Some(Ordering::Greater)
                        }) || numeric_literal(value).is_some_and(|number| {
                            minimum.is_some_and(|minimum| number < minimum)
                                || maximum.is_some_and(|maximum| number > maximum)
                        }))
                    {
                        self.emit(
                            "CQL_ARGUMENT_RANGE",
                            range_message.map_or_else(
                                || format!("'{}' is outside its documented bounds.", rule.name),
                                str::to_owned,
                            ),
                            Some(c),
                            false,
                        );
                    }
                }
            }
        }
        if spec.name == "kvParse"
            && known_args
                .get("separator")
                .and_then(|v| v.text())
                .is_some_and(|separator| separator.chars().count() != 1)
        {
            self.emit(
                "CQL_SEPARATOR",
                "kvParse separator must be one character.",
                Some(c),
                false,
            );
        }
        if spec.name == "rename" {
            if let Some(Value::Array(pairs)) = args.get("field").copied() {
                if args.contains_key("as") {
                    self.emit(
                        "CQL_RENAME_FORM",
                        "Use either scalar field with as, or explicit old/new pairs.",
                        Some(c),
                        false,
                    );
                }
                let mut inputs = BTreeSet::new();
                let mut outputs = BTreeSet::new();
                for pair in pairs {
                    if let Value::Array(pair) = pair
                        && pair.len() == 2
                    {
                        if !has_parameter(&pair[0])
                            && let Some(old) = pair[0].text()
                        {
                            inputs.insert(old);
                        }
                        if !has_parameter(&pair[1])
                            && let Some(new) = pair[1].text()
                        {
                            if !outputs.insert(new) {
                                self.emit(
                                    "CQL_RENAME_FORM",
                                    "Rename targets must be unique.",
                                    Some(c),
                                    false,
                                );
                            }
                            self.output_field(new, Some(c));
                        }
                    }
                }
                if inputs.intersection(&outputs).next().is_some() {
                    self.unsupported(
                        "CQL_UNSUPPORTED_RENAME_OVERLAP",
                        "Overlapping rename pairs need qualified simultaneous/ordered effects.",
                        Some(c),
                        false,
                    );
                }
            } else if !args.contains_key("as")
                && args.get("field").is_none_or(|value| !has_parameter(value))
            {
                self.emit(
                    "CQL_ARGUMENT_REQUIRED",
                    "Scalar rename requires as.",
                    Some(c),
                    false,
                );
            }
        }
        if spec.name == "lower" && args.contains_key("type") {
            self.emit("CQL_LOWER_TYPE", "The reference documents type but leaves its distinction from locale unresolved; verify the target behavior.", Some(c), true);
        }
        if spec.name == "getField" && !matches!(args.get("source"), Some(Value::String(_))) {
            self.emit(
                "CQL_DYNAMIC_FIELD",
                "The runtime field name is not statically grounded.",
                Some(c),
                true,
            );
        }
        args
    }
    fn correlation(
        &mut self,
        call: &Call,
        tables: &BTreeSet<String>,
        args: &BTreeMap<String, &Value>,
        mandatory: &[&Value],
        incoming: &PipelineState,
    ) {
        let aggregations = args
            .get("aggregation")
            .and_then(|v| {
                if let Value::Array(xs) = v {
                    Some(xs.as_slice())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        let names = mandatory
            .iter()
            .filter_map(|v| {
                if let Value::Query {
                    label: Some(name), ..
                } = v
                {
                    Some(name.as_str())
                } else {
                    None
                }
            })
            .collect::<BTreeSet<_>>();
        if names.is_empty() && args.get("query").is_none_or(|value| !has_parameter(value)) {
            self.emit(
                "CQL_CORRELATION_BINDINGS",
                "Correlation requires at least one mandatory named query.",
                Some(call),
                false,
            );
        }
        let mut unique = BTreeSet::new();
        let mut projections = BTreeMap::new();
        let mut has_link = false;
        let sequence = args.get("sequence").and_then(|v| v.boolean()) == Some(true);
        let unresolved_sequence = args
            .get("sequence")
            .is_some_and(|value| has_parameter(value));
        let within = args
            .get("within")
            .filter(|value| !has_parameter(value))
            .and_then(|v| v.text())
            .and_then(super::plan::duration_millis);
        if args.contains_key("sequenceBy") && !sequence && !unresolved_sequence {
            self.emit(
                "CQL_SEQUENCE_REQUIRED",
                "sequenceBy requires sequence=true.",
                Some(call),
                false,
            );
        }
        if args.contains_key("jitterTolerance") {
            if !sequence && !unresolved_sequence {
                self.emit(
                    "CQL_SEQUENCE_REQUIRED",
                    "jitterTolerance requires sequence=true.",
                    Some(call),
                    false,
                );
            }
            if let (Some(jitter), Some(within)) = (
                args.get("jitterTolerance")
                    .filter(|value| !has_parameter(value))
                    .and_then(|v| v.text())
                    .and_then(super::plan::duration_millis),
                within,
            ) && jitter > within
            {
                self.emit(
                    "CQL_JITTER_WINDOW",
                    "jitterTolerance must not exceed within.",
                    Some(call),
                    false,
                );
            }
        }
        if let Some(root) = args
            .get("root")
            .filter(|value| !has_parameter(value))
            .and_then(|v| v.text())
            && !names.contains(root)
        {
            self.emit(
                "CQL_CORRELATION_ROOT",
                "Root must name a mandatory query.",
                Some(call),
                false,
            );
        }
        if args
            .get("iterationLimit")
            .filter(|value| !has_parameter(value))
            .and_then(|v| v.unsigned_integer())
            .is_some_and(|n| n == 0 || n > 10)
        {
            self.emit(
                "CQL_ARGUMENT_RANGE",
                "iterationLimit must be in 1..=10.",
                Some(call),
                false,
            );
        }
        for (value, aggregation) in mandatory
            .iter()
            .map(|v| (*v, false))
            .chain(aggregations.iter().map(|v| (v, true)))
        {
            let Value::Query {
                label: Some(name),
                pipeline,
                attributes,
            } = value
            else {
                continue;
            };
            if !super::plan::query_label(name) || !unique.insert(name.as_str()) {
                self.emit(
                    "CQL_QUERY_LABEL",
                    "Mandatory and aggregation queries need unique ASCII labels.",
                    Some(call),
                    false,
                );
            }
            for attr in attributes {
                if has_parameter(&attr.value) {
                    self.value(&attr.value, &mut tables.clone());
                    continue;
                }
                match attr.name.as_deref() {
                    Some("include") if !aggregation => {
                        if !catalog::query_all_fields(&attr.value) && !argument_type(&attr.value,"A") { self.emit("CQL_QUERY_INCLUDE","Query include requires a field list or explicit *.",Some(call),false); }
                        if !catalog::query_all_fields(&attr.value) && catalog::fields(&attr.value).contains("*") { self.unsupported("CQL_UNSUPPORTED_INCLUDE_STAR","A quoted literal star include is not qualified; use explicit all-fields syntax.",Some(call),false); }
                    }
                    Some("after"|"before") if aggregation => {}
                    _ => self.unsupported("CQL_UNSUPPORTED_QUERY_ATTRIBUTE","This query attribute is outside its qualified mandatory/aggregation context.",Some(call),false),
                }
            }
            let mut anchors = BTreeMap::new();
            for attr in attributes.iter().filter(|a| {
                matches!(a.name.as_deref(), Some("after" | "before")) && !has_parameter(&a.value)
            }) {
                if let Some((reference, offset)) = time_anchor(&attr.value) {
                    if !names.contains(reference) {
                        self.emit(
                            "CQL_TIME_ANCHOR",
                            "Time anchors must name mandatory queries.",
                            Some(call),
                            false,
                        );
                    }
                    anchors.insert(
                        attr.name.as_deref().unwrap_or_default(),
                        (reference, offset),
                    );
                    if attr.value.text().is_some_and(|v| v.trim() != reference)
                        && args.get("sequenceBy").is_some_and(|v| {
                            catalog::fields(v)
                                .iter()
                                .any(|f| !matches!(f.as_str(), "@timestamp" | "@ingesttimestamp"))
                        })
                    {
                        self.emit(
                            "CQL_TIME_SEQUENCE_DIMENSION",
                            "Temporal offsets cannot use a custom sequenceBy dimension.",
                            Some(call),
                            false,
                        );
                    }
                } else {
                    self.unsupported(
                        "CQL_UNSUPPORTED_TIME_ANCHOR",
                        "Use a named anchor with an optional signed fixed duration.",
                        Some(call),
                        false,
                    );
                }
            }
            if anchors.len() == 1
                && within.is_some_and(|w| {
                    anchors
                        .values()
                        .any(|(_, offset)| offset.unsigned_abs() >= u128::from(w))
                })
            {
                self.emit(
                    "CQL_TIME_WINDOW",
                    "An open-interval offset must be smaller than within.",
                    Some(call),
                    false,
                );
            }
            if let (Some((after, a)), Some((before, b))) =
                (anchors.get("after"), anchors.get("before"))
            {
                let index = |label: &&str| {
                    mandatory
                        .iter()
                        .position(|v| matches!(v,Value::Query{label:Some(n),..}if n==label))
                        .unwrap_or_default()
                };
                let timestamp_order = sequence
                    && args
                        .get("jitterTolerance")
                        .is_none_or(|v| v.text().and_then(super::plan::duration_millis) == Some(0))
                    && args.get("sequenceBy").is_some_and(|v| match v {
                        Value::Array(xs) => xs.first().and_then(Value::text) == Some("@timestamp"),
                        _ => v.text() == Some("@timestamp"),
                    });
                if (after == before && a >= b)
                    || (timestamp_order && index(after) > index(before) && a >= b)
                {
                    self.emit(
                        "CQL_TIME_WINDOW",
                        "The documented window policy excludes this same-anchor zero/reversed interval or provably reversed timestamp order.",
                        Some(call),
                        false,
                    );
                } else if after != before {
                    self.emit("CQL_TIME_WINDOW_RUNTIME","Actual event timestamps determine the anchor interval; within does not bound a closed interval.",Some(call),true);
                }
            }
            let links = catalog::correlation_links(pipeline);
            has_link |= !links.is_empty();
            for (field, target, nested) in links {
                if nested {
                    self.unsupported("CQL_UNSUPPORTED_LINK_CONTEXT","Place links directly in the named query pipeline; conditional link graphs are unsupported.",Some(call),false);
                }
                if let Some((other, key)) = target.split_once('.') {
                    if !names.contains(other) || other == name || !super::ast::bare_field(key) {
                        self.emit("CQL_LINK_TARGET","Links need a different mandatory query and a qualified bare field reference.",Some(call),false);
                    }
                    if !super::ast::bare_field(field) {
                        self.unsupported(
                            "CQL_UNSUPPORTED_LINK_REFERENCE",
                            "Use a bare field identifier for the link source; this syntax is unsupported.",
                            Some(call),
                            false,
                        );
                    }
                } else {
                    self.emit(
                        "CQL_LINK_TARGET",
                        "Link target must include a mandatory query label and field.",
                        Some(call),
                        false,
                    );
                }
            }
            let bindings = std::mem::take(&mut self.bindings);
            let fields = std::mem::take(&mut self.fields);
            let mut state = incoming.clone();
            self.pipeline_state(
                pipeline,
                if aggregation {
                    QueryContext::Aggregation
                } else {
                    QueryContext::Mandatory
                },
                &mut tables.clone(),
                &mut state,
            );
            self.bindings = bindings;
            self.fields = fields;
            if aggregation && !bounded_single(pipeline, false) {
                self.unsupported("CQL_UNSUPPORTED_CORRELATION_ROWS","The aggregation pipeline must be statically bounded to at most one output row; use a qualified scalar aggregate or explicit head(1)/tail(1).",Some(call),false);
            }
            projections.insert(name.clone(), state.projected);
        }
        let queries = mandatory
            .iter()
            .map(|v| (*v, false))
            .chain(aggregations.iter().map(|v| (v, true)))
            .collect::<Vec<_>>();
        let (outputs, unknown) = catalog::correlation_outputs(args, &queries, &projections);
        self.correlation_columns
            .insert(call.start, unknown.is_empty().then_some(outputs));
        if mandatory.len() > 1
            && !has_link
            && args
                .get("globalConstraints")
                .is_none_or(|v| catalog::fields(v).is_empty())
        {
            self.emit("CQL_CORRELATION_UNLINKED","No equality links were supplied; review the intended Cartesian constellation pattern.",Some(call),true);
        }
        self.emit("CQL_CORRELATION_CAPABILITY","Correlation needs target capability, actual source timestamps and representative execution. Local acceptance does not establish temporal row behavior.",Some(call),true);
    }

    fn filter_query(&mut self, pipeline: &Pipeline, call: &Call, tables: &BTreeSet<String>) {
        if pipeline
            .steps
            .iter()
            .any(|step| match step.semantic().as_ref() {
                Step::Filter(_) => false,
                Step::Call(c) => catalog::resolve(c)
                    .is_some_and(|r| r.function.resolved_effect(&r.arguments) != Effect::Filter),
                _ => true,
            })
        {
            self.emit(
                "CQL_FILTER_QUERY_REQUIRED",
                "This argument requires a subquery containing filters only.",
                Some(call),
                false,
            );
        }
        self.enclosed_queries += 1;
        self.pipeline(pipeline, QueryContext::Continuing, &mut tables.clone());
        self.enclosed_queries -= 1;
    }
    fn inline(&mut self, value: &Value, tables: &mut BTreeSet<String>) {
        // Element pseudo-fields do not belong to the enclosing event's projection.
        let outer_fields = std::mem::take(&mut self.fields);
        let mut state = PipelineState {
            narrowed: self.narrowed,
            after_aggregate: self.after_aggregate,
            ..PipelineState::default()
        };
        self.enclosed_queries += 1;
        match value {
            Value::Query { pipeline, .. } => {
                self.nonaggregate(pipeline);
                self.pipeline_state(
                    pipeline,
                    QueryContext::Plain,
                    &mut tables.clone(),
                    &mut state,
                );
            }
            Value::Call(call) => {
                let pipeline = Pipeline {
                    advisories: vec![],
                    steps: vec![Step::Call(call.clone())],
                };
                self.nonaggregate(&pipeline);
                self.pipeline_state(
                    &pipeline,
                    QueryContext::Plain,
                    &mut tables.clone(),
                    &mut state,
                );
            }
            _ => {}
        }
        self.enclosed_queries -= 1;
        self.fields = outer_fields;
    }
    fn nonaggregate(&mut self, pipeline: &Pipeline) {
        for original_step in &pipeline.steps {
            let semantic_step = original_step.semantic();
            let step = semantic_step.as_ref();
            match step {
                Step::Call(call)
                | Step::Assign {
                    value: Value::Call(call),
                    ..
                } => {
                    if let Some(resolved) = catalog::resolve(call)
                        && matches!(
                            resolved.function.resolved_effect(&resolved.arguments),
                            Effect::Aggregate
                                | Effect::Source
                                | Effect::Definition
                                | Effect::Correlation
                        )
                    {
                        self.emit("CQL_INLINE_EFFECT", "An element pipeline cannot replace, aggregate or define an event stream.", Some(call), false);
                    }
                }
                Step::Case(branches) => {
                    for branch in branches {
                        self.nonaggregate(branch);
                    }
                }
                Step::Match { arms, .. } => {
                    for (_, branch) in arms {
                        self.nonaggregate(branch);
                    }
                }
                _ => {}
            }
        }
    }
    fn aggregates(&mut self, v: &Value, c: &Call, tables: &mut BTreeSet<String>) {
        match v {
            Value::Parameter(_) => self.value(v, tables),
            Value::Array(xs) => {
                for x in xs {
                    self.aggregates(x, c, tables)
                }
            }
            Value::Call(f)
                if catalog::function(&f.name).is_some_and(|s| s.effect == Effect::Aggregate) =>
            {
                self.function(f, tables, false, false, false);
            }
            Value::Call(f) if catalog::function(&f.name).is_none() => {
                self.function(f, tables, false, false, false);
            }
            Value::Query {
                label: None,
                pipeline,
                attributes,
            } if attributes.is_empty() => {
                let mut state = PipelineState {
                    aggregated: self.aggregate_input,
                    after_aggregate: self.after_aggregate,
                    ordered: self.ordered_input,
                    replaced: !self.stored_input,
                    narrowed: self.narrowed,
                    projected: self.projected_input.clone(),
                    ..PipelineState::default()
                };
                self.pipeline_state(
                    pipeline,
                    QueryContext::Subaggregate,
                    &mut tables.clone(),
                    &mut state,
                );
                if !pipeline_aggregates(pipeline, false) {
                    self.emit(
                        "CQL_AGGREGATE_REQUIRED",
                        "A composite aggregate pipeline must aggregate its input.",
                        Some(c),
                        false,
                    );
                }
            }
            _ => {
                self.emit(
                    "CQL_AGGREGATE_REQUIRED",
                    "This argument requires aggregate functions.",
                    Some(c),
                    false,
                );
                self.parameters(v, tables);
            }
        }
    }
    fn regex(&mut self, pattern: &str, flags: &str, c: Option<&Call>) {
        if c.is_none() {
            if flags.chars().any(|flag| !"gmidF".contains(flag)) {
                self.emit(
                    "CQL_REGEX_FLAGS",
                    "Regex literal flags must be g, m, i, d or F.",
                    c,
                    false,
                );
            }
        } else if flags.chars().any(|flag| !"imdF".contains(flag))
            || flags.chars().collect::<BTreeSet<_>>().len() != flags.len()
        {
            self.unsupported(
                "CQL_UNSUPPORTED_REGEX_FLAGS",
                "Function regex flags must be distinct i, m, d or F.",
                c,
                false,
            );
        }
        if flags.contains('i') && !pattern.is_ascii() {
            self.unsupported(
                "CQL_UNSUPPORTED_REGEX_UNICODE_CASE",
                "Unicode case folding is outside the supported regex subset.",
                c,
                false,
            );
        }
        let analysis = regex_analysis(pattern);
        if analysis.as_ref().is_ok_and(|info| info.case_insensitive) && !pattern.is_ascii() {
            self.unsupported(
                "CQL_UNSUPPORTED_REGEX_UNICODE_CASE",
                "Inline Unicode case folding is outside the supported regex subset.",
                c,
                false,
            );
        }
        if let Err((unsupported, msg)) = analysis {
            let emit = if unsupported {
                Self::unsupported
            } else {
                Self::emit
            };
            emit(
                self,
                if unsupported {
                    "CQL_UNSUPPORTED_REGEX"
                } else {
                    "CQL_REGEX_SYNTAX"
                },
                msg,
                c,
                false,
            );
        }
    }
}

fn inline_writes(value: &Value, output: &str, initial: bool) -> bool {
    match value {
        Value::Query { pipeline, .. } => pipeline_writes(pipeline, output, initial),
        Value::Call(call) => call_writes(call, output, initial),
        _ => false,
    }
}
fn pipeline_writes(pipeline: &Pipeline, output: &str, mut written: bool) -> bool {
    for original_step in &pipeline.steps {
        let semantic_step = original_step.semantic();
        let step = semantic_step.as_ref();
        match step {
            Step::Assign { field, value } => {
                if let Some(call) = catalog::aliased_call(field, value) {
                    written = call_writes(&call, output, written);
                } else if field == output {
                    written = true;
                }
            }
            Step::Call(call) => written = call_writes(call, output, written),
            Step::Case(branches) => {
                written = branches.iter().all(|p| pipeline_writes(p, output, written))
            }
            Step::Match { arms, .. } => {
                written = arms
                    .iter()
                    .all(|(_, p)| pipeline_writes(p, output, written))
            }
            Step::Filter(filter)
                if positive_regexes(filter)
                    .iter()
                    .any(|p| regex_captures(p).contains(output)) =>
            {
                written = true
            }
            _ => {}
        }
    }
    written
}

fn call_writes(call: &Call, output: &str, initial: bool) -> bool {
    let Some(resolved) = catalog::resolve(call) else {
        return initial;
    };
    let args = &resolved.arguments;
    match resolved.function.name {
        "splitString" if !args.contains_key("index") => initial,
        "eval" => initial || args.contains_key(output),
        "drop" => {
            initial
                && !args
                    .get("fields")
                    .is_some_and(|v| catalog::fields(v).contains(output))
        }
        "select" => {
            initial
                && args
                    .get("fields")
                    .is_some_and(|v| catalog::fields(v).contains(output))
        }
        "rename" => {
            let pairs = catalog::rename_bindings(args);
            pairs.iter().any(|(_, new)| *new == output)
                || initial && !pairs.iter().any(|(old, _)| *old == output)
        }
        "default" => {
            initial
                || args
                    .get("field")
                    .is_some_and(|v| catalog::fields(v).contains(output))
        }
        "regex" => {
            initial
                || args
                    .get("regex")
                    .and_then(|v| v.text())
                    .is_some_and(|p| regex_captures(p).contains(output))
        }
        _ => initial || resolved.function.default_output(args).as_deref() == Some(output),
    }
}

pub(crate) fn positive_regexes(filter: &Filter) -> Vec<&str> {
    match filter {
        Filter::Compare {
            op,
            value: Pattern::Regex { pattern, .. },
            ..
        } if op == "=" => vec![pattern],
        Filter::Text(Pattern::Regex { pattern, .. }) => vec![pattern],
        Filter::And(filters) => filters.iter().flat_map(positive_regexes).collect(),
        _ => vec![],
    }
}

pub fn regex_captures(pattern: &str) -> BTreeSet<String> {
    regex_analysis(pattern)
        .map(|info| info.captures)
        .unwrap_or_default()
}

// Check spelling before conversion: floating point can hide fractions or cross long bounds.
fn long_literal(value: &Value) -> Option<i64> {
    let (number, negative) = match value {
        Value::Number(number) => (number.as_str(), false),
        Value::Unary { op, value } if matches!(op.as_str(), "-" | "+") => {
            let Value::Number(number) = value.as_ref() else {
                return None;
            };
            (number.as_str(), op == "-")
        }
        _ => return None,
    };
    let (mantissa, exponent) = number.split_once(['e', 'E']).unwrap_or((number, "0"));
    let exponent: i64 = exponent.parse().ok()?;
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let digits = digits.trim_start_matches('0');
    if digits.is_empty() {
        return Some(0);
    }
    let scale = exponent.checked_sub(fraction.len().try_into().ok()?)?;
    let integer = if scale >= 0 {
        let zeros: usize = scale.try_into().ok()?;
        if digits.len().checked_add(zeros)? > 19 {
            return None;
        }
        format!("{digits}{}", "0".repeat(zeros))
    } else {
        let count: usize = scale.checked_neg()?.try_into().ok()?;
        let boundary = digits.len().checked_sub(count)?;
        if !digits[boundary..].bytes().all(|b| b == b'0') {
            return None;
        }
        digits[..boundary].to_string()
    };
    if negative {
        format!("-{integer}").parse().ok()
    } else {
        integer.parse().ok()
    }
}

fn signed_integer(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) | Value::String(number) => {
            let whole = if let Some((whole, fraction)) = number.split_once('.') {
                if fraction.is_empty() || !fraction.chars().all(|c| c == '0') {
                    return None;
                }
                whole
            } else {
                number
            };
            whole.parse().ok()
        }
        Value::Unary { op, value } if op == "-" => {
            if let Value::Number(number) = value.as_ref() {
                signed_integer(&Value::String(format!("-{number}")))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn time_anchor(value: &Value) -> Option<(&str, i128)> {
    let text = value.text()?.trim();
    let end = text
        .find(|c: char| c.is_whitespace() || c == '+' || c == '-')
        .unwrap_or(text.len());
    let name = &text[..end];
    if !super::plan::query_label(name) {
        return None;
    }
    let offset = text[end..].trim();
    Some((
        name,
        if offset.is_empty() {
            0
        } else {
            super::plan::signed_duration(offset)?
        },
    ))
}

fn contains_sequence(value: &Value) -> bool {
    match value {
        Value::Call(call) => {
            catalog::function(&call.name).is_some_and(|function| {
                matches!(
                    function.name,
                    "accumulate" | "neighbor" | "partition" | "slidingWindow" | "slidingTimeWindow"
                )
            }) || call.args.iter().any(|arg| contains_sequence(&arg.value))
        }
        Value::Array(values) => values.iter().any(contains_sequence),
        Value::Query { pipeline, .. } => {
            pipeline
                .steps
                .iter()
                .any(|step| match step.semantic().as_ref() {
                    Step::Call(call) => contains_sequence(&Value::Call(call.clone())),
                    Step::Assign { value, .. } => contains_sequence(value),
                    Step::Case(branches) => branches.iter().any(|pipeline| {
                        contains_sequence(&Value::Query {
                            label: None,
                            pipeline: pipeline.clone(),
                            attributes: vec![],
                        })
                    }),
                    Step::Match { arms, .. } => arms.iter().any(|(_, pipeline)| {
                        contains_sequence(&Value::Query {
                            label: None,
                            pipeline: pipeline.clone(),
                            attributes: vec![],
                        })
                    }),
                    _ => false,
                })
        }
        _ => false,
    }
}

fn pipeline_aggregates(pipeline: &Pipeline, mut aggregated: bool) -> bool {
    for original_step in &pipeline.steps {
        let semantic_step = original_step.semantic();
        let step = semantic_step.as_ref();
        match step {
            Step::Call(call) => {
                aggregated |=
                    catalog::function(&call.name).is_some_and(|f| f.effect == Effect::Aggregate)
            }
            Step::Assign { value, .. } => {
                aggregated |= catalog::assigned_aggregate(value).is_some()
            }
            Step::Case(branches) => {
                aggregated = branches.iter().all(|p| pipeline_aggregates(p, aggregated))
            }
            Step::Match { arms, .. } => {
                aggregated = arms.iter().all(|(_, p)| pipeline_aggregates(p, aggregated))
            }
            _ => {}
        }
    }
    aggregated
}

pub(crate) fn single_aggregate(value: &Value) -> bool {
    match value {
        Value::Call(call) => bounded_single(
            &Pipeline {
                advisories: vec![],
                steps: vec![Step::Call(call.clone())],
            },
            false,
        ),
        Value::Word(name)
            if catalog::function(name)
                .is_some_and(|f| f.effect == Effect::Aggregate && f.required.is_empty()) =>
        {
            bounded_single(
                &Pipeline {
                    advisories: vec![],
                    steps: vec![Step::Call(Call::new(name, vec![]))],
                },
                false,
            )
        }
        Value::Query { pipeline, .. } => bounded_single(pipeline, false),
        Value::Array(xs) => !xs.is_empty() && xs.iter().all(single_aggregate),
        _ => false,
    }
}

fn row_cap(call: &Call, default: Option<u64>) -> Option<u64> {
    match catalog::resolve(call)?.arguments.get("limit") {
        None => default,
        Some(v) if v.text() == Some("max") => Some(u64::MAX),
        Some(v) => v.unsigned_integer(),
    }
}

fn head_narrows_sort(previous: &Step, head: &Call) -> bool {
    let Step::Call(sort) = previous else {
        return false;
    };
    sort.name.eq_ignore_ascii_case("sort")
        && !matches!(
            (row_cap(sort, None), row_cap(head, head_default_limit())),
            (Some(sort), Some(head)) if sort <= head
        )
}

fn head_default_limit() -> Option<u64> {
    catalog::parameter_rules("head")
        .iter()
        .find(|rule| rule.name == "limit")?
        .default
        .as_deref()?
        .parse()
        .ok()
}

fn bounded_single(pipeline: &Pipeline, mut single: bool) -> bool {
    for original_step in &pipeline.steps {
        let semantic_step = original_step.semantic();
        let step = semantic_step.as_ref();
        match step {
            Step::Call(call) => {
                let Some(resolved) = catalog::resolve(call) else {
                    return false;
                };
                let args = &resolved.arguments;
                match resolved.function.name {
                    "count" | "sum" | "avg" | "min" | "max" | "selectFromMax" | "selectFromMin"
                    | "selectLast" | "array:union" | "array:intersection" | "callFunction"
                    | "counterAsRate" | "linReg" | "percentage" | "percentile" | "range"
                    | "stdDev" => single = true,
                    "collect" => {
                        single = args.get("multival").and_then(|v| v.boolean()) != Some(false)
                    }
                    "stats" | "array:reduceAll" => {
                        single = args.get("function").is_none_or(|v| single_aggregate(v))
                    }
                    "head" | "tail" | "sort" | "table" => {
                        single |= args.get("limit").and_then(|v| v.unsigned_integer()) == Some(1)
                    }
                    "join" => {
                        single &= args
                            .get("max")
                            .is_none_or(|v| v.unsigned_integer() == Some(1))
                    }
                    "match" => {
                        single &= args
                            .get("nrows")
                            .is_none_or(|v| v.unsigned_integer() == Some(1))
                    }
                    "split" => single = false,
                    "regex" if args.get("repeat").and_then(|v| v.boolean()) == Some(true) => {
                        single = false
                    }
                    _ if matches!(
                        resolved.function.resolved_effect(args),
                        Effect::Aggregate | Effect::Source | Effect::Correlation
                    ) =>
                    {
                        single = false
                    }
                    _ => {}
                }
            }
            Step::Assign { value, .. } if catalog::assigned_aggregate(value).is_some() => {
                single = single_aggregate(value);
            }
            Step::Case(branches) => single &= branches.iter().all(|p| bounded_single(p, single)),
            Step::Match { arms, .. } => {
                single &= arms.iter().all(|(_, p)| bounded_single(p, single))
            }
            _ => {}
        }
    }
    single
}

fn has_parameter(value: &Value) -> bool {
    match value {
        Value::Parameter(_) => true,
        Value::Array(values) => values.iter().any(has_parameter),
        Value::Call(call) => call.args.iter().any(|arg| has_parameter(&arg.value)),
        Value::Binary { left, right, .. } => has_parameter(left) || has_parameter(right),
        Value::Unary { value, .. } => has_parameter(value),
        _ => false,
    }
}

fn numeric_literal(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(number) => number.parse::<f64>().ok()?,
        Value::Unary { op, value } if matches!(op.as_str(), "+" | "-") => {
            numeric_literal(value)? * if op == "-" { -1.0 } else { 1.0 }
        }
        _ => return None,
    };
    number.is_finite().then_some(number)
}

fn compare_numeric_literal(value: &Value, bound: f64) -> Option<Ordering> {
    numeric_literal(value)?;
    if !bound.is_finite() {
        return None;
    }
    fn decimal(text: &str) -> (bool, Vec<u8>, i128) {
        let negative = text.starts_with('-');
        let text = text.trim_start_matches(['+', '-']);
        let (mantissa, exponent) = text.split_once(['e', 'E']).unwrap_or((text, "0"));
        // Exponents beyond this range already exceed every finite catalog bound.
        let exponent = exponent
            .parse::<i128>()
            .unwrap_or(if exponent.starts_with('-') {
                i128::MIN
            } else {
                i128::MAX
            });
        let fraction = mantissa
            .split_once('.')
            .map_or(0, |(_, fraction)| fraction.len());
        let digits = mantissa
            .bytes()
            .filter(|byte| *byte != b'.')
            .skip_while(|byte| *byte == b'0')
            .collect::<Vec<_>>();
        let order = exponent
            .saturating_sub(fraction as i128)
            .saturating_add(digits.len() as i128);
        (negative, digits, order)
    }
    let mut value = value;
    let mut negative = false;
    let text = loop {
        match value {
            Value::Number(text) => break text,
            Value::Unary { op, value: inner } if matches!(op.as_str(), "+" | "-") => {
                negative ^= op == "-";
                value = inner;
            }
            _ => return None,
        }
    };
    let (sign, left, left_order) = decimal(text);
    negative = (negative ^ sign) && !left.is_empty();
    let (sign, right, right_order) = decimal(&bound.to_string());
    let right_negative = sign && !right.is_empty();
    if negative != right_negative {
        return Some(if negative {
            Ordering::Less
        } else {
            Ordering::Greater
        });
    }
    let magnitude = match (left.is_empty(), right.is_empty()) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => left_order.cmp(&right_order).then_with(|| {
            let width = left.len().max(right.len());
            left.into_iter()
                .chain(std::iter::repeat(b'0'))
                .take(width)
                .cmp(right.into_iter().chain(std::iter::repeat(b'0')).take(width))
        }),
    };
    Some(if negative {
        magnitude.reverse()
    } else {
        magnitude
    })
}

enum IntervalValue {
    Absolute(i128),
    Relative(i128),
}

fn interval_value(value: &Value) -> Option<IntervalValue> {
    if let Some(epoch) = long_literal(value) {
        return Some(IntervalValue::Absolute(i128::from(epoch) * 1_000_000));
    }
    match value {
        Value::String(text) | Value::Word(text) => {
            if text == "now" {
                Some(IntervalValue::Relative(0))
            } else if let Ok(epoch) = text.parse::<i64>() {
                Some(IntervalValue::Absolute(i128::from(epoch) * 1_000_000))
            } else if let Some(duration) =
                super::plan::duration_millis(text).filter(|duration| *duration > 0)
            {
                Some(IntervalValue::Relative(-i128::from(duration) * 1_000_000))
            } else if let Some(duration) =
                super::plan::signed_duration(text).filter(|duration| *duration != 0)
            {
                Some(IntervalValue::Relative(duration * 1_000_000))
            } else {
                crate::environment::parse_time(text)
                    .ok()
                    .map(|time| IntervalValue::Absolute(time.unix_timestamp_nanos()))
            }
        }
        Value::Unary { op, value } if matches!(op.as_str(), "+" | "-") => {
            let Value::Word(text) = value.as_ref() else {
                return None;
            };
            let duration =
                i128::from(super::plan::duration_millis(text).filter(|duration| *duration > 0)?)
                    * 1_000_000;
            Some(IntervalValue::Relative(if op == "-" {
                -duration
            } else {
                duration
            }))
        }
        _ => None,
    }
}

fn finite_number_literals(value: &Value) -> bool {
    match value {
        Value::Number(_) => numeric_literal(value).is_some(),
        Value::Array(values) => values.iter().all(finite_number_literals),
        Value::Call(call) => call
            .args
            .iter()
            .all(|arg| finite_number_literals(&arg.value)),
        Value::Unary { value, .. } => finite_number_literals(value),
        Value::Binary { left, right, .. } => {
            finite_number_literals(left) && finite_number_literals(right)
        }
        _ => true,
    }
}

fn numeric_expression(value: &Value) -> bool {
    match value {
        Value::Number(_) => numeric_literal(value).is_some(),
        Value::Word(_) => true,
        Value::Call(_) => finite_number_literals(value),
        Value::String(s) => s.parse::<f64>().is_ok_and(f64::is_finite),
        Value::Unary { op, value } => matches!(op.as_str(), "+" | "-") && numeric_expression(value),
        Value::Binary { op, left, right } => {
            matches!(op.as_str(), "+" | "-" | "*" | "/" | "%")
                && numeric_expression(left)
                && numeric_expression(right)
        }
        _ => false,
    }
}

fn first_aggregate(name: &str, effect: Effect) -> bool {
    effect == Effect::Aggregate
        || matches!(
            name,
            "createEvents" | "readFile" | "readPersistedAggregation" | "correlate"
        )
}

fn argument_type(v: &Value, kind: &str) -> bool {
    if matches!(v, Value::Parameter(_))
        || matches!(
            v,
            Value::Call(_) | Value::Binary { .. } | Value::Unary { .. }
        ) && has_parameter(v)
    {
        return true;
    }
    match kind {
        "e" => !matches!(
            v,
            Value::Query { .. } | Value::Parameter(_) | Value::Array(_)
        ),
        "f" => matches!(v,Value::String(s)|Value::Word(s)if super::plan::checked_field(s).is_ok()),
        "F" => argument_type(v, "f") || argument_type(v, "A"),
        "A" => matches!(v,Value::Array(xs)if xs.iter().all(|x|argument_type(x,"f"))),
        "s" => {
            matches!(v, Value::String(_) | Value::Word(_) | Value::Number(_))
                || numeric_literal(v).is_some()
        }
        "S" => {
            argument_type(v, "s")
                || matches!(v,Value::Array(xs)if xs.iter().all(|x|argument_type(x,"s")))
        }
        "s[]" => {
            argument_type(v, "s")
                || matches!(v,Value::Array(xs)if xs.iter().all(|x|argument_type(x,"s") || matches!(x, Value::Bool(_)) || matches!(x, Value::Unary { op, value } if op == "-" && matches!(value.as_ref(), Value::Number(_)))))
        }
        "d" => v.text().is_some_and(super::plan::duration),
        "p" => argument_type(v, "e"),
        "e[]" => matches!(v, Value::Array(xs) if xs.iter().all(|x| argument_type(x, "e"))),
        "v" => {
            matches!(
                v,
                Value::String(_) | Value::Word(_) | Value::Number(_) | Value::Bool(_)
            ) || matches!(v,Value::Unary{op,value} if op=="-" && matches!(value.as_ref(),Value::Number(_)))
        }
        "P" => {
            argument_type(v, "f")
                || matches!(v,Value::Array(xs) if xs.iter().all(|x| matches!(x, Value::Parameter(_)) || matches!(x,Value::Array(pair) if pair.len()==2 && pair.iter().all(|x| argument_type(x,"f")))))
        }
        "j" => signed_integer(v).is_some(),
        "r" => argument_type(v, "s"),
        "b" => v.boolean().is_some(),
        "n" => matches!(v, Value::Number(_)) && numeric_literal(v).is_some(),
        "i" => v.unsigned_integer().is_some(),
        "l" => argument_type(v, "i") || matches!(v,Value::Word(s)|Value::String(s)if s=="max"),
        "a" => matches!(v, Value::Array(_)),
        "y" => {
            matches!(v,Value::String(s)|Value::Word(s) if s.strip_suffix("[]").is_some_and(super::ast::bare_field))
        }
        "z" => {
            matches!(v, Value::String(s) | Value::Word(s) if super::ast::bare_field(s.strip_suffix("[]").unwrap_or(s)))
        }
        "N" => {
            matches!(v, Value::Array(xs) if xs.iter().all(|x| matches!(x, Value::Parameter(_)) || numeric_literal(x).is_some()))
        }
        "B" => {
            matches!(v, Value::Array(xs) if xs.iter().all(|x| matches!(x, Value::Parameter(_)) || matches!(x, Value::Array(pair) if pair.len() == 2 && (matches!(pair[0], Value::Parameter(_)) || long_literal(&pair[0]).is_some_and(|n| (0..64).contains(&n))) && argument_type(&pair[1], "s"))))
        }
        "t" => {
            matches!(v, Value::String(_) | Value::Word(_) | Value::Number(_))
                || matches!(v, Value::Unary { op, value } if matches!(op.as_str(), "+" | "-") && matches!(value.as_ref(), Value::Number(_) | Value::Word(_)))
        }
        "R" => argument_type(v, "q") || argument_type(v, "s"),
        "u" => matches!(v, Value::Call(_)) || argument_type(v, "q"),
        "q" => matches!(v, Value::Query { label: None, attributes, .. } if attributes.is_empty()),
        "Q" => matches!(v, Value::Query { label: Some(_), .. }),
        "Q[]" => {
            matches!(v,Value::Array(xs) if xs.iter().all(|v|argument_type(v,"Q")))
        }
        "q[]" => matches!(v,Value::Array(xs)if xs.iter().all(|x|argument_type(x,"q"))),
        "g" => {
            matches!(v, Value::Call(_) | Value::Query { label: None, .. })
                || matches!(v, Value::Array(xs) if xs.iter().all(|x| argument_type(x, "g")))
        }
        _ => false,
    }
}
fn arg_fields(v: Option<&Value>) -> BTreeSet<String> {
    v.map(catalog::fields).unwrap_or_default()
}
fn costly_raw_pattern(pattern: &Pattern) -> bool {
    matches!(pattern, Pattern::Regex { .. })
        || matches!(pattern, Pattern::Text(text) if text.contains('*') && text.chars().any(|character| character != '*'))
}

fn raw_scan_field(name: &str, args: &BTreeMap<String, &Value>) -> bool {
    let fields = args.get("field").map(|value| catalog::fields(value));
    fields.is_none_or(|fields| {
        fields.contains("@rawstring")
            || fields.contains("*")
            || (name == "wildcard" && fields.is_empty())
    })
}

fn narrowing_call(call: &Call) -> bool {
    let Some(resolved) = catalog::resolve(call) else {
        return false;
    };
    if resolved.function.resolved_effect(&resolved.arguments) != Effect::Filter {
        return false;
    }
    let field = resolved
        .arguments
        .get("field")
        .map(|value| catalog::fields(value));
    if field.is_none_or(|fields| {
        fields.is_empty() || fields.contains("@rawstring") || fields.contains("*")
    }) {
        return false;
    }
    match resolved.function.name {
        "in" => resolved
            .arguments
            .get("values")
            .and_then(|v| v.string_array())
            .is_some_and(|values| !values.is_empty()),
        "regex" => resolved
            .arguments
            .get("regex")
            .and_then(|value| value.text())
            .is_some_and(|pattern| !matches!(pattern, "" | ".*" | ".+")),
        "wildcard" => resolved
            .arguments
            .get("pattern")
            .and_then(|value| value.text())
            .is_some_and(|pattern| pattern.chars().any(|character| character != '*')),
        _ => false,
    }
}

fn narrowing_filter(filter: &Filter) -> bool {
    match filter {
        Filter::Compare { field, op, value } => {
            field != "@rawstring"
                && matches!(op.as_str(), "=" | ">" | ">=" | "<" | "<=")
                && match value {
                    Pattern::Text(text) => text.chars().any(|character| character != '*'),
                    Pattern::Regex { pattern, .. } => !matches!(pattern.as_str(), "" | ".*" | ".+"),
                    _ => false,
                }
        }
        Filter::And(filters) => filters.iter().any(narrowing_filter),
        Filter::Or(filters) => !filters.is_empty() && filters.iter().all(narrowing_filter),
        Filter::Call(call) => narrowing_call(call),
        _ => false,
    }
}

/// Collects the compared fields; false when the filter holds anything but field comparisons.
fn compared_fields<'a>(filter: &'a Filter, fields: &mut Vec<&'a str>) -> bool {
    match filter {
        Filter::Compare { field, .. } => {
            fields.push(field);
            true
        }
        Filter::And(filters) | Filter::Or(filters) => {
            filters.iter().all(|filter| compared_fields(filter, fields))
        }
        Filter::Not(filter) => compared_fields(filter, fields),
        _ => false,
    }
}

fn joined_comparison(args: &BTreeMap<String, &Value>, steps: &[Step]) -> Option<String> {
    fn ordered(op: &str) -> bool {
        matches!(op, "<" | "<=" | ">" | ">=")
    }
    fn joined_word<'a>(value: &'a Value, joined: &BTreeSet<String>) -> Option<&'a String> {
        match value {
            Value::Word(field) => joined.contains(field).then_some(field),
            Value::Binary { left, right, .. } => {
                joined_word(left, joined).or_else(|| joined_word(right, joined))
            }
            Value::Unary { value, .. } => joined_word(value, joined),
            Value::Call(call) => call.args.iter().find_map(|a| joined_word(&a.value, joined)),
            Value::Array(values) => values.iter().find_map(|v| joined_word(v, joined)),
            _ => None,
        }
    }
    fn compared<'a>(value: &'a Value, joined: &BTreeSet<String>) -> Option<&'a String> {
        match value {
            Value::Binary { op, .. } if ordered(op) => joined_word(value, joined),
            Value::Binary { left, right, .. } => {
                compared(left, joined).or_else(|| compared(right, joined))
            }
            Value::Unary { value, .. } => compared(value, joined),
            _ => None,
        }
    }
    fn tested<'a>(call: &'a Call, joined: &BTreeSet<String>) -> Option<&'a String> {
        call.name
            .eq_ignore_ascii_case("test")
            .then(|| call.args.iter().find_map(|a| compared(&a.value, joined)))
            .flatten()
    }
    fn filtered<'a>(filter: &'a Filter, joined: &BTreeSet<String>) -> Option<&'a String> {
        match filter {
            Filter::Compare { field, op, .. } => {
                (ordered(op) && joined.contains(field)).then_some(field)
            }
            Filter::And(filters) | Filter::Or(filters) => {
                filters.iter().find_map(|f| filtered(f, joined))
            }
            Filter::Not(filter) => filtered(filter, joined),
            Filter::Call(call) => tested(call, joined),
            _ => None,
        }
    }
    fn assign(field: &str, value: &Value, joined: &mut BTreeSet<String>) {
        if joined_word(value, joined).is_some() {
            joined.insert(field.into());
        } else {
            joined.remove(field);
        }
    }
    fn included(function: &Value) -> BTreeSet<String> {
        match function {
            Value::Call(call) => catalog::resolve(call)
                .filter(|r| matches!(r.function.name, "selectFromMax" | "selectFromMin"))
                .and_then(|r| r.arguments.get("include").map(|v| catalog::fields(v)))
                .unwrap_or_default(),
            Value::Array(functions) => functions.iter().flat_map(included).collect(),
            _ => BTreeSet::new(),
        }
    }
    let keys = catalog::fields(args.get("key").or(args.get("field"))?);
    let one_row = match args.get("query") {
        Some(Value::Query { pipeline, .. }) => {
            let last = pipeline.steps.iter().rev().find(
                |step| !matches!(step, Step::Call(call) if call.name.eq_ignore_ascii_case("select")),
            );
            bounded_single(pipeline, false)
                || matches!(last, Some(Step::Call(call)) if catalog::resolve(call).is_some_and(|resolved| {
                    let groups = resolved.arguments.get("field").map(|f| catalog::fields(f));
                    resolved.function.name == "groupBy"
                        && groups.as_ref().is_some_and(|groups| {
                            let rest = &keys - groups;
                            groups.is_subset(&keys)
                                && match resolved.arguments.get("function") {
                                    None => rest.is_empty(),
                                    Some(f) => single_aggregate(f) && rest.is_subset(&included(f)),
                                }
                        })
                }))
        }
        _ => false,
    };
    if args.contains_key("max") || one_row {
        return None;
    }
    let mut joined = &catalog::fields(args.get("include")?) - &keys;
    for step in steps {
        let compared = match step {
            Step::Filter(filter) => filtered(filter, &joined),
            Step::Call(call) => tested(call, &joined),
            _ => None,
        };
        if let Some(field) = compared {
            return Some(field.clone());
        }
        match step {
            Step::Filter(filter) => {
                for pattern in positive_regexes(filter) {
                    for capture in regex_captures(pattern) {
                        joined.remove(&capture);
                    }
                }
            }
            Step::Call(call) if call.name.eq_ignore_ascii_case("test") => {}
            Step::Assign { field, value } if !matches!(value, Value::Call(_)) => {
                assign(field, value, &mut joined)
            }
            Step::Call(call) => {
                let eval = catalog::resolve(call).filter(|r| r.function.name == "eval")?;
                for (field, value) in &eval.ordered {
                    assign(field, value, &mut joined);
                }
            }
            _ => return None,
        }
    }
    None
}

/// Whether the query's leading filter, its filter stages before the first function call or other
/// stage, compares @timestamp by order with a number, on its own or joined by AND, in a query whose
/// answer depends on its search window only through its events: it has no subquery, and no
/// function sets, divides or reads that window. Moving such a bound to the request keeps the answer.
fn leading_fixed_time(pipeline: &Pipeline) -> bool {
    fn bounds(filter: &Filter) -> bool {
        match filter {
            Filter::Compare {
                field,
                op,
                value: Pattern::Text(text),
            } => {
                field == "@timestamp"
                    && matches!(op.as_str(), "<" | "<=" | ">" | ">=")
                    && text.parse::<serde_json::Number>().is_ok()
            }
            Filter::And(filters) => filters.iter().any(bounds),
            _ => false,
        }
    }
    fn windowed(value: &Value) -> bool {
        match value {
            Value::Query { .. } => true,
            Value::Call(call) => called(call),
            Value::Array(values) => values.iter().any(windowed),
            Value::Binary { left, right, .. } => windowed(left) || windowed(right),
            Value::Unary { value, .. } => windowed(value),
            _ => false,
        }
    }
    fn called(call: &Call) -> bool {
        ["setTimeInterval", "bucket", "timeChart", "start", "end"]
            .iter()
            .any(|name| call.name.eq_ignore_ascii_case(name))
            || call.args.iter().any(|arg| windowed(&arg.value))
    }
    fn uses_window(pipeline: &Pipeline) -> bool {
        pipeline.steps.iter().any(|step| match step {
            Step::Filter(filter) => {
                let mut calls = Vec::new();
                filter_calls(filter, &mut calls);
                calls.into_iter().any(called)
            }
            Step::Call(call) => called(call),
            Step::Assign { value, .. } => windowed(value),
            Step::Case(branches) => branches.iter().any(uses_window),
            Step::Match { arms, .. } => arms.iter().any(|(_, branch)| uses_window(branch)),
            Step::Link { .. } => false,
        })
    }
    !uses_window(pipeline)
        && pipeline
            .steps
            .iter()
            .map_while(|step| match step {
                Step::Filter(filter) if !filter.has_call() => Some(filter),
                _ => None,
            })
            .any(bounds)
}

fn filter_calls<'a>(filter: &'a Filter, calls: &mut Vec<&'a Call>) {
    match filter {
        Filter::Call(call) => calls.push(call),
        Filter::And(filters) | Filter::Or(filters) => {
            for filter in filters {
                filter_calls(filter, calls);
            }
        }
        Filter::Not(filter) => filter_calls(filter, calls),
        _ => {}
    }
}

fn unfielded_regex(filter: &Filter) -> bool {
    match filter {
        Filter::Text(Pattern::Regex { .. }) => true,
        Filter::And(filters) | Filter::Or(filters) => filters.iter().any(unfielded_regex),
        Filter::Not(filter) => unfielded_regex(filter),
        _ => false,
    }
}

fn case_insensitive_wildcard(call: &Call) -> bool {
    catalog::resolve(call).is_some_and(|resolved| {
        resolved.function.name == "wildcard"
            // Case can only matter to a pattern that holds a letter.
            && resolved
                .arguments
                .get("pattern")
                .and_then(|value| value.text())
                .is_some_and(|pattern| pattern.chars().any(char::is_alphabetic))
            && resolved
                .arguments
                .get("ignoreCase")
                .is_some_and(|value| match value {
                    Value::Bool(value) => *value,
                    Value::Word(text) | Value::String(text) => text == "true",
                    _ => false,
                })
    })
}

/// The part of a field name before its first member or element, as in `a` for `a[0].b`.
fn leading_name(field: &str) -> &str {
    &field[..field.find(['[', '.']).unwrap_or(field.len())]
}

fn named_values(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::Word(text) | Value::String(text) => {
            names.insert(leading_name(text).into());
        }
        Value::Array(values) => {
            for value in values {
                named_values(value, names);
            }
        }
        _ => {}
    }
}

fn has_non_raw_comparison(f: &Filter) -> bool {
    match f {
        Filter::Compare { field, .. } => !matches!(
            field.as_str(),
            "@rawstring" | "@timestamp" | "@timestamp.nanos"
        ),
        Filter::And(xs) | Filter::Or(xs) => xs.iter().any(has_non_raw_comparison),
        Filter::Not(x) => has_non_raw_comparison(x),
        _ => false,
    }
}

fn has_capture_marker(f: &Filter) -> bool {
    match f {
        Filter::Compare { value, .. } | Filter::Text(value) => {
            // shortcut: literal capture-like text is also refused; use regex parser metadata for broader OR support.
            matches!(value, Pattern::Regex { pattern, .. } if pattern.contains("(?<"))
        }
        Filter::And(xs) | Filter::Or(xs) => xs.iter().any(has_capture_marker),
        Filter::Not(x) => has_capture_marker(x),
        _ => false,
    }
}

#[derive(Debug)]
pub struct RegexAnalysis {
    pub captures: BTreeSet<String>,
    /// Nonempty captures on every successful path; backend materialization remains scoped evidence.
    pub guaranteed: BTreeSet<String>,
    pub case_insensitive: bool,
}
pub fn regex_syntax(pattern: &str) -> std::result::Result<(), (bool, String)> {
    regex_analysis(pattern).map(|_| ())
}

/// Parses the qualified syntax and capture paths without running a substitute regex engine.
pub fn regex_analysis(pattern: &str) -> std::result::Result<RegexAnalysis, (bool, String)> {
    enum Escape {
        Literal(char),
        Class,
        Anchor,
    }
    struct Facts {
        guaranteed: BTreeSet<String>,
        nullable: bool,
    }
    impl Facts {
        fn empty() -> Self {
            Self {
                guaranteed: BTreeSet::new(),
                nullable: true,
            }
        }
        fn alternative(previous: Option<Self>, current: Self) -> Self {
            if let Some(mut prior) = previous {
                prior
                    .guaranteed
                    .retain(|field| current.guaranteed.contains(field));
                prior.nullable |= current.nullable;
                prior
            } else {
                current
            }
        }
    }
    struct Rx {
        xs: Vec<char>,
        at: usize,
        depth: usize,
        names: BTreeSet<String>,
        case_insensitive: bool,
    }
    impl Rx {
        fn err<T>(&self, unknown: bool, s: &str) -> std::result::Result<T, (bool, String)> {
            Err((unknown, s.into()))
        }
        fn escape(&mut self) -> std::result::Result<Escape, (bool, String)> {
            let Some(c) = self.xs.get(self.at).copied() else {
                return self.err(false, "Trailing regex escape.");
            };
            self.at += 1;
            if "dDsSwW".contains(c) {
                return Ok(Escape::Class);
            }
            if "bB".contains(c) {
                return Ok(Escape::Anchor);
            }
            if "hHvVpPQEkK0123456789".contains(c) {
                return self.err(
                    true,
                    "This escape requires a broader, cloud-qualified regex profile.",
                );
            }
            if "\\.+*?()[]{}^$|/-".contains(c) {
                Ok(Escape::Literal(c))
            } else if "nrt".contains(c) {
                Ok(Escape::Literal(match c {
                    'n' => '\n',
                    'r' => '\r',
                    _ => '\t',
                }))
            } else if "Az".contains(c) {
                Ok(Escape::Anchor)
            } else {
                self.err(true, "Escape is outside the conservative regex profile.")
            }
        }
        fn class(&mut self) -> std::result::Result<(), (bool, String)> {
            if self.xs.get(self.at) == Some(&'^') {
                self.at += 1;
            }
            let start = self.at;
            let mut prev = None;
            loop {
                let Some(c) = self.xs.get(self.at).copied() else {
                    return self.err(false, "Unclosed regex character class.");
                };
                self.at += 1;
                if c == ']' && self.at > start + 1 {
                    return Ok(());
                }
                let current = if c == '\\' {
                    match self.escape()? {
                        Escape::Literal(c) => Some(c),
                        Escape::Class => None,
                        Escape::Anchor => {
                            return self.err(
                                true,
                                "Boundary escapes in character classes are outside the supported regex subset.",
                            );
                        }
                    }
                } else {
                    Some(c)
                };
                if c == '-' && prev.is_some() && self.xs.get(self.at).is_some_and(|c| *c != ']') {
                    let next = self.xs[self.at];
                    self.at += 1;
                    let end = if next == '\\' {
                        match self.escape()? {
                            Escape::Literal(c) => Some(c),
                            _ => {
                                return self.err(
                                    true,
                                    "Class range endpoints must be literal characters.",
                                );
                            }
                        }
                    } else {
                        Some(next)
                    };
                    if let (Some(start), Some(end)) = (prev, end)
                        && start > end
                    {
                        return self.err(false, "Reversed regex range.");
                    }
                    prev = None;
                } else {
                    prev = current;
                }
            }
        }
        fn group(&mut self, inside: bool) -> std::result::Result<Facts, (bool, String)> {
            self.depth += 1;
            if self.depth > 32 {
                return self.err(true, "Regex nesting exceeds the profile budget.");
            }
            let mut sequence = Facts::empty();
            let mut alternatives = None;
            while let Some(ch) = self.xs.get(self.at).copied() {
                if ch == ')' {
                    if !inside {
                        return self.err(false, "Unmatched regex closing parenthesis.");
                    }
                    self.at += 1;
                    self.depth -= 1;
                    return Ok(Facts::alternative(alternatives, sequence));
                }
                self.at += 1;
                let mut atom = Facts {
                    guaranteed: BTreeSet::new(),
                    nullable: false,
                };
                let mut repeatable = true;
                match ch {
                    '(' => {
                        let mut capture = None;
                        if self.xs.get(self.at) == Some(&'?') {
                            self.at += 1;
                            match self.xs.get(self.at) {
                                Some(':') => self.at += 1,
                                Some('<') => {
                                    self.at += 1;
                                    let start = self.at;
                                    while self.xs.get(self.at).is_some_and(|ch| ch.is_ascii_alphanumeric() || *ch == '_') { self.at += 1; }
                                    if self.at == start || self.xs.get(self.at) != Some(&'>') { return self.err(true, "Unsupported named group or lookbehind."); }
                                    if !self.xs[start].is_ascii_alphabetic() && self.xs[start] != '_' { return self.err(true, "Capture names must start with a letter or underscore in this profile."); }
                                    let name = self.xs[start..self.at].iter().collect::<String>();
                                    if !self.names.insert(name.clone()) { return self.err(false, "Duplicate capture name."); }
                                    capture = Some(name);
                                    self.at += 1;
                                }
                                Some('i' | 'm' | 's' | '-') => {
                                    let mut flags = BTreeSet::new();
                                    let mut minus = false;
                                    let mut disabled = false;
                                    while let Some(flag) = self.xs.get(self.at).copied() {
                                        if flag == '-' {
                                            if minus { return self.err(false, "Repeated inline flag separator."); }
                                            minus = true; self.at += 1; continue;
                                        }
                                        if !"ims".contains(flag) { break; }
                                        if !flags.insert(flag) { return self.err(false, "Duplicate inline flag."); }
                                        if minus { disabled = true; }
                                        else if flag == 'i' { self.case_insensitive = true; }
                                        self.at += 1;
                                    }
                                    if flags.is_empty() || minus && !disabled { return self.err(false, "Inline flags require a modifier after a separator."); }
                                    if self.xs.get(self.at) == Some(&')') { self.at += 1; continue; }
                                    if self.xs.get(self.at) != Some(&':') { return self.err(true, "Unqualified inline modifier."); }
                                    self.at += 1;
                                }
                                _ => return self.err(true, "Lookarounds and engine-specific groups are outside the supported regex subset."),
                            }
                        }
                        atom = self.group(true)?;
                        if !atom.nullable
                            && let Some(name) = capture
                        {
                            atom.guaranteed.insert(name);
                        }
                    }
                    '[' => self.class()?,
                    '\\' => {
                        if matches!(self.escape()?, Escape::Anchor) {
                            repeatable = false;
                            atom.nullable = true;
                        }
                    }
                    '^' | '$' => {
                        repeatable = false;
                        atom.nullable = true;
                    }
                    '|' => {
                        alternatives = Some(Facts::alternative(alternatives, sequence));
                        sequence = Facts::empty();
                        continue;
                    }
                    '*' | '+' | '?' | '{' | '}' => {
                        return self.err(false, "Regex quantifier has no atom or is malformed.");
                    }
                    _ => {}
                }
                if let Some(next) = self.xs.get(self.at).copied()
                    && "*+?{".contains(next)
                {
                    if !repeatable {
                        return self.err(false, "Cannot quantify an anchor.");
                    }
                    self.at += 1;
                    let mut min = if matches!(next, '*' | '?') { 0 } else { 1 };
                    if next == '{' {
                        let start = self.at;
                        while self.xs.get(self.at).is_some_and(char::is_ascii_digit) {
                            self.at += 1;
                        }
                        min = self.xs[start..self.at]
                            .iter()
                            .collect::<String>()
                            .parse::<u32>()
                            .map_err(|_| (false, "Invalid repetition count.".into()))?;
                        let mut max = Some(min);
                        if self.xs.get(self.at) == Some(&',') {
                            self.at += 1;
                            let start = self.at;
                            while self.xs.get(self.at).is_some_and(char::is_ascii_digit) {
                                self.at += 1;
                            }
                            max = if self.at == start {
                                None
                            } else {
                                Some(
                                    self.xs[start..self.at]
                                        .iter()
                                        .collect::<String>()
                                        .parse::<u32>()
                                        .map_err(|_| (false, "Invalid repetition count.".into()))?,
                                )
                            };
                        }
                        if self.xs.get(self.at) != Some(&'}') {
                            return self.err(false, "Unclosed regex repetition.");
                        }
                        self.at += 1;
                        if min > 1000 || max.is_some_and(|n| n > 1000) {
                            return self
                                .err(true, "Repetition exceeds the conservative profile limit.");
                        }
                        if max.is_some_and(|n| n < min) {
                            return self.err(false, "Reversed repetition bounds.");
                        }
                    }
                    if min == 0 {
                        atom.guaranteed.clear();
                        atom.nullable = true;
                    }
                    if self.xs.get(self.at) == Some(&'?') {
                        self.at += 1;
                    }
                    if self.xs.get(self.at).is_some_and(|ch| "*+?{".contains(*ch)) {
                        return self.err(
                            true,
                            "Repeated/possessive quantifiers are outside this profile.",
                        );
                    }
                }
                sequence.guaranteed.extend(atom.guaranteed);
                sequence.nullable &= atom.nullable;
            }
            self.depth -= 1;
            if inside {
                self.err(false, "Unclosed regex group.")
            } else {
                Ok(Facts::alternative(alternatives, sequence))
            }
        }
    }
    let mut rx = Rx {
        xs: pattern.chars().collect(),
        at: 0,
        depth: 0,
        names: BTreeSet::new(),
        case_insensitive: false,
    };
    let facts = rx.group(false)?;
    Ok(RegexAnalysis {
        captures: rx.names,
        guaranteed: facts.guaranteed,
        case_insensitive: rx.case_insensitive,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(q: &str) -> Report {
        validate(&super::super::parser::parse(q).unwrap(), q)
    }
    #[test]
    fn decimal_literal_comparison_preserves_boundaries_and_signs() {
        for (text, bound, expected) in [
            ("100.0000000000000001", 100.0, Ordering::Greater),
            ("99.9999999999999999", 100.0, Ordering::Less),
            ("100.000", 100.0, Ordering::Equal),
            ("1e2", 100.0, Ordering::Equal),
            ("-0.1", 0.0, Ordering::Less),
            ("-100.0000000000000001", -100.0, Ordering::Less),
            ("-99.9999999999999999", -100.0, Ordering::Greater),
            ("-0.0", 0.0, Ordering::Equal),
            ("0.99999999999999999", 1.0, Ordering::Less),
            ("1e-400", 0.0, Ordering::Greater),
            ("1.0e-9223372036854775808", 0.0, Ordering::Greater),
            (
                "1e-999999999999999999999999999999999999999999",
                1.0,
                Ordering::Less,
            ),
            (
                "0e999999999999999999999999999999999999999999",
                0.0,
                Ordering::Equal,
            ),
        ] {
            let value = Value::Number(text.into());
            assert_eq!(
                compare_numeric_literal(&value, bound),
                Some(expected),
                "{text}"
            );
            let negative = Value::Unary {
                op: "-".into(),
                value: Box::new(value),
            };
            assert_eq!(
                compare_numeric_literal(&negative, -bound),
                Some(expected.reverse()),
                "-{text}"
            );
        }
        assert_eq!(
            compare_numeric_literal(&Value::Number("1e400".into()), 100.0),
            None
        );
    }
    #[test]
    fn signature_checks_are_not_parse_checks() {
        assert_eq!(check("count(foo=1)").status, Acceptance::Rejected);
        assert_eq!(check("counnt()").status, Acceptance::Unknown);
        assert_eq!(check("count(distinct=wat)").status, Acceptance::Rejected);
    }
    #[test]
    fn max_group_ceiling_is_informational() {
        let report = check("groupBy([host], limit=max)");
        assert_eq!(report.status, Acceptance::Accepted);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| { d.code == "CQL_GROUPBY_LIMIT" && d.severity == Severity::Info })
        );
    }
    #[test]
    fn source_and_synthetic_rules_are_structural() {
        let r = check("x=1 | createEvents([\"a=1\"]) | a=1");
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == "CQL_SOURCE_DISCARDS_INPUT")
        );
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == "CQL_SYNTHETIC_FIELD_BEFORE_PARSE")
        );
        assert!(
            !check("createEvents([\"a=1\"]) | kvParse() | a=1")
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_SYNTHETIC_FIELD_BEFORE_PARSE")
        );
    }
    #[test]
    fn regex_profiles_are_conservative() {
        assert!(regex_syntax("\\Atext\\*\\z").is_ok());
        assert!(regex_syntax("[z-a]").is_err());
        assert!(regex_syntax("(?=a)").unwrap_err().0);
        assert!(regex_syntax("\\bword").is_ok());
        assert!(regex_syntax("[abc").is_err());
    }
}
