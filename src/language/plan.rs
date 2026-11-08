use super::ast::{self, Arg, Call, Filter, Pattern, Pipeline, Step, Value};
use crate::contracts::*;
use std::collections::BTreeSet;

pub type Result<T> = std::result::Result<T, Diagnostic>;
fn word(s: &str) -> Value {
    Value::Word(s.into())
}
fn text(s: &str) -> Value {
    Value::String(s.into())
}
fn named(k: &str, v: Value) -> Arg {
    Arg {
        name: Some(k.into()),
        value: v,
    }
}
fn unnamed(v: Value) -> Arg {
    Arg {
        name: None,
        value: v,
    }
}
fn strings(xs: &[String]) -> Value {
    Value::Array(xs.iter().map(|s| text(s)).collect())
}
fn call(name: &str, args: Vec<Arg>) -> Step {
    Step::Call(Call::new(name, args))
}

pub fn checked_text(s: &str) -> Result<()> {
    if s.len() > MAX_REQUEST_BYTES
        || s.contains('\0')
        || s.chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
    {
        Err(Diagnostic::error(
            "CQL_TEXT_LIMIT",
            "Text contains a forbidden control character or exceeds the input budget.",
        ))
    } else {
        Ok(())
    }
}
pub fn checked_field(s: &str) -> Result<()> {
    checked_text(s)?;
    if s.trim().is_empty() {
        Err(Diagnostic::error(
            "CQL_FIELD",
            "Field names cannot be empty.",
        ))
    } else {
        Ok(())
    }
}
fn scalar(v: &Scalar) -> Value {
    match v {
        Scalar::Text(s) => text(s),
        Scalar::Number(n) => Value::Number(n.to_string()),
        Scalar::Boolean(b) => Value::Bool(*b),
    }
}

