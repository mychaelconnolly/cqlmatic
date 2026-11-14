//! Context-sensitive recursive descent: filters deliberately give OR higher precedence.
use super::ast::*;
use crate::contracts::{Diagnostic, MAX_DEPTH, MAX_NODES, MAX_QUERY_UTF16};

#[derive(Debug)]
pub struct Failure {
    pub diagnostic: Diagnostic,
    pub unsupported: bool,
}
type Result<T> = std::result::Result<T, Failure>;

pub fn parse(input: &str) -> Result<Pipeline> {
    if input.contains('\0') || input.encode_utf16().count() > MAX_QUERY_UTF16 {
        return Err(Failure {
            diagnostic: Diagnostic::error(
                "CQL_INPUT_LIMIT",
                "CQL must contain no NUL and at most 66,000 UTF-16 units.",
            ),
            unsupported: false,
        });
    }
    let mut p = Parser {
        input,
        pos: 0,
        depth: 0,
        nodes: 0,
    };
    let result = p.pipeline()?;
    if pipeline_depth(&result) > MAX_DEPTH {
        return p.fail("CQL nesting or node budget exceeded.", true);
    }
    p.space();
    if p.pos != input.len() {
        return p.fail("Unconsumed CQL syntax.", true);
    }
    Ok(result)
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
    depth: usize,
    nodes: usize,
}
impl Parser<'_> {
    fn fail<T>(&self, message: &str, unsupported: bool) -> Result<T> {
        let end = self.input[self.pos..]
            .chars()
            .next()
            .map_or(self.pos, |c| self.pos + c.len_utf8());
        let mut d = Diagnostic::error(
            if unsupported {
                "CQL_UNSUPPORTED_SYNTAX"
            } else {
                "CQL_SYNTAX"
            },
            message,
        );
        if unsupported {
            d.class = crate::contracts::DiagnosticClass::Coverage;
        }
        d.span = Some(Box::new(span(self.input, self.pos, end)));
        Err(Failure {
            diagnostic: d,
            unsupported,
        })
    }
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        self.depth += 1;
        self.nodes += 1;
        if self.depth > MAX_DEPTH || self.nodes > MAX_NODES {
            self.depth -= 1;
            return self.fail("CQL nesting or node budget exceeded.", true);
        }
        let r = f(self);
        self.depth -= 1;
        r
    }
    fn rest(&self) -> &str {
        &self.input[self.pos..]
    }
    fn space(&mut self) {
        loop {
            while let Some(c) = self.rest().chars().next().filter(|c| c.is_whitespace()) {
                self.pos += c.len_utf8();
            }
            if self.rest().starts_with("//") {
                self.pos += self.rest().find('\n').unwrap_or(self.rest().len());
            } else if self.rest().starts_with("/*") {
                if let Some(n) = self.rest()[2..].find("*/") {
                    self.pos += n + 4;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    }
    fn eat(&mut self, s: &str) -> bool {
        self.space();
        if self.rest().starts_with(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }
    fn expect(&mut self, s: &str) -> Result<()> {
        if self.eat(s) {
            Ok(())
        } else {
            self.fail(&format!("Expected '{s}'."), false)
        }
    }
    fn keyword(&mut self, s: &str) -> bool {
        self.space();
        let tail = self.rest();
        if tail
            .get(..s.len())
            .is_some_and(|x| x.eq_ignore_ascii_case(s))
            && tail[s.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric() && !"_.[".contains(c))
        {
            self.pos += s.len();
            true
        } else {
            false
        }
    }
    fn ending(&mut self) -> bool {
        self.space();
        self.pos == self.input.len() || self.rest().starts_with(['|', '}', ')', ';', ','])
    }
    fn string(&mut self) -> Result<String> {
        self.space();
        let start = self.pos;
        self.expect("\"")?;
        let mut escaped = false;
        while let Some(c) = self.rest().chars().next() {
            self.pos += c.len_utf8();
            if c == '"' && !escaped {
                return serde_json::from_str(&self.input[start..self.pos]).map_err(|_| Failure {
                    diagnostic: Diagnostic::error(
                        "CQL_STRING_ESCAPE",
                        "Unsupported string escape; use JSON-compatible CQL string escapes.",
                    ),
                    unsupported: true,
                });
            }
            if c == '\n' || c == '\r' {
                return self.fail("A quoted CQL string cannot span lines.", false);
            }
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            }
        }
        self.fail("Unterminated string.", false)
    }
    fn identifier(&mut self) -> Result<String> {
        self.space();
        if self.rest().starts_with('"') {
            return self.string();
        }
        let start = self.pos;
        while let Some(c) = self.rest().chars().next() {
            if c.is_alphanumeric() || "_@#.".contains(c) {
                self.pos += c.len_utf8();
            } else if c == ':'
                && !self.rest().starts_with(":=")
                && self.rest()[1..]
                    .chars()
                    .next()
                    .is_some_and(|x| x.is_alphanumeric() || x == '_')
            {
                self.pos += 1;
            } else if c == '[' {
                let before = self.pos;
                self.pos += 1;
                while self.rest().starts_with(|x: char| x.is_ascii_digit()) {
                    self.pos += 1;
                }
                if self.pos > before + 1 && self.rest().starts_with(']') {
                    self.pos += 1;
                } else {
                    self.pos = before;
                    break;
                }
            } else {
                break;
            }
        }
        if self.pos == start {
            self.fail("Expected an identifier.", true)
        } else {
            Ok(self.input[start..self.pos].into())
        }
    }
    fn pipeline(&mut self) -> Result<Pipeline> {
        self.nested(|p| {
            let mut steps = Vec::new();
            p.space();
            if p.pos == p.input.len() || p.rest().starts_with(['}', ';']) {
                return Ok(Pipeline { steps });
            }
            loop {
                steps.push(p.step()?);
                p.space();
                if !p.eat("|") {
                    break;
                }
                if p.ending() {
                    return p.fail("A pipeline cannot end with '|'.", false);
                }
            }
            Ok(Pipeline { steps })
        })
    }
    fn step(&mut self) -> Result<Step> {
        self.space();
        let start = self.pos;
        if self.keyword("case") && self.eat("{") {
            let mut cases = Vec::new();
            loop {
                cases.push(self.pipeline()?);
                if self.eat("}") {
                    break;
                }
                self.expect(";")?;
                if self.eat("}") {
                    break;
                }
            }
            return Ok(Step::Case(cases));
        }
        self.pos = start;
        if let Ok(name) = self.identifier() {
            if self.eat(":=") {
                if !self.input[start..].starts_with('"') && reserved_field_prefix(&name) {
                    self.pos = start;
                    return self.fail(
                        "Quote assignment field names whose initial segment is NOT, AND or OR.",
                        true,
                    );
                }
                return Ok(Step::Assign {
                    field: name,
                    value: self.expression(0)?,
                });
            }
            if self.eat("=~") {
                let n = self.identifier()?;
                let mut c = self.call(n, self.pos)?;
                c.args.push(Arg {
                    name: Some("field".into()),
                    value: Value::String(name),
                });
                return Ok(Step::Call(c));
            }
            if self.eat("<=>") {
                if self.rest().trim_start().starts_with('"') {
                    return self.fail(
                        "Quoted link targets are not qualified field references.",
                        true,
                    );
                }
                let target = self.identifier()?;
                return Ok(Step::Link {
                    field: name,
                    target,
                });
            }
            if self.keyword("match") && self.eat("{") {
                let mut arms = Vec::new();
                loop {
                    self.space();
                    let unquoted = self.rest().starts_with('*');
                    let mut pat = self.pattern()?;
                    if unquoted && pat == Pattern::Text("*".into()) {
                        pat = Pattern::Any;
                    }
                    self.expect("=>")?;
                    arms.push((pat, self.pipeline()?));
                    if self.eat("}") {
                        break;
                    }
                    self.expect(";")?;
                    if self.eat("}") {
                        break;
                    }
                }
                return Ok(Step::Match { field: name, arms });
            }
            if !name.eq_ignore_ascii_case("not") && self.eat("(") {
                self.pos = start;
                let name = self.identifier()?;
                return Ok(Step::Call(self.call(name, start)?));
            }
        }
        self.pos = start;
        if self.eat("[") {
            let mut args = Vec::new();
            if !self.eat("]") {
                loop {
                    args.push(self.expression(0)?);
                    if self.eat("]") {
                        break;
                    }
                    self.expect(",")?;
                }
            }
            return Ok(Step::Call(Call::new(
                "stats",
                vec![Arg {
                    name: None,
                    value: Value::Array(args),
                }],
            )));
        }
        self.pos = start;
        Ok(Step::Filter(self.and()?))
    }
    fn and(&mut self) -> Result<Filter> {
        self.nested(|p| {
            let mut xs = vec![p.or()?];
            loop {
                if p.keyword("AND") {
                    xs.push(p.or()?);
                } else if p.ending() {
                    break;
                } else {
                    xs.push(p.or()?);
                }
            }
            Ok(if xs.len() == 1 {
                xs.remove(0)
            } else {
                Filter::And(xs)
            })
        })
    }
    fn or(&mut self) -> Result<Filter> {
        let mut xs = vec![self.primary_filter()?];
        while self.keyword("OR") {
            xs.push(self.primary_filter()?);
        }
        Ok(if xs.len() == 1 {
            xs.remove(0)
        } else {
            Filter::Or(xs)
        })
    }
    fn primary_filter(&mut self) -> Result<Filter> {
        self.nested(|p| {
            if p.keyword("NOT") || p.eat("!") {
                return Ok(Filter::Not(Box::new(p.primary_filter()?)));
            }
            if p.eat("(") {
                let f = p.and()?;
                p.expect(")")?;
                if f.has_call() {
                    return p.fail("Filter functions cannot be parenthesized as Boolean expressions; use a pipeline stage.", false);
                }
                return Ok(f);
            }
            let start = p.pos;
            if let Ok(name) = p.identifier() {
                p.space();
                if p.rest().starts_with('(') {
                    return Ok(Filter::Call(p.call(name, start)?));
                }
                for op in ["!=", ">=", "<=", "=", ">", "<"] {
                    if p.eat(op) {
                        return Ok(Filter::Compare {
                            field: name,
                            op: op.into(),
                            value: p.pattern()?,
                        });
                    }
                }
                if p.keyword("like") {
                    return Ok(Filter::Compare {
                        field: name,
                        op: "like".into(),
                        value: p.pattern()?,
                    });
                }
            }
            p.pos = start;
            if p.keyword("true") {
                return Ok(Filter::Bool(true));
            }
            if p.keyword("false") {
                return Ok(Filter::Bool(false));
            }
            Ok(Filter::Text(p.pattern()?))
        })
    }
    fn pattern(&mut self) -> Result<Pattern> {
        self.space();
        if self.rest().starts_with('"') {
            return Ok(Pattern::Text(self.string()?));
        }
        if self.eat("?") {
            return Ok(Pattern::Parameter(self.identifier()?));
        }
        if self.rest().starts_with('/') {
            self.pos += 1;
            let start = self.pos;
            let mut escaped = false;
            while let Some(c) = self.rest().chars().next() {
                if c == '/' && !escaped {
                    let pattern = self.input[start..self.pos].to_string();
                    self.pos += 1;
                    let fs = self.pos;
                    while self.rest().starts_with(|x: char| x.is_ascii_alphabetic()) {
                        self.pos += 1;
                    }
                    return Ok(Pattern::Regex {
                        pattern,
                        flags: self.input[fs..self.pos].into(),
                    });
                }
                if c == '\n' || c == '\r' {
                    return self.fail("A regex literal cannot span lines.", false);
                }
                self.pos += c.len_utf8();
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                }
            }
            return self.fail("Unterminated regular expression.", false);
        }
        let start = self.pos;
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() || "|(){},;<>!=\"".contains(c) {
                break;
            }
            if "'`".contains(c) {
                return self.fail(
                    "This unquoted pattern is outside the supported grammar.",
                    true,
                );
            }
            self.pos += c.len_utf8();
        }
        if self.pos == start {
            return self.fail("Expected a filter value.", false);
        }
        let text = &self.input[start..self.pos];
        if text.ends_with("//") {
            return self.fail(
                "Ambiguous slash syntax is outside this profile; quote the pattern.",
                true,
            );
        }
        Ok(Pattern::Text(text.into()))
    }
    fn call(&mut self, name: String, start: usize) -> Result<Call> {
        self.nested(|p| {
            p.expect("(")?;
            let mut args = Vec::new();
            if !p.eat(")") {
                loop {
                    let before = p.pos;
                    let mut argname = None;
                    if let Ok(s) = p.identifier() {
                        if p.eat("=") {
                            if p.rest().starts_with('=') {
                                p.pos = before;
                            } else {
                                argname = Some(s);
                            }
                        } else {
                            p.pos = before;
                        }
                    } else {
                        p.pos = before;
                    }
                    let value = if name.eq_ignore_ascii_case("defineTable")
                        && argname.as_deref() == Some("include")
                        && p.eat("*")
                    {
                        Value::String("*".into())
                    } else {
                        p.expression(0)?
                    };
                    args.push(Arg {
                        name: argname,
                        value,
                    });
                    if p.eat(")") {
                        break;
                    }
                    p.expect(",")?;
                }
            }
            Ok(Call {
                name,
                args,
                start,
                end: p.pos,
            })
        })
    }
    fn number_atom(&mut self) -> Option<Value> {
        self.space();
        let start = self.pos;
        let mut digits = 0;
        while self.rest().starts_with(|c: char| c.is_ascii_digit()) {
            self.pos += 1;
            digits += 1;
        }
        if self.rest().starts_with('.') {
            self.pos += 1;
            while self.rest().starts_with(|c: char| c.is_ascii_digit()) {
                self.pos += 1;
                digits += 1;
            }
        }
        if digits == 0 {
            self.pos = start;
            return None;
        }
        if self.rest().starts_with(['e', 'E']) {
            self.pos += 1;
            if self.rest().starts_with(['+', '-']) {
                self.pos += 1;
            }
            let exponent = self.pos;
            while self.rest().starts_with(|c: char| c.is_ascii_digit()) {
                self.pos += 1;
            }
            if self.pos == exponent {
                self.pos = start;
                return None;
            }
        }
        if self
            .rest()
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || "_.[".contains(c))
        {
            self.pos = start;
            return None;
        }
        Some(Value::Number(self.input[start..self.pos].into()))
    }
    fn query_attributes(&mut self) -> Result<Vec<Arg>> {
        let mut attributes: Vec<Arg> = Vec::new();
        loop {
            self.space();
            let name = ["include", "after", "before"].into_iter().find(|name| {
                self.rest()
                    .strip_prefix(name)
                    .is_some_and(|tail| tail.trim_start().starts_with(':'))
            });
            let Some(name) = name else {
                break;
            };
            self.pos += name.len();
            self.expect(":")?;
            if attributes.iter().any(|a| a.name.as_deref() == Some(name)) {
                return self.fail("Duplicate query attribute.", false);
            }
            let value = if name == "include" && self.eat("*") {
                Value::Word("*".into())
            } else if name == "include" && self.eat("[") {
                let mut fields = Vec::new();
                if !self.eat("]") {
                    loop {
                        fields.push(if self.eat("*") {
                            Value::Word("*".into())
                        } else {
                            self.expression(0)?
                        });
                        if self.eat("]") {
                            break;
                        }
                        self.expect(",")?;
                    }
                }
                Value::Array(fields)
            } else {
                self.expression(0)?
            };
            attributes.push(Arg {
                name: Some(name.into()),
                value,
            });
        }
        Ok(attributes)
    }
    fn expression(&mut self, min: u8) -> Result<Value> {
        self.nested(|p| {
            let mut left = if p.eat("!") {
                Value::Unary {
                    op: "!".into(),
                    value: Box::new(p.expression(9)?),
                }
            } else if p.eat("-") {
                Value::Unary {
                    op: "-".into(),
                    value: Box::new(p.expression(9)?),
                }
            } else if p.eat("+") {
                Value::Unary {
                    op: "+".into(),
                    value: Box::new(p.expression(9)?),
                }
            } else if p.eat("(") {
                let v = p.expression(0)?;
                p.expect(")")?;
                v
            } else if p.eat("[") {
                let mut xs = Vec::new();
                if !p.eat("]") {
                    loop {
                        xs.push(p.expression(0)?);
                        if p.eat("]") {
                            break;
                        }
                        p.expect(",")?;
                    }
                }
                Value::Array(xs)
            } else if p.eat("{") {
                let q = p.pipeline()?;
                p.expect("}")?;
                Value::Query {
                    label: None,
                    pipeline: q,
                    attributes: p.query_attributes()?,
                }
            } else if p.rest().starts_with('"') {
                Value::String(p.string()?)
            } else if p.eat("?") {
                Value::Parameter(p.identifier()?)
            } else if let Some(number) = p.number_atom() {
                number
            } else {
                p.space();
                let before = p.pos;
                let word = p.identifier()?;
                if p.eat(":") {
                    p.expect("{")?;
                    let q = p.pipeline()?;
                    p.expect("}")?;
                    Value::Query {
                        label: Some(word),
                        pipeline: q,
                        attributes: p.query_attributes()?,
                    }
                } else if p.rest().trim_start().starts_with('(') {
                    Value::Call(p.call(word, before)?)
                } else if word == "true" || word == "false" {
                    Value::Bool(word == "true")
                } else if word.parse::<serde_json::Number>().is_ok() {
                    Value::Number(word)
                } else {
                    Value::Word(word)
                }
            };
            if p.depth + value_depth(&left) > MAX_DEPTH {
                return p.fail("CQL nesting or node budget exceeded.", true);
            }
            loop {
                p.space();
                let mut found = None;
                for (op, prec) in [
                    ("<=>", 1),
                    ("==", 2),
                    ("!=", 2),
                    ("<=", 2),
                    (">=", 2),
                    ("<", 2),
                    (">", 2),
                    ("+", 4),
                    ("-", 4),
                    ("*", 6),
                    ("/", 6),
                    ("%", 6),
                ] {
                    if prec >= min && p.rest().starts_with(op) {
                        found = Some((op, prec));
                        break;
                    }
                }
                let Some((op, prec)) = found else {
                    break;
                };
                p.pos += op.len();
                let right = p.expression(prec + 1)?;
                if p.depth + 1 + value_depth(&left).max(value_depth(&right)) > MAX_DEPTH {
                    return p.fail("CQL nesting or node budget exceeded.", true);
                }
                left = Value::Binary {
                    left: Box::new(left),
                    op: op.into(),
                    right: Box::new(right),
                };
            }
            Ok(left)
        })
    }
}

