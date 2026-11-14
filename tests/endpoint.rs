//! Public contract and context regression checks.
use cqlmatic::{
    app, catalog,
    contracts::*,
    environment::Pack,
    language::{ast, parser, validate},
};
use serde_json::{Value, json};
use std::collections::BTreeSet;

fn generate(plan: Value) -> Analysis {
    app::generate(
        serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":plan}}))
            .unwrap(),
        None,
    )
}
fn check(query: &str, strict: bool) -> Analysis {
    let pack: Pack =
        serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context {
                require_observed: strict,
                ..Context::default()
            },
        },
        Some(&pack),
    )
}

#[test]
fn typed_conditions_preserve_native_expression_context() {
    let r = generate(json!({"source":{},"steps":[
        {"op":"assign","field":"high","expression":{"kind":"compare","operator":"ge","left":{"kind":"field","name":"Severity"},"right":{"kind":"literal","value":70}}},
        {"op":"assign","field":"low","expression":{"kind":"unary","operator":"not","value":{"kind":"field","name":"high"}}},
        {"op":"assign","field":"label","expression":{"kind":"call","function":"if","args":[{"value":{"kind":"call","function":"regex","args":[{"value":{"kind":"literal","value":"^4"}},{"name":"field","value":{"kind":"literal","value":"status"}}]}},{"name":"then","value":{"kind":"literal","value":"client"}},{"name":"else","value":{"kind":"literal","value":"other"}}]}}
    ]}));
    assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
    let query = r.query.unwrap();
    assert!(query.contains("Severity >= 70"));
    assert!(query.contains("!(high)"));
    assert!(query.contains("if(regex("));
    for q in ["x := a AND b", "x := a OR b", "x := a && b"] {
        assert_ne!(check(q, false).outcome, Outcome::Ready);
    }
    let positive = parser::parse("x := +(user)").unwrap();
    assert_eq!(positive.render(), "x := +(user)");
}

#[test]
fn ordered_branches_are_explicit_and_no_catchall_is_added() {
    let r = generate(json!({"source":{},"steps":[{"op":"case","branches":[
        {"source":{},"filters":[{"op":"glob","field":"FileName","pattern":"power*"}],"steps":[{"op":"assign","field":"class","expression":{"kind":"literal","value":"shell"}}]},
        {"source":{},"filters":[{"op":"eq","field":"FileName","value":"powershell.exe"}],"steps":[{"op":"assign","field":"class","expression":{"kind":"literal","value":"exact"}}]}
    ]}]}));
    assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
    let q = r.query.unwrap();
    assert!(q.find("shell").unwrap() < q.find("exact").unwrap());
    assert!(!q.contains("other"));
    assert_eq!(
        generate(json!({"source":{},"steps":[{"op":"case","branches":[]}]})).outcome,
        Outcome::Invalid
    );
}

#[test]
fn field_match_regex_data_cannot_add_syntax_and_modes_remain_distinct() {
    let plan = |pattern: Value| json!({"source":{},"steps":[{"op":"match","field":"name","arms":[{"pattern":pattern,"plan":{"source":{},"steps":[{"op":"assign","field":"x","expression":{"kind":"literal","value":1}}]}}]}]});
    let r = generate(plan(json!({"kind":"regex","pattern":"a/|count"})));
    assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
    let q = r.query.unwrap();
    assert_eq!(parser::parse(&q).unwrap().steps.len(), 1);
    assert!(q.contains("a\\/|count"));
    let literal = generate(plan(json!({"kind":"literal","value":"foo*"})))
        .query
        .unwrap();
    assert!(literal.contains("\\Afoo\\*\\z"));
    for pattern in [
        json!({"kind":"regex","pattern":"a","flags":"i | count()"}),
        json!({"kind":"regex","pattern":"a\\"}),
        json!({"kind":"regex","pattern":"a\nb"}),
    ] {
        let r = generate(plan(pattern));
        assert_ne!(r.outcome, Outcome::Ready);
        assert!(r.query.is_none());
    }
    for count in 0..6 {
        let original = format!("{}/", "\\".repeat(count));
        let escaped = ast::escape_regex_delimiter(&original);
        assert_eq!(
            escaped.chars().filter(|ch| *ch == '\\').count(),
            count + usize::from(count % 2 == 0)
        );
    }
    let like =
        generate(json!({"source":{},"filters":[{"op":"like","field":"name","pattern":"shell"}]}))
            .query
            .unwrap();
    assert!(like.contains(" like "));
}

