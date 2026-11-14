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
    let mut position = 0;
    let mut named_query = false;
    for arg in &call.args {
        if function.name == "correlate"
            && (arg.name.is_none() || arg.name.as_deref() == Some("query"))
            && matches!(arg.value, Value::Query { label: Some(_), .. })
        {
            if arg.name.is_some() && std::mem::replace(&mut named_query, true) {
                resolved.diagnostics.push(Diagnostic::error(
                    "CQL_DUPLICATE_ARGUMENT",
                    "Duplicate named query parameter; use the native unnamed query list.",
                ));
            }
            resolved.queries.push(&arg.value);
            continue;
        }
        let name = if let Some(name) = &arg.name {
            name.as_str()
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
            ("match" | "readFile", "table") => "file",
            ("join", "repo") => "view",
            _ => name,
        };
        if resolved.arguments.contains_key(name) {
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
            && args.get("includeConstraintValues").copied() != Some(&Value::Bool(false))
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
            && args.get("includeConstraintValues").copied() != Some(&Value::Bool(false))
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
            source_path: "cqlmatic/profile/core-v2".into(),
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
impl Function {
    pub fn argument_role(&self, name: &str) -> ArgumentRole {
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
                | ("match", "column" | "include")
                | ("readFile", "include")
                | ("cidr", "column")
        ) {
            return ArgumentRole::ForeignField;
        }
        match self.argument_kind(name) {
            Some("f" | "F" | "A" | "y") => ArgumentRole::InputField,
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
            (self.name, name),
            ("series", "startmatch" | "endmatch") | ("selfJoinFilter", "where")
        ) {
            return ArgumentScope::Continuing;
        }
        match self.argument_kind(name) {
            Some("q" | "Q" | "Q[]") => ArgumentScope::Independent,
            Some("q[]") => ArgumentScope::FilterQuery,
            Some("u") => ArgumentScope::Inline,
            Some("g") => ArgumentScope::Aggregate,
            _ => ArgumentScope::Value,
        }
    }
    pub fn resolved_effect(&self, args: &BTreeMap<String, &Value>) -> Effect {
        if self.name == "ioc:lookup" {
            return if args.get("strict").copied() == Some(&Value::Bool(true)) {
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
            && args.get("strict").copied() == Some(&Value::Bool(false))
        {
            Effect::Transform
        } else {
            self.effect
        }
    }
    pub fn scalar_computation(&self) -> bool {
        matches!(
            self.name,
            "in" | "wildcard" | "regex" | "cidr" | "array:contains" | "objectArray:exists"
        ) || matches!(
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
        )
    }
    pub fn default_read_fields(&self, args: &BTreeMap<String, &Value>) -> &'static [&'static str] {
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
            "regex" | "replace" | "parseJson" | "kvParse" | "splitString" => &["@rawstring"],
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
            let prefix = args.get("prefix").and_then(|v| v.text()).unwrap_or("");
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
        let output = match self.name {
            "lower" => "_lower",
            "upper" => "_upper",
            "length" | "array:length" => "_length",
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
                    if let Value::Call(call) = value
                        && let Some((fields, input)) = aggregate_outputs(call)
                    {
                        outputs.extend(fields);
                        retains |= input;
                    }
                }
            } else {
                outputs.insert("_count".into());
            }
        }
        "series" => {
            outputs = argument_fields("collect");
            outputs.extend(["@timestamp".into(), "_duration".into()]);
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
            if args.get("percent").copied() == Some(&Value::Bool(true)) {
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
        _ => return None,
    }
    Some((outputs, retains))
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
    function!("selectLast", Aggregate, ["fields"], ["fields"], ["fields":"A"]),
    function!("top", Aggregate, ["field"], ["field"], ["field":"F","as":"s","error":"n","limit":"i","max":"f","percent":"b","rest":"s","sum":"f"]),
    function!("formatTime", Transform, ["format"], ["format"], ["format":"s","as":"s","field":"e","locale":"s","timezone":"s","unit":"s"]),
    function!("select", Transform, ["fields"], ["fields"], ["fields":"F"]),
    function!("drop", Transform, ["fields"], ["fields"], ["fields":"A"]),
    function!("rename", Transform, ["field"], ["field"], ["field":"P","as":"s"]),
    function!("head", Aggregate, ["limit"], [], ["limit":"l"]),
    function!("tail", Aggregate, ["limit"], [], ["limit":"l"]),
    function!("sort", Aggregate, ["field"], [], ["field":"F","order":"S","type":"S","limit":"i","reverse":"b"]),
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
    function!("default", Transform, ["field"], ["field","value"], ["field":"F","value":"v","replaceEmpty":"b"]),
    function!("round", Transform, ["field"], ["field"], ["field":"f","as":"s","how":"s"]),
    function!("if", Transform, ["condition"], ["condition","then","else"], ["condition":"e","then":"e","else":"e","as":"s"]),
    function!("array:contains", Filter, ["array"], ["array","value"], ["array":"f","value":"e"], "cql:function:array.contains"),
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
    function!("neighbor", Transform, ["include"], ["include"], ["include":"F","direction":"s","distance":"i","prefix":"s"]),
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
    matches.into_iter().take(limit.min(100)).map(|r| {
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
