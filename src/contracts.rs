//! Versioned data contracts. Network and filesystem capabilities never enter a plan.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PROFILE: &str = "cqlmatic-ngsiem-core-v2";
pub const MAX_REQUEST_BYTES: usize = 1_048_576;
pub const MAX_QUERY_UTF16: usize = 66_000;
pub const MAX_DEPTH: usize = 64;
pub const MAX_NODES: usize = 16_384;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Scope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub type_: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum Scalar {
    Text(String),
    Number(serde_json::Number),
    Boolean(bool),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Missing {
    Include,
    Exclude,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Predicate {
    Like {
        field: String,
        pattern: String,
    },
    Call {
        function: String,
        #[serde(default)]
        args: Vec<Argument>,
        #[serde(default)]
        negate: bool,
    },
    Eq {
        field: String,
        value: Scalar,
    },
    NotEq {
        field: String,
        value: Scalar,
        missing: Missing,
    },
    Glob {
        field: String,
        pattern: String,
        #[serde(default)]
        negate: bool,
        #[serde(default)]
        missing: Option<Missing>,
    },
    Regex {
        field: String,
        pattern: String,
        #[serde(default)]
        flags: String,
        #[serde(default)]
        negate: bool,
        #[serde(default)]
        missing: Option<Missing>,
    },
    Exists {
        field: String,
        #[serde(default = "yes")]
        present: bool,
    },
    Compare {
        field: String,
        operator: Comparison,
        value: serde_json::Number,
    },
    Fields {
        left: String,
        operator: Comparison,
        right: String,
    },
    All {
        items: Vec<Predicate>,
    },
    Any {
        items: Vec<Predicate>,
    },
}

pub fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Comparison {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Expression {
    Compare {
        left: Box<Expression>,
        operator: Comparison,
        right: Box<Expression>,
    },
    Unary {
        operator: Unary,
        value: Box<Expression>,
    },
    Field {
        name: String,
    },
    Literal {
        value: Scalar,
    },
    Binary {
        left: Box<Expression>,
        operator: Arithmetic,
        right: Box<Expression>,
    },
    Call {
        function: String,
        #[serde(default)]
        args: Vec<Argument>,
    },
    Array {
        items: Vec<Expression>,
    },
    Query {
        plan: Box<Plan>,
    },
    NamedQuery {
        name: String,
        plan: Box<Plan>,
        #[serde(default)]
        include: Option<QueryFields>,
        #[serde(default)]
        after: Option<TimeAnchor>,
        #[serde(default)]
        before: Option<TimeAnchor>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum QueryFields {
    All,
    Fields { fields: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TimeAnchor {
    pub query: String,
    #[serde(default)]
    pub offset: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Unary {
    Negative,
    Positive,
    Not,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum MatchPattern {
    Literal {
        value: Scalar,
    },
    Glob {
        pattern: String,
    },
    Regex {
        pattern: String,
        #[serde(default)]
        flags: String,
    },
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub plan: Plan,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Arithmetic {
    Add,
    Subtract,
    Multiply,
    Divide,
    Remainder,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Argument {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub value: Expression,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Accuracy {
    #[default]
    Exact,
    Estimate,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Order {
    Asc,
    #[default]
    Desc,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum SortType {
    #[default]
    Number,
    String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "function", rename_all = "lowercase", deny_unknown_fields)]
pub enum Aggregate {
    Count {
        #[serde(default)]
        field: Option<String>,
        #[serde(default)]
        distinct: bool,
        #[serde(default)]
        accuracy: Accuracy,
        #[serde(default = "count_alias", rename = "as")]
        as_: String,
    },
    Sum {
        field: String,
        #[serde(rename = "as")]
        as_: String,
    },
    Avg {
        field: String,
        #[serde(rename = "as")]
        as_: String,
    },
    Min {
        field: String,
        #[serde(rename = "as")]
        as_: String,
    },
    Max {
        field: String,
        #[serde(rename = "as")]
        as_: String,
    },
}
pub fn count_alias() -> String {
    "_count".into()
}
pub fn default_limit() -> u32 {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "op", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Stage {
    Link {
        field: String,
        to: String,
        key: String,
    },
    Case {
        branches: Vec<Plan>,
    },
    Match {
        field: String,
        arms: Vec<MatchArm>,
    },
    Filter {
        predicate: Predicate,
    },
    Count {
        #[serde(default)]
        field: Option<String>,
        #[serde(default)]
        distinct: bool,
        #[serde(default)]
        accuracy: Accuracy,
        #[serde(default = "count_alias", rename = "as")]
        as_: String,
    },
    Group {
        keys: Vec<String>,
        aggregates: Vec<Aggregate>,
    },
    Select {
        fields: Vec<String>,
    },
    Sort {
        fields: Vec<String>,
        #[serde(default)]
        order: Order,
        #[serde(default, rename = "type")]
        type_: SortType,
        #[serde(default = "default_limit")]
        limit: u32,
    },
    Head {
        limit: u32,
    },
    Tail {
        limit: u32,
    },
    Assign {
        field: String,
        expression: Expression,
    },
    Call {
        function: String,
        #[serde(default)]
        args: Vec<Argument>,
    },
    Join {
        query: Box<Plan>,
        field: String,
        key: String,
        include: Vec<String>,
        #[serde(default)]
        left: bool,
        #[serde(default = "default_limit")]
        limit: u32,
        max_matches: u32,
    },
    Correlate {
        queries: Vec<NamedPlan>,
        links: Vec<Link>,
        #[serde(default)]
        sequence: bool,
        within: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NamedPlan {
    pub name: String,
    pub plan: Plan,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Link {
    pub from: String,
    pub field: String,
    pub to: String,
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub source: Scope,
    #[serde(default)]
    pub filters: Vec<Predicate>,
    #[serde(default)]
    pub steps: Vec<Stage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(
    tag = "recipe",
    content = "params",
    rename_all = "kebab-case",
    deny_unknown_fields
)]
pub enum Recipe {
    Events {
        source: Scope,
        #[serde(default)]
        filters: Vec<Predicate>,
        #[serde(default)]
        fields: Vec<String>,
        #[serde(default = "default_limit")]
        limit: u32,
    },
    CountEvents {
        source: Scope,
        #[serde(default)]
        filters: Vec<Predicate>,
    },
    CountBy {
        source: Scope,
        #[serde(default)]
        filters: Vec<Predicate>,
        keys: Vec<String>,
    },
    Distinct {
        source: Scope,
        #[serde(default)]
        filters: Vec<Predicate>,
        field: String,
        accuracy: Accuracy,
    },
    TopValues {
        source: Scope,
        #[serde(default)]
        filters: Vec<Predicate>,
        field: String,
        limit: u32,
    },
    Threshold {
        source: Scope,
        #[serde(default)]
        filters: Vec<Predicate>,
        keys: Vec<String>,
        minimum: u64,
    },
    Pivot {
        source: Scope,
        #[serde(default)]
        filters: Vec<Predicate>,
        field: String,
        value: Scalar,
        #[serde(default)]
        fields: Vec<String>,
        #[serde(default = "default_limit")]
        limit: u32,
    },
    JoinPivot {
        source: Scope,
        #[serde(default)]
        filters: Vec<Predicate>,
        query: Box<Plan>,
        field: String,
        key: String,
        include: Vec<String>,
        #[serde(default)]
        left: bool,
        #[serde(default = "default_limit")]
        limit: u32,
        max_matches: u32,
    },
    Sequence {
        queries: Vec<NamedPlan>,
        links: Vec<Link>,
        within: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
pub enum GenerateInput {
    Plan { plan: Plan },
    Recipe { request: Recipe },
    English { text: String, source: Scope },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Context {
    #[serde(default)]
    pub environment: Option<EnvironmentId>,
    #[serde(default)]
    pub confirmed_fields: Vec<String>,
    #[serde(default)]
    pub require_observed: bool,
    #[serde(default)]
    pub accept_mixed_record_kinds: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EnvironmentId {
    pub cloud: Cloud,
    pub tenant: String,
    pub view: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Cloud {
    Us1,
    Us2,
    Eu1,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GenerateRequest {
    pub schema_version: u32,
    #[serde(default)]
    pub id: Option<String>,
    pub input: GenerateInput,
    #[serde(default)]
    pub context: Context,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct CheckRequest {
    pub schema_version: u32,
    #[serde(default)]
    pub id: Option<String>,
    pub query: String,
    #[serde(default)]
    pub context: Context,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Ready,
    NeedsContext,
    Unsupported,
    Invalid,
    Error,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Acceptance {
    Accepted,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Validation {
    pub status: Acceptance,
    pub profile_id: String,
    pub execution: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Grounding {
    pub pack_id: Option<String>,
    pub basis: BTreeMap<String, String>,
    pub missing: Vec<String>,
    pub conflicting: Vec<String>,
    pub stale: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Diagnostic {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<Box<Span>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(skip)]
    pub(crate) class: DiagnosticClass,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum DiagnosticClass {
    #[default]
    Rule,
    Coverage,
}

impl Diagnostic {
    pub fn error(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            severity: Severity::Error,
            message: message.into(),
            span: None,
            pointer: None,
            reference: None,
            class: DiagnosticClass::Rule,
        }
    }
    pub fn warning(code: &str, message: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            ..Self::error(code, message)
        }
    }
    pub fn unsupported(code: &str, message: impl Into<String>) -> Self {
        Self {
            class: DiagnosticClass::Coverage,
            ..Self::error(code, message)
        }
    }
    pub fn is_unsupported(&self) -> bool {
        self.class == DiagnosticClass::Coverage
    }
    pub fn at(mut self, pointer: &str) -> Self {
        self.pointer = Some(pointer.into());
        self
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Interpretation {
    pub path: String,
    pub plan: Option<Plan>,
    pub assumptions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct Analysis {
    pub schema_version: u32,
    pub id: Option<String>,
    pub outcome: Outcome,
    pub query: Option<String>,
    pub validation: Validation,
    pub grounding: Grounding,
    pub interpretation: Interpretation,
    pub diagnostics: Vec<Diagnostic>,
}

impl Analysis {
    pub fn failed(id: Option<String>, diagnostic: Diagnostic) -> Self {
        Self {
            schema_version: 1,
            id,
            outcome: Outcome::Error,
            query: None,
            validation: Validation {
                status: Acceptance::Unknown,
                profile_id: PROFILE.into(),
                execution: "not-run".into(),
            },
            grounding: Grounding::default(),
            interpretation: Interpretation {
                path: "input".into(),
                plan: None,
                assumptions: vec![],
            },
            diagnostics: vec![diagnostic],
        }
    }
    pub fn exit_code(&self, fail_on_warning: bool) -> i32 {
        if self.outcome == Outcome::Error {
            2
        } else if self.outcome != Outcome::Ready
            || (fail_on_warning
                && self
                    .diagnostics
                    .iter()
                    .any(|d| d.severity == Severity::Warning))
        {
            1
        } else {
            0
        }
    }
}
