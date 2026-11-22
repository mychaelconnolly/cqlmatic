//! Bundled, pinned language knowledge. Packs cannot extend executable semantics.
use crate::{
    contracts::Diagnostic,
    language::ast::{Call, Pipeline, Step, Value},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::OnceLock,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentScope {
    Value,
    Independent,
    FilterQuery,
    Inline,
    Continuing,
    Aggregate,
    ElementAggregate,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgumentRole {
    InputField,
    ForeignField,
    Expression,
    Output,
    Renames,
    Removal,
    Value,
}

pub struct ResolvedCall<'a> {
    pub function: &'static Function,
    pub arguments: BTreeMap<String, &'a Value>,
    pub ordered: Vec<(String, &'a Value)>,
    pub queries: Vec<&'a Value>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ResolvedCall<'_> {
    pub fn resource_names(&self) -> BTreeSet<String> {
        self.ordered
            .iter()
            .filter(|(name, _)| name == "file")
            .flat_map(|(_, value)| fields(value))
            .collect()
    }
}

/// One mapping for validation and grounding; values remain borrowed from the parsed call.
pub fn resolve(call: &Call) -> Option<ResolvedCall<'_>> {
    let function = function(&call.name)?;
    let mut resolved = ResolvedCall {
        function,
        arguments: BTreeMap::new(),
        ordered: vec![],
        queries: vec![],
        diagnostics: vec![],
    };
    #[derive(Clone, Copy, PartialEq, Eq)]
    enum QueryForm {
        None,
        Unnamed,
        Named,
        Array,
    }
    let mut position = 0;
    let mut query_form = QueryForm::None;
    for arg in &call.args {
        if function.name == "correlate" {
            let is_labeled_query = matches!(arg.value, Value::Query { label: Some(_), .. });
            let query_array = match &arg.value {
                Value::Array(xs)
                    if !xs.is_empty()
                        && xs
                            .iter()
                            .all(|v| matches!(v, Value::Query { label: Some(_), .. })) =>
                {
                    Some(xs.as_slice())
                }
                _ => None,
            };
            if arg.name.is_none() && is_labeled_query {
                if matches!(query_form, QueryForm::Named | QueryForm::Array) {
                    resolved.diagnostics.push(Diagnostic::error(
                        "CQL_DUPLICATE_ARGUMENT",
                        "Cannot mix named query= or a query list with unnamed query arguments; pass unnamed queries or query=[...].",
                    ));
                }
                if query_form == QueryForm::None {
                    query_form = QueryForm::Unnamed;
                }
                resolved.queries.push(&arg.value);
                continue;
            }
            if arg.name.is_none()
                && let Some(xs) = query_array
            {
                if query_form != QueryForm::None {
                    resolved.diagnostics.push(Diagnostic::error(
                        "CQL_DUPLICATE_ARGUMENT",
                        "Duplicate query list; pass unnamed queries or a single query=[...] list.",
                    ));
                }
                query_form = QueryForm::Array;
                resolved.queries.extend(xs.iter());
                continue;
            }
            if arg.name.as_deref() == Some("query") && (is_labeled_query || query_array.is_some()) {
                if query_form == QueryForm::Named {
                    resolved.diagnostics.push(Diagnostic::error(
                        "CQL_DUPLICATE_ARGUMENT",
                        "Duplicate named query parameter; use unnamed queries or query=[...].",
                    ));
                } else if query_form != QueryForm::None {
                    resolved.diagnostics.push(Diagnostic::error(
                        "CQL_DUPLICATE_ARGUMENT",
                        "Cannot mix named query= with unnamed query arguments; pass unnamed queries or query=[...].",
                    ));
                }
                query_form = QueryForm::Named;
                if let Some(xs) = query_array {
                    resolved.queries.extend(xs.iter());
                } else {
                    resolved.queries.push(&arg.value);
                }
                continue;
            }
        }
        let name = if let Some(name) = &arg.name {
            name.as_str()
        } else if function.name == "readFile" && position > 0 {
            position += 1;
            "file"
        } else if let Some(name) = function.positional.get(position) {
            position += 1;
            name
        } else {
            resolved.diagnostics.push(Diagnostic::error(
                "CQL_ARGUMENT_COUNT",
                "Too many positional arguments for this function.",
            ));
            continue;
        };
        let name = match (function.name, name) {
            ("replace", "replacement") => "with",
            ("kvParse", "prefix") => "as",
            ("match" | "matchAsArray" | "readFile", "table") => "file",
            ("join", "repo") => "view",
            _ => name,
        };
        if resolved.arguments.contains_key(name) {
            if function.name == "readFile" && arg.name.is_none() && name == "file" && position > 1 {
                resolved.ordered.push((name.into(), &arg.value));
                continue;
            }
            resolved.diagnostics.push(Diagnostic::error(
                "CQL_DUPLICATE_ARGUMENT",
                format!("Duplicate argument '{name}'."),
            ));
            continue;
        }
        resolved.arguments.insert(name.into(), &arg.value);
        resolved.ordered.push((name.into(), &arg.value));
        if function.name != "eval" && !function.arguments.iter().any(|(key, _)| *key == name) {
            resolved.diagnostics.push(Diagnostic::error(
                "CQL_ARGUMENT_UNKNOWN",
                format!("Unknown '{}' argument '{name}'.", function.name),
            ));
        }
    }
    for required in function.required {
        if !resolved.arguments.contains_key(*required)
            && !(function.name == "correlate"
                && *required == "query"
                && !resolved.queries.is_empty())
        {
            resolved.diagnostics.push(Diagnostic::error(
                "CQL_ARGUMENT_REQUIRED",
                format!("'{}' requires argument '{required}'.", function.name),
            ));
        }
    }
    Some(resolved)
}

pub fn query_all_fields(value: &Value) -> bool {
    matches!(value, Value::Word(s) if s == "*")
        || matches!(value, Value::Array(xs) if xs.len() == 1 && matches!(&xs[0], Value::Word(s) if s == "*"))
}

pub fn correlation_links(pipeline: &Pipeline) -> Vec<(&str, &str, bool)> {
    fn visit<'a>(pipeline: &'a Pipeline, nested: bool, out: &mut Vec<(&'a str, &'a str, bool)>) {
        for step in &pipeline.steps {
            match step {
                Step::Link { field, target } => out.push((field, target, nested)),
                Step::Case(branches) => {
                    for branch in branches {
                        visit(branch, true, out);
                    }
                }
                Step::Match { arms, .. } => {
                    for (_, branch) in arms {
                        visit(branch, true, out);
                    }
                }
                _ => {}
            }
        }
    }
    let mut links = Vec::new();
    visit(pipeline, false, &mut links);
    links
}

/// Exact exports; None retains an explicitly unknown all-fields projection.
pub fn correlation_outputs(
    args: &BTreeMap<String, &Value>,
    queries: &[(&Value, bool)],
    columns: &BTreeMap<String, Option<BTreeSet<String>>>,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut outputs: BTreeSet<String> = ["@id", "@timestamp", "@ingesttimestamp"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut unknown = BTreeSet::new();
    for (value, aggregation) in queries {
        let Value::Query {
            label: Some(name),
            pipeline,
            attributes,
        } = value
        else {
            continue;
        };
        let mut fields = BTreeSet::new();
        let include = attributes
            .iter()
            .find(|a| a.name.as_deref() == Some("include"));
        if *aggregation || include.is_some_and(|a| query_all_fields(&a.value)) {
            if let Some(Some(known)) = columns.get(name) {
                fields.extend(known.iter().cloned());
            } else {
                unknown.insert(format!("{name}."));
            }
        } else if let Some(include) = include {
            fields.extend(self::fields(&include.value));
        }
        if !*aggregation
            && args
                .get("includeConstraintValues")
                .and_then(|v| v.boolean())
                != Some(false)
        {
            if let Some(globals) = args.get("globalConstraints") {
                fields.extend(self::fields(globals));
            }
            for (source, target, _) in correlation_links(pipeline) {
                fields.insert(source.into());
                if let Some((other, key)) = target.split_once('.') {
                    outputs.insert(format!("{other}.{key}"));
                }
            }
        } else if *aggregation
            && args
                .get("includeConstraintValues")
                .and_then(|v| v.boolean())
                != Some(false)
        {
            for (_, target, _) in correlation_links(pipeline) {
                if let Some((other, key)) = target.split_once('.') {
                    outputs.insert(format!("{other}.{key}"));
                }
            }
        }
        outputs.extend(fields.into_iter().map(|f| format!("{name}.{f}")));
    }
    (outputs, unknown)
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Record {
    pub id: String,
    pub title: String,
    pub topic: String,
    pub body: String,
    pub sources: Vec<String>,
    pub reference: String,
    pub source_path: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ParameterRule {
    pub name: String,
    pub choices: Vec<String>,
    pub minimum: Option<f64>,
    pub maximum: Option<f64>,
    pub default: Option<String>,
    /// Parameter-name omission annotated in the bundled reference parameter facts.
    pub omittable_name: bool,
}

#[derive(Debug, Deserialize)]
struct ParameterFunction {
    name: String,
    parameters: Vec<ParameterRule>,
}

pub fn parameter_rules(name: &str) -> &'static [ParameterRule] {
    #[derive(Deserialize)]
    struct Rules {
        functions: Vec<ParameterFunction>,
    }
    static RULES: OnceLock<Rules> = OnceLock::new();
    RULES
        .get_or_init(|| {
            serde_json::from_str(include_str!("../data/function-parameters.json"))
                .expect("bundled parameter facts are verified")
        })
        .functions
        .iter()
        .find(|f| f.name == name)
        .map(|f| f.parameters.as_slice())
        .unwrap_or(&[])
}

pub fn function_scope() -> &'static serde_json::Value {
    static SCOPE: OnceLock<serde_json::Value> = OnceLock::new();
    SCOPE.get_or_init(|| {
        serde_json::from_str(include_str!("../data/function-scope.json"))
            .expect("bundled function scope is verified")
    })
}

pub fn scope_entry(name: &str) -> Option<&'static serde_json::Value> {
    function_scope()["functions"]
        .as_array()?
        .iter()
        .find(|entry| entry["canonical_name"] == name)
}

pub fn records() -> &'static [Record] {
    static RECORDS: OnceLock<Vec<Record>> = OnceLock::new();
    RECORDS.get_or_init(|| {
        let mut records: Vec<Record> = serde_json::from_str(include_str!("../data/knowledge.json"))
            .expect("bundled knowledge is checked at build verification");
        records.push(Record {
            id: "cql:workflow:analyst".into(),
            title: "Analyst workflow: interpret, construct, execute and inspect".into(),
            topic: "workflow".into(),
            body: include_str!("../docs/ANALYST-WORKFLOW.md").into(),
            sources: vec!["docs/ANALYST-WORKFLOW.md".into()],
            reference: "docs/ANALYST-WORKFLOW.md".into(),
            source_path: "cqlmatic/profile/core-v3".into(),
        });
        records
    })
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum Effect {
    Filter,
    Transform,
    Aggregate,
    Source,
    Definition,
    Correlation,
}

#[derive(Debug, Clone, Serialize)]
pub struct Function {
    pub name: &'static str,
    pub positional: &'static [&'static str],
    pub required: &'static [&'static str],
    /// Argument type codes: f field, s string, b bool, n number, i integer,
    /// F field(s), A field array, S string(s), d duration, p predicate,
    /// l integer/max, a unqualified array, s[] string array, q unlabeled query,
    /// q[] filter query array, e expression, g aggregate(s), r regex,
    /// y array name, u non-aggregate inline pipeline/function, Q named query,
    /// Q[] named aggregation queries with qualified attributes.
    pub arguments: &'static [(&'static str, &'static str)],
    pub effect: Effect,
    pub reference_id: &'static str,
    pub placement: &'static str,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Requirement {
    pub when_arguments_present: &'static [&'static str],
    pub note: &'static str,
}