#[test]
fn function_predicates_cannot_turn_into_aggregates_or_retaining_transforms() {
    for (name, args) in [
        ("count", json!([])),
        (
            "regex",
            json!([{"value":{"kind":"literal","value":"x"}},{"name":"strict","value":{"kind":"literal","value":false}}]),
        ),
    ] {
        let r =
            generate(json!({"source":{},"filters":[{"op":"call","function":name,"args":args}]}));
        assert_ne!(r.outcome, Outcome::Ready);
        assert!(r.query.is_none());
    }
    let r = generate(
        json!({"source":{},"filters":[{"op":"call","function":"cidr","negate":true,"args":[{"value":{"kind":"literal","value":"ip"}},{"name":"subnet","value":{"kind":"array","items":[{"kind":"literal","value":"10.0.0.0/8"}]}}]}]}),
    );
    assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
    let q = r.query.unwrap();
    assert!(q.starts_with("!cidr("));
    assert!(!q.contains(" = \"*\""));
}

#[test]
fn endpoint_signatures_accept_supported_forms_and_reject_invalid_arguments() {
    for q in [
        "base64Decode(user,charset=\"UTF-16LE\",as=decoded)",
        "cidr(user,subnet=\"::/0\")",
        "urlDecode(user)",
        "shannonEntropy(user)",
        "splitString(user,by=\" \",index=-1)",
        "coalesce([getField(\"host-name\"),user,\"none\"])",
        "replace(\"x\",field=user)",
        "replace(\"x\",replacement=\"\",field=user)",
        "kvParse(excludeEmpty=true,onDuplicate=keepFirst,separatorPadding=yes)",
        "kvParse(prefix=parsed)",
        "wildcard(\"a*\",field=[user,other],includeEverythingOnAsterisk=true)",
        "default(field=[user,other],value=0)",
        "format(\"%s\",field=user)",
        "rename([[user,principal],[host,endpoint]])",
        "lower(user,type=en_US)",
    ] {
        assert_eq!(
            check(q, false).outcome,
            Outcome::Ready,
            "{q}: {:?}",
            check(q, false).diagnostics
        );
    }
    for q in [
        "base64Decode()",
        "base64Decode(user,charset=wat)",
        "coalesce(expressions=[{count()}])",
        "replace(\"x\",with=\"a\",replacement=\"b\")",
        "kvParse(as=a,prefix=b)",
        "kvParse(separator=\"=>\")",
        "kvParse(onDuplicate=merge)",
        "format(\"%s\")",
        "format(\"%s\",field=user,locale=en)",
        "rename(user)",
        "rename([[user,\"#type\"]])",
        "default(field=user,value=count())",
        "x:=splitString(user,by=\" \")",
        "x:=regex(\"x\",strict=false)",
    ] {
        assert_ne!(check(q, false).outcome, Outcome::Ready, "{q}");
    }
}

#[test]
fn concat_rejects_separator_in_checked_and_generated_calls() {
    for query in [
        r#"concat(field=[left,right],separator=":",as=joined)"#,
        r#"joined := concat([left,right],separator=":")"#,
    ] {
        let result = check(query, false);
        assert_eq!(result.outcome, Outcome::Invalid, "{query}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_ARGUMENT_UNKNOWN")
        );
    }
    let result = generate(
        json!({"source":{},"steps":[{"op":"call","function":"concat","args":[
            {"name":"field","value":{"kind":"array","items":[{"kind":"literal","value":"left"},{"kind":"literal","value":"right"}]}},
            {"name":"separator","value":{"kind":"literal","value":":"}},
            {"name":"as","value":{"kind":"literal","value":"joined"}}
        ]}]}),
    );
    assert_eq!(result.outcome, Outcome::Invalid);
    assert!(result.query.is_none());
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "CQL_ARGUMENT_UNKNOWN")
    );
}

