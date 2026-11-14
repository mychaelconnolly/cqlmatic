use crate::contracts::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Word(String),
    String(String),
    Number(String),
    Bool(bool),
    Array(Vec<Value>),
    Call(Call),
    Query {
        label: Option<String>,
        pipeline: Pipeline,
        attributes: Vec<Arg>,
    },
    Binary {
        left: Box<Value>,
        op: String,
        right: Box<Value>,
    },
    Unary {
        op: String,
        value: Box<Value>,
    },
    Parameter(String),
}
#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    pub name: Option<String>,
    pub value: Value,
}
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub name: String,
    pub args: Vec<Arg>,
    pub start: usize,
    pub end: usize,
}
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    Compare {
        field: String,
        op: String,
        value: Pattern,
    },
    And(Vec<Filter>),
    Or(Vec<Filter>),
    Not(Box<Filter>),
    Call(Call),
    Text(Pattern),
    Bool(bool),
}
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    Any,
    Text(String),
    Regex { pattern: String, flags: String },
    Parameter(String),
}
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Filter(Filter),
    Call(Call),
    Assign {
        field: String,
        value: Value,
    },
    Case(Vec<Pipeline>),
    Match {
        field: String,
        arms: Vec<(Pattern, Pipeline)>,
    },
    Link {
        field: String,
        target: String,
    },
}
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Pipeline {
    pub steps: Vec<Step>,
}

pub fn quote(s: &str) -> String {
    serde_json::to_string(s).expect("string serializes")
}
pub fn escape_regex_delimiter(pattern: &str) -> String {
    let mut out = String::new();
    let mut escaped = false;
    for ch in pattern.chars() {
        if ch == '/' && !escaped {
            out.push('\\');
        }
        out.push(ch);
        escaped = ch == '\\' && !escaped;
    }
    out
}
pub fn bare_field(s: &str) -> bool {
    if matches!(s, "true" | "false") {
        return false;
    }
    let mut chars = s.chars().peekable();
    if chars.peek().is_some_and(|c| matches!(c, '@' | '#')) {
        chars.next();
    }
    if !chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') {
        return false;
    }
    while let Some(c) = chars.next() {
        if c.is_alphanumeric() || c == '_' {
            continue;
        }
        if c == '.' {
            if !chars
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            {
                return false;
            }
            continue;
        }
        if c == '[' {
            let mut n = 0;
            while chars.peek().is_some_and(|c| c.is_ascii_digit()) {
                chars.next();
                n += 1;
            }
            if n == 0 || chars.next() != Some(']') {
                return false;
            }
            if chars.peek().is_some_and(|c| !matches!(c, '.' | '[')) {
                return false;
            }
            continue;
        }
        return false;
    }
    true
}
pub(super) fn reserved_field_prefix(s: &str) -> bool {
    let prefix = s.split(['.', '[']).next().unwrap_or(s);
    ["not", "and", "or"]
        .iter()
        .any(|keyword| prefix.eq_ignore_ascii_case(keyword))
}
pub fn field(s: &str) -> String {
    if bare_field(s) && !reserved_field_prefix(s) {
        s.into()
    } else {
        quote(s)
    }
}

impl Value {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Word(s) | Self::String(s) | Self::Number(s) | Self::Parameter(s) => Some(s),
            _ => None,
        }
    }
    pub fn render(&self) -> String {
        match self {
            Self::Word(s) | Self::Number(s) => s.clone(),
            Self::String(s) => quote(s),
            Self::Bool(b) => b.to_string(),
            Self::Parameter(s) => format!("?{s}"),
            Self::Array(xs) => format!(
                "[{}]",
                xs.iter().map(Self::render).collect::<Vec<_>>().join(", ")
            ),
            Self::Call(c) => c.render(),
            Self::Query {
                label,
                pipeline,
                attributes,
            } => format!(
                "{}{{ {} }}{}",
                label.as_ref().map(|s| format!("{s}: ")).unwrap_or_default(),
                pipeline.render(),
                attributes
                    .iter()
                    .map(|a| format!(" {}: {}", a.name.as_deref().unwrap_or(""), a.value.render()))
                    .collect::<String>()
            ),
            Self::Binary { left, op, right } => {
                format!("({} {op} {})", left.render(), right.render())
            }
            Self::Unary { op, value } => format!("{op}({})", value.render()),
        }
    }
}
impl Call {
    pub fn new(name: &str, args: Vec<Arg>) -> Self {
        Self {
            name: name.into(),
            args,
            start: 0,
            end: 0,
        }
    }
    pub fn render(&self) -> String {
        format!(
            "{}({})",
            self.name,
            self.args
                .iter()
                .map(|a| format!(
                    "{}{}",
                    a.name
                        .as_ref()
                        .map(|s| format!("{}=", field(s)))
                        .unwrap_or_default(),
                    a.value.render()
                ))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}
impl Pattern {
    pub fn render(&self) -> String {
        match self {
            Self::Any => "*".into(),
            Self::Text(s) => quote(s),
            Self::Regex { pattern, flags } => format!("/{pattern}/{flags}"),
            Self::Parameter(s) => format!("?{s}"),
        }
    }
}
impl Filter {
    pub fn has_call(&self) -> bool {
        match self {
            Self::Call(_) => true,
            Self::And(xs) | Self::Or(xs) => xs.iter().any(Self::has_call),
            Self::Not(x) => x.has_call(),
            _ => false,
        }
    }
    pub fn render(&self) -> String {
        match self {
            Self::Compare {
                field: f,
                op,
                value,
            } => format!("{} {op} {}", field(f), value.render()),
            Self::And(xs) => format!(
                "({})",
                xs.iter()
                    .map(Self::render)
                    .collect::<Vec<_>>()
                    .join(" AND ")
            ),
            Self::Or(xs) => format!(
                "({})",
                xs.iter().map(Self::render).collect::<Vec<_>>().join(" OR ")
            ),
            Self::Not(x) if matches!(x.as_ref(), Self::Call(_)) => format!("!{}", x.render()),
            Self::Not(x) => format!("!({})", x.render()),
            Self::Call(c) => c.render(),
            Self::Text(p) => p.render(),
            Self::Bool(b) => b.to_string(),
        }
    }
}
impl Step {
    pub fn render(&self) -> String {
        match self {
            Self::Filter(f) => f.render(),
            Self::Call(c) => c.render(),
            Self::Assign { field: f, value } => format!("{} := {}", field(f), value.render()),
            Self::Case(xs) => format!(
                "case {{ {} }}",
                xs.iter()
                    .map(Pipeline::render)
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Self::Match { field: f, arms } => format!(
                "{} match {{ {} }}",
                field(f),
                arms.iter()
                    .map(|(p, q)| format!("{} => {}", p.render(), q.render()))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Self::Link { field: f, target } => {
                let safe = target
                    .split_once('.')
                    .is_some_and(|(label, key)| super::plan::query_label(label) && bare_field(key));
                format!(
                    "{} <=> {}",
                    field(f),
                    if safe { target.clone() } else { quote(target) }
                )
            }
        }
    }
}
impl Pipeline {
    pub fn render(&self) -> String {
        self.steps
            .iter()
            .map(Step::render)
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

pub fn span(query: &str, start: usize, end: usize) -> Span {
    let start = start.min(query.len());
    let prefix = &query[..start];
    Span {
        start,
        end: end.min(query.len()),
        line: prefix.bytes().filter(|b| *b == b'\n').count() + 1,
        column: prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1,
    }
}
