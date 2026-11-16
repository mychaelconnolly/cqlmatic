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