#[test]
fn concat_rejects_non_field_values_and_expressions() {
    for query in [
        "concat([user,1])",
        "concat([user,true])",
        "concat([user,user+1])",
        "concat([user,lower(user)])",
        "concat([user,[user]])",
    ] {
        let result = check(query, false);
        assert_ne!(result.outcome, Outcome::Ready, "{query}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_ARGUMENT_TYPE")
        );
    }
    for value in [
        json!({"kind":"literal","value":1}),
        json!({"kind":"literal","value":true}),
        json!({"kind":"binary","operator":"add","left":{"kind":"field","name":"user"},"right":{"kind":"literal","value":1}}),
    ] {
        let result = generate(
            json!({"source":{},"steps":[{"op":"call","function":"concat","args":[
                {"name":"field","value":{"kind":"array","items":[{"kind":"literal","value":"user"},value]}}
            ]}]}),
        );
        assert_ne!(result.outcome, Outcome::Ready);
        assert!(result.query.is_none());
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "CQL_ARGUMENT_TYPE")
        );
    }
}

#[test]
fn concat_scalar_and_list_arguments_read_named_fields() {
    for call in [
        "concat(user,as=joined)",
        r#"concat("user",as="joined")"#,
        "concat([user],as=joined)",
        r#"concat(field=["user",user],as=joined)"#,
        "joined := concat(user)",
        r#"joined := concat(["user"])"#,
    ] {
        let query = format!("#repo=example | {call}");
        let result = check(&query, true);
        assert_eq!(
            result.outcome,
            Outcome::Ready,
            "{query}: {:?}",
            result.diagnostics
        );
        assert_eq!(
            result.grounding.basis["example:*:user"],
            "observed in a bounded sample"
        );
        assert!(!result.grounding.basis.contains_key("example:*:joined"));
    }
    let result = check(
        r#"#repo=example | concat(["user","unmeasured"],as=joined)"#,
        true,
    );
    assert_eq!(result.outcome, Outcome::NeedsContext);
    assert_eq!(
        result.grounding.basis["example:*:user"],
        "observed in a bounded sample"
    );
    assert!(
        result
            .grounding
            .missing
            .iter()
            .any(|missing| missing.contains("unmeasured"))
    );
    for fields in [
        json!({"kind":"literal","value":"user"}),
        json!({"kind":"array","items":[{"kind":"literal","value":"user"},{"kind":"field","name":"user"}]}),
    ] {
        let result = generate(json!({"source":{},"steps":[
            {"op":"assign","field":"user","expression":{"kind":"literal","value":"alice"}},
            {"op":"call","function":"concat","args":[{"name":"field","value":fields},{"name":"as","value":{"kind":"literal","value":"joined"}}]}
        ]}));
        assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
        assert_eq!(result.grounding.basis["*:*:user"], "query-produced");
        assert!(!result.grounding.basis.contains_key("*:*:alice"));
    }
}

#[test]
fn capture_analysis_tracks_nonempty_guarantees_on_every_successful_path() {
    for (pattern, expected) in [
        ("(?<x>a)", vec!["x"]),
        ("(?<x>a)?", vec![]),
        ("(?<x>a)|b", vec![]),
        ("(?<x>a)|", vec![]),
        ("(?<x>a){1,2}", vec!["x"]),
        ("(?<x>a){0,2}", vec![]),
        ("(?<x>a*)", vec![]),
        ("(?<outer>(?<inner>a)|b)", vec!["outer"]),
        ("(?<outer>(?<inner>a))", vec!["inner", "outer"]),
    ] {
        let info = validate::regex_analysis(pattern).unwrap();
        assert_eq!(
            info.guaranteed.into_iter().collect::<Vec<_>>(),
            expected,
            "{pattern}"
        );
        assert!(!info.captures.is_empty());
    }
    for pattern in [
        r"(?i)\s-(?:enc|encodedcommand)\s+(?<b64>[A-Za-z0-9+/=]+)",
        r"(?im-s:abc)\b\d+",
        r"\w+\s+\D",
    ] {
        assert!(validate::regex_syntax(pattern).is_ok(), "{pattern}");
    }
    for pattern in ["(?i-)", "[z-a]", "a\\", "(?<x>a)(?<x>b)"] {
        assert!(validate::regex_analysis(pattern).is_err(), "{pattern}");
    }
    assert!(validate::regex_analysis("(?=a)").unwrap_err().0);
}