fn regex_literal(s: &str) -> String {
    let mut out = String::from("\\A");
    for c in s.chars() {
        match c {
            '\\' | '.' | '+' | '*' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '^' | '$' | '|'
            | '/' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push_str("\\z");
    out
}

fn equality(f: &str, v: &Scalar) -> Result<Filter> {
    checked_field(f)?;
    match v {
        Scalar::Text(s) => {
            checked_text(s)?;
            if s.contains('*') {
                // CQL quoting does not escape globs; absolute regex anchors preserve literal equality.
                Ok(Filter::Compare {
                    field: f.into(),
                    op: "=".into(),
                    value: Pattern::Regex {
                        pattern: regex_literal(s),
                        flags: String::new(),
                    },
                })
            } else {
                Ok(Filter::Compare {
                    field: f.into(),
                    op: "=".into(),
                    value: Pattern::Text(s.clone()),
                })
            }
        }
        Scalar::Number(n) => Ok(Filter::Compare {
            field: f.into(),
            op: "=".into(),
            value: Pattern::Text(n.to_string()),
        }),
        Scalar::Boolean(b) => Ok(Filter::Compare {
            field: f.into(),
            op: "=".into(),
            value: Pattern::Text(b.to_string()),
        }),
    }
}
fn exists(f: &str, present: bool) -> Filter {
    Filter::Compare {
        field: f.into(),
        op: if present { "=" } else { "!=" }.into(),
        value: Pattern::Text("*".into()),
    }
}
fn negative(f: &str, positive: Filter, missing: Missing) -> Filter {
    let neg = Filter::Not(Box::new(positive));
    match missing {
        Missing::Exclude => Filter::And(vec![exists(f, true), neg]),
        Missing::Include => Filter::Or(vec![exists(f, false), neg]),
    }
}
fn comparison(op: Comparison, expression: bool) -> &'static str {
    match op {
        Comparison::Eq => {
            if expression {
                "=="
            } else {
                "="
            }
        }
        Comparison::Ne => "!=",
        Comparison::Lt => "<",
        Comparison::Le => "<=",
        Comparison::Gt => ">",
        Comparison::Ge => ">=",
    }
}

pub fn predicate(p: &Predicate, depth: usize) -> Result<Filter> {
    if depth > MAX_DEPTH {
        return Err(Diagnostic::error(
            "CQL_DEPTH",
            "Predicate nesting exceeds the profile budget.",
        ));
    }
    match p {
        Predicate::Like { field, pattern } => {
            checked_field(field)?;
            checked_text(pattern)?;
            Ok(Filter::Compare {
                field: field.clone(),
                op: "like".into(),
                value: Pattern::Text(pattern.clone()),
            })
        }
        Predicate::Call {
            function,
            args,
            negate,
        } => {
            let call = function_call(function, args, depth + 1)?;
            let resolved = crate::catalog::resolve(&call).expect("lowered known function");
            if resolved.function.resolved_effect(&resolved.arguments)
                != crate::catalog::Effect::Filter
            {
                if resolved.function.effect == crate::catalog::Effect::Filter {
                    return Err(Diagnostic::unsupported(
                        "CQL_UNSUPPORTED_PREDICATE_EFFECT",
                        "This retaining argument form is not qualified in predicate context.",
                    ));
                }
                return Err(Diagnostic::error(
                    "CQL_FILTER_FUNCTION",
                    "A typed function predicate must have a filtering effect in this argument form.",
                ));
            }
            let predicate = Filter::Call(call);
            Ok(if *negate {
                Filter::Not(Box::new(predicate))
            } else {
                predicate
            })
        }
        Predicate::Eq { field, value } => equality(field, value),
        Predicate::NotEq {
            field,
            value,
            missing,
        } => Ok(negative(field, equality(field, value)?, *missing)),
        Predicate::Glob {
            field,
            pattern,
            negate,
            missing,
        }
        | Predicate::Regex {
            field,
            pattern,
            negate,
            missing,
            ..
        } => {
            checked_field(field)?;
            checked_text(pattern)?;
            let positive = match p {
                Predicate::Glob { .. } => Filter::Compare {
                    field: field.clone(),
                    op: "=".into(),
                    value: Pattern::Text(pattern.clone()),
                },
                Predicate::Regex { flags, .. } => Filter::Call(Call::new(
                    "regex",
                    vec![
                        unnamed(text(pattern)),
                        named("field", text(field)),
                        named("flags", text(flags)),
                    ],
                )),
                _ => unreachable!(),
            };
            if *negate {
                let m = missing.ok_or_else(|| {
                    Diagnostic::error(
                        "CQL_MISSING_POLICY",
                        "Negated matching requires missing=include or exclude.",
                    )
                })?;
                Ok(negative(field, positive, m))
            } else if missing.is_some() {
                Err(Diagnostic::error(
                    "CQL_MISSING_POLICY",
                    "Missing-field policy applies only to negated matching.",
                ))
            } else {
                Ok(positive)
            }
        }
        Predicate::Exists { field, present } => {
            checked_field(field)?;
            Ok(exists(field, *present))
        }
        Predicate::Compare {
            field,
            operator,
            value,
        } => {
            checked_field(field)?;
            Ok(Filter::Compare {
                field: field.clone(),
                op: comparison(*operator, false).into(),
                value: Pattern::Text(value.to_string()),
            })
        }
        Predicate::Fields {
            left,
            operator,
            right,
        } => {
            checked_field(left)?;
            checked_field(right)?;
            if !ast::bare_field(left) || !ast::bare_field(right) {
                return Err(Diagnostic::unsupported(
                    "CQL_UNSUPPORTED_FIELD_EXPRESSION",
                    "Field comparisons require expression-compatible field names in this profile.",
                ));
            }
            Ok(Filter::Call(Call::new(
                "test",
                vec![unnamed(Value::Binary {
                    left: Box::new(word(left)),
                    op: comparison(*operator, true).into(),
                    right: Box::new(word(right)),
                })],
            )))
        }
        Predicate::All { items } | Predicate::Any { items } => {
            if items.is_empty() {
                return Err(Diagnostic::error(
                    "CQL_EMPTY_BOOLEAN",
                    "Boolean groups must have at least one predicate.",
                ));
            }
            let xs = items
                .iter()
                .map(|p| predicate(p, depth + 1))
                .collect::<Result<Vec<_>>>()?;
            Ok(if matches!(p, Predicate::All { .. }) {
                Filter::And(xs)
            } else {
                Filter::Or(xs)
            })
        }
    }
}
fn expression(e: &Expression, depth: usize) -> Result<Value> {
    if depth > MAX_DEPTH {
        return Err(Diagnostic::error(
            "CQL_DEPTH",
            "Expression nesting exceeds the profile budget.",
        ));
    }
    match e {
        Expression::Compare {
            left,
            operator,
            right,
        } => Ok(Value::Binary {
            left: Box::new(expression(left, depth + 1)?),
            op: comparison(*operator, true).into(),
            right: Box::new(expression(right, depth + 1)?),
        }),
        Expression::Unary { operator, value } => Ok(Value::Unary {
            op: match operator {
                Unary::Negative => "-",
                Unary::Positive => "+",
                Unary::Not => "!",
            }
            .into(),
            value: Box::new(expression(value, depth + 1)?),
        }),
        Expression::Field { name } => {
            checked_field(name)?;
            if !ast::bare_field(name) {
                return Err(Diagnostic::unsupported(
                    "CQL_UNSUPPORTED_FIELD_EXPRESSION",
                    "Expression field must be an unquoted CQL identifier.",
                ));
            }
            Ok(word(name))
        }
        Expression::Literal { value } => {
            if let Scalar::Text(s) = value {
                checked_text(s)?;
            }
            Ok(scalar(value))
        }
        Expression::Array { items } => Ok(Value::Array(
            items
                .iter()
                .map(|e| expression(e, depth + 1))
                .collect::<Result<_>>()?,
        )),
        Expression::Call { function, args } => {
            Ok(Value::Call(function_call(function, args, depth + 1)?))
        }
        Expression::Binary {
            left,
            operator,
            right,
        } => Ok(Value::Binary {
            left: Box::new(expression(left, depth + 1)?),
            op: match operator {
                Arithmetic::Add => "+",
                Arithmetic::Subtract => "-",
                Arithmetic::Multiply => "*",
                Arithmetic::Divide => "/",
                Arithmetic::Remainder => "%",
            }
            .into(),
            right: Box::new(expression(right, depth + 1)?),
        }),
        Expression::NamedQuery {
            name,
            plan,
            include,
            after,
            before,
        } => {
            if !label(name) {
                return Err(Diagnostic::error(
                    "CQL_QUERY_LABEL",
                    "Use a nonempty ASCII query label.",
                ));
            }
            let mut attributes = Vec::new();
            if let Some(include) = include {
                let value = match include {
                    QueryFields::All => word("*"),
                    QueryFields::Fields { fields } => {
                        for field in fields {
                            checked_field(field)?;
                            if field == "*" {
                                return Err(Diagnostic::unsupported(
                                    "CQL_UNSUPPORTED_INCLUDE_STAR",
                                    "Use explicit all-fields mode; a literal star field is not qualified here.",
                                ));
                            }
                        }
                        strings(fields)
                    }
                };
                attributes.push(named("include", value));
            }
            for (key, anchor) in [("after", after), ("before", before)] {
                if let Some(anchor) = anchor {
                    if !label(&anchor.query) {
                        return Err(Diagnostic::error(
                            "CQL_QUERY_LABEL",
                            "Time anchors need an ASCII query label.",
                        ));
                    }
                    let value = if let Some(offset) = &anchor.offset {
                        if signed_duration(offset).is_none() {
                            return Err(Diagnostic::unsupported(
                                "CQL_UNSUPPORTED_TIME_OFFSET",
                                "Use an explicit signed fixed duration such as +5m or -10s.",
                            ));
                        }
                        format!("{} {}", anchor.query, offset)
                    } else {
                        anchor.query.clone()
                    };
                    attributes.push(named(key, text(&value)));
                }
            }
            Ok(Value::Query {
                label: Some(name.clone()),
                pipeline: lower_at(plan, depth + 1)?,
                attributes,
            })
        }
        Expression::Query { plan } => Ok(Value::Query {
            label: None,
            pipeline: lower_at(plan, depth + 1)?,
            attributes: vec![],
        }),
    }
}
fn function_call(name: &str, args: &[Argument], depth: usize) -> Result<Call> {
    let Some(f) = crate::catalog::function(name) else {
        return Err(Diagnostic::unsupported(
            "CQL_UNSUPPORTED_FUNCTION",
            format!("Function '{name}' is not in the bundled coverage profile."),
        ));
    };
    let args = args
        .iter()
        .map(|a| {
            Ok(Arg {
                name: a.name.clone(),
                value: expression(&a.value, depth + 1)?,
            })
        })
        .collect::<Result<_>>()?;
    Ok(Call::new(f.name, args))
}
fn count(field: &Option<String>, distinct: bool, alias: &str) -> Call {
    let mut args = vec![named("as", text(alias))];
    if let Some(f) = field {
        args.push(named("field", text(f)));
    }
    if distinct {
        args.push(named("distinct", Value::Bool(true)));
    }
    Call::new("count", args)
}
fn match_pattern(pattern: &MatchPattern) -> Result<Pattern> {
    Ok(match pattern {
        MatchPattern::Any => Pattern::Any,
        MatchPattern::Literal {
            value: Scalar::Text(value),
        } if value.contains('*') => {
            checked_text(value)?;
            Pattern::Regex {
                pattern: regex_literal(value),
                flags: String::new(),
            }
        }
        MatchPattern::Literal { value } => {
            let text = match value {
                Scalar::Text(s) => s.clone(),
                Scalar::Number(n) => n.to_string(),
                Scalar::Boolean(b) => b.to_string(),
            };
            checked_text(&text)?;
            Pattern::Text(text)
        }
        MatchPattern::Glob { pattern } => {
            checked_text(pattern)?;
            Pattern::Text(pattern.clone())
        }
        MatchPattern::Regex { pattern, flags } => {
            checked_text(pattern)?;
            if pattern.contains(['\r', '\n']) {
                return Err(Diagnostic::unsupported(
                    "CQL_UNSUPPORTED_MULTILINE_REGEX_LITERAL",
                    "Regex literals cannot contain raw line breaks; use escaped characters.",
                ));
            }
            if flags.chars().any(|ch| !"imd".contains(ch))
                || flags.chars().collect::<BTreeSet<_>>().len() != flags.len()
            {
                return Err(Diagnostic::unsupported(
                    "CQL_UNSUPPORTED_REGEX_FLAGS",
                    "Field-match regex flags must be distinct i, m or d.",
                ));
            }
            if let Err((unknown, message)) = super::validate::regex_analysis(pattern) {
                return Err(if unknown {
                    Diagnostic::unsupported("CQL_UNSUPPORTED_REGEX", message)
                } else {
                    Diagnostic::error("CQL_REGEX_SYNTAX", message)
                });
            }
            Pattern::Regex {
                pattern: ast::escape_regex_delimiter(pattern),
                flags: flags.clone(),
            }
        }
    })
}
fn aggregate(a: &Aggregate) -> Result<Value> {
    let (name, f, alias) = match a {
        Aggregate::Count {
            field,
            distinct,
            accuracy,
            as_,
        } => {
            if !*distinct && *accuracy == Accuracy::Estimate {
                return Err(Diagnostic::error(
                    "CQL_ACCURACY_MODE",
                    "Estimated accuracy applies only to distinct cardinality.",
                ));
            }
            if *distinct && *accuracy == Accuracy::Exact {
                return Err(Diagnostic::unsupported(
                    "CQL_UNSUPPORTED_EXACT_AGGREGATE",
                    "Exact distinct inside a group requires explicit two-stage grouping; approximate count is never silently substituted.",
                ));
            }
            if *distinct && field.is_none() {
                return Err(Diagnostic::error(
                    "CQL_REQUIRED_FIELD",
                    "Distinct counting requires a field.",
                ));
            }
            checked_field(as_)?;
            return Ok(Value::Call(count(field, *distinct, as_)));
        }
        Aggregate::Sum { field, as_ } => ("sum", field, as_),
        Aggregate::Avg { field, as_ } => ("avg", field, as_),
        Aggregate::Min { field, as_ } => ("min", field, as_),
        Aggregate::Max { field, as_ } => ("max", field, as_),
    };
    checked_field(f)?;
    checked_field(alias)?;
    Ok(Value::Call(Call::new(
        name,
        vec![unnamed(text(f)), named("as", text(alias))],
    )))
}
pub fn lower(plan: &Plan) -> Result<Pipeline> {
    lower_at(plan, 0)
}
fn append_filter(q: &mut Pipeline, filter: Filter) -> Result<()> {
    match filter {
        Filter::And(items) => {
            for f in items {
                append_filter(q, f)?;
            }
        }
        Filter::Or(_) if filter.has_call() => {
            return Err(Diagnostic::unsupported(
                "CQL_UNSUPPORTED_BOOLEAN_FUNCTION",
                "Function predicates inside OR need a separately qualified composition; no query was emitted.",
            ));
        }
        f => q.steps.push(Step::Filter(f)),
    }
    Ok(())
}
fn lower_at(plan: &Plan, depth: usize) -> Result<Pipeline> {
    if depth > MAX_DEPTH {
        return Err(Diagnostic::error(
            "CQL_DEPTH",
            "Query nesting exceeds the profile budget.",
        ));
    }
    let mut q = Pipeline::default();
    for (field, v) in [
        ("#repo", &plan.source.repo),
        ("#type", &plan.source.type_),
        ("#event_simpleName", &plan.source.event),
    ] {
        if let Some(value) = v {
            q.steps
                .push(Step::Filter(equality(field, &Scalar::Text(value.clone()))?));
        }
    }
    for f in &plan.filters {
        append_filter(&mut q, predicate(f, depth + 1)?)?;
    }
    for (i, stage) in plan.steps.iter().enumerate() {
        let start = q.steps.len();
        let r: Result<()> = (|| {
            match stage {
                Stage::Link { field, to, key } => {
                    checked_field(field)?;
                    if !label(to) || !ast::bare_field(key) {
                        return Err(Diagnostic::unsupported(
                            "CQL_UNSUPPORTED_LINK_REFERENCE",
                            "Link targets need an ASCII query label and a qualified bare field name.",
                        ));
                    }
                    q.steps.push(Step::Link {
                        field: field.clone(),
                        target: format!("{to}.{key}"),
                    });
                }
                Stage::Case { branches } => {
                    if branches.is_empty() {
                        return Err(Diagnostic::error(
                            "CQL_EMPTY_CASE",
                            "Case requires at least one explicit branch; no catch-all is added.",
                        ));
                    }
                    q.steps.push(Step::Case(
                        branches
                            .iter()
                            .map(|p| lower_at(p, depth + 1))
                            .collect::<Result<_>>()?,
                    ));
                }
                Stage::Match { field, arms } => {
                    checked_field(field)?;
                    if arms.is_empty() {
                        return Err(Diagnostic::error(
                            "CQL_EMPTY_MATCH",
                            "Field match requires explicit arms.",
                        ));
                    }
                    q.steps.push(Step::Match {
                        field: field.clone(),
                        arms: arms
                            .iter()
                            .map(|arm| {
                                Ok((
                                    match_pattern(&arm.pattern)?,
                                    lower_at(&arm.plan, depth + 1)?,
                                ))
                            })
                            .collect::<Result<_>>()?,
                    });
                }
                Stage::Filter { predicate: p } => append_filter(&mut q, predicate(p, depth + 1)?)?,
                Stage::Count {
                    field,
                    distinct,
                    accuracy,
                    as_,
                } => {
                    if !*distinct && *accuracy == Accuracy::Estimate {
                        return Err(Diagnostic::error(
                            "CQL_ACCURACY_MODE",
                            "Estimated accuracy applies only to distinct cardinality.",
                        ));
                    }
                    checked_field(as_)?;
                    if *distinct && field.is_none() {
                        return Err(Diagnostic::error(
                            "CQL_REQUIRED_FIELD",
                            "Distinct counting requires a field.",
                        ));
                    }
                    if *distinct && *accuracy == Accuracy::Exact {
                        let f = field.as_ref().expect("checked");
                        checked_field(f)?;
                        q.steps.push(Step::Filter(exists(f, true)));
                        q.steps.push(call(
                            "groupBy",
                            vec![
                                unnamed(strings(std::slice::from_ref(f))),
                                named("limit", word("max")),
                            ],
                        ));
                        q.steps.push(Step::Call(count(&None, false, as_)));
                    } else {
                        q.steps.push(Step::Call(count(field, *distinct, as_)));
                    }
                }
                Stage::Group { keys, aggregates } => {
                    if keys.is_empty() || aggregates.is_empty() {
                        return Err(Diagnostic::error(
                            "CQL_EMPTY_GROUP",
                            "Grouping requires keys and aggregates.",
                        ));
                    }
                    for f in keys {
                        checked_field(f)?;
                    }
                    let mut aliases = BTreeSet::new();
                    for a in aggregates {
                        let alias = match a {
                            Aggregate::Count { as_, .. }
                            | Aggregate::Sum { as_, .. }
                            | Aggregate::Avg { as_, .. }
                            | Aggregate::Min { as_, .. }
                            | Aggregate::Max { as_, .. } => as_,
                        };
                        if keys.contains(alias) || !aliases.insert(alias) {
                            return Err(Diagnostic::error(
                                "CQL_ALIAS_COLLISION",
                                "Group keys and aggregate aliases must be distinct.",
                            ));
                        }
                    }
                    q.steps.push(call(
                        "groupBy",
                        vec![
                            unnamed(strings(keys)),
                            named(
                                "function",
                                Value::Array(
                                    aggregates.iter().map(aggregate).collect::<Result<_>>()?,
                                ),
                            ),
                            named("limit", word("max")),
                        ],
                    ));
                }
                Stage::Select { fields } => {
                    if fields.is_empty() {
                        return Err(Diagnostic::error(
                            "CQL_EMPTY_FIELDS",
                            "Projection requires fields.",
                        ));
                    }
                    for f in fields {
                        checked_field(f)?;
                    }
                    q.steps.push(call("select", vec![unnamed(strings(fields))]));
                }
                Stage::Sort {
                    fields,
                    order,
                    type_,
                    limit,
                } => {
                    if fields.is_empty() || *limit == 0 {
                        return Err(Diagnostic::error(
                            "CQL_SORT",
                            "Sort requires fields and a positive limit.",
                        ));
                    }
                    for f in fields {
                        checked_field(f)?;
                    }
                    q.steps.push(call(
                        "sort",
                        vec![
                            unnamed(strings(fields)),
                            named(
                                "order",
                                text(if *order == Order::Asc { "asc" } else { "desc" }),
                            ),
                            named(
                                "type",
                                text(if *type_ == SortType::String {
                                    "string"
                                } else {
                                    "number"
                                }),
                            ),
                            named("limit", Value::Number(limit.to_string())),
                        ],
                    ));
                }
                Stage::Head { limit } | Stage::Tail { limit } => {
                    if *limit == 0 {
                        return Err(Diagnostic::error(
                            "CQL_LIMIT",
                            "Row limits must be positive.",
                        ));
                    }
                    q.steps.push(call(
                        if matches!(stage, Stage::Head { .. }) {
                            "head"
                        } else {
                            "tail"
                        },
                        vec![unnamed(Value::Number(limit.to_string()))],
                    ));
                }
                Stage::Assign {
                    field,
                    expression: e,
                } => {
                    checked_field(field)?;
                    if field.starts_with('#') {
                        return Err(Diagnostic::error(
                            "CQL_TAG_ASSIGNMENT",
                            "Query assignments cannot create or overwrite tag namespaces.",
                        ));
                    }
                    q.steps.push(Step::Assign {
                        field: field.clone(),
                        value: expression(e, depth + 1)?,
                    });
                }
                Stage::Call { function, args } => {
                    q.steps
                        .push(Step::Call(function_call(function, args, depth + 1)?))
                }
                Stage::Join {
                    query,
                    field,
                    key,
                    include,
                    left,
                    limit,
                    max_matches,
                } => {
                    if *limit == 0 || *max_matches == 0 {
                        return Err(Diagnostic::error(
                            "CQL_JOIN_LIMIT",
                            "Join limits and max_matches must be positive.",
                        ));
                    }
                    for f in std::iter::once(field)
                        .chain(std::iter::once(key))
                        .chain(include)
                    {
                        checked_field(f)?;
                    }
                    q.steps.push(call(
                        "join",
                        vec![
                            unnamed(Value::Query {
                                label: None,
                                pipeline: lower_at(query, depth + 1)?,
                                attributes: vec![],
                            }),
                            named("field", text(field)),
                            named("key", text(key)),
                            named("include", strings(include)),
                            named("mode", text(if *left { "left" } else { "inner" })),
                            named("limit", Value::Number(limit.to_string())),
                            named("max", Value::Number(max_matches.to_string())),
                        ],
                    ));
                }
                Stage::Correlate {
                    queries,
                    links,
                    sequence,
                    within,
                } => {
                    if !q.steps.is_empty() || plan.steps.len() != 1 {
                        return Err(Diagnostic::error(
                            "CQL_CORRELATION_PLACEMENT",
                            "Correlate must be the sole top-level semantic stage with source/filters inside its named queries.",
                        ));
                    }
                    if queries.is_empty() {
                        return Err(Diagnostic::error(
                            "CQL_CORRELATION_BINDINGS",
                            "Correlation requires at least one named query.",
                        ));
                    }
                    if !duration(within) {
                        return Err(Diagnostic::error(
                            "CQL_DURATION",
                            "Use a positive duration such as 5m or 1h.",
                        ));
                    }
                    let mut names = BTreeSet::new();
                    for n in queries {
                        if !label(&n.name) || !names.insert(n.name.as_str()) {
                            return Err(Diagnostic::error(
                                "CQL_QUERY_LABEL",
                                "Named queries need unique ASCII labels.",
                            ));
                        }
                    }
                    let mut args = Vec::new();
                    for n in queries {
                        let mut branch = lower_at(&n.plan, depth + 1)?;
                        for link in links.iter().filter(|l| l.from == n.name) {
                            if !names.contains(link.to.as_str())
                                || link.from == link.to
                                || !ast::bare_field(&link.field)
                                || !ast::bare_field(&link.key)
                            {
                                return Err(Diagnostic::error(
                                    "CQL_CORRELATION_LINK",
                                    "Link fields and query labels must resolve.",
                                ));
                            }
                            branch.steps.push(Step::Link {
                                field: link.field.clone(),
                                target: format!("{}.{}", link.to, link.key),
                            });
                        }
                        args.push(unnamed(Value::Query {
                            label: Some(n.name.clone()),
                            pipeline: branch,
                            attributes: vec![],
                        }));
                    }
                    for link in links {
                        if !names.contains(link.from.as_str()) {
                            return Err(Diagnostic::error(
                                "CQL_CORRELATION_LINK",
                                "Unknown link source.",
                            ));
                        }
                    }
                    args.push(named("sequence", Value::Bool(*sequence)));
                    args.push(named("within", word(within)));
                    q.steps.push(call("correlate", args));
                }
            }
            Ok(())
        })();
        if let Err(d) = r {
            return Err(d.at(&format!("/steps/{i}")));
        }
        if q.steps.len() == start {
            return Err(Diagnostic::error(
                "CQL_EMPTY_STAGE",
                "No query stage was produced.",
            ));
        }
    }
    if q.steps.is_empty() {
        q.steps.push(Step::Filter(Filter::Bool(true)));
    }
    Ok(q)
}

pub fn duration_millis(s: &str) -> Option<u64> {
    let n = s.find(|c: char| !c.is_ascii_digit())?;
    let number = s[..n].parse::<u64>().ok()?;
    let multiplier = match s[n..].trim() {
        "ms" | "millisecond" | "milliseconds" => 1,
        "s" | "sec" | "second" | "seconds" => 1000,
        "m" | "min" | "minute" | "minutes" => 60000,
        "h" | "hour" | "hours" => 3600000,
        "d" | "day" | "days" => 86400000,
        "w" | "week" | "weeks" => 604800000,
        _ => return None,
    };
    number.checked_mul(multiplier)
}
pub fn duration(s: &str) -> bool {
    duration_millis(s).is_some_and(|n| n > 0)
}
pub fn signed_duration(s: &str) -> Option<i128> {
    let (sign, value) = if let Some(v) = s.strip_prefix('+') {
        (1, v)
    } else {
        (-1, s.strip_prefix('-')?)
    };
    Some(sign * i128::from(duration_millis(value.trim())?))
}
pub fn query_label(s: &str) -> bool {
    label(s)
}

fn label(s: &str) -> bool {
    !s.is_empty()
        && s.as_bytes()[0].is_ascii_alphabetic()
        && s.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

pub fn recipe(r: &Recipe) -> Plan {
    let (source, filters, steps) = match r {
        Recipe::Events {
            source,
            filters,
            fields,
            limit,
        } => {
            let mut steps = vec![];
            if !fields.is_empty() {
                steps.push(Stage::Select {
                    fields: fields.clone(),
                });
            }
            steps.push(Stage::Head { limit: *limit });
            (source.clone(), filters.clone(), steps)
        }
        Recipe::CountEvents { source, filters } => (
            source.clone(),
            filters.clone(),
            vec![Stage::Count {
                field: None,
                distinct: false,
                accuracy: Accuracy::Exact,
                as_: count_alias(),
            }],
        ),
        Recipe::CountBy {
            source,
            filters,
            keys,
        } => (
            source.clone(),
            filters.clone(),
            vec![Stage::Group {
                keys: keys.clone(),
                aggregates: vec![Aggregate::Count {
                    field: None,
                    distinct: false,
                    accuracy: Accuracy::Exact,
                    as_: count_alias(),
                }],
            }],
        ),
        Recipe::Distinct {
            source,
            filters,
            field,
            accuracy,
        } => (
            source.clone(),
            filters.clone(),
            vec![Stage::Count {
                field: Some(field.clone()),
                distinct: true,
                accuracy: *accuracy,
                as_: count_alias(),
            }],
        ),
        Recipe::TopValues {
            source,
            filters,
            field,
            limit,
        } => (
            source.clone(),
            filters.clone(),
            vec![
                Stage::Group {
                    keys: vec![field.clone()],
                    aggregates: vec![Aggregate::Count {
                        field: None,
                        distinct: false,
                        accuracy: Accuracy::Exact,
                        as_: count_alias(),
                    }],
                },
                Stage::Sort {
                    fields: vec![count_alias()],
                    order: Order::Desc,
                    type_: SortType::Number,
                    limit: *limit,
                },
            ],
        ),
        Recipe::Threshold {
            source,
            filters,
            keys,
            minimum,
        } => (
            source.clone(),
            filters.clone(),
            vec![
                Stage::Group {
                    keys: keys.clone(),
                    aggregates: vec![Aggregate::Count {
                        field: None,
                        distinct: false,
                        accuracy: Accuracy::Exact,
                        as_: count_alias(),
                    }],
                },
                Stage::Filter {
                    predicate: Predicate::Compare {
                        field: count_alias(),
                        operator: Comparison::Ge,
                        value: (*minimum).into(),
                    },
                },
            ],
        ),
        Recipe::Pivot {
            source,
            filters,
            field,
            value,
            fields,
            limit,
        } => {
            let mut filters = filters.clone();
            filters.push(Predicate::Eq {
                field: field.clone(),
                value: value.clone(),
            });
            let mut steps = vec![];
            if !fields.is_empty() {
                steps.push(Stage::Select {
                    fields: fields.clone(),
                });
            }
            steps.push(Stage::Head { limit: *limit });
            (source.clone(), filters, steps)
        }
        Recipe::JoinPivot {
            source,
            filters,
            query,
            field,
            key,
            include,
            left,
            limit,
            max_matches,
        } => (
            source.clone(),
            filters.clone(),
            vec![Stage::Join {
                query: query.clone(),
                field: field.clone(),
                key: key.clone(),
                include: include.clone(),
                left: *left,
                limit: *limit,
                max_matches: *max_matches,
            }],
        ),
        Recipe::Sequence {
            queries,
            links,
            within,
        } => (
            Scope::default(),
            vec![],
            vec![Stage::Correlate {
                queries: queries.clone(),
                links: links.clone(),
                sequence: true,
                within: within.clone(),
            }],
        ),
    };
    Plan {
        source,
        filters,
        steps,
    }
}
