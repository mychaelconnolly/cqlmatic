use cqlmatic::{app, catalog, contracts::*, environment::Pack};
use serde_json::json;
fn pack() -> Pack {
    let old: Pack =
        serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    let mut data = old.data.clone();
    for (index, path) in ["items[]", "items[].k", "items[0].k", "items[0]", "user.k"]
        .into_iter()
        .enumerate()
    {
        let mut field = data.fields[0].clone();
        field["id"] = format!("env:field:example:array-{index}").into();
        field["path"] = path.into();
        if index == 0 {
            field["data_type"] = "array".into();
            field["is_array"] = true.into();
        }
        data.fields.push(field);
    }
    Pack::new(old.environment, "2030-01-01T00:00:00Z".into(), data).unwrap()
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
fn inline_scopes_keep_source_members_shadowing_and_temporaries_distinct() {
    for q in [
        r#"#repo=example | objectArray:exists("items[]",var=user,condition={user.k=*}) | user=*"#,
        r#"#repo=example | select(["items[0].k"]) | objectArray:eval("items[]",asArray="out[]",var=x,function={out:=x.k}) | out[0]=*"#,
        r#"#repo=example | select(["items[0]"]) | array:eval("items[]",asArray="out[]",var=x,function={temp:=1 | out:=x*x}) | out[0]=*"#,
        r#"#repo=example | array:filter("items[]",var=x,function={test(x>1)})"#,
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.grounding);
    }
    for q in [
        r#"#repo=example | objectArray:exists("items[]",var=user,condition={user.unknown=*})"#,
        r#"#repo=example | objectArray:exists("items[]",var=x,condition={x.k=*}) | x.k=*"#,
        r#"#repo=example | select(["items[0]"]) | array:eval("items[]",asArray="out[]",var=x,function={temp:=1 | out:=x*x}) | temp=*"#,
        r#"#repo=example | objectArray:exists("items[]",var=x,condition={#repo=other | x.k=*})"#,
        r#"#repo=example | objectArray:eval("items[]",asArray="out[]",var=x,function={out:=x.k}) | out[0]=*"#,
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::NeedsContext, "{q}: {:?}", r.grounding);
    }
    let r = check(
        r#"#repo=example | objectArray:exists("items[]",var=user,condition={user.k=*})"#,
        true,
    );
    assert!(r.grounding.basis.keys().any(|k| k.ends_with("items[].k")));
    assert!(!r.grounding.basis.keys().any(|k| k.ends_with(":user.k")));
}
#[test]
fn element_contracts_reject_wrong_effects_bindings_and_missing_outputs() {
    for q in [
        r#"array:eval("v[]",function={v:=v*v})"#,
        r#"array:filter("v[]",var=x,function=test(x>0))"#,
        r#"objectArray:eval("v[]",asArray="out[]",function={out:=true})"#,
        r#"!objectArray:exists("v[]",condition={v.k=*})"#,
    ] {
        assert_eq!(
            check(q, false).outcome,
            Outcome::Ready,
            "{q}: {:?}",
            check(q, false).diagnostics
        );
    }
    for q in [
        r#"array:eval("v",function={v:=1})"#,
        r#"array:eval("v[]",asArray="out[]",function="out:=1")"#,
        r#"array:eval("v[]",asArray="out[]",function={other:=1})"#,
        r#"array:eval("v[]",asArray="out[]",function={out:=1 | drop([out])})"#,
        r#"array:eval("v[]",asArray="out[]",function={case {v=* | out:=1; * | other:=1}})"#,
        r#"objectArray:eval("v[]",asArray="v[]",function={v:=1})"#,
        r#"objectArray:eval("v[]",asArray="out[]",var="x.k",function={out:=1})"#,
        r#"array:filter("v[]",function={count()})"#,
        r##"array:eval("v[]",asArray="#tag[]",function={out:=1})"##,
        r#"array:filter("v[][]",function={true})"#,
        r#"array:eval("v[]",asArray="out[]",function={case {v=* | count() | out:=1; * | out:=1}})"#,
        r#"objectArray:exists("v[]",condition={createEvents(["v=1"])})"#,
    ] {
        assert_ne!(check(q, false).outcome, Outcome::Ready, "{q}");
    }
}
#[test]
fn typed_inline_queries_and_record_aliases_are_public_and_bounded() {
    let request:GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"call","function":"array:eval","args":[{"value":{"kind":"literal","value":"v[]"}},{"name":"asArray","value":{"kind":"literal","value":"out[]"}},{"name":"var","value":{"kind":"literal","value":"x"}},{"name":"function","value":{"kind":"query","plan":{"source":{},"steps":[{"op":"assign","field":"out","expression":{"kind":"binary","operator":"multiply","left":{"kind":"field","name":"x"},"right":{"kind":"field","name":"x"}}}]}}}]}]}}})).unwrap();
    let r = app::generate(request, None);
    assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
    assert!(r.query.unwrap().contains("function={ out := (x * x) }"));
    for name in [
        "array:eval",
        "array:filter",
        "objectArray:eval",
        "objectArray:exists",
        "array:length",
        "array:contains",
    ] {
        let function = catalog::function(name).unwrap();
        assert!(
            catalog::records()
                .iter()
                .any(|r| r.id == function.reference_id)
        );
        let rows = catalog::search(&format!("cql:function:{name}"), 100, false);
        assert!(
            rows.iter()
                .any(|r| r["id"] == function.reference_id && r["supported"] == true)
        );
    }
    assert!(catalog::function("array.eval").is_none());
    assert_eq!(
        check(r#"array:length("v[]") | _length=*"#, false).outcome,
        Outcome::Ready
    );
}