impl Requirement {
    pub fn applies(&self, args: &BTreeMap<String, &Value>) -> bool {
        self.when_arguments_present.is_empty()
            || self
                .when_arguments_present
                .iter()
                .any(|name| args.contains_key(*name))
    }
}

impl Function {
    pub fn requirements(&self) -> &'static [Requirement] {
        match self.name {
            "array:intersection" | "array:reduceColumn" | "array:reduceRow" | "array:union" => {
                &[Requirement {
                    when_arguments_present: &[],
                    note: "The ArrayFunctions feature flag must be enabled on the target.",
                }]
            }
            "beta:repeating" => &[Requirement {
                when_arguments_present: &[],
                note: "The RepeatingQueries feature must be enabled; the function has no effect in a historical query.",
            }],
            "readPersistedAggregation" => &[
                Requirement {
                    when_arguments_present: &[],
                    note: "The target must support readPersistedAggregation (documented from version 1.252) and have the named persisted aggregation.",
                },
                Requirement {
                    when_arguments_present: &["span", "timezone"],
                    note: "The span/timezone rollup form requires version 1.263 or newer and a rollup-enabled persisted aggregation.",
                },
            ],
            "remoteTable" => &[Requirement {
                when_arguments_present: &[],
                note: "The target must be Falcon NG-SIEM with an existing federated connection for the named remote source.",
            }],
            "explain:asTable" => &[Requirement {
                when_arguments_present: &[],
                note: "This experimental function is for ad hoc profiling only, not scheduled searches or triggers.",
            }],
            "matchAsArray" => &[Requirement {
                when_arguments_present: &[],
                note: "The named lookup file or ad hoc table must exist and be accessible on the target.",
            }],
            "reverseDns" => &[Requirement {
                when_arguments_present: &[],
                note: "External DNS resolution must be available to the target; hostname enrichment can fail at runtime.",
            }],
            _ => &[],
        }
    }
    pub fn argument_role(&self, name: &str) -> ArgumentRole {
        if matches!(self.name, "parseCsv" | "parseFixedWidth") && name == "columns"
            || self.name == "bitfield:extractFlags" && name == "output"
        {
            return ArgumentRole::Output;
        }
        if self.name == "array:drop" && name == "array" {
            return ArgumentRole::Removal;
        }
        if self.name == "eval" {
            return ArgumentRole::Expression;
        }
        if self.name == "parseJson" && matches!(name, "include" | "exclude") {
            return ArgumentRole::Value;
        }
        if self.name == "rename" && name == "field" {
            return ArgumentRole::Renames;
        }
        if self.name == "drop" && name == "fields" {
            return ArgumentRole::Removal;
        }
        if matches!(name, "as" | "asArray" | "var" | "timezoneAs") {
            return ArgumentRole::Output;
        }
        if matches!(
            (self.name, name),
            ("join", "key" | "include")
                | ("defineTable", "include")
                | ("remoteTable", "include")
                | ("match", "column" | "include")
                | ("matchAsArray", "column" | "include")
                | ("readFile", "include")
                | ("cidr", "column")
        ) {
            return ArgumentRole::ForeignField;
        }
        match self.argument_kind(name) {
            Some("f" | "F" | "A" | "y" | "z") => ArgumentRole::InputField,
            Some("e" | "p" | "e[]") => ArgumentRole::Expression,
            _ if self.name == "eval" => ArgumentRole::Expression,
            _ => ArgumentRole::Value,
        }
    }
    pub fn argument_kind(&self, name: &str) -> Option<&'static str> {
        self.arguments
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, kind)| *kind)
    }
    pub fn argument_scope(&self, name: &str) -> ArgumentScope {
        if matches!(
            self.name,
            "array:reduceAll" | "array:reduceColumn" | "array:reduceRow"
        ) && name == "function"
        {
            return ArgumentScope::ElementAggregate;
        }
        if matches!(
            (self.name, name),
            ("series", "startmatch" | "endmatch") | ("selfJoinFilter", "where")
        ) {
            return ArgumentScope::Continuing;
        }
        match self.argument_kind(name) {
            Some("q" | "Q" | "Q[]" | "R") => ArgumentScope::Independent,
            Some("q[]") => ArgumentScope::FilterQuery,
            Some("u") => ArgumentScope::Inline,
            Some("g") => ArgumentScope::Aggregate,
            _ => ArgumentScope::Value,
        }
    }
    pub fn resolved_effect(&self, args: &BTreeMap<String, &Value>) -> Effect {
        if self.name == "matchAsArray" {
            return if args.get("strict").and_then(|v| v.boolean()) == Some(false) {
                Effect::Transform
            } else {
                Effect::Filter
            };
        }
        if self.name == "ioc:lookup" {
            return if args.get("strict").and_then(|v| v.boolean()) == Some(true) {
                Effect::Filter
            } else {
                Effect::Transform
            };
        }
        if self.name == "join"
            && (args.contains_key("include")
                || args.get("mode").and_then(|v| v.text()) == Some("left"))
        {
            return Effect::Transform;
        }
        if matches!(self.name, "regex" | "match")
            && args.get("strict").and_then(|v| v.boolean()) == Some(false)
        {
            Effect::Transform
        } else {
            self.effect
        }
    }
    pub fn scalar_computation(&self) -> bool {
        self.name.starts_with("math:")
            || matches!(
                self.name,
                "in" | "wildcard" | "regex" | "cidr" | "array:contains" | "objectArray:exists"
            )
            || matches!(
                self.name,
                "lower"
                    | "upper"
                    | "length"
                    | "concat"
                    | "replace"
                    | "format"
                    | "round"
                    | "if"
                    | "array:length"
                    | "base64Decode"
                    | "urlDecode"
                    | "shannonEntropy"
                    | "splitString"
                    | "coalesce"
                    | "getField"
                    | "formatTime"
                    | "base64Encode"
                    | "communityId"
                    | "concatArray"
                    | "crypto:md5"
                    | "crypto:sha1"
                    | "crypto:sha256"
                    | "duration"
                    | "end"
                    | "eventFieldCount"
                    | "formatDuration"
                    | "geography:distance"
                    | "geohash"
                    | "hash"
                    | "json:prettyPrint"
                    | "now"
                    | "parseHexString"
                    | "parseInt"
                    | "start"
                    | "stripAnsiCodes"
                    | "subnet"
                    | "tokenHash"
                    | "unit:convert"
                    | "urlEncode"
                    | "writeJson"
                    | "xml:prettyPrint"
                    | "text:editDistance"
                    | "text:length"
                    | "text:positionOf"
                    | "text:substring"
                    | "text:trim"
                    | "time:dayOfMonth"
                    | "time:dayOfWeek"
                    | "time:dayOfWeekName"
                    | "time:dayOfYear"
                    | "time:hour"
                    | "time:millisecond"
                    | "time:minute"
                    | "time:month"
                    | "time:monthName"
                    | "time:second"
                    | "time:weekOfYear"
                    | "time:year"
            )
    }
    pub fn default_read_fields(&self, args: &BTreeMap<String, &Value>) -> &'static [&'static str] {
        if self.name == "parseTimestamp" {
            return if args.contains_key("timezone") || args.contains_key("timezoneField") {
                &[]
            } else {
                &["@timezone"]
            };
        }
        if self.name == "geohash" {
            return match (args.contains_key("lat"), args.contains_key("lon")) {
                (false, false) => &["ip.lat", "ip.lon"],
                (false, true) => &["ip.lat"],
                (true, false) => &["ip.lon"],
                (true, true) => &[],
            };
        }
        if self.name.starts_with("time:") {
            return match (
                args.contains_key("field"),
                self.name == "time:dayOfMonth"
                    || args.contains_key("timezoneField")
                    || args.contains_key("timezone"),
            ) {
                (false, false) => &["@timestamp", "@timezone"],
                (false, true) => &["@timestamp"],
                (true, false) => &["@timezone"],
                (true, true) => &[],
            };
        }
        if self.name == "wildcard"
            && args
                .get("field")
                .is_none_or(|v| matches!(v,Value::Array(xs) if xs.is_empty()))
        {
            return &["<original-event-fields>"];
        }
        if self.name == "findTimestamp" {
            return match (
                args.contains_key("field"),
                args.contains_key("timezoneField") || args.contains_key("timezone"),
            ) {
                (false, false) => &["@rawstring", "@timezone"],
                (false, true) => &["@rawstring"],
                (true, false) => &["@timezone"],
                (true, true) => &[],
            };
        }
        if matches!(self.name, "bucket" | "timeChart" | "session" | "series") {
            return &["@timestamp"];
        }
        if args.contains_key("field") {
            return &[];
        }
        match self.name {
            "regex" | "replace" | "parseJson" | "kvParse" | "splitString" | "parseCsv"
            | "parseFixedWidth" | "parseCEF" | "parseLEEF" | "parseXml" | "json:prettyPrint"
            | "xml:prettyPrint" | "stripAnsiCodes" | "hashMatch" | "writeJson" => &["@rawstring"],
            "parseUrl" => &["url"],
            "sample" => &["@timestamp"],
            "formatTime" => &["@timestamp"],
            "asn" | "ipLocation" => &["ip"],
            "split" => &["_events[]"],
            _ => &[],
        }
    }
    pub fn named_outputs(
        &self,
        args: &BTreeMap<String, &Value>,
    ) -> Option<(Vec<String>, Vec<String>)> {
        if matches!(self.name, "parseCsv" | "parseFixedWidth") {
            return Some((
                args.get("columns")
                    .map(|v| fields(v).into_iter().collect())
                    .unwrap_or_default(),
                vec![],
            ));
        }
        if self.name == "bitfield:extractFlags" {
            let outputs = args
                .get("output")
                .and_then(|value| {
                    if let Value::Array(pairs) = value {
                        Some(pairs)
                    } else {
                        None
                    }
                })
                .into_iter()
                .flatten()
                .filter_map(|pair| {
                    if let Value::Array(pair) = pair {
                        pair.get(1).and_then(|v| v.text()).map(str::to_owned)
                    } else {
                        None
                    }
                })
                .collect();
            return Some((outputs, vec![]));
        }
        if matches!(
            self.name,
            "array:append" | "array:dedup" | "array:sort" | "array:rename"
        ) {
            let array = args
                .get("asArray")
                .or_else(|| args.get("array"))
                .and_then(|v| v.text())?;
            return Some((vec![], vec![format!("{}[]", array.trim_end_matches("[]"))]));
        }
        if self.name == "bitfield:extractFlagsAsArray" {
            return Some((
                vec![],
                vec![
                    args.get("asArray")
                        .and_then(|v| v.text())
                        .unwrap_or("_flags[]")
                        .into(),
                ],
            ));
        }
        if self.name == "matchAsArray" {
            let array = args.get("asArray").and_then(|v| v.text())?;
            return Some((vec![], vec![format!("{}[]", array.trim_end_matches("[]"))]));
        }
        if matches!(self.name, "parseUri" | "parseUrl") {
            let prefix = args
                .get(if self.name == "parseUri" {
                    "prefix"
                } else {
                    "as"
                })
                .or_else(|| args.get("field"))
                .and_then(|v| v.text())
                .unwrap_or("url");
            let host = if self.name == "parseUri" {
                "hostname"
            } else {
                "host"
            };
            return Some((
                [
                    "scheme", "username", "password", host, "port", "path", "query", "fragment",
                ]
                .iter()
                .map(|name| format!("{prefix}.{name}"))
                .collect(),
                vec![],
            ));
        }
        if self.name == "parseTimestamp" {
            return Some((
                vec![
                    args.get("as")
                        .and_then(|v| v.text())
                        .unwrap_or("@timestamp")
                        .into(),
                    args.get("timezoneAs")
                        .and_then(|v| v.text())
                        .unwrap_or("@timezone")
                        .into(),
                ],
                vec![],
            ));
        }
        if self.name == "array:reduceRow" {
            let prefix = args.get("as").and_then(|v| v.text())?;
            let (outputs, _) = aggregate_value_outputs(args.get("function")?)?;
            return Some((
                outputs
                    .into_iter()
                    .map(|field| format!("{prefix}.{field}"))
                    .collect(),
                vec![],
            ));
        }
        if self.name == "text:editDistanceAsArray" {
            let prefix = args
                .get("asArray")
                .and_then(|v| v.text())
                .unwrap_or("_distance[]");
            let prefix = prefix.strip_suffix("[]").unwrap_or(prefix);
            return Some((
                vec![],
                ["distance", "reference"]
                    .iter()
                    .map(|member| format!("{prefix}[].{member}"))
                    .collect(),
            ));
        }
        if self.name == "findTimestamp" {
            return Some((
                vec![
                    args.get("as")
                        .and_then(|v| v.text())
                        .unwrap_or("@timestamp")
                        .into(),
                    args.get("timezoneAs")
                        .and_then(|v| v.text())
                        .unwrap_or("@timezone")
                        .into(),
                ],
                vec![],
            ));
        }
        if self.name == "neighbor" {
            let prefix = args
                .get("prefix")
                .and_then(|v| v.text())
                .unwrap_or("_neighbor");
            return Some((
                args.get("include")
                    .into_iter()
                    .flat_map(|v| fields(v))
                    .map(|field| {
                        if prefix.is_empty() {
                            field
                        } else {
                            format!("{prefix}.{field}")
                        }
                    })
                    .collect(),
                vec![],
            ));
        }
        let (prefix, suffixes): (&str, &[&str]) = match self.name {
            "asn" | "ipLocation" => (
                args.get("as")
                    .or_else(|| args.get("field"))
                    .and_then(|v| v.text())
                    .unwrap_or("ip"),
                if self.name == "asn" {
                    &["asn", "org"]
                } else {
                    &["country", "city", "lon", "lat"]
                },
            ),
            "ioc:lookup" => (
                args.get("prefix").and_then(|v| v.text()).unwrap_or("ioc"),
                &["detected"],
            ),
            _ => return None,
        };
        let members = if self.name == "ioc:lookup" {
            args.get("include")
                .map(|v| fields(v))
                .unwrap_or_else(|| {
                    [
                        "indicator",
                        "labels",
                        "last_updated",
                        "malicious_confidence",
                        "published_date",
                        "type",
                    ]
                    .into_iter()
                    .map(str::to_string)
                    .collect()
                })
                .into_iter()
                .map(|column| format!("{prefix}[].{column}"))
                .collect()
        } else {
            vec![]
        };
        Some((
            suffixes.iter().map(|s| format!("{prefix}.{s}")).collect(),
            members,
        ))
    }
    pub fn default_output(&self, args: &BTreeMap<String, &Value>) -> Option<String> {
        if let Some(name) = self.name.strip_prefix("math:") {
            return Some(
                args.get("as")
                    .and_then(|v| v.text())
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("_{name}")),
            );
        }
        let output = match self.name {
            "bitfield:extractFlagsAsString" => "_flags",
            "reverseDns" => "_hostname",
            "hashRewrite" => args.get("field").and_then(|v| v.text())?,
            "base64Encode" => "_base64Encode",
            "beta:param" => args.get("as").and_then(|v| v.text())?,
            "callFunction" => "_function",
            "communityId" => "_community_id",
            "concatArray" => "_concatArray",
            "counterAsRate" => "_rate",
            "crypto:md5" => "_md5",
            "crypto:sha1" => "_sha1",
            "crypto:sha256" => "_sha256",
            "duration" => "_duration",
            "end" => "_end",
            "eventFieldCount" => "_eventFieldCount",
            "eventSize" => "_eventSize",
            "formatDuration" => args
                .get("field")
                .and_then(|v| v.text())
                .unwrap_or("@rawstring"),
            "geography:distance" => "_distance",
            "geohash" => "_geohash",
            "hash" => "_hash",
            "json:prettyPrint" => args
                .get("field")
                .and_then(|v| v.text())
                .unwrap_or("@rawstring"),
            "math:abs" => "_abs",
            "math:arccos" => "_arccos",
            "math:arcsin" => "_arcsin",
            "math:arctan" => "_arctan",
            "math:arctan2" => "_arctan2",
            "math:ceil" => "_ceil",
            "math:cos" => "_cos",
            "math:cosh" => "_cosh",
            "math:deg2rad" => "_deg2rad",
            "math:exp" => "_exp",
            "math:expm1" => "_expm1",
            "math:floor" => "_floor",
            "math:log" => "_log",
            "math:log10" => "_log10",
            "math:log1p" => "_log1p",
            "math:log2" => "_log2",
            "math:mod" => "_mod",
            "math:pow" => "_pow",
            "math:rad2deg" => "_rad2deg",
            "math:sin" => "_sin",
            "math:sinh" => "_sinh",
            "math:sqrt" => "_sqrt",
            "math:tan" => "_tan",
            "math:tanh" => "_tanh",
            "now" => "_now",
            "parseHexString" => "_parsehexstring",
            "parseInt" => args
                .get("field")
                .and_then(|v| v.text())
                .unwrap_or("@rawstring"),
            "parseTimestamp" => "@timestamp",
            "percentage" => "_percentage",
            "range" => "_range",
            "start" => "_start",
            "stdDev" => "_stddev",
            "stripAnsiCodes" => args
                .get("field")
                .and_then(|v| v.text())
                .unwrap_or("@rawstring"),
            "subnet" => "_subnet",
            "tokenHash" => "_tokenHash",
            "unit:convert" => args
                .get("field")
                .and_then(|v| v.text())
                .unwrap_or("@rawstring"),
            "urlEncode" => "_urlencode",
            "writeJson" => "_json",
            "xml:prettyPrint" => args
                .get("field")
                .and_then(|v| v.text())
                .unwrap_or("@rawstring"),
            "lower" => "_lower",
            "upper" => "_upper",
            "length" | "array:length" | "text:length" => "_length",
            "text:editDistance" => "_distance",
            "text:positionOf" => "_position",
            "text:substring" => "_string",
            "text:trim" => "_trim",
            "time:dayOfMonth" => "_dayOfMonth",
            "time:dayOfWeek" => "_dayOfWeek",
            "time:dayOfWeekName" => "_dayOfWeekName",
            "time:dayOfYear" => "_dayOfYear",
            "time:hour" => "_hour",
            "time:millisecond" => "_millisecond",
            "time:minute" => "_minute",
            "time:month" => "_month",
            "time:monthName" => "_monthName",
            "time:second" => "_second",
            "time:weekOfYear" => "_weekOfYear",
            "time:year" => "_year",
            "concat" => "_concat",
            "format" => "_format",
            "if" => "_if",
            "base64Decode" => "_base64Decode",
            "urlDecode" => "_urldecode",
            "shannonEntropy" => "_shannonentropy",
            "coalesce" => "_coalesce",
            "getField" => "_getField",
            "splitString" => "_splitstring",
            "replace" => args
                .get("field")
                .and_then(|v| v.text())
                .unwrap_or("@rawstring"),
            "round" => args.get("field").and_then(|v| v.text())?,
            "formatTime" => args.get("as").and_then(|v| v.text())?,
            _ => return None,
        };
        Some(
            args.get("as")
                .and_then(|v| v.text())
                .unwrap_or(output)
                .into(),
        )
    }
}

