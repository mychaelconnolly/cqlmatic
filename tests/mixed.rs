use cqlmatic::{app, contracts::*, environment::Pack};
use serde_json::json;
fn pack() -> Pack {
    serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap()
}
fn check(query: &str, strict: bool) -> Analysis {
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
        Some(&pack()),
    )
}
#[test]
fn native_join_table_file_and_enrichment_contracts_are_public() {
    for q in [
        r#"join({user=*},field=[user,host],key=[name,machine],include=[role],limit=200000,max=2,view="other",live=false)"#,
        r#"!join({user=*},field=user,key=user)"#,
        r#"defineTable({user=*},name="t",include=*) | readFile(table="t",include=[user],limit=1)"#,
        r#"defineTable({user=*},name="t",include=[user,role]) | match(table="t",field=[user],column=[user],strict=false,nrows=max,mode=string)"#,
        r#"readFile(["one.csv","two.csv"],include=[user],limit=2)"#,
        r#"asn() | ipLocation(field=ip,as=geo) | ioc:lookup([domain,url],type=url,confidenceThreshold=low,include=[labels,type],prefix=hits)"#,
        r#"!ioc:lookup(domain,type=domain,strict=true)"#,
    ] {
        let r = check(q, false);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.diagnostics);
    }
    let request: GenerateRequest = serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"call","function":"join","args":[{"value":{"kind":"query","plan":{"source":{},"filters":[{"op":"exists","field":"user"}]}}},{"name":"field","value":{"kind":"array","items":[{"kind":"literal","value":"user"},{"kind":"literal","value":"host"}]}},{"name":"key","value":{"kind":"array","items":[{"kind":"literal","value":"name"},{"kind":"literal","value":"machine"}]}}]}]}}})).unwrap();
    let r = app::generate(request, None);
    assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
    assert!(r.query.unwrap().contains("field=[\"user\", \"host\"]"));
}
#[test]
fn alias_key_and_definition_errors_cannot_hide_in_nested_calls() {
    for q in [
        r#"join({user=*},field=[a,b],key=[c])"#,
        r#"join({user=*},field=[])"#,
        r#"join({user=*},field=a,repo="a",view="b")"#,
        r#"join({user=*},field=a,limit=max)"#,
        r#"join({user=*},field=a,limit=200001)"#,
        r#"match(file="a.csv",table="a",field=x)"#,
        r#"match(file="a.csv",field=[a,b],column=a)"#,
        r#"match(file="a.csv",field=[a],mode=glob)"#,
        r#"match(file="a.csv",field=a,nrows=501)"#,
        r#"!match(table="missing",field=x)"#,
        r#"readFile(table="missing")"#,
        r#"readFile(file="a",table="b")"#,
        r#"readFile("a.csv",limit=max)"#,
        r#"defineTable(name="t",query={true})"#,
        r#"user=* | defineTable(name="t",query={true},include=[user])"#,
        r#"case {* | defineTable(name="t",query={true},include=[user])}"#,
        r#"defineTable(name="t",query={join({true},field=user)},include=[user])"#,
        r#"defineTable(name="t",query={true},include=[user]) | selfJoinFilter(user,where=[{user=a},{user=b}])"#,
        r#"!ioc:lookup(domain,type=domain)"#,
        r#"ioc:lookup(domain,type=domain,include=[unknown])"#,
        r##"asn(field="#repo")"##,
    ] {
        assert_ne!(check(q, false).outcome, Outcome::Ready, "{q}");
    }
    let q = (0..11)
        .map(|i| format!("defineTable(name=\"t{i}\",query={{true}},include=[user])"))
        .collect::<Vec<_>>()
        .join(" | ");
    assert!(
        check(&q, false)
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_TABLE_LIMIT")
    );
}
#[test]
fn searchable_targets_cannot_borrow_matching_tag_observations() {
    let view = serde_json::to_string(&pack().environment.view).unwrap();
    let same = format!("#repo=example | join({{#repo=example | user=*}},field=user,view={view})");
    assert_eq!(check(&same, true).outcome, Outcome::Ready);
    let other = r#"#repo=example | join({#repo=example | user=*},field=user,view="other")"#;
    let r = check(other, true);
    assert_eq!(r.outcome, Outcome::NeedsContext);
    assert!(
        r.grounding
            .basis
            .iter()
            .any(|(k, v)| k.starts_with("view=other:") && v.contains("searchable target"))
    );
    let nested = r#"defineTable(name="t",query={#repo=example},include="*",view="other") | readFile(table="t") | join({#repo=example | user=*},field=user,include=[])"#;
    let r = check(nested, true);
    // Independently enforce table/join incompatibility, but inspect target lineage too.
    assert_ne!(r.outcome, Outcome::Ready);
    assert!(
        r.grounding
            .basis
            .iter()
            .any(|(k, v)| k == "example:*:user" && v.contains("observed"))
    );
    assert!(
        r.grounding
            .basis
            .keys()
            .any(|k| k.starts_with("view=other:"))
    );
}
#[test]
fn table_exports_default_columns_and_unknown_overwrites_remain_distinct() {
    for q in [
        r#"defineTable({#repo=example | user:=1 | role:=2 | select([user,role])},name="t",include="*") | #repo=example | drop([role]) | match(table="t",field=user) | role=*"#,
        r#"defineTable({#repo=example | select(user)},name="a",include=[user]) | defineTable({#repo=example | x:=1 | select(x)},name="b",include=[x]) | readFile(table=["a","b"],include=[user,x])"#,
        r#"#repo=example | x:=1 | join({#repo=example},field=user) | y:=x | y=*"#,
        r#"#repo=example | select(user) | ipLocation(user,as=geo) | geo.country=*"#,
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.grounding);
    }
    for q in [
        r#"#repo=example | x:=1 | match(file="unknown.csv",field=user) | y:=x | y=*"#,
        r#"#repo=example | asn()"#,
        r#"#repo=example | ipLocation(user,as=geo) | geo.country=*"#,
    ] {
        assert_eq!(check(q, true).outcome, Outcome::NeedsContext, "{q}");
    }
    let r = check(
        r#"#repo=example | x:=1 | match(file="unknown.csv",field=user) | select(user) | k:=user | k=*"#,
        true,
    );
    assert_eq!(r.outcome, Outcome::NeedsContext); // Resource and earlier reads remain unknown.
    assert_eq!(r.grounding.basis["example:*:k"], "query-produced");
}

