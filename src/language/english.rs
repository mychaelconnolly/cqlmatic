//! Bounded English, not prefix matching or probabilistic intent inference.
use crate::contracts::*;

#[derive(Debug, Clone)]
enum Token {
    Word(String),
    Quoted(String),
    Comma,
    Open,
    Close,
}
struct Reader {
    tokens: Vec<Token>,
    at: usize,
    groups: usize,
}
type Result<T> = std::result::Result<T, Diagnostic>;
fn error(message: &str) -> Diagnostic {
    Diagnostic::error("CQL_INTENT_UNRESOLVED", message).at("/input/text")
}
fn unsupported(message: &str) -> Diagnostic {
    Diagnostic::unsupported("CQL_UNSUPPORTED_INTENT", message).at("/input/text")
}

fn lex(text: &str) -> Result<Vec<Token>> {
    super::plan::checked_text(text)?;
    if text.len() > 16_384 {
        return Err(error("English requests are limited to 16 KiB."));
    }
    let mut at = 0;
    let mut out = Vec::new();
    while at < text.len() {
        let c = text[at..].chars().next().expect("not empty");
        if c.is_whitespace() {
            at += c.len_utf8();
            continue;
        }
        match c {
            ',' => {
                out.push(Token::Comma);
                at += 1;
            }
            '(' => {
                out.push(Token::Open);
                at += 1;
            }
            ')' => {
                out.push(Token::Close);
                at += 1;
            }
            '"' => {
                let start = at;
                at += 1;
                let mut escaped = false;
                let mut closed = false;
                while at < text.len() {
                    let c = text[at..].chars().next().unwrap();
                    at += c.len_utf8();
                    if c == '"' && !escaped {
                        closed = true;
                        break;
                    }
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    }
                }
                if !closed {
                    return Err(error("Unterminated quoted text."));
                }
                out.push(Token::Quoted(
                    serde_json::from_str(&text[start..at])
                        .map_err(|_| error("Use JSON escaping inside quoted text."))?,
                ));
            }
            _ => {
                let start = at;
                while at < text.len() {
                    let c = text[at..].chars().next().unwrap();
                    if c.is_whitespace() || ",()\"".contains(c) {
                        break;
                    }
                    at += c.len_utf8();
                }
                out.push(Token::Word(text[start..at].into()));
            }
        }
    }
    Ok(out)
}
impl Reader {
    fn eat(&mut self, word: &str) -> bool {
        if matches!(self.tokens.get(self.at),Some(Token::Word(w)) if w.eq_ignore_ascii_case(word)) {
            self.at += 1;
            true
        } else {
            false
        }
    }
    fn want(&mut self, word: &str) -> Result<()> {
        if self.eat(word) {
            Ok(())
        } else {
            Err(error(&format!("Expected '{word}'.")))
        }
    }
    fn name(&mut self) -> Result<String> {
        match self.tokens.get(self.at).cloned() {
            Some(Token::Word(w) | Token::Quoted(w)) => {
                self.at += 1;
                super::plan::checked_field(&w)?;
                Ok(w)
            }
            _ => Err(error("Supply an explicit field, source, or event name.")),
        }
    }
    fn number(&mut self) -> Result<u32> {
        let s = self.name()?;
        s.parse::<u32>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| error("Supply a positive integer."))
    }
    fn literal(&mut self) -> Result<Scalar> {
        match self.tokens.get(self.at).cloned() {
            Some(Token::Quoted(s)) => {
                self.at += 1;
                Ok(Scalar::Text(s))
            }
            Some(Token::Word(s)) => {
                if let Ok(n) = s.parse::<serde_json::Number>() {
                    self.at += 1;
                    Ok(Scalar::Number(n))
                } else if s == "true" || s == "false" {
                    self.at += 1;
                    Ok(Scalar::Boolean(s == "true"))
                } else {
                    Err(error(
                        "Quote literal text, including identities, paths and IP addresses.",
                    ))
                }
            }
            _ => Err(error("Expected a quoted string, number or boolean.")),
        }
    }
    fn literal_text(&mut self) -> Result<String> {
        match self.literal()? {
            Scalar::Text(s) => Ok(s),
            _ => Err(error("This matching mode requires quoted text.")),
        }
    }
    fn names(&mut self) -> Result<Vec<String>> {
        let mut xs = vec![self.name()?];
        while matches!(self.tokens.get(self.at), Some(Token::Comma)) {
            self.at += 1;
            xs.push(self.name()?);
        }
        Ok(xs)
    }
    fn missing(&mut self) -> Result<Missing> {
        if self.eat("including") {
            self.want("missing")?;
            Ok(Missing::Include)
        } else if self.eat("excluding") {
            self.want("missing")?;
            Ok(Missing::Exclude)
        } else {
            Err(error(
                "Specify 'including missing' or 'excluding missing' for a negated comparison.",
            ))
        }
    }
    fn simple(&mut self) -> Result<Predicate> {
        if matches!(self.tokens.get(self.at), Some(Token::Open)) {
            self.at += 1;
            self.groups += 1;
            if self.groups > 32 {
                return Err(error("English boolean nesting exceeds the budget."));
            }
            let f = self.boolean()?;
            if !matches!(self.tokens.get(self.at), Some(Token::Close)) {
                return Err(error("Close the parenthesized condition."));
            }
            self.at += 1;
            self.groups -= 1;
            return Ok(f);
        }
        let field = self.name()?;
        if self.eat("exists") {
            return Ok(Predicate::Exists {
                field,
                present: true,
            });
        }
        if self.eat("is") {
            if self.eat("missing") {
                return Ok(Predicate::Exists {
                    field,
                    present: false,
                });
            }
            if self.eat("present") {
                return Ok(Predicate::Exists {
                    field,
                    present: true,
                });
            }
            if self.eat("not") {
                let value = self.literal()?;
                return Ok(Predicate::NotEq {
                    field,
                    value,
                    missing: self.missing()?,
                });
            }
            return Ok(Predicate::Eq {
                field,
                value: self.literal()?,
            });
        }
        if self.eat("equals") || self.eat("=") {
            return Ok(Predicate::Eq {
                field,
                value: self.literal()?,
            });
        }
        if self.eat("differs") {
            self.want("from")?;
            let value = self.literal()?;
            return Ok(Predicate::NotEq {
                field,
                value,
                missing: self.missing()?,
            });
        }
        let before = self.at;
        let mut cmp = None;
        for (s, op) in [
            (">=", Comparison::Ge),
            ("<=", Comparison::Le),
            (">", Comparison::Gt),
            ("<", Comparison::Lt),
        ] {
            if self.eat(s) {
                cmp = Some(op);
                break;
            }
        }
        if let Some(operator) = cmp {
            let Scalar::Number(value) = self.literal()? else {
                return Err(error("Numeric comparisons require a number."));
            };
            return Ok(Predicate::Compare {
                field,
                operator,
                value,
            });
        }
        self.at = before;
        if self.eat("matches") {
            if self.eat("glob") {
                return Ok(Predicate::Glob {
                    field,
                    pattern: self.literal_text()?,
                    negate: false,
                    missing: None,
                });
            }
            if self.eat("regex") {
                let pattern = self.literal_text()?;
                let flags = if self.eat("case-insensitive") {
                    "i"
                } else {
                    ""
                };
                return Ok(Predicate::Regex {
                    field,
                    pattern,
                    flags: flags.into(),
                    negate: false,
                    missing: None,
                });
            }
            return Err(error("Specify 'matches glob' or 'matches regex'."));
        }
        if self.eat("contains") {
            let s = self.literal_text()?;
            let mut pattern = String::new();
            for c in s.chars() {
                if "\\.+*?()[]{}^$|/".contains(c) {
                    pattern.push('\\');
                }
                match c {
                    '\n' => pattern.push_str("\\n"),
                    '\r' => pattern.push_str("\\r"),
                    '\t' => pattern.push_str("\\t"),
                    _ => pattern.push(c),
                }
            }
            return Ok(Predicate::Regex {
                field,
                pattern,
                flags: String::new(),
                negate: false,
                missing: None,
            });
        }
        let message = "Use equals, is not, exists, is missing, contains, a numeric comparison, matches glob, or matches regex.";
        Err(if self.at == self.tokens.len() {
            error(message)
        } else {
            unsupported(message)
        })
    }
    fn boolean(&mut self) -> Result<Predicate> {
        let mut items = vec![self.simple()?];
        let mut operator = None;
        loop {
            let next = if self.eat("and") {
                Some(true)
            } else if self.eat("or") {
                Some(false)
            } else {
                None
            };
            let Some(next) = next else {
                break;
            };
            if operator.is_some_and(|op| op != next) {
                return Err(error(
                    "Parenthesize mixed AND/OR conditions; English precedence is ambiguous.",
                ));
            }
            operator = Some(next);
            items.push(self.simple()?);
        }
        Ok(match operator {
            None => items.remove(0),
            Some(true) => Predicate::All { items },
            Some(false) => Predicate::Any { items },
        })
    }
}