#[test]
fn implicit_field_selection_cannot_be_qualified_by_a_placeholder_collision() {
    for query in [
        r#"#repo=example | wildcard("x")"#,
        r#"#repo=example | wildcard("x",field=[])"#,
        r#"#repo=example | split()"#,
        r#"#repo=example | "<original-event-fields>":=1 | wildcard("x")"#,
        r#"#repo=example | "<runtime-field-name>":=1 | name:="user" | getField(name)"#,
    ] {
        assert_eq!(check(query, true).outcome, Outcome::NeedsContext, "{query}");
        assert_eq!(check(query, false).outcome, Outcome::Ready, "{query}");
    }
    assert_eq!(
        check(r#"#repo=example | wildcard("x",field=[user])"#, true).outcome,
        Outcome::Ready
    );
}

#[test]
fn logical_arrays_cannot_be_created_or_retained_by_literal_name_collisions() {
    for query in [
        r#"#repo=example | "spoof[]":=1 | spoof[0]=*"#,
        r#"#repo=example | "spoof[]":=1 | array:length("spoof[]",as=n)"#,
        r#"#repo=example | "items[].unknown":=1 | objectArray:exists("items[]",condition={items.unknown=*})"#,
        r#"#repo=example | select(["items[]"]) | array:length("items[]",as=n)"#,
        r#"#repo=example | select(["items[0]"]) | array:eval("items[]",asArray="out[]",var=x,function={out:=x}) | regex("x",field="out[]")"#,
    ] {
        let r = check(query, true);
        assert_eq!(
            r.outcome,
            Outcome::NeedsContext,
            "{query}: {:?}",
            r.grounding
        );
    }
    let query = r#"#repo=example | select(["items[0]"]) | array:eval("items[]",asArray="out[]",var=x,function={out:=x}) | select(["out[0]"]) | array:length("out[]",as=n)"#;
    assert_eq!(
        check(query, true).outcome,
        Outcome::Ready,
        "{:?}",
        check(query, true).grounding
    );
}

#[test]
fn pseudo_fields_are_not_linted_as_enclosing_projection_columns() {
    let query = r#"#repo=example | select(["items[0].k"]) | !objectArray:exists("items[]",var=x,condition={x.k=*})"#;
    let r = check(query, true);
    assert_eq!(r.outcome, Outcome::Ready);
    assert!(
        !r.diagnostics.iter().any(|d| d.code == "CQL_DROPPED_FIELD"),
        "{:?}",
        r.diagnostics
    );
}

#[test]
fn array_context_shape_and_binding_controls_cover_raw_and_public_calls() {
    for q in [
        r#"array:filter("v[]",var=x,function=match(table="missing",field=x))"#,
        r#"array:filter("v[]",var=x,function=match())"#,
        r#"array:filter("v[]",var=x,function=neighbor(x))"#,
        r##"array:filter("#tag[]",var=x,function=test(x>0))"##,
        r#"array:eval("v[bad][]",asArray="out[]",var=x,function={out:=x})"#,
        r#"array:eval("v[]",asArray="a..b[]",var=x,function={"a..b":=x})"#,
        r#"array:eval("v[]",asArray="out[]",var=x,function={splitString(x,by=",",as=out)})"#,
        r#"objectArray:eval("v[]",asArray="out[]",var=x,function={splitString(x.k,by=",",as=out)})"#,
        r#"objectArray:eval("items[]",asArray="out[]",var=x,function={x.k:=1 | out:=x.k})"#,
        r#"objectArray:eval("items[]",asArray="out[]",var=x,function={drop([x.k]) | out:=x.k})"#,
        r#"objectArray:eval("items[]",asArray="out[]",var=x,function={rename(x.k,as=y) | out:=y})"#,
    ] {
        assert_ne!(check(q, false).outcome, Outcome::Ready, "{q}");
    }
    for q in [
        r#"array:eval("v[]",asArray="out[]",var=x,function={splitString(x,by=",",index=0,as=out)})"#,
        r#"array:eval("v[]",asArray="out[]",var=x,function={out:=1 | splitString(x,by=",",as=out)})"#,
        r##"array:filter("#tag[]",asArray="out[]",var=x,function=test(x>0))"##,
    ] {
        assert_eq!(
            check(q, false).outcome,
            Outcome::Ready,
            "{q}: {:?}",
            check(q, false).diagnostics
        );
    }
    let q = r#"#repo=example | x.k:=1 | objectArray:eval("items[]",asArray="out[]",var=x,function={out:=x.k})"#;
    assert_eq!(
        check(q, true).outcome,
        Outcome::NeedsContext,
        "{:?}",
        check(q, true).grounding
    );
    let q = r#"#repo=example | user.k:=1 | array:filter("items[]",var=user,function={user.k=*})"#;
    assert_eq!(
        check(q, true).outcome,
        Outcome::Ready,
        "flat bindings do not remap unrelated physical members: {:?}",
        check(q, true).grounding
    );
    for (function, args) in [
        (
            "array:filter",
            json!([{"value":{"kind":"literal","value":"v[]"}},{"name":"function","value":{"kind":"call","function":"match","args":[{"name":"table","value":{"kind":"literal","value":"missing"}},{"name":"field","value":{"kind":"literal","value":"v"}}]}}]),
        ),
        (
            "array:eval",
            json!([{"value":{"kind":"literal","value":"v[bad][]"}},{"name":"function","value":{"kind":"query","plan":{"source":{},"steps":[{"op":"assign","field":"v[bad]","expression":{"kind":"literal","value":1}}]}}}]),
        ),
        (
            "array:filter",
            json!([{"value":{"kind":"literal","value":"#tag[]"}},{"name":"function","value":{"kind":"call","function":"test","args":[{"value":{"kind":"literal","value":true}}]}}]),
        ),
    ] {
        let req:GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"call","function":function,"args":args}]}}})).unwrap();
        assert_ne!(app::generate(req, None).outcome, Outcome::Ready);
    }
}