#[test]
fn imports_keep_owned_values_and_negation_ignores_only_exports() {
    for q in [
        r#"defineTable({user:=1 | role:=2 | select([user,role])},name="t",include=*) | #repo=example | role:=0 | match(table="t",field=user,include=[role]) | role=*"#,
        r#"defineTable({user:=1 | role:=2 | select([user,role])},name="t",include=*) | #repo=example | role:=0 | match(table="t",field=user) | role=*"#,
        r#"defineTable({#repo=example | select(user)},name="t",include=[user]) | #repo=example | !match(table="t",field=user,include=[missing])"#,
        r#"defineTable({#repo=example},name="t",include="*") | #repo=example | !match(table="t",field=user)"#,
        r##"defineTable({#repo=example},name="t",include="*") | #repo=example | !match(table="t",field=user,include=["#repo"])"##,
    ] {
        let r = check(q, true);
        assert_eq!(
            r.outcome,
            Outcome::Ready,
            "{q}: {:?} {:?}",
            r.diagnostics,
            r.grounding
        );
    }
    for q in [
        r##"match(file="unknown.csv",field=user,include=["#repo"])"##,
        r#"objectArray:exists("items[]",var=x,condition={match(file="unknown.csv",field=x.k,include=[x.k])})"#,
        r##"defineTable({#repo=example},name="t",include=[user,"#repo"]) | match(table="t",field=user)"##,
    ] {
        assert_ne!(check(q, false).outcome, Outcome::Ready, "{q}");
    }
    let q = r#"defineTable({#repo=example | select(user)},name="t",include=[user]) | #repo=example | !match(table="t",field=unknown)"#;
    assert_eq!(check(q, true).outcome, Outcome::NeedsContext);
}
#[test]
fn ioc_columns_are_exact_and_constructed_prefixes_stay_bounded() {
    for q in [
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain,include=[indicator]) | ioc[0].indicator=*"#,
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain) | ioc[0].labels=*"#,
    ] {
        assert_eq!(
            check(q, true).outcome,
            Outcome::Ready,
            "{q}: {:?}",
            check(q, true).grounding
        );
    }
    for q in [
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain,include=[indicator]) | ioc[0].labels=*"#,
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain) | ioc[0].madeup=*"#,
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain) | ioc[0].indicator.nested=*"#,
    ] {
        assert_eq!(
            check(q, true).outcome,
            Outcome::NeedsContext,
            "{q}: {:?}",
            check(q, true).grounding
        );
    }
    let q = r#"#repo=example | select(user) | ioc:lookup(user,type=domain,prefix="spoof[]") | spoof[0][0].indicator=*"#;
    assert_eq!(check(q, true).outcome, Outcome::Unsupported);
}
#[test]
fn projection_and_all_source_column_requirements_agree() {
    for q in [
        r#"#repo=example | select(user) | ipLocation(user,as=geo) | geo.country=*"#,
        r#"#repo=example | select(user) | asn(user,as=net) | net.asn=*"#,
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain,strict=true) | ioc.detected=* | ioc[0].indicator=*"#,
        r#"defineTable({user:=1 | role:=2 | select([user,role])},name="t",include=[user,role]) | #repo=example | select(user) | match(table="t",field=user,include=[role]) | role=*"#,
        r#"defineTable({#repo=example | select(user)},name="t",include=[user]) | #repo=example | select(user) | !match(table="t",field=user,include=[missing])"#,
        r#"#repo=example | select(user) | !join({other:=1 | other=*},field=user,key=other)"#,
    ] {
        let r = check(q, false);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.diagnostics);
        assert!(
            !r.diagnostics.iter().any(|d| d.code == "CQL_DROPPED_FIELD"),
            "{q}: {:?}",
            r.diagnostics
        );
    }
    let q = r#"defineTable({#repo=example | select(user)},name="t",include=[user]) | readFile(table="t",include=[missing])"#;
    let r = check(q, true);
    assert_eq!(r.outcome, Outcome::NeedsContext);
    assert!(r.grounding.basis.keys().any(|k| k.ends_with(":missing")));
}
#[test]
fn native_max_sentinel_is_a_public_literal_without_integer_coercion() {
    for (function, args) in [
        (
            "match",
            json!([{"name":"file","value":{"kind":"literal","value":"lookup.csv"}},{"name":"field","value":{"kind":"literal","value":"user"}},{"name":"nrows","value":{"kind":"literal","value":"max"}}]),
        ),
        (
            "groupBy",
            json!([{"name":"field","value":{"kind":"literal","value":"user"}},{"name":"limit","value":{"kind":"literal","value":"max"}}]),
        ),
    ] {
        let r=app::generate(serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"call","function":function,"args":args}]}}})).unwrap(),None);
        assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
        assert!(r.query.unwrap().contains("=\"max\""));
    }
    assert_ne!(
        check(r#"readFile("lookup.csv",limit="max")"#, false).outcome,
        Outcome::Ready
    );
}