/// Statically named writes, with dynamic output names kept separate from required columns.
pub struct OutputWrites {
    pub fields: BTreeSet<String>,
    pub required: BTreeSet<String>,
    pub dynamic_prefix: Option<String>,
    pub preserves_existing: bool,
}

pub fn output_writes(call: &Call) -> Option<OutputWrites> {
    let resolved = resolve(call)?;
    let spec = resolved.function;
    let args = &resolved.arguments;
    let mut writes = OutputWrites {
        fields: BTreeSet::new(),
        required: BTreeSet::new(),
        dynamic_prefix: None,
        preserves_existing: false,
    };
    if let Some((outputs, members)) = spec.named_outputs(args) {
        writes.fields.extend(outputs);
        writes.fields.extend(members);
    }
    if let Some(output) = spec.default_output(args) {
        if spec.name == "splitString" && !args.contains_key("index") {
            writes.fields.insert(format!("{output}[]"));
        } else {
            writes.required.insert(output);
        }
    }
    match spec.name {
        "lowercase" => {
            let rename = args
                .get("include")
                .and_then(|v| v.text())
                .is_some_and(|v| matches!(v, "fields" | "both"));
            if let Some(value) = args.get("field") {
                for field in fields(value) {
                    if field.contains(['*', '?']) {
                        writes.dynamic_prefix = Some(String::new());
                    } else {
                        writes
                            .fields
                            .insert(if rename { field.to_lowercase() } else { field });
                    }
                }
            }
        }
        "setField" => {
            if let Some(Value::String(field)) = args.get("target") {
                writes.required.insert(field.clone());
            } else {
                writes.dynamic_prefix = Some(String::new());
            }
        }
        "hashRewrite" => {
            if args.get("replaceInRawstring").and_then(|v| v.boolean()) != Some(false) {
                writes.fields.insert("@rawstring".into());
            }
        }
        "parseCEF" | "parseLEEF" => {
            writes.dynamic_prefix = Some(String::new());
        }
        "parseXml" | "eventInternals" => {
            writes.dynamic_prefix = Some(
                args.get("prefix")
                    .and_then(|v| v.text())
                    .unwrap_or(if spec.name == "eventInternals" {
                        "@"
                    } else {
                        ""
                    })
                    .into(),
            );
        }
        "array:reduceRow" if !args.contains_key("as") => {
            writes.dynamic_prefix = Some(String::new());
        }
        "matchAsArray" if !args.contains_key("asArray") => {
            writes.dynamic_prefix = Some(String::new());
        }
        "split" => {
            let prefix = args
                .get("field")
                .and_then(|v| v.text())
                .unwrap_or("_events");
            writes.fields.insert("_index".into());
            writes.dynamic_prefix = Some(
                if args.get("strip").and_then(|v| v.boolean()) == Some(true) {
                    String::new()
                } else {
                    prefix.into()
                },
            );
        }
        "eval" => writes.required.extend(args.keys().cloned()),
        "default" => {
            if let Some(value) = args.get("field") {
                writes.required.extend(fields(value));
            }
        }
        "regex" => {
            if let Some(pattern) = args.get("regex").and_then(|v| v.text())
                && let Ok(info) = crate::language::validate::regex_analysis(pattern)
            {
                if args.get("repeat").and_then(|v| v.boolean()) == Some(true) {
                    writes
                        .fields
                        .extend(info.captures.into_iter().map(|field| format!("{field}[]")));
                } else {
                    writes.fields.extend(info.captures);
                    if spec.resolved_effect(args) == Effect::Filter {
                        writes.required.extend(info.guaranteed);
                    }
                }
            }
        }
        "parseJson" | "kvParse" => {
            writes.dynamic_prefix = Some(
                args.get(if spec.name == "parseJson" {
                    "prefix"
                } else {
                    "as"
                })
                .and_then(|v| v.text())
                .unwrap_or("")
                .into(),
            );
            writes.preserves_existing = spec.name == "kvParse"
                && args.get("override").and_then(|v| v.boolean()) != Some(true);
        }
        "array:eval" | "array:filter" | "objectArray:eval" => {
            if let Some(array) = args.get("asArray").or_else(|| args.get("array")) {
                writes.fields.extend(fields(array));
            }
        }
        "createEvents" => {
            writes.required.insert("@rawstring".into());
        }
        _ => {}
    }
    writes.fields.extend(writes.required.iter().cloned());
    Some(writes)
}

