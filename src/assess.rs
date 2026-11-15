//! Pure, post-execution checks of a saved MCP search result, not an execution attestation.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::{
    app,
    catalog::{self, ArgumentRole},
    contracts::{Context, Diagnostic, Severity},
    environment::{self, Observation, Pack},
    language::{
        ast::{Call, Filter, Pattern, Pipeline, Step, Value},
        parser, validate,
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
    Silent,
    Unverified,
}

/// What a final-stage cap counts in the returned rows.
#[derive(Clone)]
enum CapUnit {
    /// Each returned row counts toward the cap.
    Rows,
    /// A group can emit several rows; distinct tuples of these grouping keys count groups.
    Groups(BTreeSet<String>),
    /// A group can emit several rows and its keys cannot be matched reliably in the rows.
    Unknown,
}

/// Whether a top-level row limit still bounds the returned rows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RowBound {
    /// Not a top-level row limit.
    None,
    /// Every later stage is proven not to add rows.
    Holds,
    /// A later stage is not proven to keep the row count within the limit.
    Lost,
}

#[derive(Clone)]
struct LimitingStage {
    start: usize,
    info: CapInfo,
    kind: CapKind,
    /// Target setting that a `limit=max` sentinel resolves to; its effective value is unknown.
    setting: Option<&'static str>,
    final_stage: bool,
    series: bool,
    rest_row: bool,
    unit: CapUnit,
    row_bound: RowBound,
    value_fields: BTreeSet<String>,
    values_observable: bool,
}

struct CapSpec {
    name: &'static str,
    default: usize,
    /// Target setting used by `limit=max`; `None` where this profile accepts no `max` sentinel.
    max_setting: Option<&'static str>,
    numeric: CapKind,
    series: bool,
}

// Defaults: cql:function:groupBy, :top, :sort, :head, :tail, :table,
// :timeChart, :bucket, cql:function:collect (2,000 per group) and
// cql:function:join (100,000 subquery rows) in the bundled knowledge catalog.
// A `limit=max` sentinel resolves to a target setting (GroupMaxLimit or StateRowLimit)
// whose effective value is deployment configuration, so it is never a numeric cap here.
const CAP_SPECS: &[CapSpec] = &[
    CapSpec {
        name: "groupBy",
        default: 20_000,
        max_setting: Some("GroupMaxLimit"),
        numeric: CapKind::Silent,
        series: false,
    },
    CapSpec {
        name: "top",
        default: 10,
        max_setting: None,
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "sort",
        default: 200,
        max_setting: Some("StateRowLimit"),
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "head",
        default: 200,
        max_setting: Some("StateRowLimit"),
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "tail",
        default: 200,
        max_setting: Some("StateRowLimit"),
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "table",
        default: 200,
        max_setting: Some("StateRowLimit"),
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "timeChart",
        default: 10,
        max_setting: None,
        numeric: CapKind::Silent,
        series: true,
    },
    CapSpec {
        name: "bucket",
        default: 10,
        max_setting: None,
        numeric: CapKind::Silent,
        series: true,
    },
    CapSpec {
        name: "collect",
        default: 2_000,
        max_setting: None,
        numeric: CapKind::Intended,
        series: false,
    },
    CapSpec {
        name: "join",
        default: 100_000,
        max_setting: None,
        numeric: CapKind::Silent,
        series: false,
    },
];