#[test]
fn capture_grounding_preserves_incoming_uncertainty_and_known_query_values() {
    for q in [
        "#repo=example | regex(\"(?<x>a)\",field=user) | x=*",
        "#repo=example | x:=1 | regex(\"(?<x>a)?\",field=user,strict=false) | x=*",
        "#repo=example | select([user]) | regex(\"(?<x>a)?\",field=user,strict=false) | x=*",
        "#repo=example | user=/(?<x>a)/ | x=*",
    ] {
        assert_eq!(
            check(q, true).outcome,
            Outcome::Ready,
            "{q}: {:?}",
            check(q, true).grounding
        );
    }
    for q in [
        "#repo=example | regex(\"(?<x>a)?\",field=user) | x=*",
        "#repo=example | regex(\"(?<x>a)\",field=user,strict=false) | x=*",
        "#repo=example | regex(\"(?<x>)\",field=user) | x=*",
    ] {
        assert_eq!(check(q, true).outcome, Outcome::NeedsContext, "{q}");
    }
}

#[test]
fn accessors_outputs_and_rename_pairs_do_not_invent_observations() {
    for q in [
        "#repo=example | drop([x]) | x:=getField(\"user\") | x=*",
        "#repo=example | drop([_lower]) | lower(user) | _lower=*",
        "#repo=example | drop([_coalesce]) | coalesce([user,\"none\"]) | _coalesce=*",
        "#repo=example | drop([principal]) | rename([[user,principal]]) | principal=*",
    ] {
        assert_eq!(
            check(q, true).outcome,
            Outcome::Ready,
            "{q}: {:?}",
            check(q, true).grounding
        );
    }
    for q in [
        "#repo=example | getField(\"unmeasured\")",
        "#repo=example | name:=\"user\" | getField(name)",
        "#repo=example | kvParse(as=p) | p=*",
        "#repo=example | rename([[unmeasured,principal]])",
    ] {
        assert_eq!(
            check(q, true).outcome,
            Outcome::NeedsContext,
            "{q}: {:?}",
            check(q, true).grounding
        );
    }
}

#[test]
fn bundled_catalog_has_unique_ids_and_complete_records() {
    let mut ids = BTreeSet::new();
    for record in catalog::records() {
        assert!(
            ids.insert(&record.id),
            "Duplicate catalog ID: {}",
            record.id
        );
        assert!(!record.title.trim().is_empty(), "{}", record.id);
        assert!(!record.body.trim().is_empty(), "{}", record.id);
        assert!(!record.reference.trim().is_empty(), "{}", record.id);
        assert!(!record.source_path.trim().is_empty(), "{}", record.id);
        assert!(
            record
                .sources
                .iter()
                .all(|source| !source.trim().is_empty()),
            "{}",
            record.id
        );
    }
}

#[test]
fn nullable_writes_keep_implicit_incoming_dependencies_without_rewriting_cql() {
    for query in [
        "#repo=example | x:=user | x=*",
        "#repo=example | base64Decode(user,as=x) | x=*",
        "#repo=example | lower(user,as=x) | x=*",
        "#repo=example | splitString(user,by=\" \",as=parts) | parts[0]=*",
    ] {
        assert_eq!(check(query, true).outcome, Outcome::NeedsContext, "{query}");
        let ordinary = check(query, false);
        assert_eq!(ordinary.outcome, Outcome::Ready, "{query}");
        assert!(
            ordinary
                .grounding
                .basis
                .values()
                .any(|basis| basis.contains("conditional"))
        );
    }
    for query in [
        "#repo=example | drop([x]) | x:=user | x=*",
        "#repo=example | x:=1 | x:=user | x=*",
        "#repo=example | select([user]) | splitString(user,by=\" \",as=parts) | parts[0]=*",
        "#repo=example | drop([unmeasured])",
    ] {
        assert_eq!(
            check(query, true).outcome,
            Outcome::Ready,
            "{query}: {:?}",
            check(query, true).grounding
        );
    }
    assert_eq!(
        check("#repo=example | drop([x]) | x:=user | y:=x | y=*", true).outcome,
        Outcome::NeedsContext
    );
    let r = generate(
        json!({"source":{},"steps":[{"op":"assign","field":"x","expression":{"kind":"field","name":"user"}}]}),
    );
    assert_eq!(r.outcome, Outcome::Ready);
    assert!(!r.query.unwrap().contains("drop("));
}