pub fn imported_fields(
    name: &str,
    args: &BTreeMap<String, &Value>,
    columns: Option<&BTreeSet<String>>,
) -> Option<BTreeSet<String>> {
    if let Some(include) = args.get("include") {
        return Some(fields(include));
    }
    if name == "join" {
        return Some(BTreeSet::new());
    }
    if name != "match" {
        return None;
    }
    let keys = args
        .get("column")
        .or_else(|| args.get("field"))
        .map(|v| fields(v))
        .unwrap_or_default();
    Some(columns?.difference(&keys).cloned().collect())
}

pub fn all_columns(value: &Value) -> bool {
    matches!(value, Value::String(s) | Value::Word(s) if s == "*")
}

pub fn fields(value: &Value) -> BTreeSet<String> {
    match value {
        Value::String(s) | Value::Word(s) => BTreeSet::from([s.clone()]),
        Value::Array(xs) => xs.iter().flat_map(fields).collect(),
        _ => BTreeSet::new(),
    }
}

/// Known aggregate outputs and whether an aggregator also retains input fields.
pub fn aggregate_outputs(call: &Call) -> Option<(BTreeSet<String>, bool)> {
    let resolved = resolve(call)?;
    let args = &resolved.arguments;
    let argument_fields = |name: &str| args.get(name).map(|v| fields(v)).unwrap_or_default();
    let mut outputs = BTreeSet::new();
    let mut retains = false;
    match resolved.function.name {
        "head" | "tail" | "sort" => retains = true,
        "collect" | "selectLast" | "table" => outputs = argument_fields("fields"),
        "selectFromMax" | "selectFromMin" => outputs = argument_fields("include"),
        "fieldset" => {
            outputs.insert("field".into());
        }
        "groupBy" | "stats" | "bucket" | "timeChart" | "session" => {
            if matches!(resolved.function.name, "groupBy" | "bucket") {
                outputs = argument_fields("field");
            }
            if resolved.function.name == "timeChart" {
                outputs = argument_fields("series");
            }
            if matches!(resolved.function.name, "bucket" | "timeChart") {
                outputs.insert("_bucket".into());
            }
            if let Some(value) = args.get("function") {
                let functions = match value {
                    Value::Array(xs) => xs.as_slice(),
                    _ => std::slice::from_ref(*value),
                };
                for value in functions {
                    let (fields, input) = aggregate_value_outputs(value)?;
                    outputs.extend(fields);
                    retains |= input;
                }
            } else {
                outputs.insert("_count".into());
            }
        }
        "series" => {
            outputs = argument_fields("collect");
            outputs.extend(["@timestamp".into(), "_duration".into()]);
        }
        "neighbor" => {
            if let Some((fields, _)) = resolved.function.named_outputs(args) {
                outputs.extend(fields);
            }
            retains = true;
        }
        "accumulate" | "slidingWindow" | "slidingTimeWindow" | "partition" | "window"
        | "appendAggregation" | "array:reduceAll" => {
            let mut child_retains = false;
            if let Some(value) = args.get("function") {
                let values = match *value {
                    Value::Array(values) => values.as_slice(),
                    value => std::slice::from_ref(value),
                };
                for value in values {
                    let (fields, input) = aggregate_value_outputs(value)?;
                    outputs.extend(fields);
                    child_retains |= input;
                }
            } else {
                outputs.insert("_count".into());
            }
            retains = matches!(
                resolved.function.name,
                "accumulate" | "slidingWindow" | "slidingTimeWindow" | "appendAggregation"
            ) || resolved.function.name == "partition" && child_retains;
        }
        "array:intersection" | "array:union" | "array:reduceColumn" => {
            let default = match resolved.function.name {
                "array:intersection" => "_intersection",
                "array:union" => "_union",
                _ => "_reduceColumn",
            };
            outputs.insert(format!(
                "{}[]",
                args.get("as")
                    .and_then(|v| v.text())
                    .unwrap_or(default)
                    .trim_end_matches("[]")
            ));
        }
        "selfJoin" => {
            outputs.extend(argument_fields("field"));
            outputs.extend(argument_fields("select"));
            outputs.extend(argument_fields("collect"));
        }
        "linReg" => {
            let prefix = args.get("prefix").and_then(|v| v.text()).unwrap_or("_");
            outputs.extend(
                ["slope", "intercept", "r2", "n"]
                    .into_iter()
                    .map(|name| format!("{prefix}{name}")),
            );
        }
        "percentile" => {
            let defaults = Value::Array(vec![
                Value::Number("50".into()),
                Value::Number("75".into()),
                Value::Number("99".into()),
            ]);
            let Value::Array(values) = args.get("percentiles").copied().unwrap_or(&defaults) else {
                return None;
            };
            let prefix = args.get("as").and_then(|v| v.text()).unwrap_or("");
            outputs.extend(
                values
                    .iter()
                    .map(|value| format!("{prefix}_{}", value.render())),
            );
        }
        "top" => {
            outputs = argument_fields("field");
            let default = if args.contains_key("sum") {
                "_sum"
            } else if args.contains_key("max") {
                "_max"
            } else {
                "_count"
            };
            outputs.insert(
                args.get("as")
                    .and_then(|v| v.text())
                    .unwrap_or(default)
                    .into(),
            );
            if args.get("percent").and_then(|v| v.boolean()) == Some(true) {
                outputs.insert("percent".into());
            }
        }
        "count" | "sum" | "avg" | "min" | "max" => {
            outputs.insert(
                args.get("as")
                    .and_then(|v| v.text())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("_{}", resolved.function.name.to_lowercase())),
            );
        }
        _ if resolved.function.effect == Effect::Aggregate => {
            outputs.insert(resolved.function.default_output(args)?);
        }
        _ => return None,
    }
    Some((outputs, retains))
}

