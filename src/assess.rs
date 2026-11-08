//! Pure, post-execution checks of a saved MCP search result, not an execution attestation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{
    catalog::{self, ArgumentRole},
    contracts::{Context, Diagnostic, Severity},
    environment::{self, Observation, Pack},
    language::{
        ast::{Call, Filter, Pattern, Pipeline, Step, Value},
        parser,
    },
    mcp, sha256_hex,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AssessRequest {
    pub schema_version: u32,
    #[serde(default)]
    pub id: Option<String>,
    pub query: String,
    /// Saved MCP payload (not just its rows). Normalization hashes its JSON reserialization.
    pub result: JsonValue,
    #[serde(default)]
    pub context: Context,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum AssessOutcome {
    NoFindings,
    Advisory,
    Inconclusive,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CapInfo {
    pub stage: String,
    pub cap: usize,
    pub defaulted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CapKind {
    Intended,
    Ceiling,
    Silent,
    Unverified,
}

struct LimitingStage {
    info: CapInfo,
    kind: CapKind,
    final_stage: bool,
    series: bool,
    rest_row: bool,
    value_fields: BTreeSet<String>,
    values_observable: bool,
}

struct CapSpec {
    name: &'static str,
    default: usize,
    maximum: Option<usize>,
    numeric: CapKind,
    series: bool,
}

// Defaults: cql:function:groupBy, :top, :sort, :head, :tail, :table,
// :timeChart, :bucket, cql:function:collect (2,000 per group) and
// cql:function:join (100,000 subquery rows) in the bundled knowledge catalog.
const CAP_SPECS: &[CapSpec] = &[
    CapSpec {
        name: "groupBy",
        default: 20_000,
        maximum: Some(1_000_000),
        numeric: CapKind::Silent,
        series: false,
    },
    CapSpec {
        name: "top",
        default: 10,
        maximum: None,
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "sort",
        default: 200,
        maximum: None,
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "head",
        default: 200,
        maximum: Some(200_000),
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "tail",
        default: 200,
        maximum: Some(200_000),
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "table",
        default: 200,
        maximum: Some(200_000),
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "timeChart",
        default: 10,
        maximum: None,
        numeric: CapKind::Silent,
        series: true,
    },
    CapSpec {
        name: "bucket",
        default: 10,
        maximum: Some(500),
        numeric: CapKind::Silent,
        series: true,
    },
    CapSpec {
        name: "collect",
        default: 2_000,
        maximum: None,
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "join",
        default: 100_000,
        maximum: None,
        numeric: CapKind::Silent,
        series: false,
    },
];

fn cap_spec(name: &str) -> Option<&'static CapSpec> {
    CAP_SPECS.iter().find(|spec| spec.name == name)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct ColumnStats {
    /// Rows containing this key, including null and empty-string values.
    pub present: usize,
    pub null: usize,
    pub empty: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AssessResult {
    pub schema_version: u32,
    pub id: Option<String>,
    pub outcome: AssessOutcome,
    pub query_sha256: String,
    pub rows: usize,
    pub caps: Vec<CapInfo>,
    pub columns: BTreeMap<String, ColumnStats>,
    pub findings: Vec<Diagnostic>,
}

impl AssessResult {
    fn add(&mut self, effect: AssessOutcome, finding: Diagnostic) {
        self.outcome = match (self.outcome, effect) {
            (AssessOutcome::Error, _) | (_, AssessOutcome::Error) => AssessOutcome::Error,
            (AssessOutcome::Inconclusive, _) | (_, AssessOutcome::Inconclusive) => {
                AssessOutcome::Inconclusive
            }
            (AssessOutcome::Advisory, _) | (_, AssessOutcome::Advisory) => AssessOutcome::Advisory,
            _ => AssessOutcome::NoFindings,
        };
        self.findings.push(finding);
    }

    fn info(&mut self, code: &str, message: impl Into<String>) {
        let mut finding = Diagnostic::warning(code, message);
        finding.severity = Severity::Info;
        self.findings.push(finding);
    }
}

/// Inspect only the query, the supplied pack and the saved MCP payload; no tenant call is made.
pub fn assess(request: AssessRequest, pack: Option<&Pack>) -> AssessResult {
    let mut result = AssessResult {
        schema_version: 1,
        id: request.id,
        outcome: AssessOutcome::NoFindings,
        query_sha256: sha256_hex(request.query.as_bytes()),
        rows: 0,
        caps: vec![],
        columns: BTreeMap::new(),
        findings: vec![],
    };
    result.info(
        "RESULT_COMPLETENESS_UNATTESTED",
        "The MCP result carries no completeness attestation; these checks cannot certify the answer.",
    );
    if request.schema_version != 1 {
        result.add(
            AssessOutcome::Error,
            Diagnostic::error("REQUEST_VERSION", "Only schema_version=1 is supported."),
        );
        return result;
    }
    let pipeline = match parser::parse(&request.query) {
        Ok(pipeline) => pipeline,
        Err(failure) => {
            let mut diagnostic = Diagnostic::error(
                "RESULT_QUERY_PARSE",
                "Cannot assess the result against a query outside the supported parser.",
            );
            diagnostic.span = failure.diagnostic.span;
            result.add(AssessOutcome::Error, diagnostic);
            return result;
        }
    };
    let (caps, output_fields, count_fields, coverage_fields) = inspect_query(&pipeline);
    result.caps = caps
        .iter()
        .filter(|stage| stage.kind != CapKind::Unverified)
        .map(|stage| stage.info.clone())
        .collect();

    // serde_json::Value has already lost original key order and whitespace: normalize's
    // raw_sha256 describes these reserialized bytes, not the original MCP wire bytes.
    let raw = match serde_json::to_vec(&request.result) {
        Ok(raw) => raw,
        Err(_) => {
            result.add(
                AssessOutcome::Error,
                Diagnostic::error(
                    "RESULT_SERIALIZATION",
                    "Cannot serialize the saved MCP result.",
                ),
            );
            return result;
        }
    };
    let normalized = match mcp::normalize(&raw) {
        Ok(normalized) => normalized,
        Err(diagnostic) => {
            let (code, message) = match diagnostic.code.as_str() {
                "MCP_DEADLINE" => ("RESULT_TIMEOUT", "The MCP search tool timed out."),
                "MCP_TOOL_ERROR" => ("RESULT_ERROR", "The MCP search tool reported an error."),
                "MCP_INCOMPLETE" => (
                    "RESULT_INCOMPLETE",
                    "The MCP search tool reported incomplete results.",
                ),
                _ => (
                    "RESULT_SHAPE",
                    "The saved MCP result has an unsupported shape or exceeds the size limit.",
                ),
            };
            result.add(
                AssessOutcome::Inconclusive,
                Diagnostic::warning(code, message),
            );
            return result;
        }
    };
    if let Some(pack) = pack {
        if let Err(diagnostic) = pack.validate() {
            result.add(AssessOutcome::Error, diagnostic);
            return result;
        }
        if request
            .context
            .environment
            .as_ref()
            .is_some_and(|identity| identity != &pack.environment)
        {
            result.add(
                AssessOutcome::Error,
                Diagnostic::error(
                    "PACK_ENVIRONMENT",
                    "The supplied environment pack is for a different environment.",
                ),
            );
            return result;
        }
    }
    result.rows = normalized.rows.len();
    for cap in &caps {
        if cap.info.stage == "collect" {
            assess_collect_cap(cap, &normalized.rows, &mut result);
        } else if cap.info.stage == "join" {
            if cap.kind == CapKind::Ceiling {
                result.info(
                    "RESULT_UPSTREAM_CAP_UNVERIFIED",
                    "The requested join() subquery limit=max does not certify completeness; subquery row counts are not visible in the outer result.",
                );
            } else {
                result.add(
                    AssessOutcome::Inconclusive,
                    Diagnostic::warning(
                        "RESULT_UPSTREAM_CAP_UNVERIFIED",
                        format!(
                            "The {} join() subquery row cap of {} could discard matches; subquery row counts are not visible in the outer result.",
                            if cap.info.defaulted { "default" } else { "explicit" },
                            cap.info.cap
                        ),
                    ),
                );
            }
        } else if cap.kind == CapKind::Unverified {
            result.add(
                AssessOutcome::Inconclusive,
                Diagnostic::warning(
                    "RESULT_UPSTREAM_CAP_UNVERIFIED",
                    format!(
                        "The {} limit is not in the verified cap table; its effect on results is unknown.",
                        cap.info.stage
                    ),
                ),
            );
        } else if cap.series {
            if cap.kind == CapKind::Silent {
                result.add(
                    AssessOutcome::Inconclusive,
                    Diagnostic::warning(
                        "RESULT_UPSTREAM_CAP_UNVERIFIED",
                        format!(
                            "The {} series cap of {} could discard series; result row counts do not measure series.",
                            cap.info.stage, cap.info.cap
                        ),
                    ),
                );
            } else {
                result.info(
                    "RESULT_UPSTREAM_CAP_UNVERIFIED",
                    format!(
                        "The {} series ceiling of {} does not certify completeness.",
                        cap.info.stage, cap.info.cap
                    ),
                );
            }
        } else if cap.final_stage {
            if result.rows != cap.info.cap
                && !(cap.rest_row && result.rows == cap.info.cap.saturating_add(1))
            {
                continue;
            }
            if cap.kind == CapKind::Intended {
                result.info(
                    "RESULT_CAP_REACHED",
                    format!(
                        "{} rows reached the requested {} size; this does not attest completeness.",
                        result.rows, cap.info.stage
                    ),
                );
            } else {
                result.add(
                    AssessOutcome::Inconclusive,
                    Diagnostic::warning("RESULT_CAP_REACHED", cap_message(&cap.info, result.rows)),
                );
            }
        } else if cap.kind == CapKind::Silent {
            result.add(
                AssessOutcome::Inconclusive,
                Diagnostic::warning(
                    "RESULT_UPSTREAM_CAP_UNVERIFIED",
                    format!(
                        "An intermediate {} cap of {} could discard rows; intermediate row counts are unobservable.",
                        cap.info.stage, cap.info.cap
                    ),
                ),
            );
        } else {
            result.info(
                "RESULT_UPSTREAM_CAP_UNVERIFIED",
                format!(
                    "An intermediate {} cap of {} was requested; intermediate row counts are unobservable.",
                    cap.info.stage, cap.info.cap
                ),
            );
        }
    }
    if result.rows == 0 {
        if let Some(pack) = pack {
            check_coverage(&pipeline, pack, &coverage_fields, &mut result);
        }
        result.add(
            AssessOutcome::Inconclusive,
            Diagnostic::warning(
                "RESULT_EMPTY",
                "Zero rows are not evidence of absence through this path.",
            ),
        );
        return result;
    }

    if normalized.rows.iter().any(|row| {
        count_fields.iter().any(|field| {
            row.get(field).is_some_and(|value| {
                matches!(value, JsonValue::String(text) if text == "0")
                    || value.as_i64() == Some(0)
                    || value.as_u64() == Some(0)
            })
        })
    }) {
        result.add(
            AssessOutcome::Advisory,
            Diagnostic::warning(
                "RESULT_ZERO_COUNT",
                "A zero aggregate count is not evidence that the underlying data is absent.",
            ),
        );
    }
    if result.rows >= 10_000 {
        result.add(
            AssessOutcome::Inconclusive,
            Diagnostic::warning(
                "RESULT_ROW_LIMIT",
                "The MCP path may not return every row at 10,000 rows or more.",
            ),
        );
    }

    let columns = if output_fields.is_empty() {
        normalized
            .rows
            .iter()
            .flat_map(|row| row.keys().cloned())
            .collect::<BTreeSet<_>>()
    } else {
        output_fields
    };
    for column in columns {
        let mut stats = ColumnStats::default();
        for row in &normalized.rows {
            if let Some(value) = row.get(&column) {
                stats.present += 1;
                stats.null += usize::from(value.is_null());
                stats.empty += usize::from(value.as_str() == Some(""));
            }
        }
        let missing = result.rows - stats.present;
        if missing + stats.null + stats.empty > 0 {
            result.add(
                AssessOutcome::Advisory,
                Diagnostic::warning(
                    "RESULT_NULL_FIELDS",
                    format!(
                        "Output column '{column}' has missing {missing}/{rows}, null {null}/{rows}, empty {empty}/{rows} rows.",
                        rows = result.rows,
                        null = stats.null,
                        empty = stats.empty
                    ),
                ),
            );
        }
        result.columns.insert(column, stats);
    }
    if let Some(pack) = pack {
        check_coverage(&pipeline, pack, &coverage_fields, &mut result);
    }
    result
}

fn assess_collect_cap(
    cap: &LimitingStage,
    rows: &[serde_json::Map<String, JsonValue>],
    result: &mut AssessResult,
) {
    if !cap.values_observable || cap.value_fields.is_empty() {
        result.add(
            AssessOutcome::Inconclusive,
            Diagnostic::warning(
                "RESULT_UPSTREAM_CAP_UNVERIFIED",
                format!(
                    "The collect() per-group value cap of {} is unverifiable: the collected values cannot be counted in the outer result.",
                    cap.info.cap
                ),
            ),
        );
        return;
    }

    let mut reached = false;
    let mut uncountable = false;
    for row in rows {
        for field in &cap.value_fields {
            match row.get(field) {
                Some(JsonValue::Array(values)) => reached |= values.len() >= cap.info.cap,
                Some(JsonValue::String(value)) => {
                    let count = if value.is_empty() {
                        0
                    } else {
                        value.split('\n').count()
                    };
                    reached |= count >= cap.info.cap;
                }
                _ => uncountable = true,
            }
        }
    }
    if reached {
        let message = format!(
            "A collected value list reached the {} collect() per-group cap of {}; values beyond the cap may be missing.",
            if cap.info.defaulted {
                "default"
            } else {
                "requested"
            },
            cap.info.cap
        );
        if cap.kind == CapKind::Intended {
            result.info("RESULT_CAP_REACHED", message);
        } else {
            result.add(
                AssessOutcome::Inconclusive,
                Diagnostic::warning("RESULT_CAP_REACHED", message),
            );
        }
    }
    if uncountable || (cap.info.defaulted && !reached) {
        result.add(
            AssessOutcome::Inconclusive,
            Diagnostic::warning(
                "RESULT_UPSTREAM_CAP_UNVERIFIED",
                format!(
                    "The collect() per-group cap of {} cannot be verified from {}.",
                    cap.info.cap,
                    if uncountable {
                        "missing or non-array/non-string collected values in the outer result"
                    } else {
                        "the observed values below the default cap; upstream completeness is unattested"
                    }
                ),
            ),
        );
    }
}

fn inspect_query(
    pipeline: &Pipeline,
) -> (
    Vec<LimitingStage>,
    BTreeSet<String>,
    BTreeSet<String>,
    BTreeSet<String>,
) {
    let mut caps = Vec::new();
    collect_caps(pipeline, false, &mut caps);
    let mut output_fields = BTreeSet::new();
    let mut count_fields = BTreeSet::new();
    let mut coverage_fields = BTreeSet::new();
    for step in &pipeline.steps {
        match step {
            Step::Filter(filter) => filter_fields(filter, &mut coverage_fields),
            Step::Call(call) => {
                let Some(resolved) = catalog::resolve(call) else {
                    continue;
                };
                let name = resolved.function.name;
                if resolved.function.resolved_effect(&resolved.arguments) == catalog::Effect::Filter
                {
                    call_filter_fields(call, &mut coverage_fields);
                }
                if name == "groupBy"
                    && let Some(field) = resolved.arguments.get("field")
                {
                    coverage_fields.extend(catalog::fields(field));
                }
                if let Some((outputs, retains)) = catalog::aggregate_outputs(call) {
                    if !retains {
                        output_fields = outputs.clone();
                        count_fields.clear();
                    } else {
                        output_fields.extend(outputs.iter().cloned());
                    }
                    if name == "count" {
                        count_fields.extend(outputs);
                    } else if matches!(name, "stats" | "groupBy") {
                        if let Some(value) = resolved.arguments.get("function") {
                            let functions = match *value {
                                Value::Array(values) => values.as_slice(),
                                value => std::slice::from_ref(value),
                            };
                            for value in functions {
                                if let Value::Call(nested) = value
                                    && nested.name.eq_ignore_ascii_case("count")
                                    && let Some((outputs, _)) = catalog::aggregate_outputs(nested)
                                {
                                    count_fields.extend(outputs);
                                }
                            }
                        } else {
                            count_fields.insert("_count".into());
                        }
                    }
                } else if name == "select"
                    && let Some(fields) = resolved.arguments.get("fields")
                {
                    let selected = catalog::fields(fields);
                    output_fields = selected.clone();
                    count_fields.retain(|field| selected.contains(field));
                } else if name == "drop"
                    && let Some(fields) = resolved.arguments.get("fields")
                {
                    for field in catalog::fields(fields) {
                        output_fields.remove(&field);
                        count_fields.remove(&field);
                    }
                } else if name == "rename" {
                    for (old, new) in catalog::rename_bindings(&resolved.arguments) {
                        if output_fields.remove(old) {
                            output_fields.insert(new.into());
                        }
                        if count_fields.remove(old) {
                            count_fields.insert(new.into());
                        }
                    }
                }
            }
            Step::Assign { field, .. } => {
                output_fields.insert(field.clone());
                count_fields.remove(field);
            }
            _ => {}
        }
    }
    (caps, output_fields, count_fields, coverage_fields)
}

fn collect_caps(pipeline: &Pipeline, nested: bool, caps: &mut Vec<LimitingStage>) {
    for (index, step) in pipeline.steps.iter().enumerate() {
        match step {
            Step::Call(call) => {
                collect_call_caps(
                    call,
                    !nested && index + 1 == pipeline.steps.len(),
                    nested,
                    caps,
                );
            }
            Step::Filter(filter) => collect_filter_caps(filter, nested, caps),
            Step::Assign { value, .. } => collect_value_caps(value, nested, caps),
            Step::Case(branches) => {
                for branch in branches {
                    collect_caps(branch, true, caps);
                }
            }
            Step::Match { arms, .. } => {
                for (_, branch) in arms {
                    collect_caps(branch, true, caps);
                }
            }
            Step::Link { .. } => {}
        }
    }
}

fn collect_call_caps(call: &Call, final_stage: bool, nested: bool, caps: &mut Vec<LimitingStage>) {
    let resolved = catalog::resolve(call);
    if let Some(spec) = resolved.as_ref().and_then(|r| cap_spec(r.function.name)) {
        let limit = resolved
            .as_ref()
            .unwrap()
            .arguments
            .get("limit")
            .and_then(|v| v.text());
        let (cap, kind) = match limit {
            None => (Some(spec.default), CapKind::Silent),
            Some("max") => (spec.maximum, CapKind::Ceiling),
            Some(value) => (value.parse().ok(), spec.numeric),
        };
        if let Some(cap) = cap {
            caps.push(LimitingStage {
                info: CapInfo {
                    stage: spec.name.into(),
                    cap,
                    defaulted: limit.is_none(),
                },
                kind,
                final_stage,
                series: spec.series,
                rest_row: spec.name == "top"
                    && resolved
                        .as_ref()
                        .unwrap()
                        .arguments
                        .get("rest")
                        .is_some_and(|v| {
                            !matches!(v, Value::Bool(false)) && v.text() != Some("false")
                        }),
                value_fields: if spec.name == "collect" {
                    resolved
                        .as_ref()
                        .unwrap()
                        .arguments
                        .get("fields")
                        .map_or_else(BTreeSet::new, |value| catalog::fields(value))
                } else {
                    BTreeSet::new()
                },
                values_observable: !nested
                    && resolved.as_ref().is_none_or(|r| {
                        r.arguments
                            .get("multival")
                            .is_none_or(|v| v.text() != Some("false"))
                            && r.arguments
                                .get("separator")
                                .is_none_or(|v| v.text() == Some("\n"))
                    }),
            });
        }
    } else if call.args.iter().any(|arg| {
        arg.name.as_deref().is_some_and(|name| {
            name.to_ascii_lowercase().contains("limit") || matches!(name, "max" | "nrows")
        })
    }) && resolved
        .as_ref()
        .is_none_or(|r| r.function.effect == catalog::Effect::Aggregate)
    {
        caps.push(LimitingStage {
            info: CapInfo {
                stage: call.name.clone(),
                cap: 0,
                defaulted: false,
            },
            kind: CapKind::Unverified,
            final_stage,
            series: false,
            rest_row: false,
            value_fields: BTreeSet::new(),
            values_observable: false,
        });
    }
    for argument in &call.args {
        collect_value_caps(&argument.value, nested, caps);
    }
}

fn collect_filter_caps(filter: &Filter, nested: bool, caps: &mut Vec<LimitingStage>) {
    match filter {
        Filter::Call(call) => collect_call_caps(call, false, nested, caps),
        Filter::And(filters) | Filter::Or(filters) => {
            for filter in filters {
                collect_filter_caps(filter, nested, caps);
            }
        }
        Filter::Not(filter) => collect_filter_caps(filter, nested, caps),
        _ => {}
    }
}

fn collect_value_caps(value: &Value, nested: bool, caps: &mut Vec<LimitingStage>) {
    match value {
        Value::Query {
            pipeline,
            attributes,
            ..
        } => {
            collect_caps(pipeline, true, caps);
            for attribute in attributes {
                collect_value_caps(&attribute.value, true, caps);
            }
        }
        Value::Call(call) => collect_call_caps(call, false, nested, caps),
        Value::Array(values) => {
            for value in values {
                collect_value_caps(value, nested, caps);
            }
        }
        Value::Binary { left, right, .. } => {
            collect_value_caps(left, nested, caps);
            collect_value_caps(right, nested, caps);
        }
        Value::Unary { value, .. } => collect_value_caps(value, nested, caps),
        _ => {}
    }
}

fn filter_fields(filter: &Filter, fields: &mut BTreeSet<String>) {
    match filter {
        Filter::Compare { field, .. } => {
            if !field.starts_with('#') {
                fields.insert(field.clone());
            }
        }
        Filter::And(filters) | Filter::Or(filters) => {
            for filter in filters {
                filter_fields(filter, fields);
            }
        }
        Filter::Not(filter) => filter_fields(filter, fields),
        Filter::Call(call) => call_filter_fields(call, fields),
        _ => {}
    }
}

fn call_filter_fields(call: &Call, fields: &mut BTreeSet<String>) {
    if let Some(resolved) = catalog::resolve(call) {
        for (name, value) in resolved.ordered {
            match resolved.function.argument_role(&name) {
                ArgumentRole::InputField => fields.extend(catalog::fields(value)),
                ArgumentRole::Expression => expression_fields(value, fields),
                _ => {}
            }
        }
    }
}

fn expression_fields(value: &Value, fields: &mut BTreeSet<String>) {
    match value {
        Value::Word(field) => {
            fields.insert(field.clone());
        }
        Value::Binary { left, right, .. } => {
            expression_fields(left, fields);
            expression_fields(right, fields);
        }
        Value::Unary { value, .. } => expression_fields(value, fields),
        Value::Array(values) => {
            for value in values {
                expression_fields(value, fields);
            }
        }
        Value::Call(call) => {
            if let Some(resolved) = catalog::resolve(call) {
                for (name, value) in resolved.ordered {
                    match resolved.function.argument_role(&name) {
                        ArgumentRole::InputField => fields.extend(catalog::fields(value)),
                        ArgumentRole::Expression => expression_fields(value, fields),
                        _ => {}
                    }
                }
            }
        }
        _ => {}
    }
}

fn cap_message(last: &CapInfo, rows: usize) -> String {
    let spec = cap_spec(&last.stage).expect("reported cap has a verified specification");
    let (default, max) = (spec.default, spec.maximum);
    let setting = if last.defaulted {
        "default"
    } else {
        "explicit"
    };
    let maximum = max.map_or_else(
        || format!(", {} has no limit=max in this profile", last.stage),
        |n| format!(", limit=max is {n} rows"),
    );
    let assurance = if max.is_some() {
        "even limit=max does not certify completeness."
    } else {
        "hitting the cap does not certify completeness."
    };
    format!(
        "{rows} rows reached the {setting} {} cap of {}; the platform default is {default} rows{maximum}; {assurance}",
        last.stage, last.cap
    )
}

fn check_coverage(
    pipeline: &Pipeline,
    pack: &Pack,
    fields: &BTreeSet<String>,
    result: &mut AssessResult,
) {
    let mut repo = None;
    let mut type_ = None;
    let mut uncertain = false;
    for step in &pipeline.steps {
        if let Step::Filter(filter) = step {
            source_selector(filter, &mut repo, &mut type_, &mut uncertain);
        }
    }
    if uncertain || (repo.is_none() && type_.is_none()) {
        return;
    }
    let sources = pack
        .data
        .sources
        .iter()
        .filter_map(|value| environment::source(value).ok())
        .filter(|source| {
            source.selectors.as_ref().is_some_and(|selectors| {
                repo.as_ref()
                    .is_none_or(|repo| selectors.repo.as_ref() == Some(repo))
                    && type_
                        .as_ref()
                        .is_none_or(|type_| selectors.type_.as_ref() == Some(type_))
            })
        })
        .collect::<Vec<_>>();
    if sources.len() != 1 {
        return;
    }
    let source_id = &sources[0].id;
    let pack_fields = pack
        .data
        .fields
        .iter()
        .filter_map(|value| environment::field(value).ok())
        .filter(|field| &field.source_id == source_id)
        .collect::<Vec<_>>();
    for key in fields {
        let (path, is_tag) = key
            .strip_prefix('#')
            .map_or((key.as_str(), false), |path| (path, true));
        let Some(field) = pack_fields
            .iter()
            .find(|field| field.path == path && field.is_tag.unwrap_or(false) == is_tag)
        else {
            continue;
        };
        let Some(rate) = field.coverage.as_ref().and_then(|c| c.rate) else {
            continue;
        };
        if rate >= 1.0 {
            continue;
        }
        let parent = path.rsplit_once('.').map(|(parent, _)| parent);
        let siblings = parent
            .map(|parent| {
                pack_fields
                    .iter()
                    .filter(|other| {
                        other.path != path
                            && other.is_tag.unwrap_or(false) == is_tag
                            && other.evidence_kind == "observed"
                            && other.coverage.as_ref().and_then(|c| c.observed) != Some(false)
                            && other.refresh_status.is_none()
                            && environment::observation(pack, &other.provenance_ids)
                                == Observation::Qualified
                            && other
                                .path
                                .rsplit_once('.')
                                .is_some_and(|(p, _)| p == parent)
                    })
                    .take(5)
                    .map(|other| {
                        if is_tag {
                            format!("#{}", other.path)
                        } else {
                            other.path.clone()
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        result.add(
            AssessOutcome::Advisory,
            Diagnostic::warning(
                "RESULT_KEY_COVERAGE",
                format!(
                    "Key '{key}' has observed pack coverage rate {rate} in this source; other observed fields under the same parent: {}. Check alternative actor paths.",
                    if siblings.is_empty() { "none".into() } else { siblings.join(", ") }
                ),
            ),
        );
    }
}

fn source_selector(
    filter: &Filter,
    repo: &mut Option<String>,
    type_: &mut Option<String>,
    uncertain: &mut bool,
) {
    match filter {
        Filter::Compare { field, op, value } if matches!(field.as_str(), "#repo" | "#type") => {
            let slot = if field == "#repo" { repo } else { type_ };
            if let Pattern::Text(value) = value
                && op == "="
                && !value.contains('*')
            {
                if slot.as_ref().is_some_and(|prior| prior != value) {
                    *uncertain = true;
                }
                *slot = Some(value.clone());
            } else {
                *uncertain = true;
            }
        }
        Filter::And(filters) => {
            for filter in filters {
                source_selector(filter, repo, type_, uncertain);
            }
        }
        Filter::Or(filters) if filters.iter().any(mentions_source) => *uncertain = true,
        Filter::Not(filter) if mentions_source(filter) => *uncertain = true,
        Filter::Call(_) if mentions_source(filter) => *uncertain = true,
        _ => {}
    }
}

fn mentions_source(filter: &Filter) -> bool {
    match filter {
        Filter::Compare { field, .. } => matches!(field.as_str(), "#repo" | "#type"),
        Filter::And(filters) | Filter::Or(filters) => filters.iter().any(mentions_source),
        Filter::Not(filter) => mentions_source(filter),
        Filter::Call(call) => catalog::resolve(call).is_some_and(|resolved| {
            resolved.arguments.get("field").is_some_and(|field| {
                catalog::fields(field)
                    .iter()
                    .any(|field| matches!(field.as_str(), "#repo" | "#type"))
            })
        }),
        _ => false,
    }
}