#[test]
fn split_uses_native_prefix_default_index_and_known_projection_lineage() {
    for q in [
        r#"#repo=example | select(user) | splitString(user,by=",",as=items) | split(items) | items=* | _index=* | user=*"#,
        r#"#repo=example | select(user) | "items[0].k":=1 | split(items,strip=true) | k=* | _index=* | user=*"#,
        r#"#repo=example | select(user) | splitString(user,by=",",as=_events) | split() | _events=* | _index=*"#,
    ] {
        assert_eq!(
            check(q, true).outcome,
            Outcome::Ready,
            "{q}: {:?}",
            check(q, true).grounding
        );
    }
    let q =
        r#"#repo=example | select(user) | "items[0]":=1 | split(items,strip=true) | items[0]=*"#;
    assert_eq!(check(q, true).outcome, Outcome::Ready);
    let r = check("#repo=example | split()", true);
    assert_eq!(r.outcome, Outcome::NeedsContext);
    assert!(r.grounding.basis.keys().any(|k| k.ends_with("_events[]")));
    assert!(
        !r.grounding
            .basis
            .keys()
            .any(|k| k.contains("<array-fields>"))
    );
    for q in [
        r#"#repo=example | split(items,strip=true) | k=*"#,
        r#"#repo=example | select(user) | splitString(user,by=",",as=items) | split(items) | items[0]=*"#,
    ] {
        assert_eq!(check(q, true).outcome, Outcome::NeedsContext, "{q}");
    }
}