/// Direct assignment supplies these transformations' documented output alias.
pub fn aliased_call(field: &str, value: &Value) -> Option<Call> {
    let Value::Call(call) = value else {
        return None;
    };
    let resolved = resolve(call)?;
    if resolved.arguments.contains_key("as") {
        return None;
    }
    if !(matches!(
        resolved.function.name,
        "rename" | "parseUrl" | "eventSize" | "formatTime"
    ) || resolved.function.name == "splitString" && !resolved.arguments.contains_key("index"))
    {
        return None;
    }
    let mut call = call.clone();
    call.args.push(crate::language::ast::Arg {
        name: Some("as".into()),
        value: Value::String(field.into()),
    });
    Some(call)
}

pub fn assigned_aggregate(value: &Value) -> Option<&Call> {
    let Value::Call(call) = value else {
        return None;
    };
    let spec = function(&call.name)?;
    let (outputs, retains) = aggregate_outputs(call)?;
    (spec.effect == Effect::Aggregate
        && !retains
        && outputs.len() == 1
        && outputs.iter().all(|field| !field.contains("[]")))
    .then_some(call)
}

pub fn aggregate_value_outputs(value: &Value) -> Option<(BTreeSet<String>, bool)> {
    match value {
        Value::Call(call) => aggregate_outputs(call),
        Value::Word(name)
            if function(name)
                .is_some_and(|f| f.effect == Effect::Aggregate && f.required.is_empty()) =>
        {
            aggregate_outputs(&Call {
                name: name.clone(),
                args: vec![],
                start: 0,
                end: 0,
            })
        }
        Value::Query { pipeline, .. } => {
            let mut outputs = BTreeSet::new();
            let mut retains = true;
            // Later reducers can replace an uncertain intermediate schema.
            let mut complete = true;
            for original in &pipeline.steps {
                let semantic = original.semantic();
                let step = semantic.as_ref();
                let aliased = match step {
                    Step::Assign { field, value } => aliased_call(field, value).map(Step::Call),
                    _ => None,
                };
                let step = aliased.as_ref().unwrap_or(step);
                match step {
                    Step::Call(call) => {
                        let resolved = resolve(call)?;
                        if let Some((fields, input)) = aggregate_outputs(call) {
                            if !input {
                                outputs.clear();
                                retains = false;
                                complete = true;
                            }
                            outputs.extend(fields);
                        } else if resolved.function.effect == Effect::Aggregate {
                            outputs.clear();
                            retains = false;
                            complete = false;
                        } else if resolved.function.name == "select" {
                            outputs = resolved
                                .arguments
                                .get("fields")
                                .map(|v| fields(v))
                                .unwrap_or_default();
                            retains = false;
                            complete = true;
                        } else if resolved.function.name == "drop" {
                            if retains {
                                complete = false;
                            }
                            if let Some(value) = resolved.arguments.get("fields") {
                                for field in fields(value) {
                                    outputs.remove(&field);
                                }
                            }
                        } else if resolved.function.name == "rename" {
                            if retains {
                                complete = false;
                            }
                            for (old, new) in rename_bindings(&resolved.arguments) {
                                if outputs.remove(old) {
                                    outputs.insert(new.into());
                                }
                            }
                        } else if let Some(writes) = output_writes(call) {
                            if writes.dynamic_prefix.is_some()
                                || resolved.function.name == "lowercase"
                                    && resolved
                                        .arguments
                                        .get("include")
                                        .and_then(|value| value.text())
                                        .is_some_and(|mode| matches!(mode, "fields" | "both"))
                                    && resolved.arguments.get("field").is_some_and(|value| {
                                        fields(value)
                                            .iter()
                                            .any(|field| *field != field.to_lowercase())
                                    })
                            {
                                complete = false;
                            }
                            outputs.extend(writes.fields);
                        }
                    }
                    Step::Assign { field, value } => {
                        if assigned_aggregate(value).is_some() {
                            outputs.clear();
                            retains = false;
                            complete = true;
                        }
                        outputs.insert(field.clone());
                    }
                    Step::Case(branches) => {
                        let incoming = outputs.clone();
                        let mut branch_outputs = BTreeSet::new();
                        let mut retains_branch = false;
                        let mut branches_complete = !branches.is_empty();
                        for branch in branches {
                            let Some((mut fields, input)) =
                                aggregate_value_outputs(&Value::Query {
                                    label: None,
                                    pipeline: branch.clone(),
                                    attributes: vec![],
                                })
                            else {
                                branches_complete = false;
                                retains_branch = true;
                                continue;
                            };
                            if input {
                                fields.extend(incoming.iter().cloned());
                                branches_complete &= complete;
                            }
                            branch_outputs.extend(fields);
                            retains_branch |= input;
                        }
                        outputs = branch_outputs;
                        retains &= retains_branch;
                        complete = branches_complete;
                    }
                    Step::Match { arms, .. } => {
                        let incoming = outputs.clone();
                        let mut branch_outputs = BTreeSet::new();
                        let mut retains_branch = false;
                        let mut branches_complete = !arms.is_empty();
                        for (_, branch) in arms {
                            let Some((mut fields, input)) =
                                aggregate_value_outputs(&Value::Query {
                                    label: None,
                                    pipeline: branch.clone(),
                                    attributes: vec![],
                                })
                            else {
                                branches_complete = false;
                                retains_branch = true;
                                continue;
                            };
                            if input {
                                fields.extend(incoming.iter().cloned());
                                branches_complete &= complete;
                            }
                            branch_outputs.extend(fields);
                            retains_branch |= input;
                        }
                        outputs = branch_outputs;
                        retains &= retains_branch;
                        complete = branches_complete;
                    }
                    _ => {}
                }
            }
            complete.then_some((outputs, retains))
        }
        _ => None,
    }
}