fn call_depth(call: &Call) -> usize {
    1 + call
        .args
        .iter()
        .map(|arg| value_depth(&arg.value))
        .max()
        .unwrap_or(0)
}

fn value_depth(value: &Value) -> usize {
    match value {
        Value::Binary { left, right, .. } => 1 + value_depth(left).max(value_depth(right)),
        Value::Unary { value, .. } => 1 + value_depth(value),
        Value::Array(values) => 1 + values.iter().map(value_depth).max().unwrap_or(0),
        Value::Call(call) => call_depth(call),
        Value::Query {
            pipeline,
            attributes,
            ..
        } => {
            1 + pipeline_depth(pipeline).max(
                attributes
                    .iter()
                    .map(|arg| value_depth(&arg.value))
                    .max()
                    .unwrap_or(0),
            )
        }
        _ => 1,
    }
}

fn filter_depth(filter: &Filter) -> usize {
    match filter {
        Filter::And(filters) | Filter::Or(filters) => {
            1 + filters.iter().map(filter_depth).max().unwrap_or(0)
        }
        Filter::Not(filter) => 1 + filter_depth(filter),
        Filter::Call(call) => call_depth(call),
        _ => 1,
    }
}

fn pipeline_depth(pipeline: &Pipeline) -> usize {
    1 + pipeline
        .steps
        .iter()
        .map(|step| match step {
            Step::Assign { value, .. } => value_depth(value),
            Step::Call(call) => call_depth(call),
            Step::Filter(filter) => filter_depth(filter),
            Step::Case(branches) => 1 + branches.iter().map(pipeline_depth).max().unwrap_or(0),
            Step::Match { arms, .. } => {
                1 + arms
                    .iter()
                    .map(|(_, pipeline)| pipeline_depth(pipeline))
                    .max()
                    .unwrap_or(0)
            }
            Step::Link { .. } => 1,
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn or_binds_tighter_and_comments_are_not_code() {
        let q = parse("a=1 AND b=2 OR c=3 // count(foo=1)\n | count()").unwrap();
        let Step::Filter(Filter::And(xs)) = &q.steps[0] else {
            panic!("wrong grouping")
        };
        assert!(matches!(xs[1], Filter::Or(_)));
    }
    #[test]
    fn distinct_filter_and_expression_contexts() {
        assert!(parse("a=b | test(a == b) | x := a / 2").is_ok());
        assert!(parse("groupBy(").is_err());
        assert!(parse("count() garbage").is_err());
        assert!(parse("x=/[a-z]+/i | count()").is_ok());
    }
    #[test]
    fn nested_queries_and_links() {
        assert!(
            parse("correlate(A: { x=1 }, B: { y=2 | aid <=> A.aid }, sequence=true, within=10m)")
                .is_ok()
        );
    }
    #[test]
    fn depth_is_bounded() {
        assert!(parse(&format!("{}a=1{}", "(".repeat(90), ")".repeat(90))).is_err());
    }
    #[test]
    fn expression_depth_includes_binary_spines_and_containers() {
        let chain = |count| vec!["a"; count].join("+");
        assert!(parse("x := a * (b + c)").is_ok());
        assert!(parse(&format!("x := {}", chain(MAX_DEPTH - 2))).is_ok());
        assert!(parse(&format!("x := {}", chain(MAX_DEPTH - 1))).is_err());
        assert!(parse(&format!("x := [{}]", chain(MAX_DEPTH - 3))).is_ok());
        for query in [
            format!("x := [{}]", chain(MAX_DEPTH - 2)),
            format!("x := min({})", chain(MAX_DEPTH - 2)),
            format!("x := 1 + ({})", chain(MAX_DEPTH - 2)),
        ] {
            assert!(parse(&query).is_err(), "{query}");
        }
    }
}