#[test]
fn table_exports_include_requested_physical_array_members_without_inventing_arity() {
    let q = r#"defineTable({#repo=example | select(user) | splitString(user,by=",",as=parts)},name="t",include=["parts[0]"]) | readFile(table="t") | parts[0]=*"#;
    assert_eq!(
        check(q, true).outcome,
        Outcome::Ready,
        "{:?}",
        check(q, true).grounding
    );
    let q = r#"defineTable({#repo=example | select(user) | splitString(user,by=",",as=parts)},name="t",include="*") | #repo=example | match(table="t",field=user)"#;
    let r = check(q, true);
    assert_eq!(r.outcome, Outcome::NeedsContext);
    assert!(
        r.grounding
            .basis
            .keys()
            .any(|k| k.ends_with("<lookup-output-columns>"))
    );
}

#[test]
fn known_all_column_exports_keep_possible_branch_destinations() {
    for q in [
        r##"defineTable({#repo=example | select([user,"#repo"])},name="t",include="*") | #repo=example | match(table="t",field=user)"##,
        r##"defineTable({#repo=example | case {user=a | select([user,"#repo"]); * | select(user)}},name="t",include="*") | #repo=example | match(table="t",field=user)"##,
        r#"defineTable({#repo=example | x.k:=1 | select([user,x.k])},name="t",include="*") | objectArray:exists("items[]",var=x,condition={match(table="t",field=x.k,column=user)})"#,
    ] {
        assert_ne!(
            check(q, false).outcome,
            Outcome::Ready,
            "{q}: {:?}",
            check(q, false).diagnostics
        );
    }
    let q = r#"defineTable({#repo=example | select(user) | splitString(user,by=",",as=parts)},name="t",include="*") | #repo=example | match(table="t",field=user)"#;
    assert_eq!(check(q, false).outcome, Outcome::Ready); // Indexed arity remains unknown, rather than a fake parts column.
}
#[test]
fn exact_ioc_members_compose_with_generated_containers_without_owning_other_columns() {
    for q in [
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain,include=[indicator]) | objectArray:exists("ioc[]",var=x,condition={x.indicator=*})"#,
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain,include=[indicator]) | objectArray:eval("ioc[]",var=x,asArray="values[]",function={values:=x.indicator}) | values[0]=*"#,
    ] {
        assert_eq!(
            check(q, true).outcome,
            Outcome::Ready,
            "{q}: {:?}",
            check(q, true).grounding
        );
    }
    for q in [
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain,include=[indicator]) | objectArray:exists("ioc[]",var=x,condition={x.labels=*})"#,
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain,include=[indicator]) | objectArray:exists("ioc[]",var=x,condition={x.indicator.nested=*})"#,
        r#"#repo=example | ioc:lookup(user,type=domain,include=[indicator]) | objectArray:exists("ioc[]",var=x,condition={x.indicator=*})"#,
        r#"#repo=example | select(user) | ioc:lookup(user,type=domain,include=[indicator]) | split(ioc,strip=true) | indicator=*"#,
    ] {
        assert_eq!(
            check(q, true).outcome,
            Outcome::NeedsContext,
            "{q}: {:?}",
            check(q, true).grounding
        );
    }
    let request:GenerateRequest=serde_json::from_value(json!({"schema_version":1,"context":{"require_observed":true},"input":{"mode":"plan","plan":{"source":{"repo":"example"},"steps":[{"op":"select","fields":["user"]},{"op":"call","function":"ioc:lookup","args":[{"value":{"kind":"literal","value":"user"}},{"name":"type","value":{"kind":"literal","value":"domain"}},{"name":"include","value":{"kind":"array","items":[{"kind":"literal","value":"indicator"}]}}]},{"op":"call","function":"objectArray:exists","args":[{"value":{"kind":"literal","value":"ioc[]"}},{"name":"var","value":{"kind":"literal","value":"x"}},{"name":"condition","value":{"kind":"query","plan":{"source":{},"filters":[{"op":"exists","field":"x.indicator"}]}}}]}]}}})).unwrap();
    let r = app::generate(request, Some(&pack()));
    assert_eq!(
        r.outcome,
        Outcome::Ready,
        "{:?} {:?}",
        r.diagnostics,
        r.grounding
    );
}

