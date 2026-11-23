use cqlmatic::{app, contracts::*};
fn check(query: &str) -> Analysis {
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context::default(),
        },
        None,
    )
}
fn rows(accepted: &[&str], rejected: &[&str]) {
    for q in accepted {
        let r = check(q);
        assert_eq!(r.validation.status, Acceptance::Accepted, "{q}: {r:?}");
    }
    for q in rejected {
        let r = check(q);
        assert_eq!(r.validation.status, Acceptance::Rejected, "{q}: {r:?}");
    }
}
#[test]
fn empty_stages() {
    rows(
        &[
            "x=1 |",
            "x=1 | count() |",
            "x=1 | count() | ",
            "| x=1 | count()",
        ],
        &[
            "x=1 | | count()",
            "x=1 | count() | |",
            "x=1 ||",
            "a=1 || b=2",
        ],
    );
}
#[test]
fn reserved_words() {
    rows(
        &[
            "a=1 or a=3",
            "a=1 OR a=3",
            "a=1 and b=2",
            "a=1 AND b=2",
            "NOT a=1",
            "not a=1",
            "a like 1",
            "x=1 | \"Or\"",
            "x=1 | \"and\"",
            "x=1 | \"true\"",
            "x=1 | where",
            "x=1 | by",
            "x=1 | is",
            "x=1 | null",
            "x=1 | as",
            "in=1",
            "order=asc",
            "Organization=x",
        ],
        &[
            "a=1 Or a=3",
            "a=1 oR a=3",
            "a=1 And b=2",
            "a=1 aNd b=2",
            "Not a=1",
            "a Like 1",
            "a LIKE 1",
            "x=1 | True",
            "x=1 | TRUE",
            "x=1 | False",
            "x=1 | in",
            "x=1 | match",
            "x=1 | case",
        ],
    );
}
#[test]
fn row_limits() {
    rows(
        &[
            "head(200000)",
            "tail(200000)",
            "sort(a, limit=200000)",
            "sort(a, limit=20001)",
            "table([a], limit=200000)",
            "groupBy(a, limit=1000000)",
            "top(a, limit=20000)",
            "head(limit=max)",
            "tail(limit=max)",
            "sort(a, limit=max)",
            "table([a], limit=max)",
            "groupBy(a, limit=max)",
        ],
        &[
            "head(0)",
            "head(-1)",
            "head(200001)",
            "tail(0)",
            "tail(200001)",
            "sort(a, limit=0)",
            "sort(a, limit=200001)",
            "sort(a, limit=10000000)",
            "table([a], limit=0)",
            "table([a], limit=200001)",
            "groupBy(a, limit=0)",
            "groupBy(a, limit=1000001)",
            "top(a, limit=0)",
            "top(a, limit=max)",
            "table([a], type=any)",
        ],
    );
}
#[test]
fn argument_conversion() {
    rows(
        &[
            r#"head(limit="2")"#,
            r#"head("10")"#,
            r#"tail("2")"#,
            r#"sort(a, limit="3")"#,
            r#"top(a, limit="2")"#,
            r#"groupBy(a, limit="5")"#,
            r#"bucket(span=1h, buckets="10")"#,
            r#"sort(a, limit="max")"#,
            r#"groupBy(a, limit="max")"#,
            "head(limit=1.0)",
            r#"replace(regex="1", with=7, field=a)"#,
            "replace(regex=1, with=7, field=a)",
            "lower(a, as=5)",
            "rename(a, as=1)",
            r#"in(a, values=["1"], ignoreCase="true")"#,
            r#"count(a, distinct="true")"#,
            r#"sort(a, reverse="true")"#,
            "in(a, values=[1], ignoreCase=True)",
            "in(a, values=[1], ignoreCase=TRUE)",
            "in(a, values=1)",
            r#"in(a, values="1")"#,
            "in(a, values=x)",
            "in(a, values=[true])",
            "in(a, values=[])",
            "sort(a, order=ascending)",
            "sort(a, order=descending)",
            "sort(a, order=Asc)",
            "sort(a, order=DESC)",
            r#"sort(a, order="asc")"#,
            r#"sort(a, type="number")"#,
        ],
        &[
            r#"head(limit="abc")"#,
            "head(1.5)",
            r#"top(a, limit="x")"#,
            r#"tail(limit="200001")"#,
            r#"sort(a, limit="0")"#,
            "in(a, values=[1], ignoreCase=1)",
            r#"in(a, values=[1], ignoreCase="yes")"#,
            "count(a, distinct=yes)",
            "in(a, values=[[1]])",
            "in(a, values={a=1})",
            "sort(a, order=bogus)",
            "sort(a, type=Number)",
        ],
    );
}
#[test]
fn regex_literal_flags() {
    rows(
        &[
            "a=/a1/g",
            "a=/a1/gm",
            "a=/x/gi",
            "a=/x/gmidF",
            "a=/x/ii",
            "a=/a1/i",
            "a=/a1/m",
            "a=/x/dF",
            "/x/gg",
        ],
        &["a=/a1/iz", "a=/x/s", "a=/x/u", "a=/x/x"],
    );
    assert_ne!(
        check(r#"regex("x", flags=g)"#).validation.status,
        Acceptance::Accepted
    );
}
#[test]
fn parameter_defaults() {
    rows(
        &[r#"a=?{p="1"}"#, "a=?{p=1}", r#"head(limit=?{p="2"})"#],
        &["head(limit=?{p=0})"],
    );
    assert_eq!(check("a=?p").validation.status, Acceptance::Unknown);
}
#[test]
fn quotation_marks() {
    rows(
        &[
            r#"a="1""#,
            r#""a"=1"#,
            r#"a="it's""#,
            r#"a="say “hi”""#,
            "a=/it's/",
            "// don't",
            "a=O’Brien",
        ],
        &[
            "a=“1”",
            "in(a, values=[“1”])",
            "a=‘1’",
            "a='1'",
            "in(a, values=['1'])",
            "`a`=1",
            "a=`1`",
        ],
    );
    assert_eq!(check("a=it's").validation.status, Acceptance::Unknown);
}
#[test]
fn string_escapes() {
    rows(
        &[
            r#"p="C:\Windows\cmd.exe""#,
            r#"p="a\sb""#,
            r#"p="a\qb""#,
            r#"p="a\*b""#,
            r#"p="a\x41b""#,
            r#"p="a\0b""#,
            r#"p="x\$y""#,
            r#"p="x\/y""#,
            r#"p="C:\\Windows\\cmd.exe""#,
            r#"p="a\u0041b""#,
            r#"p="\U0001F600""#,
            r#"p="C:\temp\new.exe""#,
        ],
        &[r#"p="\u{1F600}""#, "p=\"x\\"],
    );
    for q in [r#"p="C:\temp\new.exe""#, r#"p="a\tb""#] {
        assert!(
            check(q)
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_STRING_ESCAPE_CONTROL")
        );
    }
    for q in [r#"p="a\t b""#, r#"p="a\n""#] {
        assert!(
            !check(q)
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_STRING_ESCAPE_CONTROL")
        );
    }
}
#[test]
fn transform_negation() {
    let accepted = [
        "!lower(a, as=l)",
        "not upper(a, as=u)",
        "!parseJson(field=b)",
        "!select([a])",
        "!drop([b])",
        "!rename(a, as=c)",
    ];
    rows(
        &accepted,
        &[
            "!count()",
            "!groupBy(a)",
            "!sort(a)",
            "!head(1)",
            "!table([a])",
            r#"!createEvents(["a=1"])"#,
            "!eval(x=1)",
        ],
    );
    for q in accepted {
        assert!(
            check(q)
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_NEGATION_NO_EFFECT")
        );
    }
    rows(
        &[
            "!in(a, values=[1])",
            r#"!regex("x", field=b)"#,
            r#"!cidr(ip, subnet="10.0.0.0/8")"#,
            "!a=1",
            "not not a=1",
            "select([a]) | !lower(a, as=l) | l=x",
        ],
        &["!lower(a, bogus=1)"],
    );
}
#[test]
fn tail_timestamp() {
    for q in [
        "groupBy(n) | tail(2)",
        "table([n]) | tail(2)",
        "count() | tail(1)",
        "top(a) | tail(1)",
        "select([n]) | tail(2)",
    ] {
        let r = check(q);
        assert_eq!(r.validation.status, Acceptance::Accepted, "{q}: {r:?}");
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == "CQL_TAIL_WITHOUT_TIMESTAMP"),
            "{q}"
        );
    }
    for q in [
        "tail(2)",
        "sort(n) | tail(2)",
        "head(10) | tail(2)",
        "table([@timestamp, n]) | tail(2)",
        "groupBy(n) | head(2)",
        "count() | @timestamp := 1 | tail(2)",
        "count() | parseJson() | tail(2)",
    ] {
        let r = check(q);
        assert_eq!(r.validation.status, Acceptance::Accepted, "{q}: {r:?}");
        assert!(
            !r.diagnostics
                .iter()
                .any(|d| d.code == "CQL_TAIL_WITHOUT_TIMESTAMP"),
            "{q}"
        );
    }
}
#[test]
fn output_array_names() {
    rows(
        &[
            r#"text:editDistanceAsArray(target=b, references=["a","b"], maxDistance=2, asArray="d[]")"#,
        ],
        &[
            r#"text:editDistanceAsArray(target=b, references=["a","b"], maxDistance=2, asArray="d")"#,
        ],
    );
}

#[test]
fn canonical_strings_and_empty_stages() {
    use cqlmatic::language::parser;
    for (query, expected) in [
        ("| x=1 | count() |", "x = \"1\" | count()"),
        (r#"p="a\tb""#, r#"p = "a\tb""#),
        (r#"p="a\u0041b""#, r#"p = "aAb""#),
    ] {
        assert_eq!(parser::parse(query).unwrap().render(), expected);
    }
    assert!(
        check("| x=1 |")
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_EMPTY_STAGE" && d.severity == Severity::Warning)
    );
    rows(
        &[],
        &[
            r#"count(distinct="true")"#,
            r#"bucket(span=1h, buckets="1501")"#,
        ],
    );
}

#[test]
fn syntax_boundaries_preserve_text_and_comments() {
    rows(&["a=O_’Brien", "a=O-’Brien"], &["not", "NOT"]);
    for q in ["x=1 | // comment", "case { | x=1 | ; * }"] {
        let r = check(q);
        assert_eq!(r.validation.status, Acceptance::Accepted, "{q}: {r:?}");
        assert!(
            r.diagnostics.iter().any(|d| d.code == "CQL_EMPTY_STAGE"),
            "{q}"
        );
    }
}

#[test]
fn advisory_filters_preserve_acceptance() {
    let cases = [
        ("x=1 | count", "CQL_BARE_FUNCTION_NAME"),
        ("x=1 | head 10", "CQL_BARE_FUNCTION_NAME"),
        ("x=1 | sort a", "CQL_BARE_FUNCTION_NAME"),
        ("x=1 | table a", "CQL_BARE_FUNCTION_NAME"),
        ("x=1 | groupBy a", "CQL_BARE_FUNCTION_NAME"),
        ("x=1 | stats count by a", "CQL_BARE_FUNCTION_NAME"),
        ("x=1 | Count", "CQL_BARE_FUNCTION_NAME"),
        ("where a=1", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | search b=2", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | dedup c", "CQL_BARE_COMMAND_WORD"),
        ("fields a", "CQL_BARE_COMMAND_WORD"),
        ("rex a", "CQL_BARE_COMMAND_WORD"),
        ("spath a", "CQL_BARE_COMMAND_WORD"),
        ("lookup a", "CQL_BARE_COMMAND_WORD"),
        ("TSTATS a", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | limit 20", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | LIMIT 200", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | take 50", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | order by a desc", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | project a", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | project-away a", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | project-rename a=b", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | summarize by a", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | extend y=1", "CQL_BARE_COMMAND_WORD"),
        ("x=1 | distinct a", "CQL_BARE_COMMAND_WORD"),
        ("a=1 && b=2", "CQL_OPERATOR_TEXT"),
        ("a=1 or b=2 c=3", "CQL_OR_PRECEDENCE"),
        ("a=1 or b=2 and c=3", "CQL_OR_PRECEDENCE"),
        ("c=3 a=1 or b=2", "CQL_OR_PRECEDENCE"),
        ("a=1 and not b=2 or c=3", "CQL_OR_PRECEDENCE"),
        ("a != null", "CQL_NULL_TEXT"),
        ("a = null", "CQL_NULL_TEXT"),
        ("a=NULL", "CQL_NULL_TEXT"),
        ("case { count; * }", "CQL_BARE_FUNCTION_NAME"),
        ("a match { * => count }", "CQL_BARE_FUNCTION_NAME"),
        ("join({ count }, field=a)", "CQL_BARE_FUNCTION_NAME"),
        ("case { a=1 && b=2; * }", "CQL_OPERATOR_TEXT"),
        ("a match { * => b=null }", "CQL_NULL_TEXT"),
        ("join({ a=1 or b=2 c=3 }, field=a)", "CQL_OR_PRECEDENCE"),
        ("case { (a=1 or b=2 c=3); * }", "CQL_OR_PRECEDENCE"),
    ];
    for (query, code) in cases {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{query}: {result:?}"
        );
        let diagnostics: Vec<_> = result
            .diagnostics
            .iter()
            .filter(|d| advisory_code(&d.code))
            .collect();
        assert_eq!(diagnostics.len(), 1, "{query}: {result:?}");
        assert_eq!(diagnostics[0].code, code, "{query}");
        assert_eq!(
            diagnostics[0].severity,
            if code == "CQL_OR_PRECEDENCE" {
                Severity::Info
            } else {
                Severity::Warning
            }
        );
        if code == "CQL_OR_PRECEDENCE" {
            assert!(diagnostics[0].message.contains("read as ("));
            assert!(diagnostics[0].message.contains("parentheses"));
        }
    }
}

fn advisory_code(code: &str) -> bool {
    matches!(
        code,
        "CQL_BARE_FUNCTION_NAME"
            | "CQL_BARE_COMMAND_WORD"
            | "CQL_OPERATOR_TEXT"
            | "CQL_OR_PRECEDENCE"
            | "CQL_NULL_TEXT"
    )
}

#[test]
fn bare_command_word_warnings_suggest_cql_alternatives() {
    let cases = [
        ("x=1 | limit 20", "head() or tail()"),
        ("x=1 | take 10", "head() or tail()"),
        ("x=1 | order by a desc", "sort()"),
        ("x=1 | project a", "select(), drop(), or rename()"),
        ("x=1 | project-away a", "select(), drop(), or rename()"),
        ("x=1 | project-rename a=b", "select(), drop(), or rename()"),
        ("x=1 | summarize count() by a", "groupBy()"),
        ("x=1 | extend y=1", "field := expression"),
        (
            "x=1 | distinct a",
            "groupBy() or count(field=..., distinct=true)",
        ),
    ];
    for (query, expected_hint) in cases {
        let result = check(query);
        let warning = result
            .diagnostics
            .iter()
            .find(|d| d.code == "CQL_BARE_COMMAND_WORD")
            .unwrap_or_else(|| panic!("missing CQL_BARE_COMMAND_WORD for {query}: {result:?}"));
        assert_eq!(warning.severity, Severity::Warning, "{query}");
        assert!(
            warning.message.contains(expected_hint),
            "{query}: expected {expected_hint:?} in {:?}",
            warning.message
        );
    }
}

#[test]
fn explicit_filter_intent_needs_no_advisory() {
    for query in [
        "x=1 | \"count\"",
        "x=1 | \"limit\"",
        "x=1 | \"take\"",
        "x=1 | limit=20",
        "x=1 | rate limit",
        "x=1 | count()",
        "x=1 | head(20)",
        "x=1 | error count",
        "count=5",
        "x=1 | head=3",
        "x=1 | timeout",
        "x=1 | failed login",
        "a=1 \"&&\" b=2",
        "a=1 and b=2",
        "a=1 b=2",
        "(a=1 or b=2) c=3",
        "a=1 or b=2",
        "a=1 or (b=2 c=3)",
        "a=\"null\"",
        "a=*",
        "not a=*",
        "a=nullable",
        "x=1 | \"where\" a=1",
        "a=?{p=\"null\"}",
        "a=1 /* && null count */",
        "a=/null/",
        "case { \"count\"; * }",
        "a match { * => \"count\" }",
        "join({ \"count\" }, field=a)",
        "a=1 and not (b=2 or c=3)",
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{query}: {result:?}"
        );
        assert!(
            !result.diagnostics.iter().any(|d| advisory_code(&d.code)),
            "{query}: {result:?}"
        );
    }
    for query in [
        "x=1 | count() | count",
        "count() | case { count; * }",
        "count() | a match { * => count }",
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Rejected,
            "{query}: {result:?}"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_FREE_TEXT_AFTER_AGGREGATE")
        );
        assert!(
            !result.diagnostics.iter().any(|d| advisory_code(&d.code)),
            "{query}: {result:?}"
        );
    }
}

#[test]
fn parameter_default_depth_is_bounded() {
    let q = format!("a={}1{}", "?{p=".repeat(2048), "}".repeat(2048));
    let r = check(&q);
    assert!(
        r.diagnostics
            .iter()
            .any(|d| d.message.contains("nesting or node budget")),
        "{r:?}"
    );
    rows(&["a=?{p=?{q=1}}"], &[]);
}

#[test]
fn quote_boundaries_follow_expression_context() {
    rows(
        &[
            "x := a / 2 | message=\"/`literal`\"",
            "x := a / 2 | y := b / 4 | m=\"it's\"",
            "a=/x/ | b=\"“quoted”\"",
            "// don't\n a=1",
        ],
        &[],
    );
    for q in ["x := a / 2 | m='1'", "a=/it's/ | b='1'", "x := \"a\" + 'b'"] {
        assert!(
            check(q).diagnostics.iter().any(|d| d.code == "CQL_QUOTE"),
            "{q}"
        );
    }
}

#[test]
fn empty_string_negation_warns_without_presence_check() {
    for query in [
        "a != \"\"",
        "not a = \"\"",
        "!a = \"\"",
        "b = 1 | a != \"\"",
        "a = \"x\" or a != \"\"",
        "case { a != \"\" | c := 1; * }",
        "x := b | x != \"\"",
        "wildcard(\"bad*\", field=[message, backup]) | message != \"\"",
        "wildcard(\"*\", field=message, includeEverythingOnAsterisk=true) | message != \"\"",
        "cidr(ip, subnet=\"10.0.0.0/8\", negate=true) | ip != \"\"",
        "eval(z=a, a=1) | z != \"\"",
        "user=* | appendAggregation(count()) | user != \"\"",
        "status match { * => status != \"\" }",
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{query}: {result:?}"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_EMPTY_STRING_NEGATION" && d.severity == Severity::Warning),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for quiet in [
        "a = * a != \"\"",
        "a != \"\" a = *",
        "a = \"*\" | a != \"\"",
        "a = * | b = 1 | a != \"\"",
        "a = \"foo*\" | a != \"\"",
        "test(a != \"\")",
        "a != \"SYSTEM\"",
        "default(field=a, value=\"\") | a != \"\"",
        "a := \"seed\" | a != \"\"",
        "in(a, values=[\"x\", \"\"]) | a != \"\"",
        "not (a != \"\")",
        "groupBy([a], function=count()) | a != \"\"",
        "eval(z=1, a=z) | a != \"\"",
        "a=* | eval(a=missing) | a != \"\"",
        "count(as=n) | n != \"\"",
        "n := count() | n != \"\"",
        "status match { ok => status != \"\"; * => false }",
    ] {
        let result = check(quiet);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{quiet}: {result:?}"
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_EMPTY_STRING_NEGATION"),
            "{quiet}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn or_with_bare_text_warns_before_aggregate() {
    for query in [
        "a = \"1\" or \"2\"",
        "a = 1 or 2",
        "\"2\" or a = \"1\"",
        "a = \"1\" or \"2\" or \"3\"",
        "a = 1 or (\"2\" or \"3\")",
        "case { a = \"1\" or \"2\" | c := 1; * }",
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{query}: {result:?}"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_OR_FREE_TEXT" && d.severity == Severity::Warning),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for quiet in [
        "a = \"1\" or a = \"2\"",
        "\"1\" or \"2\"",
        "a = \"1\" or *",
        "a = \"1\" or /foo/",
        "in(a, values=[\"1\", \"2\"])",
    ] {
        let result = check(quiet);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{quiet}: {result:?}"
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_OR_FREE_TEXT"),
            "{quiet}: {:?}",
            result.diagnostics
        );
    }
    let post = check("count() | a = \"1\" or \"2\"");
    assert_eq!(post.validation.status, Acceptance::Rejected);
    assert!(
        post.diagnostics
            .iter()
            .any(|d| d.code == "CQL_FREE_TEXT_AFTER_AGGREGATE")
    );
    assert!(
        !post
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_OR_FREE_TEXT")
    );
}

#[test]
fn regex_windows_path_single_backslash_warns() {
    for query in [
        r#"p=/C:\Windows\System32/"#,
        r#"p=/^C:\Windows\System32/i"#,
        r#"p=/\bC:\Windows/"#,
        r#"p=/[A-Z]:\Windows\System32/"#,
        r#"p=/C:\scripts\run\.bat/"#,
        r#"p=/C:\\Windows\System32/"#,
        r#"p=/\\System32\net\.exe/"#,
        r#"p=/C:\temp\new\.exe/"#,
        r#"regex(field=p, regex="C:\\Windows\\System32")"#,
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{query}: {result:?}"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_REGEX_PATH_ESCAPE" && d.severity == Severity::Warning),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for quiet in [
        r#"p=/C:\\Windows\\System32/"#,
        r#"p=/C:\\Windows\\System32\\net\.exe/"#,
        r#"regex(field=p, regex="C:\\\\Windows\\\\System32")"#,
        r#"p=/\w+/"#,
        r#"p=/\bWindows\b/"#,
        r#"p=/\d+\.\d+/"#,
        r#"p=/http:\sfoo/"#,
        r#"p=/^Q:\sWho$/"#,
        r#"p=/error:\s+timeout/"#,
        r#"p=/[A-Za-z]:\\Windows/"#,
    ] {
        let result = check(quiet);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{quiet}: {result:?}"
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_REGEX_PATH_ESCAPE"),
            "{quiet}: {:?}",
            result.diagnostics
        );
    }
}

#[test]
fn unquoted_open_paren_in_filter_value_warns() {
    for query in [
        "a=x(y)",
        r#"p=C:\Program(x86)\app.exe"#,
        "a=foo(bar) | count()",
        "a=?{v=x}suffix(y)",
    ] {
        let result = check(query);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{query}: {result:?}"
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_FILTER_PAREN_SPLIT" && d.severity == Severity::Warning),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    for quiet in [
        "a=\"x(y)\"",
        r#"p="C:\Program(x86)\app.exe""#,
        "a=/x\\(y\\)/",
        "a=1 (b=2 or c=3)",
        "(a=1)",
        "a=1 | count()",
    ] {
        let result = check(quiet);
        assert_eq!(
            result.validation.status,
            Acceptance::Accepted,
            "{quiet}: {result:?}"
        );
        assert!(
            !result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_FILTER_PAREN_SPLIT"),
            "{quiet}: {:?}",
            result.diagnostics
        );
    }
}