pub fn rename_bindings<'a>(args: &BTreeMap<String, &'a Value>) -> Vec<(&'a str, &'a str)> {
    match args.get("field").copied() {
        Some(Value::Array(pairs)) => pairs
            .iter()
            .filter_map(|pair| match pair {
                Value::Array(pair) if pair.len() == 2 => Some((pair[0].text()?, pair[1].text()?)),
                _ => None,
            })
            .collect(),
        Some(field) => field
            .text()
            .zip(args.get("as").and_then(|v| v.text()))
            .into_iter()
            .collect(),
        _ => vec![],
    }
}

macro_rules! function {
    ($name:literal, $effect:ident, [$($p:literal),*], [$($r:literal),*], [$($a:literal:$t:literal),*]) => {
        function!($name, $effect, [$($p),*], [$($r),*], [$($a:$t),*], concat!("cql:function:",$name))
    };
    ($name:literal, $effect:ident, [$($p:literal),*], [$($r:literal),*], [$($a:literal:$t:literal),*], $reference:expr) => {
        Function { name: $name, positional: &[$($p),*], required: &[$($r),*], arguments: &[$(($a,$t)),*], effect: Effect::$effect, reference_id: $reference, placement: "pipeline; nested use requires the scalar, aggregate or subquery context checked by this profile" }
    };
}

