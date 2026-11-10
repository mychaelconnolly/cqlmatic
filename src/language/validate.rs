use super::ast::*;
use crate::{
    catalog::{self, Effect},
    contracts::*,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug)]
pub struct Report {
    pub status: Acceptance,
    pub diagnostics: Vec<Diagnostic>,
    pub fields: BTreeSet<String>,
}

pub fn validate(pipeline: &Pipeline, query: &str) -> Report {
    let mut checker = Checker {
        query,
        diagnostics: vec![],
        fields: BTreeSet::new(),
        nodes: 0,
        bindings: vec![],
        nesting: 0,
        definitions: 0,
        joins: false,
        join_seen: false,
        correlation_seen: false,
        table_columns: BTreeMap::new(),
        correlation_columns: BTreeMap::new(),
        narrowed: false,
    };
    checker.pipeline(pipeline, QueryContext::Plain, &mut BTreeSet::new());
    if checker.definitions > 0 && checker.joins {
        checker.emit(
            "CQL_TABLE_JOIN_CONFLICT",
            "defineTable cannot be combined with join or selfJoinFilter.",
            None,
            false,
        );
    }
    if checker.join_seen && checker.correlation_seen {
        checker.emit(
            "CQL_CORRELATION_JOIN_CONFLICT",
            "correlate cannot be combined with join.",
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
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum QueryContext {
    Plain,
    Conditional,
    Continuing,
    Mandatory,
    Aggregation,
}

#[derive(Clone, Default)]
struct PipelineState {
    has_input: bool,
    aggregated: bool,
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
    nodes: usize,
    bindings: Vec<(String, bool)>,
    nesting: usize,
    definitions: usize,
    joins: bool,
    join_seen: bool,
    correlation_seen: bool,
    table_columns: BTreeMap<String, Option<BTreeSet<String>>>,
    correlation_columns: BTreeMap<usize, Option<BTreeSet<String>>>,
    narrowed: bool,
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
    }
    fn pipeline(&mut self, p: &Pipeline, context: QueryContext, tables: &mut BTreeSet<String>) {
        self.pipeline_state(
            p,
            context,
            tables,
            &mut PipelineState {
                narrowed: context == QueryContext::Continuing && self.narrowed,
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
            let mut branch = incoming.clone();
            self.pipeline_state(pipeline, context, &mut tables.clone(), &mut branch);
            outcomes.push(branch);
        }
        let mut merged = PipelineState::merge(&outcomes);
        merged.ordered &= incoming.ordered;
        merged
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
        let PipelineState {
            mut has_input,
            mut aggregated,
            mut synthetic,
            mut replaced,
            mut extracted,
            mut ordered,
            mut narrowed,
            mut projected,
        } = state.clone();
        let mut mandatory_link = false;
        for step in &p.steps {
            self.narrowed = narrowed;
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
            match step {
                Step::Filter(f) => {
                    let previous = std::mem::take(&mut self.fields);
                    self.filter(f, tables, false);
                    narrowed |= narrowing_filter(f);
                    let reads = self.fields.clone();
                    self.fields.extend(previous);
                    if let Some(available) = &mut projected {
                        for pattern in positive_regexes(f) {
                            available.extend(regex_captures(pattern));
                        }
                    }
                    if synthetic && !extracted && has_non_raw_comparison(f) {
                        self.emit("CQL_SYNTHETIC_FIELD_BEFORE_PARSE","createEvents emits raw strings. Parse before filtering extracted fields.",None,true);
                    }
                    if let Some(available) = &projected {
                        for f in reads
                            .iter()
                            .filter(|f| !available.contains(*f))
                            .cloned()
                            .collect::<Vec<_>>()
                        {
                            self.emit(
                                "CQL_DROPPED_FIELD",
                                format!(
                                    "'{f}' is not among the known projected/aggregate outputs."
                                ),
                                None,
                                true,
                            );
                        }
                    }
                }
                Step::Assign { field, value } => {
                    self.field(field);
                    self.output_field(field, None);
                    self.scalar_value(value, tables);
                    if let Some(xs) = &mut projected {
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
                            synthetic,
                            replaced,
                            extracted,
                            ordered,
                            narrowed,
                            projected: projected.clone(),
                        },
                    );
                    aggregated = merged.aggregated;
                    synthetic = merged.synthetic;
                    replaced = merged.replaced;
                    extracted = merged.extracted;
                    ordered = merged.ordered;
                    narrowed = merged.narrowed;
                    projected = merged.projected;
                }
                Step::Match { field, arms } => {
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
                    let merged = self.branches(
                        arms.iter().map(|(_, p)| p).collect(),
                        context,
                        tables,
                        PipelineState {
                            has_input,
                            aggregated,
                            synthetic,
                            replaced,
                            extracted,
                            ordered,
                            narrowed,
                            projected: projected.clone(),
                        },
                    );
                    aggregated = merged.aggregated;
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
                    if spec.effect == Effect::Definition && (self.nesting != 1 || has_input) {
                        self.emit("CQL_TABLE_PLACEMENT", "defineTable declarations must form a top-level preamble before other operations.", Some(c), false);
                    }
                    let previous = std::mem::take(&mut self.fields);
                    let args = self.function(c, tables, false, false);
                    let reads = self.fields.clone();
                    self.fields.extend(previous);
                    if let Some(available) = &projected {
                        for field in reads.iter().filter(|field| !available.contains(*field)) {
                            self.emit(
                                "CQL_DROPPED_FIELD",
                                format!(
                                    "'{field}' is not among the known projected/aggregate outputs."
                                ),
                                Some(c),
                                true,
                            );
                        }
                    }
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
                    if matches!(context, QueryContext::Mandatory | QueryContext::Aggregation)
                        && (matches!(
                            effect,
                            Effect::Source | Effect::Definition | Effect::Correlation
                        ) || context == QueryContext::Mandatory && effect == Effect::Aggregate)
                    {
                        self.emit(
                            "CQL_CORRELATION_BRANCH",
                            "Mandatory correlation queries cannot aggregate or replace the stream; aggregation queries cannot replace or nest correlation.",
                            Some(c),
                            false,
                        );
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
                        "definetable" => {
                            if let Some(n) = args.get("name").and_then(|v| v.text())
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
                                let names = args
                                    .get("file")
                                    .map(|v| catalog::fields(v))
                                    .unwrap_or_default();
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
                        ordered =
                            matches!(name.as_str(), "head" | "tail" | "sort" | "table" | "top");
                    }
                    if effect != Effect::Definition {
                        has_input = true;
                    }
                }
            }
            if !matches!(step,Step::Call(c) if catalog::function(&c.name).is_some_and(|f|f.effect==Effect::Definition))
            {
                has_input = true;
            }
        }
        self.nesting -= 1;
        self.narrowed = enclosing_narrowed;
        *state = PipelineState {
            has_input,
            aggregated,
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
                        "AND and OR cannot combine filter function calls; use successive pipeline steps for conjunctions.",
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
                    if resolved.function.effect == Effect::Filter {
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
                self.function(c, tables, false, negated);
            }
            Filter::Text(p) => self.pattern(p),
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
                self.function(c, tables, false, false);
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
    fn mutation(&mut self, field: &str, call: Option<&Call>) {
        if self
            .bindings
            .iter()
            .rev()
            .find(|(binding, _)| {
                field == binding
                    || field
                        .strip_prefix(binding)
                        .is_some_and(|tail| tail.starts_with('.') || tail.starts_with('['))
            })
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
                        && (resolved.arguments.get("repeat").copied() == Some(&Value::Bool(true))
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
                self.function(c, tables, true, false);
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
        for mut diagnostic in resolved.diagnostics {
            diagnostic.span = Some(Box::new(span(self.query, c.start, c.end)));
            diagnostic.reference = Some(spec.reference_id.into());
            self.diagnostics.push(diagnostic);
        }
        let args = resolved.arguments;
        if !self.narrowed
            && matches!(spec.name, "regex" | "wildcard")
            && raw_scan_field(spec.name, &args)
        {
            self.raw_scan_warning(Some(c));
        }
        let mut query_projection = None;
        if spec.name == "defineTable" {
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
        self.joins |= matches!(spec.name, "join" | "selfJoinFilter");
        self.join_seen |= spec.name == "join";
        self.correlation_seen |= spec.name == "correlate";
        if matches!(spec.name, "match" | "readFile")
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
        if matches!(spec.name, "join" | "match") {
            let other = if spec.name == "join" { "key" } else { "column" };
            if let Some(field) = args.get("field") {
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
            let glob = args.get("glob").copied() == Some(&Value::Bool(true))
                || args.get("mode").and_then(|v| v.text()) == Some("glob");
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
        if let Some((outputs, _)) = spec.named_outputs(&args) {
            for output in outputs {
                self.output_field(&output, Some(c));
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
                && value
                    .text()
                    .is_some_and(|v| v.parse::<u64>().is_ok_and(|n| n == 0 || n > *maximum))
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
                if let Some(value) = args.get(key).and_then(|v| v.text())
                    && !(super::plan::duration(value) || key == "span" && value == "auto")
                {
                    self.unsupported("CQL_UNSUPPORTED_BUCKET_SPAN", "Use auto for span or a positive fixed duration. Calendar, anchored and numeric minSpan forms are not qualified.", Some(c), false);
                }
            }
            if let Some(Value::Array(units)) = args.get("unit") {
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
            && let Some(value) = args.get("memlimit").and_then(|v| v.text())
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
        if spec.name == "formatTime" && !scalar && !args.contains_key("as") {
            self.emit("CQL_ARGUMENT_REQUIRED", "Standalone formatTime requires as; an assignment supplies its scalar output context.", Some(c), false);
        }
        if spec.name == "count" && args.get("distinct").copied() == Some(&Value::Bool(true)) {
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
            && args
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
            && let Some(output) = spec.default_output(&args)
        {
            self.output_field(&output, Some(c));
        }
        if spec.name == "default"
            && let Some(fields) = args.get("field")
        {
            for field in catalog::fields(fields) {
                self.output_field(&field, Some(c));
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
            if kind == "y" && value.text().is_some_and(|s| s.contains('`')) {
                self.unsupported(
                    "CQL_UNSUPPORTED_ARRAY_NAME",
                    "Quoted special array members are outside the supported syntax subset.",
                    Some(c),
                    false,
                );
                continue;
            }
            if !argument_type(value, kind) {
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
                );
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
                && spec.argument_role(name) != catalog::ArgumentRole::ForeignField
                && spec.name != "correlate"
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
                self.aggregates(value, c);
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
            } else {
                self.value(value, tables);
            }
        }
        if spec.name == "defineTable"
            && let Some(name) = args.get("name").and_then(|v| v.text())
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
            if let Some(binding) = args.get("var").and_then(|v| v.text())
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
                && let Some(v) = args.get(key)
                && v.text()
                    .is_some_and(|s| s != "max" && s.parse::<u64>().is_ok_and(|n| n == 0))
            {
                self.emit(
                    "CQL_ARGUMENT_RANGE",
                    format!("'{key}' must be positive."),
                    Some(c),
                    false,
                );
            }
        }
        for (key, value) in &args {
            let choices: &[&str] = match (spec.name, key.as_str()) {
                ("sort" | "table", "order") => &["asc", "desc"],
                ("sort" | "table", "type") => &["number", "string"],
                ("join", "mode") => &["inner", "left"],
                ("round", "how") => &["round", "ceil", "floor"],
                ("neighbor", "direction") => &["preceding", "succeeding"],
                ("formatTime", "unit") => &["auto", "milliseconds", "seconds"],
                ("match", "mode") => &["string", "glob", "cidr"],
                ("ioc:lookup", "type") => &["domain", "ip_address", "url"],
                ("ioc:lookup", "confidenceThreshold") => &["unverified", "low", "medium", "high"],
                ("ioc:lookup", "include") => &[
                    "indicator",
                    "labels",
                    "last_updated",
                    "malicious_confidence",
                    "published_date",
                    "type",
                ],
                ("base64Decode", "charset") => &["UTF-8", "UTF-16LE", "UTF-16BE", "ISO-8859-1"],
                ("kvParse", "onDuplicate") => &["keepFirst", "keepLast"],
                ("kvParse", "separatorPadding") => &["yes", "no", "unknown"],
                _ => &[],
            };
            if choices.is_empty() {
                continue;
            }
            for choice in match value {
                Value::Array(xs) => xs.iter().filter_map(Value::text).collect::<Vec<_>>(),
                _ => value.text().into_iter().collect(),
            } {
                if !choices.contains(&choice) {
                    self.emit(
                        "CQL_ARGUMENT_VALUE",
                        format!(
                            "'{key}' must be one of {} in this profile.",
                            choices.join(", ")
                        ),
                        Some(c),
                        false,
                    );
                }
            }
        }
        if spec.name == "kvParse"
            && args
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
                        && let (Some(old), Some(new)) = (pair[0].text(), pair[1].text())
                    {
                        inputs.insert(old);
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
                if inputs.intersection(&outputs).next().is_some() {
                    self.unsupported(
                        "CQL_UNSUPPORTED_RENAME_OVERLAP",
                        "Overlapping rename pairs need qualified simultaneous/ordered effects.",
                        Some(c),
                        false,
                    );
                }
            } else if !args.contains_key("as") {
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
        if names.is_empty() {
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
        let sequence = args.get("sequence").copied() == Some(&Value::Bool(true));
        let within = args
            .get("within")
            .and_then(|v| v.text())
            .and_then(super::plan::duration_millis);
        if args.contains_key("sequenceBy") && !sequence {
            self.emit(
                "CQL_SEQUENCE_REQUIRED",
                "sequenceBy requires sequence=true.",
                Some(call),
                false,
            );
        }
        if args.contains_key("jitterTolerance") {
            if !sequence {
                self.emit(
                    "CQL_SEQUENCE_REQUIRED",
                    "jitterTolerance requires sequence=true.",
                    Some(call),
                    false,
                );
            }
            if let (Some(jitter), Some(within)) = (
                args.get("jitterTolerance")
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
        if let Some(root) = args.get("root").and_then(|v| v.text())
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
            .and_then(|v| v.text())
            .and_then(|v| v.parse::<u64>().ok())
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
            for attr in attributes
                .iter()
                .filter(|a| matches!(a.name.as_deref(), Some("after" | "before")))
            {
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
        if pipeline.steps.iter().any(|step| match step {
            Step::Filter(_) => false,
            Step::Call(c) => catalog::resolve(c)
                .is_some_and(|r| r.function.resolved_effect(&r.arguments) != Effect::Filter),
            _ => true,
        }) {
            self.emit(
                "CQL_FILTER_QUERY_REQUIRED",
                "This argument requires a subquery containing filters only.",
                Some(call),
                false,
            );
        }
        self.pipeline(pipeline, QueryContext::Continuing, &mut tables.clone());
    }
    fn inline(&mut self, value: &Value, tables: &mut BTreeSet<String>) {
        // Element pseudo-fields do not belong to the enclosing event's projection.
        let outer_fields = std::mem::take(&mut self.fields);
        let mut state = PipelineState {
            narrowed: self.narrowed,
            ..PipelineState::default()
        };
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
        self.fields = outer_fields;
    }
    fn nonaggregate(&mut self, pipeline: &Pipeline) {
        for step in &pipeline.steps {
            match step {
                Step::Call(call) => {
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
    fn aggregates(&mut self, v: &Value, c: &Call) {
        match v {
            Value::Array(xs) => {
                for x in xs {
                    self.aggregates(x, c)
                }
            }
            Value::Call(f)
                if catalog::function(&f.name).is_some_and(|s| s.effect == Effect::Aggregate) => {}
            Value::Call(f) if catalog::function(&f.name).is_none() => {}
            _ => self.emit(
                "CQL_AGGREGATE_REQUIRED",
                "This argument requires aggregate functions.",
                Some(c),
                false,
            ),
        }
    }
    fn regex(&mut self, pattern: &str, flags: &str, c: Option<&Call>) {
        if flags.chars().any(|c| !"imd".contains(c))
            || flags.chars().collect::<BTreeSet<_>>().len() != flags.len()
        {
            self.unsupported("CQL_UNSUPPORTED_REGEX_FLAGS","Only distinct i, m, d suffix flags are covered; inline i, m, s modifiers are checked separately.",c,false);
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
    for step in &pipeline.steps {
        match step {
            Step::Assign { field, .. } if field == output => written = true,
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
        Value::Number(number) => number.parse().ok(),
        Value::Unary { op, value } if op == "-" => {
            if let Value::Number(number) = value.as_ref() {
                format!("-{number}").parse().ok()
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

fn bounded_single(pipeline: &Pipeline, mut single: bool) -> bool {
    fn aggregate(value: &Value) -> bool {
        match value {
            Value::Call(call) => bounded_single(
                &Pipeline {
                    steps: vec![Step::Call(call.clone())],
                },
                false,
            ),
            Value::Array(xs) => !xs.is_empty() && xs.iter().all(aggregate),
            _ => false,
        }
    }
    for step in &pipeline.steps {
        match step {
            Step::Call(call) => {
                let Some(resolved) = catalog::resolve(call) else {
                    return false;
                };
                let args = &resolved.arguments;
                match resolved.function.name {
                    "count" | "sum" | "avg" | "min" | "max" | "selectFromMax" | "selectFromMin"
                    | "selectLast" => single = true,
                    "collect" => {
                        single = args.get("multival").copied() != Some(&Value::Bool(false))
                    }
                    "stats" => single = args.get("function").is_none_or(|v| aggregate(v)),
                    "head" | "tail" | "sort" | "table" => {
                        single |= args
                            .get("limit")
                            .and_then(|v| v.text())
                            .and_then(|v| v.parse::<u64>().ok())
                            == Some(1)
                    }
                    "join" => single &= args.get("max").is_none_or(|v| v.text() == Some("1")),
                    "match" => single &= args.get("nrows").is_none_or(|v| v.text() == Some("1")),
                    "split" => single = false,
                    "regex" if args.get("repeat").copied() == Some(&Value::Bool(true)) => {
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
            Step::Case(branches) => single &= branches.iter().all(|p| bounded_single(p, single)),
            Step::Match { arms, .. } => {
                single &= arms.iter().all(|(_, p)| bounded_single(p, single))
            }
            _ => {}
        }
    }
    single
}

fn argument_type(v: &Value, kind: &str) -> bool {
    match kind {
        "e" => !matches!(
            v,
            Value::Query { .. } | Value::Parameter(_) | Value::Array(_)
        ),
        "f" => matches!(v,Value::String(s)|Value::Word(s)if super::plan::checked_field(s).is_ok()),
        "F" => argument_type(v, "f") || argument_type(v, "A"),
        "A" => matches!(v,Value::Array(xs)if xs.iter().all(|x|argument_type(x,"f"))),
        "s" => matches!(v, Value::String(_) | Value::Word(_)),
        "S" => {
            argument_type(v, "s")
                || matches!(v,Value::Array(xs)if xs.iter().all(|x|argument_type(x,"s")))
        }
        "s[]" => matches!(v,Value::Array(xs)if xs.iter().all(|x|argument_type(x,"s"))),
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
                || matches!(v,Value::Array(xs) if xs.iter().all(|x| matches!(x,Value::Array(pair) if pair.len()==2 && pair.iter().all(|x| argument_type(x,"f")))))
        }
        "j" => signed_integer(v).is_some(),
        "r" => matches!(v, Value::String(_)),
        "b" => matches!(v, Value::Bool(_)),
        "n" => matches!(v, Value::Number(_)),
        "i" => matches!(v,Value::Number(s)if s.parse::<u64>().is_ok()),
        "l" => argument_type(v, "i") || matches!(v,Value::Word(s)|Value::String(s)if s=="max"),
        "a" => matches!(v, Value::Array(_)),
        "y" => {
            matches!(v,Value::String(s)|Value::Word(s) if s.strip_suffix("[]").is_some_and(super::ast::bare_field))
        }
        "u" => matches!(v, Value::Call(_)) || argument_type(v, "q"),
        "q" => matches!(v, Value::Query { label: None, attributes, .. } if attributes.is_empty()),
        "Q" => matches!(v, Value::Query { label: Some(_), .. }),
        "Q[]" => {
            matches!(v,Value::Array(xs) if xs.iter().all(|v|argument_type(v,"Q")))
        }
        "q[]" => matches!(v,Value::Array(xs)if xs.iter().all(|x|argument_type(x,"q"))),
        "g" => matches!(v, Value::Call(_) | Value::Array(_)),
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
        "in" => {
            matches!(resolved.arguments.get("values"), Some(Value::Array(values)) if !values.is_empty())
        }
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
            let mut count = 0;
            let mut prev = None;
            loop {
                let Some(c) = self.xs.get(self.at).copied() else {
                    return self.err(false, "Unclosed regex character class.");
                };
                self.at += 1;
                if c == ']' {
                    if count == 0 {
                        return self.err(false, "Empty character class.");
                    }
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
                count += 1;
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