pub fn interpret(text: &str, source: &Scope) -> Result<Plan> {
    let mut r = Reader {
        tokens: lex(text)?,
        at: 0,
        groups: 0,
    };
    let mut source = source.clone();
    let mut keys = Vec::new();
    let mut distinct = None;
    let mut top = None;
    let mut latest = false;
    let is_count = if r.eat("count") {
        if r.eat("distinct") {
            let field = r.name()?;
            let accuracy = if r.eat("exactly") {
                Accuracy::Exact
            } else if r.eat("approximately") {
                Accuracy::Estimate
            } else {
                return Err(error(
                    "Distinct counting requires 'exactly' or 'approximately'.",
                ));
            };
            distinct = Some((field, accuracy));
        } else {
            r.want("events")?;
            if r.eat("by") {
                keys = r.names()?;
            }
        }
        true
    } else if r.eat("top") {
        top = Some(r.number()?);
        keys = r.names()?;
        true
    } else if r.eat("show") || r.eat("find") || r.eat("list") {
        if r.eat("latest") {
            latest = true;
        } else {
            r.eat("earliest");
        }
        r.want("events")?;
        false
    } else {
        return Err(unsupported(
            "Start with 'show events', 'count events', 'count distinct FIELD exactly/approximately', or 'top N FIELD'.",
        ));
    };
    let mut filters = Vec::new();
    let mut selected = Vec::new();
    let mut limit = 200;
    let mut had_limit = false;
    let mut had_where = false;
    let mut had_source = false;
    let mut had_event = false;
    while r.at < r.tokens.len() {
        if r.eat("from") {
            if had_source {
                return Err(error("Specify a source only once."));
            }
            had_source = true;
            let repo = r.name()?;
            if source.repo.as_ref().is_some_and(|s| s != &repo) {
                return Err(error(
                    "Text source conflicts with the supplied source context.",
                ));
            }
            source.repo = Some(repo);
        } else if r.eat("event") {
            if had_event {
                return Err(error("Specify an event type only once."));
            }
            had_event = true;
            let event = r.name()?;
            if source.event.as_ref().is_some_and(|s| s != &event) {
                return Err(error("Text event type conflicts with supplied context."));
            }
            source.event = Some(event);
        } else if r.eat("where") {
            if had_where {
                return Err(error(
                    "Use a single where clause with explicit boolean grouping.",
                ));
            }
            had_where = true;
            filters.push(r.boolean()?);
        } else if r.eat("by") {
            if !is_count || !keys.is_empty() || distinct.is_some() {
                return Err(error("Grouping is allowed once for event counts."));
            }
            keys = r.names()?;
        } else if r.eat("select") {
            if is_count || !selected.is_empty() {
                return Err(error("Select applies once to event output."));
            }
            selected = r.names()?;
        } else if r.eat("limit") {
            if had_limit || top.is_some() || distinct.is_some() || (is_count && keys.is_empty()) {
                return Err(error("Limit applies once to events or grouped results."));
            }
            had_limit = true;
            limit = r.number()?;
        } else {
            return Err(unsupported(
                "Unrecognized or unconsumed text. No query was generated; use a structured plan for this request.",
            ));
        }
    }
    let mut steps = Vec::new();
    if let Some((field, accuracy)) = distinct {
        steps.push(Stage::Count {
            field: Some(field),
            distinct: true,
            accuracy,
            as_: count_alias(),
        });
    } else if !keys.is_empty() {
        steps.push(Stage::Group {
            keys,
            aggregates: vec![Aggregate::Count {
                field: None,
                distinct: false,
                accuracy: Accuracy::Exact,
                as_: count_alias(),
            }],
        });
        if top.is_some() || had_limit {
            steps.push(Stage::Sort {
                fields: vec![count_alias()],
                order: Order::Desc,
                type_: SortType::Number,
                limit: top.unwrap_or(limit),
            });
        }
    } else if is_count {
        steps.push(Stage::Count {
            field: None,
            distinct: false,
            accuracy: Accuracy::Exact,
            as_: count_alias(),
        });
    } else {
        if !selected.is_empty() {
            steps.push(Stage::Select { fields: selected });
        }
        steps.push(if latest {
            Stage::Tail { limit }
        } else {
            Stage::Head { limit }
        });
    }
    Ok(Plan {
        source,
        filters,
        steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_prefix_success_or_ignored_exceptions() {
        for s in [
            "show events except admins",
            "count events where x equals \"y\" without service accounts",
            "count events where a equals 1 and b equals 2 or c equals 3",
        ] {
            assert!(interpret(s, &Scope::default()).is_err(), "{s}");
        }
        assert!(
            interpret(
                "count events where (a equals 1 and b equals 2) or c equals 3",
                &Scope::default()
            )
            .is_ok()
        );
    }
    #[test]
    fn preserved_literals_and_explicit_missing() {
        let p=interpret("show events from \"example_repo\" where FileName equals \"A*B.exe\" select FileName limit 5",&Scope::default()).unwrap();
        assert!(matches!(&p.filters[0],Predicate::Eq{value:Scalar::Text(s),..} if s=="A*B.exe"));
        assert!(
            interpret(
                "count events where UserName is not \"admin\"",
                &Scope::default()
            )
            .is_err()
        );
    }
}
