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
    // Recognize function composition only when the complete query can be parsed.
    parse_pipeline(input, false).or_else(|failure| {
        if failure.unsupported {
            parse_pipeline(input, true).map_err(|alternative| {
                if alternative.diagnostic.code == "CQL_QUOTE" {
                    alternative
                } else {
                    failure
                }
            })
        } else {
            Err(failure)
        }
    })
}

fn parse_pipeline(input: &str, function_composition: bool) -> Result<Pipeline> {
    let mut p = Parser {
        input,
        pos: 0,
        depth: 0,
        nodes: 0,
        function_composition,
        advisories: vec![],
        last_or: false,
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
    function_composition: bool,
    advisories: Vec<Advisory>,
    last_or: bool,
}
impl Parser<'_> {
    fn fail<T>(&self, message: &str, unsupported: bool) -> Result<T> {
        let end = self.input[self.pos..]
            .chars()
            .next()
            .map_or(self.pos, |c| self.pos + c.len_utf8());
        if let Some(c) = self.rest().chars().next().filter(|c| {
            *c == '`'
                || ("'‘’“”".contains(*c)
                    && (self.pos == 0
                        || self.input[..self.pos]
                            .chars()
                            .next_back()
                            .is_some_and(|p| p.is_whitespace() || "=([{,;|<>!+".contains(p))))
        }) {
            return Err(Failure {
                diagnostic: Diagnostic::error(
                    "CQL_QUOTE",
                    format!("'{c}' is not a string delimiter; use straight double quotes."),
                ),
                unsupported: false,
            });
        }
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
        if tail.get(..s.len()).is_some_and(|x| {
            x == s.to_ascii_lowercase()
                || (s != "like" && s != "true" && s != "false" && x == s.to_ascii_uppercase())
        }) && tail[s.len()..]
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
                return decode_string(&self.input[start..self.pos]).map_err(|_| Failure {
                    diagnostic: Diagnostic::error(
                        "CQL_STRING_ESCAPE",
                        "Invalid hexadecimal string escape; use four or eight hexadecimal digits.",
                    ),
                    unsupported: false,
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
        self.unquoted_identifier()
    }
    fn unquoted_identifier(&mut self) -> Result<String> {
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
    fn check_assignment_target(&mut self, start: usize, field: &str) -> Result<()> {
        if !self.input[start..].starts_with('"') && reserved_field_prefix(field) {
            self.pos = start;
            return self.fail(
                "Quote assignment field names whose initial segment is NOT, AND or OR.",
                true,
            );
        }
        Ok(())
    }
    fn pipeline(&mut self) -> Result<Pipeline> {
        self.nested(|p| {
            let mut steps = Vec::new();
            let mut advisories = Vec::new();
            p.space();
            if p.pos == p.input.len() || p.rest().starts_with(['}', ';']) {
                return Ok(Pipeline { steps, advisories });
            }
            if p.eat("|") && p.ending() {
                return p.fail("Expected a stage after the leading pipe.", false);
            }
            loop {
                let enclosing = std::mem::take(&mut p.advisories);
                let step = p.step()?;
                advisories.extend(
                    std::mem::replace(&mut p.advisories, enclosing)
                        .into_iter()
                        .map(|advice| (steps.len(), advice)),
                );
                steps.push(step);
                p.space();
                if !p.eat("|") {
                    break;
                }
                if p.ending() {
                    if p.rest().starts_with('|') {
                        return p.fail("An interior pipeline stage cannot be empty.", false);
                    }
                    break;
                }
            }
            Ok(Pipeline { steps, advisories })
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
                self.check_assignment_target(start, &name)?;
                return Ok(Step::Assign {
                    field: name,
                    value: self.expression(0)?,
                });
            }
            if self.eat("=~") {
                let negated = self.eat("!");
                self.space();
                let call_start = self.pos;
                let n = self.unquoted_identifier()?;
                let mut c = self.call(n, call_start)?;
                c.args.push(Arg {
                    name: Some("field".into()),
                    value: Value::String(name),
                });
                return Ok(if negated {
                    Step::Filter(Filter::Not(Box::new(Filter::Call(c))))
                } else {
                    Step::Call(c)
                });
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
            if !name.eq_ignore_ascii_case("not")
                && !self.input[start..].starts_with('"')
                && self.eat("(")
            {
                self.pos = start;
                let name = self.unquoted_identifier()?;
                let call = self.call(name, start)?;
                let end = self.pos;
                let composition =
                    self.function_composition && (self.keyword("AND") || self.keyword("OR"));
                self.pos = end;
                if composition {
                    return self.nested(|p| {
                        let first = p.or_tail(Filter::Call(call))?;
                        Ok(Step::Filter(p.and_tail(first)?))
                    });
                }
                return Ok(Step::Call(call));
            }
        }
        self.pos = start;
        if self.eat("[") {
            let mut args = Vec::new();
            if !self.eat("]") {
                loop {
                    args.push(self.aggregate_list_item()?);
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
        self.space();
        let first = self.pos;
        let filter = self.and()?;
        if matches!(&filter, Filter::Text(_) | Filter::And(_) | Filter::Or(_)) {
            let mut head = &filter;
            while let Filter::And(xs) | Filter::Or(xs) = head {
                head = &xs[0];
            }
            if let Filter::Text(Pattern::Text(word)) = head
                && self.input[first..].starts_with(word)
                && !word.starts_with('"')
            {
                if let Some(function) = crate::catalog::FUNCTIONS
                    .iter()
                    .find(|f| f.name.eq_ignore_ascii_case(word))
                {
                    self.advise("CQL_BARE_FUNCTION_NAME", format!("{word} is searched as text; write {}() to call the function, or quote it to search for the text.", function.name), false);
                } else if [
                    "where", "search", "dedup", "fields", "rex", "spath", "lookup", "tstats",
                ]
                .iter()
                .any(|w| word.eq_ignore_ascii_case(w))
                {
                    self.advise(
                        "CQL_BARE_COMMAND_WORD",
                        format!(
                            "{word} is not a query-language command here and is searched as text."
                        ),
                        false,
                    );
                }
            }
        }
        Ok(Step::Filter(filter))
    }
    fn advise(&mut self, code: &'static str, message: impl Into<String>, informational: bool) {
        self.advisories.push(Advisory {
            code,
            message: message.into(),
            informational,
        });
    }
    fn and(&mut self) -> Result<Filter> {
        self.nested(|p| {
            let first = p.or()?;
            p.and_tail(first)
        })
    }
    fn and_tail(&mut self, first: Filter) -> Result<Filter> {
        let mut direct_or = self.last_or;
        let mut xs = vec![first];
        loop {
            if self.keyword("AND") {
                xs.push(self.or()?);
                direct_or |= self.last_or;
            } else if self.ending() {
                break;
            } else {
                xs.push(self.or()?);
                direct_or |= self.last_or;
            }
        }
        let filter = if xs.len() == 1 {
            xs.remove(0)
        } else {
            Filter::And(xs)
        };
        if direct_or && matches!(filter, Filter::And(_)) {
            self.advise("CQL_OR_PRECEDENCE", format!("or binds tighter than and: this is read as {}. Add parentheses to make the intent explicit.", filter.render()), true);
        }
        Ok(filter)
    }
    fn or(&mut self) -> Result<Filter> {
        let first = self.primary_filter()?;
        self.or_tail(first)
    }
    fn or_tail(&mut self, first: Filter) -> Result<Filter> {
        let mut xs = vec![first];
        while self.keyword("OR") {
            xs.push(self.primary_filter()?);
        }
        self.last_or = xs.len() > 1;
        Ok(if xs.len() == 1 {
            xs.remove(0)
        } else {
            Filter::Or(xs)
        })
    }
    fn primary_filter(&mut self) -> Result<Filter> {
        self.nested(|p| {
            let word_negation = p.keyword("NOT");
            if word_negation || p.eat("!") {
                if word_negation && p.ending() {
                    return Err(Failure { diagnostic: Diagnostic::error("CQL_RESERVED_WORD", "'not' is reserved; supply a filter to negate, or quote it to search for text."), unsupported: false });
                }
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
                if !p.input[start..].starts_with('"') && p.rest().starts_with('(') {
                    return Ok(Filter::Call(p.call(name, start)?));
                }
                for op in ["!=", ">=", "<=", "=", ">", "<"] {
                    if p.eat(op) {
                        return Ok(Filter::Compare {
                            field: name.clone(),
                            op: op.into(),
                            value: p.comparison_pattern(&name)?,
                        });
                    }
                }
                if p.keyword("like") {
                    return Ok(Filter::Compare {
                        field: name.clone(),
                        op: "like".into(),
                        value: p.comparison_pattern(&name)?,
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
            let quoted = p.rest().starts_with('"');
            let pattern = p.pattern()?;
            if !quoted && let Pattern::Text(word) = &pattern
                && ["and", "or", "not", "like", "in", "match", "case", "true", "false"].contains(&word.to_ascii_lowercase().as_str())
            {
                return Err(Failure { diagnostic: Diagnostic::error("CQL_RESERVED_WORD", format!("'{word}' is reserved; use the lower-case or upper-case operator spelling where supported, or quote it to search for text.")), unsupported: false });
            }
            if !quoted && pattern == Pattern::Text("&&".into()) {
                p.advise("CQL_OPERATOR_TEXT", "&& is searched as text; use and or a space between the terms.", false);
            }
            Ok(Filter::Text(if !quoted && matches!(&pattern, Pattern::Text(text) if text == "*") {
                Pattern::Any
            } else {
                pattern
            }))
        })
    }
    fn comparison_pattern(&mut self, field: &str) -> Result<Pattern> {
        self.space();
        let bare = !self.rest().starts_with(['"', '?']);
        let pattern = self.pattern()?;
        if bare && matches!(&pattern, Pattern::Text(word) if word.eq_ignore_ascii_case("null")) {
            self.advise("CQL_NULL_TEXT", format!("The value is the text \"null\"; use {}=* for has the field and not {}=* for does not have the field.", field, field), false);
        }
        Ok(pattern)
    }
    fn pattern(&mut self) -> Result<Pattern> {
        self.space();
        self.saved_call()?;
        if self.rest().starts_with('"') {
            return Ok(Pattern::Text(self.string()?));
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
        let mut text = String::new();
        while let Some(c) = self.rest().chars().next() {
            if c == '$' {
                return self.fail("Put values containing $ in double quotes.", false);
            }
            if c == '?' {
                self.pos += 1;
                if self.rest().starts_with('{') {
                    self.pos += 1;
                    self.identifier()?;
                    self.expect("=")?;
                    let value = self.nested(|p| p.pattern())?;
                    self.expect("}")?;
                    match value {
                        Pattern::Text(default) => text.push_str(&default),
                        Pattern::Parameter(name) => return Ok(Pattern::Parameter(name)),
                        other if text.is_empty() => return Ok(other),
                        _ => {
                            return self.fail("Put the parameter default in double quotes.", false);
                        }
                    }
                    continue;
                }
                let start = self.pos;
                while self
                    .rest()
                    .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                {
                    self.pos += 1;
                }
                if self.pos == start {
                    return self.fail("Put values containing ? in double quotes.", false);
                }
                return Ok(Pattern::Parameter(self.input[start..self.pos].into()));
            }
            if c.is_whitespace() || "|(){},;<>!=\"".contains(c) {
                break;
            }
            if "'`".contains(c) || (text.is_empty() && "‘’“”".contains(c)) {
                return self.fail(
                    "This unquoted pattern is outside the supported grammar.",
                    true,
                );
            }
            self.pos += c.len_utf8();
            text.push(c);
        }
        if text.is_empty() {
            return self.fail("Expected a filter value.", false);
        }
        if text.ends_with("//") {
            return self.fail(
                "Ambiguous slash syntax is outside this profile; quote the pattern.",
                true,
            );
        }
        Ok(Pattern::Text(text))
    }
    fn saved_call(&mut self) -> Result<()> {
        self.space();
        if !self.rest().starts_with('$') {
            return Ok(());
        }
        let start = self.pos;
        self.pos += 1;
        let name = self.pos;
        let named = if self.rest().starts_with('"') {
            self.string().is_ok()
        } else {
            while let Some(c) = self.rest().chars().next() {
                if c.is_whitespace() || "|(){},;<>!=\"'`".contains(c) {
                    break;
                }
                self.pos += c.len_utf8();
            }
            self.pos > name
        };
        let invocation = named && self.eat("(");
        self.pos = start;
        if invocation {
            self.fail(
                "Saved-query and resource invocations require resolution outside this profile.",
                true,
            )
        } else {
            Ok(())
        }
    }
    fn call(&mut self, name: String, start: usize) -> Result<Call> {
        self.nested(|p| {
            p.expect("(")?;
            let mut args = Vec::new();
            let mut positional = 0;
            if !p.eat(")") {
                let spec = crate::catalog::function(&name);
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
                    let aggregate_argument = spec.is_some_and(|spec| {
                        argname
                            .as_deref()
                            .or_else(|| spec.positional.get(positional).copied())
                            .is_some_and(|arg| spec.argument_kind(arg) == Some("g"))
                    });
                    let mut value = if name.eq_ignore_ascii_case("defineTable")
                        && argname.as_deref() == Some("include")
                        && p.eat("*")
                    {
                        Value::String("*".into())
                    } else if name.eq_ignore_ascii_case("wildcard")
                        && matches!(argname.as_deref(), None | Some("pattern"))
                    {
                        if let Some(pattern) = p.bare_glob() {
                            Value::String(pattern)
                        } else {
                            p.expression(0)?
                        }
                    } else {
                        p.expression_with_aggregate_aliases(0, aggregate_argument)?
                    };
                    if name.eq_ignore_ascii_case("sample")
                        && matches!(argname.as_deref(), None | Some("percentage"))
                    {
                        coerce_quoted_number(&mut value);
                    } else if name.eq_ignore_ascii_case("percentile")
                        && argname.as_deref() == Some("percentiles")
                        && let Value::Array(values) = &mut value
                    {
                        for value in values {
                            coerce_quoted_number(value);
                        }
                    }
                    positional += usize::from(argname.is_none());
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
            if name.eq_ignore_ascii_case("select") && args.len() > 1 && positional == args.len() {
                args = vec![Arg {
                    name: None,
                    value: Value::Array(args.into_iter().map(|arg| arg.value).collect()),
                }];
            }
            Ok(Call {
                name,
                args,
                start,
                end: p.pos,
            })
        })
    }
    fn bare_glob(&mut self) -> Option<String> {
        self.space();
        let start = self.pos;
        while let Some(c) = self
            .rest()
            .chars()
            .next()
            .filter(|c| c.is_alphanumeric() || "_@#.:$*-".contains(*c))
        {
            self.pos += c.len_utf8();
        }
        let end = self.pos;
        self.space();
        if self.input[start..end].contains('*') && self.rest().starts_with([',', ')']) {
            Some(self.input[start..end].into())
        } else {
            self.pos = start;
            None
        }
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
    fn aggregate_list_item(&mut self) -> Result<Value> {
        self.space();
        let start = self.pos;
        if let Ok(alias) = self.identifier()
            && self.eat(":=")
        {
            self.check_assignment_target(start, &alias)?;
            self.saved_call()?;
            let name = self.unquoted_identifier();
            self.space();
            if let Ok(name) = name
                && self.rest().starts_with('(')
            {
                let mut call = self.call(name, start)?;
                call.args.push(Arg {
                    name: Some("as".into()),
                    value: Value::String(alias),
                });
                return Ok(Value::Call(call));
            }
            return self.fail(
                "Expected a function call after ':=' in the aggregate list.",
                false,
            );
        }
        self.pos = start;
        self.expression(0)
    }
    fn expression(&mut self, min: u8) -> Result<Value> {
        self.expression_with_aggregate_aliases(min, false)
    }
    fn expression_with_aggregate_aliases(
        &mut self,
        min: u8,
        aggregate_argument: bool,
    ) -> Result<Value> {
        self.nested(|p| {
            p.saved_call()?;
            if p.pos == p.input.len() || p.rest().starts_with([',', ')', ']', '}', ';', '|']) {
                return p.fail("Expected an expression.", false);
            }
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
                        xs.push(if aggregate_argument {
                            p.aggregate_list_item()?
                        } else {
                            p.expression(0)?
                        });
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
                if p.eat("{") {
                    p.identifier()?;
                    p.expect("=")?;
                    let value = p.expression(0)?;
                    p.expect("}")?;
                    value
                } else {
                    Value::Parameter(p.identifier()?)
                }
            } else if let Some(number) = p.number_atom() {
                number
            } else {
                p.space();
                let before = p.pos;
                let word = p.unquoted_identifier()?;
                if aggregate_argument && p.eat(":=") {
                    return p.fail("Use ':=' aliases inside an aggregate function list.", false);
                } else if p.eat(":") {
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

fn coerce_quoted_number(value: &mut Value) {
    if let Value::String(text) = value
        && text.trim().parse::<f64>().is_ok_and(f64::is_finite)
    {
        *value = Value::Number(text.trim().into());
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

fn decode_string(raw: &str) -> std::result::Result<String, ()> {
    let mut json = String::from("\"");
    let mut chars = raw[1..raw.len() - 1].chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            json.push(c);
            continue;
        }
        let next = chars.next().ok_or(())?;
        if next == 'U' {
            let digits: String = chars.by_ref().take(8).collect();
            if digits.len() != 8 {
                return Err(());
            }
            let ch = u32::from_str_radix(&digits, 16)
                .ok()
                .and_then(char::from_u32)
                .ok_or(())?;
            let encoded = serde_json::to_string(&ch.to_string()).map_err(|_| ())?;
            json.push_str(&encoded[1..encoded.len() - 1]);
        } else if "tnrbf\\\"u".contains(next) {
            json.push('\\');
            json.push(next);
        } else {
            json.push_str("\\\\");
            json.push(next);
        }
    }
    json.push('"');
    serde_json::from_str(&json).map_err(|_| ())
}

pub(super) fn lexical_diagnostics(input: &str) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    let mut previous = None;
    while i < chars.len() {
        if chars[i] == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
        } else if chars[i] == '"' || chars[i] == '/' {
            let delimiter = chars[i];
            i += 1;
            while i < chars.len() && chars[i] != delimiter {
                if chars[i] == '\\' {
                    if delimiter == '"'
                        && let Some(escape) = chars.get(i + 1)
                        && "tnrbf".contains(*escape)
                        && chars.get(i + 2).is_some_and(char::is_ascii_alphanumeric)
                    {
                        let name = match escape {
                            't' => "tab",
                            'n' => "newline",
                            'r' => "carriage return",
                            'b' => "backspace",
                            _ => "form feed",
                        };
                        out.push(Diagnostic::warning("CQL_STRING_ESCAPE_CONTROL", format!("The escape is a {name} control character and will not match a literal backslash; write \\\\ for a backslash.")));
                    }
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            previous = Some('v');
        } else {
            let c = chars[i];
            if !c.is_whitespace() {
                if (c == '|' && matches!(previous, None | Some('{' | ';')))
                    || (matches!(c, '}' | ';') && previous == Some('|'))
                {
                    out.push(Diagnostic::warning(
                        "CQL_EMPTY_STAGE",
                        "The empty stage has no effect and is usually a typing slip.",
                    ));
                }
                previous = Some(c);
            }
            i += 1;
        }
    }
    if previous == Some('|') {
        out.push(Diagnostic::warning(
            "CQL_EMPTY_STAGE",
            "The empty stage has no effect and is usually a typing slip.",
        ));
    }
    out
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