#[test]
fn default_scalar_outputs_preserve_bounded_table_export_schemas() {
    for operation in [
        "lower(user)",
        "round(user)",
        "array:length(\"v[]\")",
        "lower(user,as=_lower)",
    ] {
        let q = format!(
            "defineTable({{#repo=example | select([user,\"#repo\"]) | {operation}}},name=\"t\",include=\"*\") | #repo=example | match(table=\"t\",field=user)"
        );
        assert_eq!(
            check(&q, false).outcome,
            Outcome::Invalid,
            "{q}: {:?}",
            check(&q, false).diagnostics
        );
    }
    let q = r#"#repo=example | select(user) | lower(user) | _lower=*"#;
    assert!(
        !check(q, false)
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_DROPPED_FIELD")
    );
}

#[test]
fn bounded_writes_and_local_file_reads_preserve_known_export_destinations() {
    for operation in [
        "eval(k=1)",
        "default(user,value=\"fallback\")",
        "default([user,k],value=\"fallback\")",
    ] {
        let q = format!(
            "defineTable({{#repo=example | select([user,\"#repo\"]) | {operation}}},name=\"t\",include=\"*\") | #repo=example | match(table=\"t\",field=user)"
        );
        assert_eq!(
            check(&q, false).outcome,
            Outcome::Invalid,
            "{q}: {:?}",
            check(&q, false).diagnostics
        );
    }
    for source in [
        "readFile(table=\"a\")",
        "readFile(table=\"a\",include=[user,\"#repo\"])",
    ] {
        let q = format!(
            "defineTable({{#repo=example | select([user,\"#repo\"])}},name=\"a\",include=\"*\") | defineTable({{{source}}},name=\"b\",include=\"*\") | #repo=example | match(table=\"b\",field=user)"
        );
        assert_eq!(
            check(&q, false).outcome,
            Outcome::Invalid,
            "{q}: {:?}",
            check(&q, false).diagnostics
        );
    }
    assert_eq!(
        check(r##"default("#repo",value="changed")"##, false).outcome,
        Outcome::Invalid
    );
}

#[test]
fn known_default_lookup_imports_have_the_same_schema_as_explicit_imports() {
    for includes in ["", ",include=[role]"] {
        let q = format!(
            "defineTable({{#repo=example | role:=1 | select([user,role])}},name=\"a\",include=\"*\") | defineTable({{#repo=example | select([user,\"#repo\"]) | match(table=\"a\",field=user{includes})}},name=\"b\",include=\"*\") | #repo=example | match(table=\"b\",field=user)"
        );
        assert_eq!(
            check(&q, true).outcome,
            Outcome::Invalid,
            "{q}: {:?}",
            check(&q, true).diagnostics
        );
    }
    let q = r#"defineTable({user:=1 | role:=2 | select([user,role])},name="a",include="*") | #repo=example | select(user) | match(table="a",field=user) | role=*"#;
    assert!(
        !check(q, false)
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_DROPPED_FIELD")
    );
}