pub static FUNCTIONS: &[Function] = &[
    function!("accumulate", Aggregate, ["function"], ["function"], ["current":"s","function":"g"]),
    function!("appendAggregation", Aggregate, ["function"], ["function"], ["function":"g"]),
    function!("array:append", Transform, ["array"], ["array","values"], ["array":"y","values":"e[]"]),
    function!("array:dedup", Transform, ["array"], ["array"], ["array":"y","asArray":"s"]),
    function!("array:drop", Transform, ["array"], ["array"], ["array":"y"]),
    function!("array:exists", Filter, ["array"], ["array","condition"], ["array":"y","condition":"u","var":"s"]),
    function!("array:intersection", Aggregate, ["array"], ["array"], ["array":"z","as":"s"]),
    function!("array:reduceAll", Aggregate, ["array"], ["array","function","var"], ["array":"y","function":"g","var":"s"]),
    function!("array:reduceColumn", Aggregate, ["array"], ["array","function","var"], ["array":"z","as":"s","function":"g","var":"s"]),
    function!("array:reduceRow", Transform, ["array"], ["array","function","var"], ["array":"z","as":"s","function":"g","var":"s"]),
    function!("array:regex", Filter, ["array"], ["array","regex"], ["array":"z","flags":"s","regex":"r"]),
    function!("array:rename", Transform, ["array"], ["array","asArray"], ["array":"y","asArray":"s"]),
    function!("array:sort", Transform, ["array"], ["array"], ["array":"y","asArray":"s","order":"s","type":"s"]),
    function!("array:union", Aggregate, ["array"], ["array"], ["array":"z","as":"s"]),
    function!("base64Encode", Transform, ["field"], ["field"], ["as":"s","field":"f"]),
    function!("beta:param", Transform, ["parameter"], ["as","parameter"], ["as":"s","parameter":"s"]),
    function!("beta:repeating", Transform, ["interval"], [], ["as":"s","interval":"d"]),
    function!("bitfield:extractFlags", Transform, [], ["field","output"], ["field":"f","onlyTrue":"b","output":"B"]),
    function!("bitfield:extractFlagsAsArray", Transform, [], ["field","flagNames"], ["asArray":"s","field":"f","flagNames":"B"]),
    function!("bitfield:extractFlagsAsString", Transform, [], ["field","flagNames"], ["as":"s","field":"f","flagNames":"B","separator":"s"]),
    function!("callFunction", Aggregate, ["function"], ["field","function"], ["as":"s","field":"f","function":"s"]),
    function!("communityId", Transform, [], ["destinationip","proto","sourceip"], ["as":"s","destinationip":"f","destinationport":"f","icmpcode":"f","icmptype":"f","proto":"f","seed":"i","sourceip":"f","sourceport":"f"]),
    function!("concatArray", Transform, ["field"], ["field"], ["as":"s","field":"z","from":"i","prefix":"s","separator":"s","suffix":"s","to":"i"]),
    function!("counterAsRate", Aggregate, ["field"], ["field"], ["as":"s","field":"f"]),
    function!("crypto:md5", Transform, ["field"], ["field"], ["as":"s","field":"F"]),
    function!("crypto:sha1", Transform, ["field"], ["field"], ["as":"s","field":"F"]),
    function!("crypto:sha256", Transform, ["field"], ["field"], ["as":"s","field":"F"]),
    function!("dropEvent", Filter, [], [], []),
    function!("duration", Transform, ["duration"], ["duration"], ["as":"s","duration":"s"]),
    function!("end", Transform, ["as"], [], ["as":"s"]),
    function!("eventFieldCount", Transform, [], [], ["as":"s"]),
    function!("eventInternals", Transform, [], [], ["prefix":"s"]),
    function!("eventSize", Transform, [], [], ["as":"s"]),
    function!("explain:asTable", Aggregate, [], [], ["showPrefilters":"b"]),
    function!("fieldstats", Aggregate, [], [], ["limit":"i"]),
    function!("formatDuration", Transform, ["field"], ["field"], ["as":"s","field":"f","from":"s","precision":"i"]),
    function!("geography:distance", Transform, [], ["lat1","lat2","lon1","lon2"], ["as":"s","lat1":"f","lat2":"f","lon1":"f","lon2":"f"]),
    function!("geohash", Transform, [], [], ["as":"s","lat":"f","lon":"f","precision":"i"]),
    function!("hash", Transform, ["field"], ["field"], ["as":"s","field":"F","limit":"i","seed":"s"]),
    function!("hashMatch", Filter, ["input"], ["input","salt"], ["bits":"i","field":"f","hash":"s","input":"s","salt":"s"]),
    function!("hashRewrite", Transform, ["field"], ["field","salt"], ["as":"s","bits":"i","field":"f","hash":"s","replaceInRawstring":"b","salt":"s"]),
    function!("json:prettyPrint", Transform, ["field"], [], ["as":"s","field":"f","step":"i","strict":"b"]),
    function!("linReg", Aggregate, [], ["x","y"], ["prefix":"s","x":"f","y":"f"]),
    function!("lowercase", Transform, ["field"], ["field"], ["field":"F","include":"s","locale":"s"]),
    function!("matchAsArray", Transform, ["file"], ["field","file","nrows"], ["asArray":"s","column":"F","field":"F","file":"s","ignoreCase":"b","include":"F","nrows":"i","strict":"b"]),
    function!("now", Transform, ["as"], [], ["as":"s"]),
    function!("parseCEF", Transform, ["field"], [], ["field":"f","headerprefix":"s","keeplabels":"b","labelprefix":"s","prefix":"s"]),
    function!("parseCsv", Transform, ["field"], ["columns"], ["columns":"F","delimiter":"s","excludeEmpty":"b","field":"f","trim":"b"]),
    function!("parseFixedWidth", Transform, ["field"], ["columns","widths"], ["columns":"F","field":"f","trim":"b","widths":"N"]),
    function!("parseHexString", Transform, ["field"], ["field"], ["as":"s","charset":"s","field":"f"]),
    function!("parseInt", Transform, ["field"], ["field"], ["as":"s","endian":"s","field":"f","radix":"v"]),
    function!("parseLEEF", Transform, ["field"], [], ["delimiter":"s","field":"f","headerprefix":"s","keeplabels":"b","labelprefix":"s","parsetime":"b","prefix":"s","timezone":"s"]),
    function!("parseTimestamp", Transform, ["format"], ["field"], ["addErrors":"b","as":"s","caseSensitive":"b","field":"f","format":"s","timezone":"s","timezoneAs":"s","timezoneField":"f"]),
    function!("parseUri", Transform, ["field"], ["field"], ["defaultBase":"s","field":"f","prefix":"s"]),
    function!("parseUrl", Transform, ["field"], [], ["as":"s","field":"f"]),
    function!("parseXml", Transform, ["field"], [], ["field":"f","prefix":"s","strict":"b"]),
    function!("partition", Aggregate, ["function"], ["condition","function"], ["condition":"u","function":"g","split":"s"]),
    function!("percentage", Aggregate, ["condition"], ["condition"], ["as":"s","condition":"u"]),
    function!("percentile", Aggregate, ["field"], ["field"], ["accuracy":"n","as":"s","field":"f","percentiles":"N"]),
    function!("range", Aggregate, ["field"], ["field"], ["as":"s","field":"f"]),
    function!("readPersistedAggregation", Source, ["tag"], ["tag"], ["end":"s","span":"s","start":"s","tag":"s","timezone":"s","view":"s"]),
    function!("remoteTable", Definition, [], ["connection","name","query"], ["connection":"s","end":"s","include":"F","name":"s","start":"s","query":"R"]),
    function!("reverseDns", Transform, ["field"], ["field"], ["as":"s","field":"f","limit":"i","server":"s"]),
    function!("sample", Filter, ["percentage"], [], ["field":"f","percentage":"n"]),
    function!("sankey", Aggregate, [], ["source","target"], ["source":"f","target":"f","weight":"g"]),
    function!("selfJoin", Aggregate, ["field"], ["field","where"], ["collect":"F","field":"F","limit":"i","postfilter":"b","prefilter":"b","select":"F","where":"q[]"]),
    function!("setField", Transform, [], ["target","value"], ["target":"e","value":"e"]),
    function!("setTimeInterval", Definition, [], ["start"], ["end":"t","start":"t","timezone":"s"]),
    function!("slidingTimeWindow", Aggregate, ["function"], ["function","span"], ["current":"s","function":"g","span":"t","timestampfield":"f"]),
    function!("slidingWindow", Aggregate, ["function"], ["events","function"], ["current":"s","events":"i","function":"g"]),
    function!("start", Transform, ["as"], [], ["as":"s"]),
    function!("stdDev", Aggregate, ["field"], ["field"], ["as":"s","field":"f"]),
    function!("stripAnsiCodes", Transform, ["field"], [], ["as":"s","field":"f"]),
    function!("subnet", Transform, ["field"], ["bits","field"], ["as":"s","bits":"i","field":"f"]),
    function!("tokenHash", Transform, ["field"], ["field"], ["as":"s","field":"f"]),
    function!("transpose", Aggregate, ["pivot"], [], ["column":"f","header":"f","limit":"i","pivot":"f"]),
    function!("unit:convert", Transform, ["field"], ["field"], ["as":"s","binary":"b","field":"f","from":"s","keepUnit":"b","to":"s","unit":"s"]),
    function!("urlEncode", Transform, ["field"], ["field"], ["as":"s","field":"f"]),
    function!("window", Aggregate, ["function"], [], ["buckets":"i","function":"g","span":"d"]),
    function!("worldMap", Aggregate, [], [], ["ip":"f","lat":"f","lon":"f","magnitude":"g","precision":"i"]),
    function!("writeJson", Transform, ["field"], [], ["as":"s","field":"F"]),
    function!("xml:prettyPrint", Transform, ["field"], [], ["as":"s","field":"f","step":"i","strict":"b","width":"i"]),
    function!("math:abs", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:arccos", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:arcsin", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:arctan", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:arctan2", Transform, [], ["x","y"], ["x":"f","y":"f","as":"s"]),
    function!("math:ceil", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:cos", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:cosh", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:deg2rad", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:exp", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:expm1", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:floor", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:log", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:log10", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:log1p", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:log2", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:mod", Transform, ["field"], ["field","divisor"], ["field":"f","divisor":"e","as":"s"]),
    function!("math:pow", Transform, ["field"], ["field","exponent"], ["field":"f","exponent":"e","as":"s"]),
    function!("math:rad2deg", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:sin", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:sinh", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:sqrt", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:tan", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("math:tanh", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("text:contains", Filter, ["string"], ["string","substring"], ["string":"e","substring":"e"]),
    function!("text:endsWith", Filter, ["string"], ["string","substring"], ["string":"e","substring":"e"]),
    function!("text:startsWith", Filter, ["string"], ["string","substring"], ["string":"e","substring":"e"]),
    function!("text:editDistance", Transform, [], ["maxDistance","reference","target"], ["allowTranspositions":"b","as":"s","ignoreCase":"b","maxDistance":"i","reference":"e","target":"e"]),
    function!("text:editDistanceAsArray", Transform, [], ["maxDistance","references","target"], ["allowTranspositions":"b","asArray":"y","ignoreCase":"b","maxDistance":"i","references":"e[]","target":"e"]),
    function!("text:length", Transform, ["string"], ["string"], ["string":"e","as":"s"]),
    function!("text:positionOf", Transform, ["string"], ["string","character"], ["string":"e","character":"e","begin":"e","occurrence":"e","as":"s"]),
    function!("text:substring", Transform, ["string"], ["string"], ["string":"e","begin":"e","end":"e","as":"s"]),
    function!("text:trim", Transform, ["field"], ["field"], ["field":"e","mode":"s","as":"s"]),
    function!("time:dayOfMonth", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:dayOfWeek", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:dayOfWeekName", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:dayOfYear", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:hour", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:millisecond", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:minute", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:month", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:monthName", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:second", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:weekOfYear", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("time:year", Transform, ["field"], [], ["field":"f","as":"s","timezone":"s","timezoneField":"f"]),
    function!("cidr", Filter, ["field"], ["field"], ["field":"f","subnet":"S","file":"s","column":"f","negate":"b"]),
    function!("base64Decode", Transform, ["field"], ["field"], ["field":"f","as":"s","charset":"s"]),
    function!("urlDecode", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("shannonEntropy", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("splitString", Transform, ["field"], ["by"], ["field":"f","by":"r","as":"s","index":"j"]),
    function!("coalesce", Transform, ["expressions"], ["expressions"], ["expressions":"e[]","as":"s","ignoreEmpty":"b"]),
    function!("getField", Transform, ["source"], ["source"], ["source":"e","as":"s"]),
    function!("count", Aggregate, ["field"], [], ["field":"f","distinct":"b","as":"s"]),
    function!("groupBy", Aggregate, ["field"], ["field"], ["field":"F","function":"g","limit":"l"]),
    function!("stats", Aggregate, ["function"], [], ["function":"g"]),
    function!("sum", Aggregate, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("avg", Aggregate, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("min", Aggregate, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("max", Aggregate, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("collect", Aggregate, ["fields"], ["fields"], ["fields":"F","limit":"i","multival":"b","separator":"s"]),
    function!("selectFromMax", Aggregate, ["field"], ["field","include"], ["field":"f","include":"F"]),
    function!("selectFromMin", Aggregate, ["field"], ["field","include"], ["field":"f","include":"F"]),
    function!("selectLast", Aggregate, ["fields"], ["fields"], ["fields":"F"]),
    function!("top", Aggregate, ["field"], ["field"], ["field":"F","as":"s","error":"n","limit":"i","max":"f","percent":"b","rest":"s","sum":"f"]),
    function!("formatTime", Transform, ["format"], ["format","as"], ["format":"s","as":"s","field":"e","locale":"s","timezone":"s","unit":"s"]),
    function!("select", Transform, ["fields"], ["fields"], ["fields":"F"]),
    function!("drop", Transform, ["fields"], ["fields"], ["fields":"F"]),
    function!("rename", Transform, ["field"], ["field"], ["field":"P","as":"s"]),
    function!("head", Aggregate, ["limit"], [], ["limit":"l"]),
    function!("tail", Aggregate, ["limit"], [], ["limit":"l"]),
    function!("sort", Aggregate, ["field"], [], ["field":"F","order":"S","type":"S","limit":"l","reverse":"b"]),
    function!("table", Aggregate, ["fields"], ["fields"], ["fields":"F","sortby":"F","order":"S","type":"S","limit":"l","reverse":"b"]),
    function!("fieldset", Aggregate, [], [], []),
    function!("eval", Transform, [], [], []),
    function!("test", Filter, ["expression"], ["expression"], ["expression":"p"]),
    function!("in", Filter, ["field"], ["field","values"], ["field":"f","values":"s[]","ignoreCase":"b"]),
    function!("wildcard", Filter, ["pattern"], ["pattern"], ["pattern":"s","field":"F","ignoreCase":"b","includeEverythingOnAsterisk":"b"]),
    function!("regex", Filter, ["regex"], ["regex"], ["regex":"r","field":"f","flags":"s","strict":"b","repeat":"b","limit":"i"]),
    function!("parseJson", Transform, ["field"], [], ["field":"f","prefix":"s","exclude":"F","include":"F","handleNull":"s","removePrefixes":"s[]","excludeEmpty":"b"]),
    function!("kvParse", Transform, ["field"], [], ["field":"F","as":"s","prefix":"s","excludeEmpty":"b","onDuplicate":"s","separator":"s","separatorPadding":"s","override":"b"]),
    function!("lower", Transform, ["field"], ["field"], ["field":"f","as":"s","locale":"s","type":"s"]),
    function!("upper", Transform, ["field"], ["field"], ["field":"f","as":"s","locale":"s"]),
    function!("length", Transform, ["field"], ["field"], ["field":"f","as":"s"]),
    function!("concat", Transform, ["field"], ["field"], ["field":"F","as":"s"]),
    function!("split", Transform, ["field"], [], ["field":"f","strip":"b"]),
    function!("replace", Transform, ["regex"], ["regex"], ["regex":"r","with":"s","field":"f","as":"s","flags":"s"]),
    function!("format", Transform, ["format"], ["format","field"], ["format":"s","field":"F","as":"s","timezone":"s"]),
    function!("default", Transform, ["value"], ["field","value"], ["field":"F","value":"v","replaceEmpty":"b"]),
    function!("round", Transform, ["field"], ["field"], ["field":"f","as":"s","how":"s"]),
    function!("if", Transform, ["condition"], ["condition","then","else"], ["condition":"e","then":"e","else":"e","as":"s"]),
    function!("array:contains", Filter, ["array"], ["array","value"], ["array":"f","value":"s"], "cql:function:array.contains"),
    function!("array:length", Transform, ["array"], ["array"], ["array":"f","as":"s"], "cql:function:array.length"),
    function!("array:eval", Transform, ["array"], ["array","function"], ["array":"y","asArray":"y","function":"u","var":"s"], "cql:function:array.eval"),
    function!("array:filter", Transform, ["array"], ["array","function"], ["array":"y","asArray":"y","function":"u","var":"s"], "cql:function:array.filter"),
    function!("objectArray:eval", Transform, ["array"], ["array","asArray","function"], ["array":"y","asArray":"y","function":"u","var":"s"], "cql:function:objectArray.eval"),
    function!("objectArray:exists", Filter, ["array"], ["array","condition"], ["array":"y","condition":"u","var":"s"], "cql:function:objectArray.exists"),
    function!("join", Filter, ["query"], ["query","field"], ["query":"q","field":"F","key":"F","include":"F","mode":"s","limit":"i","max":"i","start":"s","end":"s","view":"s","repo":"s","live":"b"]),
    function!("defineTable", Definition, ["query"], ["name","query","include"], ["name":"s","query":"q","include":"F","start":"s","end":"s","view":"s"]),
    function!("match", Filter, ["file"], ["file","field"], ["file":"s","table":"s","field":"F","column":"F","include":"F","strict":"b","ignoreCase":"b","nrows":"l","glob":"b","mode":"s"]),
    function!("asn", Transform, ["field"], [], ["field":"f","as":"s"]),
    function!("ipLocation", Transform, ["field"], [], ["field":"f","as":"s"]),
    function!("ioc:lookup", Filter, ["field"], ["field","type"], ["field":"F","type":"s","confidenceThreshold":"s","include":"s[]","prefix":"s","strict":"b"], "cql:function:ioc.lookup"),
    function!("selfJoinFilter", Filter, ["field"], ["field","where"], ["field":"F","where":"q[]","prefilter":"b"]),
    function!("correlate", Correlation, ["query"], ["query"], ["query":"Q","sequence":"b","within":"d","globalConstraints":"F","sequenceBy":"F","root":"s","maxPerRoot":"i","iterationLimit":"i","jitterTolerance":"d","includeConstraintValues":"b","includeMatchesOnceOnly":"b","aggregation":"Q[]"]),
    function!("bucket", Aggregate, ["span"], [], ["field":"F","function":"g","span":"s","buckets":"i","limit":"i","minSpan":"s","timezone":"s","unit":"S"]),
    function!("timeChart", Aggregate, ["series"], [], ["series":"f","function":"g","span":"s","buckets":"i","limit":"i","minSpan":"s","timezone":"s","unit":"S"]),
    function!("session", Aggregate, ["function"], [], ["function":"g","maxpause":"d"]),
    function!("series", Aggregate, ["collect"], ["collect"], ["collect":"F","maxpause":"d","maxduration":"d","startmatch":"q","endmatch":"q","separator":"s","memlimit":"s"]),
    function!("neighbor", Aggregate, ["include"], ["include"], ["include":"F","direction":"s","distance":"i","prefix":"s"]),
    function!("readFile", Source, ["file"], ["file"], ["file":"S","table":"S","include":"F","limit":"i"]),
    function!("findTimestamp", Transform, [], [], ["addErrors":"b","as":"s","field":"f","timezone":"s","timezoneField":"f","timezoneAs":"s"]),
    function!("createEvents", Source, ["rawstring"], ["rawstring"], ["rawstring":"s[]"]),
];

pub fn function(name: &str) -> Option<&'static Function> {
    FUNCTIONS.iter().find(|f| f.name.eq_ignore_ascii_case(name))
}

pub fn search(term: &str, limit: usize, full: bool) -> Vec<serde_json::Value> {
    let needle = term
        .strip_prefix("cql:function:")
        .and_then(function)
        .map(|f| f.reference_id)
        .unwrap_or(term)
        .to_lowercase();
    let mut matches = records()
        .iter()
        .filter(|r| {
            format!("{} {} {}", r.id, r.title, r.body)
                .to_lowercase()
                .contains(&needle)
        })
        .collect::<Vec<_>>();
    matches.sort_by_key(|r| !r.id.eq_ignore_ascii_case(&needle));
    matches.into_iter().take(limit.min(250)).map(|r| {
        if full { serde_json::to_value(r).expect("record serializes") }
        else { serde_json::json!({"id":r.id,"title":r.title,"topic":r.topic,"sources":r.sources,"supported":r.id.strip_prefix("cql:function:").and_then(function).is_some() || FUNCTIONS.iter().any(|f|f.reference_id == r.id)}) }
    }).collect()
}

pub fn standard(name: &str, term: &str, limit: usize) -> Vec<serde_json::Value> {
    let data = match name {
        "cps" => include_str!("../data/corpus/standards/cps.jsonl"),
        "ecs" => include_str!("../data/corpus/standards/ecs.jsonl"),
        _ => return vec![],
    };
    let needle = term.to_lowercase();
    let mut out = Vec::new();
    for line in data.lines().filter(|s| !s.trim().is_empty()) {
        let record: serde_json::Value =
            serde_json::from_str(line).expect("bundled standards are verified");
        if name == "cps" {
            if record.to_string().to_lowercase().contains(&needle) {
                let mut record = record;
                record["evidence"] =
                    serde_json::json!("documented standard; not deployment observation");
                out.push(record);
            }
        } else if let Some(fields) = record["fields"].as_array() {
            for field in fields.iter().filter(|f| {
                f.as_str()
                    .is_some_and(|s| s.to_lowercase().contains(&needle))
            }) {
                out.push(serde_json::json!({"standard":"ecs","version":record["version"],"path":field,"field_set":record["field_set"],"provenance_ids":record["provenance_ids"],"evidence":"documented standard; not deployment observation"}));
                if out.len() >= limit {
                    return out;
                }
            }
        }
        if out.len() >= limit {
            break;
        }
    }
    out
}