#[test]
fn default_inputs_and_named_outputs_keep_their_dependencies() {
    for query in [
        "readFile(\"external.csv\") | regex(\"x\")",
        "readFile(\"external.csv\") | replace(\"x\")",
        "readFile(\"external.csv\") | parseJson()",
        "readFile(\"external.csv\") | kvParse()",
        "readFile(\"external.csv\") | splitString(by=\" \")",
    ] {
        assert_eq!(check(query, true).outcome, Outcome::NeedsContext, "{query}");
        assert!(
            check(query, false)
                .grounding
                .basis
                .keys()
                .any(|s| s.contains("@rawstring"))
        );
    }
    let query = "#repo=example | drop([x]) | replace(\"a\",field=user,as=x) | x=*";
    assert_eq!(
        check(query, true).outcome,
        Outcome::Ready,
        "{:?}",
        check(query, true).grounding
    );
    assert_eq!(
        check("#repo=example | drop([x]) | round(user,as=x) | x=*", true).outcome,
        Outcome::Ready
    );
}

#[test]
fn match_patterns_preserve_source_context() {
    for query in [
        r#"user match { * => x:=1 }"#,
        r#"user match { "*" => x:=1 }"#,
    ] {
        assert_eq!(
            parser::parse(query).unwrap().render(),
            query.replace(":=", " := ")
        );
    }
    for query in [
        r#"#repo=example | user != /(?<x>a)/ | x=*"#,
        r#"#repo=example | !(user = /(?<x>a)/) | x=*"#,
        r#"#repo match { "other" => user=* }"#,
        r#"#repo=example | #repo match { "other" => user=* }"#,
        r#"#repo match { "exam*" => user=* }"#,
        r#"#repo match { /(?<source>other)/ => user=* }"#,
        r#"#repo=/(?<source>other)/ | user=*"#,
        r#"#repo=other user=/(?<x>a)/ | user=*"#,
        r#"#repo=example | x:=1 | lower(user,as=x) | y:=x | y=*"#,
        r#"#repo=example | x:=!(1e999) | x=*"#,
        r#"#repo=example | default(field=x,value="none") | x=*"#,
    ] {
        assert_eq!(
            check(query, true).outcome,
            Outcome::NeedsContext,
            "{query}: {:?}",
            check(query, true).grounding
        );
    }
    for query in [
        r#"#repo=example | user = /(?<x>a)/ | x=*"#,
        r#"#repo match { "example" => user=* }"#,
        r#"#repo=example | user match { /(?<x>a)/ => x=* }"#,
        r#"#repo=example | drop([x]) | default(field=x,value="none") | x=*"#,
        r#"#repo=example | select([user]) | default(field=x,value="none") | x=*"#,
        r#"#repo=example | x:=1 | rename(x,as=y) | z:=y | z=*"#,
    ] {
        assert_eq!(
            check(query, true).outcome,
            Outcome::Ready,
            "{query}: {:?}",
            check(query, true).grounding
        );
    }
    assert_eq!(
        check(r#"!regex("a",field=user,strict=false)"#, false).outcome,
        Outcome::Unsupported
    );
    let request: GenerateRequest =
        serde_json::from_str(include_str!("../examples/endpoint-classify.request.json")).unwrap();
    let GenerateInput::Plan { plan } = &request.input else {
        panic!("plan")
    };
    let Stage::Case { branches } = &plan.steps[0] else {
        panic!("case")
    };
    let Predicate::Regex { pattern, .. } = &branches[0].filters[0] else {
        panic!("regex")
    };
    assert_eq!(pattern, r"(?i)-enc(?:odedcommand)?\s+");
    assert_eq!(app::generate(request, None).outcome, Outcome::Ready);
}