/// Column statistics and per-column findings kept in one result; every column is still inspected.
const MAX_REPORTED_COLUMNS: usize = 1_000;

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
    let inspection = inspect_query(&pipeline, &request.query);
    let caps = &inspection.caps;
    let output_fields = &inspection.outputs;
    let count_fields = &inspection.counts;
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
    for issue in &inspection.issues {
        result.add(
            AssessOutcome::Inconclusive,
            Diagnostic::warning("RESULT_QUERY_COVERAGE", issue),
        );
    }
    for function in &inspection.dynamic {
        result.info(
            "RESULT_OUTPUT_COVERAGE",
            format!("{function}() writes dynamically named fields; output-column checks cover only statically known and observed columns."),
        );
    }
    for cap in caps {
        if cap.kind == CapKind::Unverified {
            result.add(
                AssessOutcome::Inconclusive,
                Diagnostic::warning("RESULT_UPSTREAM_CAP_UNVERIFIED", unverified_message(cap)),
            );
        } else if cap.info.stage == "collect" {
            assess_collect_cap(cap, &normalized.rows, &mut result);
        } else if cap.info.stage == "join" {
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
        } else if cap.series {
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
        } else if cap.final_stage {
            let counted = match &cap.unit {
                CapUnit::Rows => Some((result.rows, "rows")),
                CapUnit::Groups(keys) => {
                    distinct_groups(keys, &normalized.rows).map(|groups| (groups, "groups"))
                }
                CapUnit::Unknown => None,
            };
            let Some((count, unit)) = counted else {
                result.add(
                    AssessOutcome::Inconclusive,
                    Diagnostic::warning(
                        "RESULT_UPSTREAM_CAP_UNVERIFIED",
                        format!(
                            "The {} {} cap of {} cannot be checked from returned rows: a group can return several rows, and its grouping keys can be changed by the sub-aggregation or are not matched exactly in the rows.",
                            if cap.info.defaulted { "default" } else { "explicit" },
                            cap.info.stage,
                            cap.info.cap
                        ),
                    ),
                );
                continue;
            };
            if count > cap.info.cap.saturating_add(usize::from(cap.rest_row)) {
                result.add(
                    AssessOutcome::Inconclusive,
                    Diagnostic::warning(
                        "RESULT_CAP_EXCEEDED",
                        exceeded_message(cap, count, unit, ""),
                    ),
                );
                continue;
            }
            if count < cap.info.cap {
                continue;
            }
            if cap.kind == CapKind::Intended {
                result.info(
                    "RESULT_CAP_REACHED",
                    format!(
                        "{count} {unit} reached the requested {} size; this does not attest completeness.",
                        cap.info.stage
                    ),
                );
            } else {
                result.add(
                    AssessOutcome::Inconclusive,
                    Diagnostic::warning("RESULT_CAP_REACHED", cap_message(&cap.info, count, unit)),
                );
            }
        } else {
            let over = result.rows > cap.info.cap.saturating_add(usize::from(cap.rest_row));
            if over && cap.row_bound == RowBound::Holds {
                result.add(
                    AssessOutcome::Inconclusive,
                    Diagnostic::warning(
                        "RESULT_CAP_EXCEEDED",
                        exceeded_message(
                            cap,
                            result.rows,
                            "rows",
                            ", and later stages cannot add rows",
                        ),
                    ),
                );
            } else if over && cap.row_bound == RowBound::Lost && !cap.info.defaulted {
                // A later stage is not proven to keep rows within the limit, so the result cannot
                // be shown consistent with the query.
                result.add(
                    AssessOutcome::Inconclusive,
                    Diagnostic::warning(
                        "RESULT_UPSTREAM_CAP_UNVERIFIED",
                        format!(
                            "{} rows exceed the requested {} limit of {}{}, and a later stage is not proven to keep rows within it; the result's consistency with the supplied query is unverified.",
                            result.rows,
                            cap.info.stage,
                            cap.info.cap,
                            if cap.rest_row { " plus its rest row" } else { "" }
                        ),
                    ),
                );
            }
            if cap.kind == CapKind::Silent {
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
    }
    if result.rows == 0 {
        if let Some(pack) = pack {
            check_coverage(&inspection, pack, &mut result);
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

    let observed_columns = normalized
        .rows
        .iter()
        .flat_map(|row| row.keys().cloned())
        .collect::<BTreeSet<_>>();
    let columns = if output_fields.is_empty() {
        observed_columns
    } else {
        output_fields
            .iter()
            .cloned()
            .chain(
                observed_columns
                    .into_iter()
                    .filter(|field| inspection.conditional(field)),
            )
            .collect()
    };
    let (column_stats, _) = column_statistics(&normalized.rows, columns);
    let inspected = column_stats.len();
    let mut flagged = 0;
    for (column, stats) in column_stats {
        let missing = result.rows - stats.present;
        if missing + stats.null + stats.empty > 0 && !inspection.conditional(&column) {
            flagged += 1;
            if flagged <= MAX_REPORTED_COLUMNS {
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
        }
        if result.columns.len() < MAX_REPORTED_COLUMNS {
            result.columns.insert(column, stats);
        }
    }
    if inspected > MAX_REPORTED_COLUMNS {
        result.info(
            "RESULT_COLUMN_REPORT_LIMIT",
            format!(
                "Column statistics list the first {} of {inspected} inspected columns by name, and RESULT_NULL_FIELDS findings cover the first {} of {flagged} columns with missing, null or empty values; the outcome reflects every inspected column.",
                MAX_REPORTED_COLUMNS,
                flagged.min(MAX_REPORTED_COLUMNS)
            ),
        );
    }
    if let Some(pack) = pack {
        check_coverage(&inspection, pack, &mut result);
    }
    result
}

/// Counts the inspected columns in one pass over the row keys, returning the statistics and the
/// number of row-key lookups made. Scanning every row once per column would be quadratic when
/// rows carry distinct keys.
fn column_statistics(
    rows: &[serde_json::Map<String, JsonValue>],
    columns: BTreeSet<String>,
) -> (BTreeMap<String, ColumnStats>, usize) {
    let mut stats = columns
        .into_iter()
        .map(|column| (column, ColumnStats::default()))
        .collect::<BTreeMap<_, _>>();
    let mut lookups = 0;
    for row in rows {
        for (key, value) in row {
            lookups += 1;
            if let Some(stats) = stats.get_mut(key) {
                stats.present += 1;
                stats.null += usize::from(value.is_null());
                stats.empty += usize::from(value.as_str() == Some(""));
            }
        }
    }
    (stats, lookups)
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

#[derive(Clone, Default, PartialEq, Eq)]
struct SourceCoverage {
    repo: Option<String>,
    type_: Option<String>,
    uncertain: bool,
    fields: BTreeSet<String>,
}

#[derive(Clone, Default)]
struct Inspection {
    caps: Vec<LimitingStage>,
    outputs: BTreeSet<String>,
    optional: BTreeSet<String>,
    optional_prefixes: BTreeSet<String>,
    counts: BTreeSet<String>,
    source: SourceCoverage,
    dependencies: Vec<SourceCoverage>,
    issues: BTreeSet<String>,
    dynamic: BTreeSet<String>,
}

impl Inspection {
    fn conditional(&self, field: &str) -> bool {
        !self.outputs.contains(field)
            && (self
                .optional
                .iter()
                .any(|pattern| output_matches(pattern, field))
                || self
                    .optional_prefixes
                    .iter()
                    .any(|prefix| field.starts_with(prefix)))
    }

    fn write(&mut self, field: String, required: bool) {
        self.counts.retain(|count| !output_matches(&field, count));
        for cap in &mut self.caps {
            if cap
                .value_fields
                .iter()
                .any(|value| fields_overlap(&field, value))
            {
                cap.values_observable = false;
            }
        }
        if required {
            self.optional.remove(&field);
            self.outputs.insert(field);
        } else if !self.outputs.contains(&field) {
            self.optional.insert(field);
        }
    }

    fn dependency(&mut self, other: &Self) {
        for cap in &other.caps {
            if !self
                .caps
                .iter()
                .any(|prior| prior.start == cap.start && prior.info.stage == cap.info.stage)
            {
                let mut cap = cap.clone();
                cap.final_stage = false;
                cap.row_bound = RowBound::None;
                self.caps.push(cap);
            }
        }
        for source in std::iter::once(&other.source).chain(&other.dependencies) {
            if !source.fields.is_empty() && !self.dependencies.contains(source) {
                self.dependencies.push(source.clone());
            }
        }
        self.issues.extend(other.issues.iter().cloned());
        self.dynamic.extend(other.dynamic.iter().cloned());
    }

    fn project(&mut self, selected: BTreeSet<String>) {
        self.optional.retain(|field| selected.contains(field));
        self.optional_prefixes.clear();
        self.counts.retain(|field| selected.contains(field));
        for cap in &mut self.caps {
            // A collected field the projection does not keep exactly is gone for good.
            if cap
                .value_fields
                .iter()
                .any(|field| !selected.contains(field))
            {
                cap.values_observable = false;
            }
            cap.value_fields.retain(|field| selected.contains(field));
        }
        self.outputs = selected.difference(&self.optional).cloned().collect();
    }

    /// Collected lists overlapping a field this stage removes or renames away can no longer be
    /// counted, even if a later stage recreates the name.
    fn remove(&mut self, removed: &BTreeSet<String>) {
        for cap in &mut self.caps {
            if cap
                .value_fields
                .iter()
                .any(|value| removed.iter().any(|field| fields_overlap(field, value)))
            {
                cap.values_observable = false;
            }
        }
    }

    /// The top-level call starting at `start` bounds the returned rows from here on.
    fn start_row_bounds(&mut self, start: usize) {
        for cap in &mut self.caps {
            if cap.start == start
                && cap.kind != CapKind::Unverified
                && matches!(cap.unit, CapUnit::Rows)
                && matches!(
                    cap.info.stage.as_str(),
                    "head" | "tail" | "sort" | "table" | "top" | "groupBy"
                )
            {
                cap.row_bound = RowBound::Holds;
            }
        }
    }
}

fn output_matches(pattern: &str, field: &str) -> bool {
    if let Some((prefix, suffix)) = pattern.split_once("[]") {
        field.strip_prefix(prefix).is_some_and(|tail| {
            tail.strip_prefix('[')
                .and_then(|tail| tail.split_once(']'))
                .is_some_and(|(index, tail)| {
                    index.bytes().all(|b| b.is_ascii_digit()) && tail == suffix
                })
        })
    } else {
        pattern == field
    }
}

/// Whether two field names can refer to the same data. It reuses the structured-member and
/// array relations of query planning (`out` and `out.x`, `b[]` and `b[0]`), and adds the
/// conservative cases: indexed elements of one array (`b[0]` and `b[1]`) and any wildcard name.
fn fields_overlap(a: &str, b: &str) -> bool {
    fn root(name: &str) -> &str {
        name.split_once('[').map_or(name, |(root, _)| root)
    }
    a.contains('*')
        || b.contains('*')
        || app::path_contains(a, b)
        || app::path_contains(b, a)
        || app::array_covers(a, b)
        || app::array_covers(b, a)
        || ((a.contains('[') || b.contains('[')) && root(a) == root(b))
}

/// Every name among `names` that overlaps `field` is spelled exactly like it.
fn overlaps_only_exactly<'a>(names: impl IntoIterator<Item = &'a String>, field: &str) -> bool {
    names
        .into_iter()
        .all(|name| !fields_overlap(name, field) || name == field)
}

fn inspect_query(pipeline: &Pipeline, query: &str) -> Inspection {
    let mut inspection =
        inspect_pipeline(pipeline, Inspection::default(), &mut BTreeMap::new(), false);
    for diagnostic in validate::validate(pipeline, query).diagnostics {
        if diagnostic.severity == Severity::Error {
            inspection.issues.insert(format!(
                "{} identifies an unsupported or invalid query stage; result checks for that stage are incomplete.",
                diagnostic.code
            ));
        }
    }
    inspection
}

fn inspect_pipeline(
    pipeline: &Pipeline,
    mut state: Inspection,
    tables: &mut BTreeMap<String, Inspection>,
    nested: bool,
) -> Inspection {
    for (index, step) in pipeline.steps.iter().enumerate() {
        let keeps_bound = !nested && keeps_row_bound(step);
        for cap in &mut state.caps {
            cap.final_stage = false;
            if !keeps_bound && cap.row_bound == RowBound::Holds {
                cap.row_bound = RowBound::Lost;
            }
        }
        match step {
            Step::Filter(filter) => inspect_filter(filter, &mut state, tables, nested, false),
            Step::Assign { field, value } => {
                if let Some(call) = catalog::aliased_call(field, value) {
                    inspect_call(
                        &call,
                        &mut state,
                        tables,
                        nested,
                        index + 1 == pipeline.steps.len(),
                    );
                } else if let Some(call) = catalog::assigned_aggregate(value) {
                    inspect_call(
                        call,
                        &mut state,
                        tables,
                        nested,
                        index + 1 == pipeline.steps.len(),
                    );
                    let outputs = std::mem::take(&mut state.outputs);
                    let counted = outputs.iter().any(|output| state.counts.contains(output));
                    state.counts.clear();
                    state.optional.clear();
                    state.optional_prefixes.clear();
                    state.outputs.insert(field.clone());
                    if counted {
                        state.counts.insert(field.clone());
                    }
                    for cap in &mut state.caps {
                        if cap.start == call.start && !cap.value_fields.is_empty() {
                            cap.value_fields = BTreeSet::from([field.clone()]);
                        } else if cap
                            .value_fields
                            .iter()
                            .any(|value| fields_overlap(value, field))
                        {
                            // The assignment replaces an earlier collected list with a new value.
                            cap.values_observable = false;
                        }
                    }
                } else {
                    let mut value_result = Inspection::default();
                    inspect_value(value, &state, &mut value_result, tables, nested, false);
                    state.dependency(&value_result);
                    state.write(field.clone(), true);
                }
            }
            Step::Case(branches) => {
                let branches = branches
                    .iter()
                    .filter(|branch| !rejects_all(branch))
                    .map(|branch| {
                        inspect_pipeline(branch, state.clone(), &mut tables.clone(), true)
                    })
                    .collect();
                state = merge_inspections(state, branches);
            }
            Step::Match { arms, .. } => {
                let branches = arms
                    .iter()
                    .filter(|(_, branch)| !rejects_all(branch))
                    .map(|(pattern, branch)| {
                        let mut input = state.clone();
                        if let Pattern::Regex { pattern, .. } = pattern {
                            inspect_captures(pattern, true, &mut input);
                        }
                        if matches!(pattern, Pattern::Parameter(_)) {
                            input.issues.insert("A match pattern contains an unresolved parameter; branch coverage is unknown.".into());
                        }
                        inspect_pipeline(branch, input, &mut tables.clone(), true)
                    })
                    .collect();
                state = merge_inspections(state, branches);
            }
            Step::Call(call) => {
                inspect_call(
                    call,
                    &mut state,
                    tables,
                    nested,
                    index + 1 == pipeline.steps.len(),
                );
                if !nested {
                    state.start_row_bounds(call.start);
                }
            }
            Step::Link { .. } => {}
        }
        // An assigned call starts the same row bound as its call form.
        if let Step::Assign {
            value: Value::Call(call),
            ..
        } = step
            && !nested
        {
            state.start_row_bounds(call.start);
        }
    }
    state
}

/// Stages that cannot add rows: filters without function calls, projection (`select`, `drop`),
/// `rename`, the row selectors `head`, `tail` and `sort` (which pass events through), and
/// per-event assignments of call-free expressions. Every other stage leaves a bound unproven.
fn keeps_row_bound(step: &Step) -> bool {
    match step {
        Step::Filter(filter) => call_free_filter(filter),
        Step::Call(call) => plain_call(call, &["select", "drop", "rename", "head", "tail", "sort"]),
        // The assigned form of `rename` is the same rename.
        Step::Assign { field, value } => {
            !nests(value)
                || catalog::aliased_call(field, value)
                    .is_some_and(|call| plain_call(&call, &["rename"]))
        }
        _ => false,
    }
}

/// A filter without function calls; it can remove rows but never add them.
fn call_free_filter(filter: &Filter) -> bool {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => filters.iter().all(call_free_filter),
        Filter::Not(filter) => call_free_filter(filter),
        Filter::Call(_) => false,
        Filter::Compare { .. } | Filter::Text(_) | Filter::Bool(_) => true,
    }
}

/// A value containing a function call or a pipeline anywhere.
fn nests(value: &Value) -> bool {
    match value {
        Value::Call(_) | Value::Query { .. } => true,
        Value::Array(values) => values.iter().any(nests),
        Value::Binary { left, right, .. } => nests(left) || nests(right),
        Value::Unary { value, .. } => nests(value),
        _ => false,
    }
}

fn rejects_all(pipeline: &Pipeline) -> bool {
    for step in pipeline.steps.iter().rev() {
        if matches!(step, Step::Filter(Filter::Bool(false))) {
            return true;
        }
        if let Step::Call(call) = step
            && catalog::resolve(call).is_some_and(|resolved| {
                resolved.function.resolved_effect(&resolved.arguments) == catalog::Effect::Source
            })
        {
            break;
        }
    }
    false
}

fn merge_inspections(mut input: Inspection, branches: Vec<Inspection>) -> Inspection {
    let Some(first) = branches.first() else {
        return input;
    };
    input.outputs = first.outputs.clone();
    input.counts = first.counts.clone();
    input.optional.clear();
    input.optional_prefixes.clear();
    input.caps.clear();
    input.dependencies.clear();
    input.source = first.source.clone();
    for branch in &branches {
        input.outputs.retain(|field| branch.outputs.contains(field));
        input.counts.retain(|field| branch.counts.contains(field));
        input
            .optional
            .extend(branch.outputs.iter().chain(&branch.optional).cloned());
        input
            .optional_prefixes
            .extend(branch.optional_prefixes.iter().cloned());
        input.dependency(branch);
        if input.source.repo != branch.source.repo
            || input.source.type_ != branch.source.type_
            || input.source.uncertain != branch.source.uncertain
        {
            input.source = SourceCoverage {
                uncertain: true,
                ..SourceCoverage::default()
            };
        } else {
            input
                .source
                .fields
                .extend(branch.source.fields.iter().cloned());
        }
    }
    // A collected list stays countable only if every surviving branch still holds an observable
    // copy under the same names; a branch that replaced the source no longer holds it.
    for cap in &mut input.caps {
        if !branches.iter().all(|branch| {
            branch.caps.iter().any(|copy| {
                copy.start == cap.start
                    && copy.info.stage == cap.info.stage
                    && copy.values_observable
                    && copy.value_fields == cap.value_fields
            })
        }) {
            cap.values_observable = false;
        }
        // A row limit that held or was lost before the branches is at best lost after them; a
        // call-free OR restores a held bound itself.
        if branches.iter().flat_map(|branch| &branch.caps).any(|copy| {
            copy.start == cap.start
                && copy.info.stage == cap.info.stage
                && copy.row_bound != RowBound::None
        }) {
            cap.row_bound = RowBound::Lost;
        }
    }
    input
        .optional
        .retain(|field| !input.outputs.contains(field));
    input
}

fn inspect_call(
    call: &Call,
    state: &mut Inspection,
    tables: &mut BTreeMap<String, Inspection>,
    nested: bool,
    final_stage: bool,
) {
    let Some(resolved) = catalog::resolve(call) else {
        state.issues.insert(format!("{}() is outside the modeled function catalog; its output and stream effects are unknown.", call.name));
        state.outputs.clear();
        state.counts.clear();
        collect_call_caps(call, final_stage && !nested, nested, &mut state.caps);
        return;
    };
    let spec = resolved.function;
    let args = &resolved.arguments;
    let effect = spec.resolved_effect(args);
    let mut exported = None;
    if matches!(spec.name, "defineTable" | "remoteTable") {
        if let Some(query) = args.get("query") {
            let remote = spec.name == "remoteTable";
            let mut table = if let Value::Query { pipeline, .. } = query {
                let mut query_tables = if remote {
                    BTreeMap::new()
                } else {
                    tables.clone()
                };
                inspect_pipeline(pipeline, Inspection::default(), &mut query_tables, true)
            } else {
                Inspection::default()
            };
            if !resolved.diagnostics.is_empty()
                || call.args.iter().any(|arg| has_parameter(&arg.value))
            {
                table.issues.insert(format!("{}() has unresolved or unsupported arguments; checks for that declaration are incomplete.", spec.name));
            }
            if remote {
                table.source = SourceCoverage::default();
                table.dependencies.clear();
                table.caps.push(LimitingStage {
                    start: call.start,
                    info: CapInfo {
                        stage: "remoteTable".into(),
                        cap: 0,
                        defaulted: true,
                    },
                    kind: CapKind::Unverified,
                    setting: None,
                    final_stage: false,
                    series: false,
                    rest_row: false,
                    unit: CapUnit::Rows,
                    row_bound: RowBound::None,
                    value_fields: BTreeSet::new(),
                    values_observable: false,
                });
                if !matches!(query, Value::Query { .. })
                    && args
                        .get("include")
                        .is_none_or(|value| catalog::all_columns(value))
                {
                    table.optional_prefixes.insert(String::new());
                    table.dynamic.insert("remoteTable".into());
                }
            }
            if let Some(include) = args.get("include")
                && !catalog::all_columns(include)
            {
                table.project(catalog::fields(include));
            }
            if let Some(name) = args.get("name").and_then(|v| v.text()) {
                state.issues.extend(table.issues.iter().cloned());
                tables.insert(name.into(), table);
            }
        }
        return;
    }
    if effect == catalog::Effect::Source {
        if spec.name == "readFile" {
            let names = resolved.resource_names();
            let inputs = names
                .iter()
                .filter_map(|name| tables.get(name).cloned())
                .collect::<Vec<_>>();
            if !inputs.is_empty() {
                let mut merged = merge_inspections(Inspection::default(), inputs);
                if names.iter().any(|name| !tables.contains_key(name)) {
                    merged.optional.extend(std::mem::take(&mut merged.outputs));
                    merged.counts.clear();
                }
                exported = Some(merged);
            }
        }
        let prior_issues = std::mem::take(&mut state.issues);
        *state = exported.unwrap_or_default();
        state.issues.extend(prior_issues);
        for cap in &mut state.caps {
            cap.final_stage = false;
        }
        if let Some(include) = args.get("include") {
            state.project(catalog::fields(include));
        }
    }
    let earlier_caps = state.caps.len();
    collect_call_caps(call, final_stage && !nested, nested, &mut state.caps);
    if !resolved.diagnostics.is_empty() || call.args.iter().any(|arg| has_parameter(&arg.value)) {
        state.issues.insert(format!(
            "{}() has unresolved or unsupported arguments; checks for that stage are incomplete.",
            spec.name
        ));
        state.optional.extend(std::mem::take(&mut state.outputs));
        state.counts.clear();
        return;
    }
    if spec.name == "regex"
        && args
            .get("regex")
            .and_then(|v| v.text())
            .is_some_and(|pattern| validate::regex_analysis(pattern).is_err())
    {
        state.issues.insert("A regex is outside the modeled capture syntax; its output-column checks are incomplete.".into());
    }
    let incoming = state.clone();
    let mut nested_counts = BTreeSet::new();
    let mut nested_required = BTreeSet::new();
    let mut nested_optional = BTreeSet::new();
    // Each sub-aggregate separately, with its own lineage of the incoming collected lists.
    let mut children = Vec::new();
    for (name, value) in &resolved.ordered {
        let scope = spec.argument_scope(name);
        let aggregate = matches!(
            scope,
            catalog::ArgumentScope::Aggregate | catalog::ArgumentScope::ElementAggregate
        );
        let inherited = if matches!(
            scope,
            catalog::ArgumentScope::Aggregate
                | catalog::ArgumentScope::Continuing
                | catalog::ArgumentScope::FilterQuery
        ) {
            &incoming
        } else {
            &Inspection::default()
        };
        let values = match value {
            Value::Array(values) if aggregate => values.iter().collect(),
            value => vec![*value],
        };
        for value in values {
            // A non-retaining sub-aggregate is answerable only for the collected fields it
            // outputs; the parent checks every other collected field against its own outputs.
            let scoped;
            let inherited = match catalog::aggregate_value_outputs(value) {
                Some((outputs, false)) if scope == catalog::ArgumentScope::Aggregate => {
                    let mut lineage = inherited.clone();
                    for cap in &mut lineage.caps {
                        cap.value_fields.retain(|field| {
                            outputs.iter().any(|output| fields_overlap(output, field))
                        });
                    }
                    scoped = lineage;
                    &scoped
                }
                _ => inherited,
            };
            let mut value_result = Inspection::default();
            inspect_value(
                value,
                inherited,
                &mut value_result,
                tables,
                nested || scope != catalog::ArgumentScope::Aggregate,
                aggregate,
            );
            if aggregate {
                nested_counts.extend(value_result.counts.iter().cloned());
                nested_required.extend(value_result.outputs.iter().cloned());
                nested_optional.extend(value_result.optional.iter().cloned());
            }
            state.dependency(&value_result);
            if aggregate {
                children.push((value, value_result));
            }
        }
    }
    for value in &resolved.queries {
        let mut value_result = Inspection::default();
        inspect_value(
            value,
            &Inspection::default(),
            &mut value_result,
            tables,
            true,
            false,
        );
        state.dependency(&value_result);
    }
    if effect == catalog::Effect::Filter {
        call_filter_fields(call, &mut state.source.fields);
    }
    if spec.name == "groupBy"
        && let Some(field) = args.get("field")
    {
        state.source.fields.extend(catalog::fields(field));
    }
    if let Some((outputs, retains)) = catalog::aggregate_outputs(call) {
        let prior_counts = state.counts.clone();
        if !retains {
            // Earlier collected lists stay countable only under outputs proven to copy the value
            // unchanged: this call's own keys or selections, or, for a grouping or `stats`
            // sub-aggregate, a listed value selector whose own lineage still holds the list. Every
            // other output is computed, whatever its name.
            let aliased = resolved
                .ordered
                .iter()
                .filter(|(name, _)| spec.argument_role(name) == ArgumentRole::Output)
                .flat_map(|(_, value)| catalog::fields(value))
                .collect::<BTreeSet<_>>();
            let copied = copied_outputs(spec.name, args);
            // Names are compared by overlap: an output that can refer to the same data as the
            // collected field but is spelled differently (`b[]` for `b[0]`, `b` for `b[0]`,
            // a wildcard) is never its copy.
            let rewritten = |cap: &LimitingStage, field: &String| {
                // An aggregate that does not carry the field drops it; it stays gone even if a
                // later stage recreates the name.
                if !outputs.iter().any(|output| fields_overlap(output, field)) {
                    return true;
                }
                // This call's own unchanged copies cannot alter any data they overlap.
                if !overlaps_only_exactly(
                    outputs.iter().filter(|output| !copied.contains(*output)),
                    field,
                ) || aliased.iter().any(|alias| fields_overlap(alias, field))
                {
                    return true;
                }
                let mut producers = children
                    .iter()
                    .filter(|(_, child)| {
                        child
                            .outputs
                            .iter()
                            .chain(&child.optional)
                            .any(|output| fields_overlap(output, field))
                    })
                    .peekable();
                if producers.peek().is_none() {
                    return !copied.contains(field);
                }
                !(matches!(spec.name, "stats" | "groupBy" | "bucket" | "timeChart")
                    && producers.all(|(value, child)| {
                        selects_value(value, field)
                            && overlaps_only_exactly(
                                child.outputs.iter().chain(&child.optional),
                                field,
                            )
                            && child.caps.iter().any(|copy| {
                                copy.start == cap.start
                                    && copy.info.stage == cap.info.stage
                                    && copy.values_observable
                                    && copy.value_fields.contains(field)
                            })
                    }))
            };
            for cap in &mut state.caps[..earlier_caps] {
                if cap.value_fields.iter().any(|field| rewritten(cap, field)) {
                    cap.values_observable = false;
                }
            }
            state.outputs = outputs.clone();
            state.optional.clear();
            state.optional_prefixes.clear();
            state.counts.clear();
        } else {
            for field in &outputs {
                state.write(field.clone(), true);
            }
        }
        state.optional.extend(
            state
                .outputs
                .iter()
                .filter(|field| field.contains("[]"))
                .cloned(),
        );
        state.outputs.retain(|field| !field.contains("[]"));
        for field in nested_optional.difference(&nested_required) {
            if !retains || !incoming.outputs.contains(field) {
                state.outputs.remove(field);
                state.optional.insert(field.clone());
            }
        }
        if spec.name == "neighbor"
            && let Some(writes) = catalog::output_writes(call)
        {
            for field in writes.fields.difference(&writes.required) {
                if !incoming.outputs.contains(field) {
                    state.outputs.remove(field);
                    state.optional.insert(field.clone());
                }
            }
        }
        if spec.name == "appendAggregation" {
            state.optional.extend(std::mem::take(&mut state.outputs));
            state.outputs = incoming
                .outputs
                .intersection(&nested_required)
                .cloned()
                .collect();
            state
                .optional
                .retain(|field| !state.outputs.contains(field));
            state.optional_prefixes.insert(String::new());
            state.counts = incoming
                .counts
                .intersection(&nested_counts)
                .cloned()
                .collect();
        } else if spec.name == "count" {
            state.counts.extend(outputs);
        } else if matches!(
            spec.name,
            "table" | "selectLast" | "selectFromMax" | "selectFromMin"
        ) {
            state
                .counts
                .extend(prior_counts.intersection(&outputs).cloned());
        } else {
            state
                .counts
                .extend(nested_counts.intersection(&outputs).cloned());
            if !args.contains_key("function")
                && matches!(
                    spec.name,
                    "groupBy" | "stats" | "bucket" | "timeChart" | "session" | "window"
                )
            {
                state.counts.insert("_count".into());
            } else if spec.name == "top" && !args.contains_key("sum") && !args.contains_key("max") {
                state.counts.insert(
                    args.get("as")
                        .and_then(|v| v.text())
                        .unwrap_or("_count")
                        .into(),
                );
            }
        }
    } else if matches!(
        effect,
        catalog::Effect::Aggregate | catalog::Effect::Correlation
    ) {
        state.issues.insert(format!("{}() has no complete static output model; output and count checks for that stage are incomplete.", spec.name));
        state.outputs.clear();
        state.counts.clear();
    }
    if let Some(writes) = catalog::output_writes(call) {
        for field in writes.fields {
            let required = writes.required.contains(&field);
            state.write(field, required);
        }
        if let Some(prefix) = writes.dynamic_prefix {
            if !writes.preserves_existing {
                state.counts.retain(|field| !field.starts_with(&prefix));
                for cap in &mut state.caps {
                    if cap
                        .value_fields
                        .iter()
                        .any(|field| field.starts_with(&prefix))
                    {
                        cap.values_observable = false;
                    }
                }
            }
            state.optional_prefixes.insert(prefix);
            state.dynamic.insert(spec.name.into());
        }
    }
    // Fields this stage removes or renames away: the catalog's removal arguments (`drop`,
    // `array:drop`), an `array:rename` to a new name, and `lowercase` field renaming. `rename`,
    // projections and aggregates are handled where they change the tracked names.
    let mut removed = resolved
        .ordered
        .iter()
        .filter(|(name, _)| spec.argument_role(name) == ArgumentRole::Removal)
        .flat_map(|(_, value)| catalog::fields(value))
        .collect::<BTreeSet<_>>();
    if spec.name == "array:rename"
        && args.get("asArray").and_then(|v| v.text()) != args.get("array").and_then(|v| v.text())
        && let Some(array) = args.get("array")
    {
        removed.extend(catalog::fields(array));
    }
    if spec.name == "lowercase"
        && args
            .get("include")
            .and_then(|v| v.text())
            .is_some_and(|value| matches!(value, "fields" | "both"))
        && let Some(value) = args.get("field")
    {
        removed.extend(
            catalog::fields(value)
                .into_iter()
                .filter(|field| *field != field.to_lowercase()),
        );
    }
    state.remove(&removed);
    let table = args
        .get("file")
        .and_then(|value| value.text())
        .and_then(|name| tables.get(name));
    if spec.name != "readFile"
        && let Some(table) = table
    {
        state.dependency(table);
    }
    match spec.name {
        "select" => {
            if let Some(fields) = args.get("fields") {
                state.project(catalog::fields(fields));
            }
        }
        "drop" => {
            if let Some(fields) = args.get("fields") {
                for field in catalog::fields(fields) {
                    state.outputs.remove(&field);
                    state.optional.remove(&field);
                    state.counts.remove(&field);
                }
            }
        }
        "lowercase"
            if args
                .get("include")
                .and_then(|v| v.text())
                .is_some_and(|value| matches!(value, "fields" | "both")) =>
        {
            if let Some(value) = args.get("field") {
                for field in catalog::fields(value) {
                    if field != field.to_lowercase() {
                        state.outputs.remove(&field);
                        state.optional.insert(field);
                    }
                }
            }
        }
        "rename" => {
            for (old, new) in catalog::rename_bindings(args) {
                for cap in &mut state.caps {
                    let renamed = cap.value_fields.remove(old);
                    // Another collected name overlapping either side is moved or overwritten in a
                    // way this model does not track.
                    if cap.value_fields.iter().any(|value| {
                        fields_overlap(value, old) || (old != new && fields_overlap(value, new))
                    }) {
                        cap.values_observable = false;
                    }
                    if renamed {
                        cap.value_fields.insert(new.into());
                    }
                }
                let required = state.outputs.remove(old);
                let optional = state.optional.remove(old);
                let count = state.counts.remove(old);
                state.outputs.remove(new);
                state.optional.remove(new);
                state.counts.remove(new);
                if required {
                    state.outputs.insert(new.into());
                }
                if optional {
                    state.optional.insert(new.into());
                }
                if count {
                    state.counts.insert(new.into());
                }
            }
        }
        "join" | "match" => {
            let columns = table
                .filter(|table| table.optional_prefixes.is_empty())
                .map(|table| {
                    table
                        .outputs
                        .union(&table.optional)
                        .cloned()
                        .collect::<BTreeSet<_>>()
                });
            if let Some(fields) = catalog::imported_fields(spec.name, args, columns.as_ref()) {
                for field in fields {
                    state.write(field, false);
                }
            } else if spec.name == "match" {
                state.counts.clear();
                state.dynamic.insert(spec.name.into());
            }
        }
        _ => {}
    }
}

fn inspect_value(
    value: &Value,
    inherited: &Inspection,
    state: &mut Inspection,
    tables: &BTreeMap<String, Inspection>,
    nested: bool,
    aggregate: bool,
) {
    match value {
        Value::Query {
            pipeline,
            attributes,
            ..
        } => {
            let inspected =
                inspect_pipeline(pipeline, inherited.clone(), &mut tables.clone(), true);
            state.counts.extend(inspected.counts.iter().cloned());
            state.outputs.extend(inspected.outputs.iter().cloned());
            state.optional.extend(inspected.optional.iter().cloned());
            state
                .optional_prefixes
                .extend(inspected.optional_prefixes.iter().cloned());
            state.dependency(&inspected);
            for attribute in attributes {
                inspect_value(&attribute.value, inherited, state, tables, true, false);
            }
        }
        Value::Call(call) => {
            let mut inspected = inherited.clone();
            inspect_call(call, &mut inspected, &mut tables.clone(), nested, false);
            state.counts.extend(inspected.counts.iter().cloned());
            state.outputs.extend(inspected.outputs.iter().cloned());
            state.optional.extend(inspected.optional.iter().cloned());
            state
                .optional_prefixes
                .extend(inspected.optional_prefixes.iter().cloned());
            state.dependency(&inspected);
        }
        Value::Array(values) => {
            for value in values {
                inspect_value(value, inherited, state, tables, nested, aggregate);
            }
        }
        Value::Binary { left, right, .. } => {
            inspect_value(left, inherited, state, tables, nested, false);
            inspect_value(right, inherited, state, tables, nested, false);
        }
        Value::Unary { value, .. } => inspect_value(value, inherited, state, tables, nested, false),
        Value::Word(name) if aggregate && catalog::aggregate_value_outputs(value).is_some() => {
            inspect_value(
                &Value::Call(Call::new(name, vec![])),
                inherited,
                state,
                tables,
                nested,
                false,
            );
        }
        Value::Parameter(_) => {
            state.issues.insert(
                "An unresolved query parameter prevents complete checks for its stage.".into(),
            );
        }
        _ => {}
    }
}

fn has_parameter(value: &Value) -> bool {
    match value {
        Value::Parameter(_) => true,
        Value::Call(call) => call.args.iter().any(|arg| has_parameter(&arg.value)),
        Value::Array(values) => values.iter().any(has_parameter),
        Value::Binary { left, right, .. } => has_parameter(left) || has_parameter(right),
        Value::Unary { value, .. } => has_parameter(value),
        _ => false,
    }
}

fn inspect_captures(pattern: &str, strict: bool, state: &mut Inspection) {
    if let Ok(info) = validate::regex_analysis(pattern) {
        for field in info.captures {
            let required = strict && info.guaranteed.contains(&field);
            state.write(field, required);
        }
    } else {
        state.issues.insert("A regex is outside the modeled capture syntax; its output-column checks are incomplete.".into());
    }
}

fn inspect_filter(
    filter: &Filter,
    state: &mut Inspection,
    tables: &mut BTreeMap<String, Inspection>,
    nested: bool,
    negated: bool,
) {
    filter_fields(filter, &mut state.source.fields);
    source_selector(
        filter,
        &mut state.source.repo,
        &mut state.source.type_,
        &mut state.source.uncertain,
    );
    match filter {
        Filter::Call(call) => {
            if negated {
                let mut inspected = state.clone();
                inspect_call(call, &mut inspected, tables, nested, false);
                state.dependency(&inspected);
            } else {
                inspect_call(call, state, tables, nested, false);
            }
        }
        Filter::And(filters) => {
            for filter in filters {
                inspect_filter(filter, state, tables, nested, negated);
            }
        }
        Filter::Or(filters) => {
            let branches = filters
                .iter()
                .map(|filter| {
                    let mut branch = state.clone();
                    inspect_filter(filter, &mut branch, &mut tables.clone(), nested, negated);
                    branch
                })
                .collect::<Vec<_>>();
            // Call-free alternatives filter the same stream, so a row bound every alternative
            // keeps still bounds the merged result.
            let bounded = state
                .caps
                .iter()
                .filter(|cap| {
                    cap.row_bound == RowBound::Holds
                        && filters.iter().all(call_free_filter)
                        && branches.iter().all(|branch| {
                            branch.caps.iter().any(|copy| {
                                copy.start == cap.start
                                    && copy.info.stage == cap.info.stage
                                    && copy.info.cap == cap.info.cap
                                    && copy.row_bound == RowBound::Holds
                            })
                        })
                })
                .map(|cap| (cap.start, cap.info.stage.clone()))
                .collect::<Vec<_>>();
            *state = merge_inspections(state.clone(), branches);
            for cap in &mut state.caps {
                if bounded.contains(&(cap.start, cap.info.stage.clone())) {
                    cap.row_bound = RowBound::Holds;
                }
            }
        }
        Filter::Not(filter) => inspect_filter(filter, state, tables, nested, !negated),
        Filter::Compare {
            op,
            value: Pattern::Regex { pattern, .. },
            ..
        } => {
            if !negated && op == "=" {
                inspect_captures(pattern, true, state);
            }
        }
        Filter::Text(Pattern::Regex { pattern, .. }) if !negated => {
            inspect_captures(pattern, true, state)
        }
        Filter::Compare {
            value: Pattern::Parameter(_),
            ..
        }
        | Filter::Text(Pattern::Parameter(_)) => {
            state.issues.insert(
                "An unresolved filter parameter prevents complete checks for its stage.".into(),
            );
        }
        _ => {}
    }
}

fn collect_call_caps(call: &Call, final_stage: bool, nested: bool, caps: &mut Vec<LimitingStage>) {
    let resolved = catalog::resolve(call);
    if let Some(spec) = resolved.as_ref().and_then(|r| cap_spec(r.function.name)) {
        let limit = resolved.as_ref().unwrap().arguments.get("limit");
        let (cap, kind, setting) = match limit.map(|value| value.text()) {
            None => (spec.default, CapKind::Silent, None),
            Some(Some("max")) => (0, CapKind::Unverified, spec.max_setting),
            // A negative, parameterized or otherwise non-literal limit has no known value.
            Some(text) => text
                .and_then(|text| text.parse().ok())
                .map_or((0, CapKind::Unverified, None), |cap| {
                    (cap, spec.numeric, None)
                }),
        };
        caps.push(LimitingStage {
            start: call.start,
            info: CapInfo {
                stage: spec.name.into(),
                cap,
                defaulted: limit.is_none(),
            },
            kind,
            setting,
            final_stage,
            series: spec.series,
            rest_row: spec.name == "top"
                && resolved
                    .as_ref()
                    .unwrap()
                    .arguments
                    .get("rest")
                    .is_some_and(|v| !matches!(v, Value::Bool(false)) && v.text() != Some("false")),
            unit: if spec.name == "groupBy" {
                group_unit(&resolved.as_ref().unwrap().arguments)
            } else {
                CapUnit::Rows
            },
            row_bound: RowBound::None,
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
                    r.arguments.get("multival").is_none_or(|v| {
                        !matches!(v, Value::Bool(false)) && v.text() != Some("false")
                    }) && r
                        .arguments
                        .get("separator")
                        .is_none_or(|v| v.text() == Some("\n"))
                }),
        });
    } else if resolved
        .as_ref()
        .is_none_or(|r| r.function.effect == catalog::Effect::Aggregate)
    {
        let limiting = |name: &str| {
            name.to_ascii_lowercase().contains("limit") || matches!(name, "max" | "nrows")
        };
        let explicit = resolved.as_ref().map_or_else(
            || {
                call.args
                    .iter()
                    .any(|arg| arg.name.as_deref().is_some_and(limiting))
            },
            |r| r.arguments.keys().any(|name| limiting(name)),
        );
        // An omitted limit still applies the default documented in the bundled parameter data.
        let defaulted = !explicit
            && resolved.as_ref().is_some_and(|r| {
                catalog::parameter_rules(r.function.name)
                    .iter()
                    .any(|rule| rule.default.is_some() && limiting(&rule.name))
            });
        if explicit || defaulted {
            caps.push(LimitingStage {
                start: call.start,
                info: CapInfo {
                    stage: call.name.clone(),
                    cap: 0,
                    defaulted,
                },
                kind: CapKind::Unverified,
                setting: None,
                final_stage,
                series: false,
                rest_row: false,
                unit: CapUnit::Rows,
                row_bound: RowBound::None,
                value_fields: BTreeSet::new(),
                values_observable: false,
            });
        }
    }
}

fn unverified_message(cap: &LimitingStage) -> String {
    let stage = &cap.info.stage;
    if stage == "remoteTable" {
        "remoteTable() limits each table to 200 MB; final row counts do not reveal upstream byte volume or rule out truncation.".into()
    } else if let Some(setting) = cap.setting {
        format!(
            "{stage}(limit=max) uses the target's {setting}; its effective numeric cap is unknown, so returned rows cannot verify whether it was reached or certify completeness."
        )
    } else if cap_spec(stage).is_some() {
        format!(
            "The {stage} limit value has no verified numeric cap; its effect on results is unknown."
        )
    } else {
        format!(
            "The {}{stage} limit is not in the verified cap table; its effect on results is unknown.",
            if cap.info.defaulted { "default " } else { "" }
        )
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

/// Sub-aggregation calls under which a multi-row `groupBy` still identifies groups by key.
/// `head`, `tail` and `sort` pass events through unchanged. The others emit only the outputs the
/// catalog lists for them; when none of those is a grouping key, every key value in a row comes
/// from `groupBy` itself, whose catalog outputs include its keys.
const KEY_PRESERVING_CALLS: &[&str] = &[
    "head",
    "tail",
    "sort",
    "top",
    "collect",
    "table",
    "selectLast",
    "selectFromMax",
    "selectFromMin",
    "count",
    "sum",
    "avg",
    "min",
    "max",
];

/// Calls that copy existing values unchanged into their outputs, used to carry a collected list
/// through a sub-aggregation.
const VALUE_SELECTORS: &[&str] = &["selectLast", "selectFromMax", "selectFromMin", "table"];

/// How a final `groupBy` cap is counted. Without a sub-aggregation that can emit several rows
/// per group, each row is one group. Rows identify groups by their grouping keys only when every
/// sub-aggregation is a listed call, or an inline pipeline of comparison filters and listed calls;
/// anything else leaves the group count unverified.
fn group_unit(args: &BTreeMap<String, &Value>) -> CapUnit {
    let Some(function) = args.get("function").copied() else {
        return CapUnit::Rows;
    };
    if validate::single_aggregate(function) {
        return CapUnit::Rows;
    }
    let keys = args
        .get("field")
        .map_or_else(BTreeSet::new, |field| catalog::fields(field));
    // Rows are matched by exact key name, so array, indexed or wildcard keys cannot be counted.
    if keys
        .iter()
        .any(|key| key.contains('[') || key.contains('*'))
    {
        return CapUnit::Unknown;
    }
    let functions = match function {
        Value::Array(functions) => functions.as_slice(),
        function => std::slice::from_ref(function),
    };
    let keeps_keys = |call: &Call| {
        plain_call(call, KEY_PRESERVING_CALLS)
            && catalog::aggregate_outputs(call).is_some_and(|(outputs, _)| {
                !outputs
                    .iter()
                    .any(|output| keys.iter().any(|key| fields_overlap(output, key)))
            })
    };
    let keys_kept = functions.iter().all(|function| match function {
        Value::Call(call) => keeps_keys(call),
        Value::Word(name) => keeps_keys(&Call::new(name, vec![])),
        Value::Query {
            pipeline,
            attributes,
            ..
        } => {
            attributes.is_empty()
                && pipeline.steps.iter().all(|step| match step {
                    Step::Filter(filter) => comparison_only(filter),
                    Step::Call(call) => keeps_keys(call),
                    _ => false,
                })
        }
        _ => false,
    });
    if keys_kept {
        CapUnit::Groups(keys)
    } else {
        CapUnit::Unknown
    }
}

/// A resolved call to one of `names` whose arguments contain no nested call or pipeline.
fn plain_call(call: &Call, names: &[&str]) -> bool {
    catalog::resolve(call).is_some_and(|resolved| {
        resolved.diagnostics.is_empty()
            && names.contains(&resolved.function.name)
            && !call.args.iter().any(|arg| nests(&arg.value))
    })
}

/// A filter of literal or wildcard comparisons: it writes no field (no regular expression that
/// could capture, no function call) and can only remove rows.
fn comparison_only(filter: &Filter) -> bool {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => filters.iter().all(comparison_only),
        Filter::Not(filter) => comparison_only(filter),
        Filter::Compare { value, .. } | Filter::Text(value) => {
            matches!(value, Pattern::Any | Pattern::Text(_))
        }
        Filter::Bool(_) => true,
        Filter::Call(_) => false,
    }
}

/// Outputs of a non-retaining aggregate that copy an existing value unchanged: grouping keys and
/// selected fields. Names the aggregate can also compute are never treated as copies.
fn copied_outputs(name: &str, args: &BTreeMap<String, &Value>) -> BTreeSet<String> {
    let fields = |arg: &str| {
        args.get(arg)
            .map_or_else(BTreeSet::new, |value| catalog::fields(value))
    };
    let copied = match name {
        "groupBy" | "bucket" | "top" => fields("field"),
        "timeChart" => fields("series"),
        "table" | "selectLast" => fields("fields"),
        "selectFromMax" | "selectFromMin" => fields("include"),
        _ => BTreeSet::new(),
    };
    copied
        .into_iter()
        .filter(|field| {
            !matches!(
                field.as_str(),
                "_count" | "_bucket" | "_sum" | "_max" | "percent"
            )
        })
        .collect()
}

/// A sub-aggregate that only selects `field`'s existing value: a listed value selector naming it,
/// or an inline pipeline of comparison filters, `head`/`tail`/`sort` and such selectors.
fn selects_value(value: &Value, field: &str) -> bool {
    let selector = |call: &Call| {
        plain_call(call, VALUE_SELECTORS)
            && catalog::resolve(call).is_some_and(|resolved| {
                copied_outputs(resolved.function.name, &resolved.arguments).contains(field)
            })
    };
    match value {
        Value::Call(call) => selector(call),
        Value::Query {
            pipeline,
            attributes,
            ..
        } => {
            attributes.is_empty()
                && pipeline.steps.iter().all(|step| match step {
                    Step::Filter(filter) => comparison_only(filter),
                    Step::Call(call) => {
                        plain_call(call, &["head", "tail", "sort"]) || selector(call)
                    }
                    _ => false,
                })
        }
        _ => false,
    }
}

/// Distinct grouping-key tuples in the returned rows. A key absent from a row is its own value,
/// unless the row holds another spelling of the same data (`a.x` or `a[0]` for key `a`): then the
/// row lookup cannot identify the group and the count is unknown.
fn distinct_groups(
    keys: &BTreeSet<String>,
    rows: &[serde_json::Map<String, JsonValue>],
) -> Option<usize> {
    let mut groups = BTreeSet::new();
    for row in rows {
        let mut tuple = Vec::with_capacity(keys.len());
        for key in keys {
            let value = row.get(key);
            if value.is_none() && row.keys().any(|field| fields_overlap(field, key)) {
                return None;
            }
            tuple.push(value.map(JsonValue::to_string));
        }
        groups.insert(tuple);
    }
    Some(groups.len())
}

fn exceeded_message(cap: &LimitingStage, count: usize, unit: &str, later: &str) -> String {
    format!(
        "{count} {unit} exceed the {} {} limit of {}{}{later}; the saved result is inconsistent with the supplied query, so it cannot be assessed against it. Confirm the exact executed query and the complete unedited response.",
        if cap.info.defaulted {
            "default"
        } else {
            "requested"
        },
        cap.info.stage,
        cap.info.cap,
        if cap.rest_row {
            " plus its rest row"
        } else {
            ""
        }
    )
}

fn cap_message(last: &CapInfo, count: usize, unit: &str) -> String {
    let spec = cap_spec(&last.stage).expect("reported cap has a verified specification");
    let setting = if last.defaulted {
        "default"
    } else {
        "explicit"
    };
    let maximum = spec.max_setting.map_or_else(
        || format!(", {} has no limit=max in this profile", last.stage),
        |target| {
            format!(
                ", limit=max uses the target's {target}, whose effective numeric cap is unknown"
            )
        },
    );
    format!(
        "{count} {unit} reached the {setting} {} cap of {}; the platform default is {} rows{maximum}; hitting the cap does not certify completeness.",
        last.stage, last.cap, spec.default
    )
}

fn check_coverage(inspection: &Inspection, pack: &Pack, result: &mut AssessResult) {
    let mut scopes: Vec<SourceCoverage> = Vec::new();
    for source in std::iter::once(&inspection.source).chain(&inspection.dependencies) {
        if let Some(prior) = scopes.iter_mut().find(|scope| {
            scope.repo == source.repo
                && scope.type_ == source.type_
                && scope.uncertain == source.uncertain
        }) {
            prior.fields.extend(source.fields.iter().cloned());
        } else {
            scopes.push(source.clone());
        }
    }
    for source in &scopes {
        check_source_coverage(source, pack, result);
    }
}

fn check_source_coverage(scope: &SourceCoverage, pack: &Pack, result: &mut AssessResult) {
    let SourceCoverage {
        repo,
        type_,
        uncertain,
        fields,
    } = scope;
    if *uncertain || (repo.is_none() && type_.is_none()) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_statistics_look_up_each_row_key_once() {
        // Each row carries its own key, so every inspected column is missing from the other rows.
        let rows = (0..2_000)
            .map(|i| serde_json::Map::from_iter([(format!("k{i}"), JsonValue::from("v"))]))
            .collect::<Vec<_>>();
        let columns = rows
            .iter()
            .flat_map(|row| row.keys().cloned())
            .collect::<BTreeSet<_>>();
        let (stats, lookups) = column_statistics(&rows, columns);
        // Scanning every row once per column would take 2,000 x 2,000 lookups.
        assert_eq!(lookups, 2_000);
        assert_eq!(stats.len(), 2_000);
        let single = ColumnStats {
            present: 1,
            null: 0,
            empty: 0,
        };
        assert!(stats.values().all(|stats| *stats == single));

        // Lookups follow the keys present in rows, not inspected columns times rows.
        let row = serde_json::Map::from_iter([
            ("a".to_string(), JsonValue::Null),
            ("b".to_string(), JsonValue::from("")),
            ("c".to_string(), JsonValue::from("x")),
        ]);
        let columns = BTreeSet::from(["a".to_string(), "b".to_string(), "z".to_string()]);
        let (stats, lookups) = column_statistics(&vec![row; 3], columns);
        assert_eq!(lookups, 9);
        assert_eq!(
            stats["a"],
            ColumnStats {
                present: 3,
                null: 3,
                empty: 0
            }
        );
        assert_eq!(
            stats["b"],
            ColumnStats {
                present: 3,
                null: 0,
                empty: 3
            }
        );
        assert_eq!(stats["z"], ColumnStats::default());
        assert!(!stats.contains_key("c"));
    }
}