#[test]
fn array_paths_and_scope_resets_preserve_only_current_uncertainty() {
    for q in [
        r#"#repo=example | select(user) | splitString(user,by=",",as=items) | split(items) | select(user) | items.k:=user | items.k=*"#,
        r#"#repo=example | select(user) | splitString(user,by=",",as=items) | split(items) | stats() | items.k:=_count | items.k=*"#,
        r#"#repo=example | select(user) | splitString(user,by=",",as=items) | split(items,strip=true) | count() | default(k,value=1) | k=*"#,
        r#"#repo=example | user.k:=1 | array:eval("items[]",asArray="out[]",var=user,function={out:=user.k})"#,
        r#"#repo=example | select(user) | "a[0].v[0]":=1 | array:eval("a[0].v[]",asArray="out[]",var=x,function={out:=x})"#,
        r#"#repo=example | select(user) | "outer[0].items[0]":=1 | array:length("outer[0].items[]",as=n)"#,
        r#"#repo=example | select(user) | "items[0].arr[0]":=1 | objectArray:exists("items[]",var=x,condition={x.arr[0]=*})"#,
        r#"#repo=example | select(user) | "a[0].v[0].k":=1 | split("a[0].v",strip=true) | k=*"#,
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.grounding);
    }
    for q in [
        r#"#repo=example | select(user) | "items[0].arr[1]":=1 | objectArray:exists("items[]",var=x,condition={x.arr[0]=*})"#,
        r#"#repo=example | select(user) | "a[1].v[0]":=1 | array:length("a[0].v[]",as=n)"#,
        r#"#repo=example | split(items,strip=true) | k=* | count()"#,
        r#"#repo=example | split(items,strip=true) | select(k) | k=*"#,
    ] {
        assert_eq!(check(q, true).outcome, Outcome::NeedsContext, "{q}");
    }
    let q =
        r#"objectArray:exists("items[]",var=x,condition={join(query={x.k:=1},field=x.k,key=x.k)})"#;
    assert_eq!(
        check(q, false).outcome,
        Outcome::Ready,
        "{:?}",
        check(q, false).diagnostics
    );
    let q = r#"objectArray:exists("items[]",var=x,condition={series(x.k,startmatch={x.k:=1})})"#;
    assert!(
        check(q, false)
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_UNSUPPORTED_PSEUDO_WRITE")
    );
}

#[test]
fn overwriting_an_array_does_not_promise_prior_member_presence() {
    let q = r#"#repo=example | "v[0]":=1 | array:filter("items[]",asArray="v[]",var=x,function={x=*}) | out:=v[0] | out=*"#;
    assert_eq!(
        check(q, true).outcome,
        Outcome::NeedsContext,
        "{:?}",
        check(q, true).grounding
    );
}

#[test]
fn an_array_write_can_recreate_explicitly_removed_physical_members() {
    let q = r#"#repo=example | drop(["out[0]"]) | array:eval("items[]",asArray="out[]",var=x,function={out:=1}) | out[0]=*"#;
    assert_eq!(
        check(q, true).outcome,
        Outcome::Ready,
        "{:?}",
        check(q, true).grounding
    );
    let q = r#"#repo=example | drop(["out[0]"]) | array:eval("items[]",asArray="out[]",var=x,function={out:=1}) | out[1]=*"#;
    assert_eq!(check(q, true).outcome, Outcome::NeedsContext);

    let q = r#"#repo=example | select(["items[0]"]) | drop(["out[0]"]) | array:eval("items[]",asArray="out[]",var=x,function={out:=x}) | out[0]=*"#;
    assert_eq!(
        check(q, true).outcome,
        Outcome::Ready,
        "{:?}",
        check(q, true).grounding
    );
}
