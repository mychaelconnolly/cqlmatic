use cqlmatic::{app, contracts::*, environment::Pack};
use serde_json::json;

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
fn temporal_functions_keep_native_arguments_and_caps() {
    for q in [
        r#"findTimestamp(field=date,as=event_time,timezone=UTC,timezoneAs=zone,addErrors=false) | formatTime("%F",field=event_time,as=day)"#,
        r#"bucket(1h,field=[user],function=[count(),sum(bytes)],buckets=1500,limit=500,minSpan="1 hour",timezone=UTC,unit=["",""])"#,
        r#"timeChart(user,span=1d,minSpan=1h,limit=500,timezone=UTC,unit="")"#,
        r#"session(function=count(),maxpause=15m)"#,
        r#"series(user,maxpause=5m,maxduration=1h,startmatch={kind=start},endmatch={kind=end},memlimit="1KiB")"#,
        r#"count() | neighbor(_count,direction=succeeding,distance=10000,prefix=next)"#,
    ] {
        let r = check(q, false);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.diagnostics);
    }
    for q in [
        "bucket(buckets=0)",
        "bucket(buckets=1501)",
        "bucket(limit=501)",
        "timeChart(limit=max)",
        "timeChart(buckets=0)",
        "head(1) | neighbor(user,distance=10001)",
        "head(1) | neighbor(user,direction=next)",
        "neighbor(user)",
        "series(user,memlimit=\"2KiB\")",
        "series(user,memlimit=\"0B\")",
        "bucket(function=[count(),sum(bytes)],unit=[a,b,c])",
        "findTimestamp(format=yyyy)",
    ] {
        assert_eq!(check(q, false).outcome, Outcome::Invalid, "{q}");
    }
    for q in [
        "bucket(span=1month)",
        "bucket(minSpan=1month)",
        "bucket(span=1h@h)",
    ] {
        assert_ne!(check(q, false).outcome, Outcome::Ready, "{q}");
    }
}

#[test]
fn temporal_aggregates_retire_inputs_and_export_only_outputs() {
    for (aggregate, output) in [
        ("bucket(1h)", "_bucket"),
        ("timeChart(span=1h)", "_count"),
        ("session()", "_count"),
        ("series(user)", "_duration"),
    ] {
        let r = check(
            &format!("user:=\"a\" | {aggregate} | select({output})"),
            false,
        );
        assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
        assert!(!r.diagnostics.iter().any(|d| d.code == "CQL_DROPPED_FIELD"));
        let r = check(&format!("user:=\"a\" | {aggregate} | select(user)"), false);
        if !aggregate.starts_with("series") {
            assert!(
                r.diagnostics.iter().any(|d| d.code == "CQL_DROPPED_FIELD"),
                "{aggregate}"
            );
        }
    }
    let r = check("bucket(1h) | select(user)", true);
    assert_eq!(r.outcome, Outcome::NeedsContext);
    assert!(
        r.grounding
            .missing
            .iter()
            .any(|m| m.contains("not exported"))
    );
}

#[test]
fn named_queries_attributes_and_links_round_trip_through_public_generation() {
    let req: GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"call","function":"correlate","args":[
      {"value":{"kind":"named-query","name":"Login","plan":{"source":{},"filters":[{"op":"exists","field":"user"}]},"include":{"mode":"fields","fields":["user","host"]}}},
      {"value":{"kind":"named-query","name":"Purchase","plan":{"source":{},"steps":[{"op":"link","field":"user","to":"Login","key":"user"}]},"include":{"mode":"all"}}},
      {"name":"aggregation","value":{"kind":"array","items":[{"kind":"named-query","name":"Views","plan":{"source":{},"steps":[{"op":"link","field":"user","to":"Login","key":"user"},{"op":"call","function":"count"}]},"after":{"query":"Login","offset":"+5s"},"before":{"query":"Purchase","offset":"-1s"}}]}},
      {"name":"sequence","value":{"kind":"literal","value":true}}, {"name":"within","value":{"kind":"literal","value":"5m"}}
    ]}]}}})).unwrap();
    let r = app::generate(req, None);
    assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
    let query = r.query.unwrap();
    assert!(query.contains("include: [\"user\", \"host\"]"), "{query}");
    assert!(query.contains("include: *"));
    assert!(query.contains("user <=> Login.user"));
    assert!(query.contains("after: \"Login +5s\""));
    assert_eq!(check(&query, false).outcome, Outcome::Ready);
}

#[test]
fn correlation_contexts_and_temporal_references_are_checked() {
    for q in [
        "correlate(A:{true})",
        "correlate(A:{user=* | lower(user,as=normal)} include:[normal], B:{user <=> A.normal},within=5m)",
        "correlate(A:{true},aggregation=[N:{count()} after:\"A -5s\"],within=1m)",
        "correlate(A:{true},aggregation=[N:{groupBy(user) | head(1) | _count>0}])",
        "correlate(A:{true},aggregation=[N:{stats([count(),sum(bytes)]) | n:=_count+1}])",
        "correlate(A:{true},B:{user <=> A.user},sequence=true,sequenceBy=@ingesttimestamp,jitterTolerance=1s,within=1m,root=A,iterationLimit=10)",
    ] {
        let r = check(q, false);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.diagnostics);
    }
    for q in [
        "correlate(A:{true},A:{true})",
        "correlate(A:{true},aggregation=[A:{count()}])",
        "correlate(A:{user <=> A.user})",
        "correlate(A:{user <=> Z.user})",
        "correlate(A:{count()})",
        "correlate(A:{createEvents([\"a\"])})",
        "correlate(A:{correlate(B:{true})})",
        "correlate(A:{true},aggregation=[N:{count() | user <=> A.user}])",
        "correlate(A:{true},aggregation=[N:{user <=> N.user | count()}])",
        "correlate(A:{true},root=Z)",
        "correlate(A:{true},sequenceBy=user)",
        "correlate(A:{true},sequence=true,jitterTolerance=2m,within=1m)",
        "correlate(A:{true},iterationLimit=11)",
        "correlate(A:{true},iterationLimit=0)",
        "correlate(A:{true},aggregation=[N:{count()} after:\"Z\"])",
        "correlate(A:{true},aggregation=[N:{count()} after:\"A +1m\"],within=1m)",
        "correlate(A:{true},aggregation=[N:{count()} after:\"A +1s\" before:\"A -1s\"])",
        "correlate(A:{true},B:{true},sequence=true,sequenceBy=@timestamp,aggregation=[N:{count()} after:\"B\" before:\"A\"])",
        "correlate(A:{true},sequence=true,sequenceBy=ordinal,aggregation=[N:{count()} after:\"A +1s\"])",
        "correlate(A:{true}) | join({true},field=user)",
    ] {
        let r = check(q, false);
        assert_eq!(r.outcome, Outcome::Invalid, "{q}: {:?}", r.diagnostics);
    }
    for q in [
        "correlate(A:{true},aggregation=[N:{groupBy(user)}])",
        "correlate(A:{true},aggregation=[N:{head(5)}])",
        "correlate(A:{true},aggregation=[N:{count() | split(_events)}])",
        "correlate(A:{true},aggregation=[N:{count()} after:\"A +1month\"])",
        "correlate(A:{true} after:\"A\")",
        "correlate(A:{case {* | user <=> B.user}},B:{true})",
    ] {
        let r = check(q, false);
        assert_eq!(r.outcome, Outcome::Unsupported, "{q}: {:?}", r.diagnostics);
    }
}

#[test]
fn correlation_exports_preserve_each_source_and_unknown_shapes() {
    let prefix = "correlate(A:{user=*} include:[user],B:{host <=> A.user},aggregation=[N:{user <=> A.user | count()}])";
    let r = check(
        &format!("{prefix} | select([A.user,B.host,N._count,@timestamp])"),
        false,
    );
    assert_eq!(r.outcome, Outcome::Ready);
    assert!(
        !r.diagnostics.iter().any(|d| d.code == "CQL_DROPPED_FIELD"),
        "{:?}",
        r.diagnostics
    );
    let r = check(&format!("{prefix} | select(user)"), true);
    assert_eq!(r.outcome, Outcome::NeedsContext);
    assert!(
        r.grounding
            .missing
            .iter()
            .any(|m| m.contains("user: not exported"))
    );
    let r = check("correlate(A:{user=*} include:*) | select(A.unknown)", true);
    assert_eq!(r.outcome, Outcome::NeedsContext);
    assert!(r.grounding.missing.iter().any(|m| m.contains("A.unknown")));
    let r = check(
        "correlate(A:{user=*} include:[user],B:{host <=> A.user},includeConstraintValues=false) | select(B.host)",
        true,
    );
    assert!(
        r.grounding
            .missing
            .iter()
            .any(|m| m.contains("B.host: not exported")),
        "{:?}",
        r.grounding
    );
}

#[test]
fn correlation_inherits_prefix_without_leaking_between_queries() {
    for q in [
        "#repo=other | correlate(A:{user=*})",
        "#repo=example | drop([user]) | correlate(A:{user=*})",
        "#repo=example | correlate(A:{local:=1},B:{local=*})",
    ] {
        assert_eq!(check(q, true).outcome, Outcome::NeedsContext, "{q}");
    }
    assert_eq!(
        check("alias:=1 | correlate(A:{alias=*})", true).outcome,
        Outcome::Ready
    );
    let mut req: GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{"repo":"other"},"steps":[{"op":"call","function":"correlate","args":[{"value":{"kind":"named-query","name":"A","plan":{"source":{},"filters":[{"op":"exists","field":"user"}]}}}]}]}}})).unwrap();
    req.context.require_observed = true;
    let pack: Pack =
        serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    assert_eq!(
        app::generate(req, Some(&pack)).outcome,
        Outcome::NeedsContext
    );
    let r = check(
        "select(user) | correlate(A:{true} include:*) | select(A.user)",
        false,
    );
    assert!(
        !r.diagnostics.iter().any(|d| d.code == "CQL_DROPPED_FIELD"),
        "{:?}",
        r.diagnostics
    );
}

#[test]
fn unknown_correlation_columns_do_not_prove_absence_or_entire_array_ownership() {
    let prefix = "correlate(A:{#repo=example | user=*} include:*)";
    for tail in [
        r#"s:="x" | splitString(s,by=",",as="A.parts") | "A.parts[3]"=*"#,
        r#"case { @timestamp=123456789 | "A.secret":=1; * } | "A.secret"=*"#,
        r#""A.items[0]":=1 | array:eval("A.items[]",asArray="out[]",var=x,function={out:=x}) | out[0]=*"#,
        r#""A.items[0].k":=1 | objectArray:exists("A.items[]",var=x,condition={x.k=*})"#,
    ] {
        let r = check(&format!("{prefix} | {tail}"), true);
        assert_eq!(
            r.outcome,
            Outcome::NeedsContext,
            "{tail}: {:?}",
            r.grounding
        );
    }
    for tail in [
        r#""A.secret":=1 | "A.secret"=*"#,
        r#"drop(["A.secret"]) | case { @timestamp=123456789 | "A.secret":=1; * } | "A.secret"=*"#,
        r#"select(@timestamp) | s:="x" | splitString(s,by=",",as="A.parts") | "A.parts[3]"=*"#,
    ] {
        assert_eq!(
            check(&format!("{prefix} | {tail}"), true).outcome,
            Outcome::Ready,
            "{tail}"
        );
    }
    let q = r#"correlate(A:{#repo=example | user=*} include:[user]) | "A.items[0]":=1 | array:eval("A.items[]",asArray="out[]",var=x,function={out:=x}) | out[0]=*"#;
    assert_eq!(check(q, true).outcome, Outcome::Ready);
}

#[test]
fn correlation_placement_links_and_single_row_proof_follow_effects() {
    for q in [
        "case {* | correlate(A:{true})}",
        "user match { * => correlate(A:{true}) }",
        "case {* | user <=> A.user}",
        "user match { * => user <=> A.user}",
        "series(user,startmatch={correlate(A:{true})})",
        "series(user,startmatch={count()})",
        "series(user,endmatch={createEvents([\"x\"])})",
        "correlate(A:{user <=> B.user | user:=1},B:{true})",
        "correlate(A:{user <=> B.user | host=*},B:{true})",
    ] {
        assert_eq!(check(q, false).outcome, Outcome::Invalid, "{q}");
    }
    assert_eq!(
        check(
            "defineTable({correlate(A:{true})},name=\"t\",include=*) | readFile(table=\"t\")",
            false
        )
        .outcome,
        Outcome::Ready
    );
    let orphan = json!({"source":{},"steps":[{"op":"link","field":"user","to":"A","key":"user"}]});
    for branch in [
        json!({"op":"case","branches":[orphan.clone()]}),
        json!({"op":"match","field":"user","arms":[{"pattern":{"kind":"any"},"plan":orphan}]}),
    ] {
        let request: GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[branch]}}})).unwrap();
        let result = app::generate(request, None);
        assert_eq!(result.outcome, Outcome::Invalid);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_LINK_PLACEMENT")
        );
    }
    let repeat = r#"count() | s:="aa" | regex("a",field=s,repeat=true)"#;
    assert_eq!(
        check(
            &format!("correlate(A:{{true}},aggregation=[N:{{{repeat}}}])"),
            false
        )
        .outcome,
        Outcome::Unsupported
    );
    for reducer in ["count()", "head(1)", "tail(1)"] {
        let q = format!("correlate(A:{{true}},aggregation=[N:{{{repeat} | {reducer}}}])");
        assert_eq!(check(&q, false).outcome, Outcome::Ready, "{q}");
    }
    assert_eq!(
        check("correlate(A:{true},aggregation=[N:{stats()}])", false).outcome,
        Outcome::Ready
    );
    let req: GenerateRequest=serde_json::from_value(json!({"schema_version":1,"input":{"mode":"plan","plan":{"source":{},"steps":[{"op":"call","function":"correlate","args":[{"value":{"kind":"named-query","name":"A","plan":{"source":{},"steps":[{"op":"link","field":"user","to":"B","key":"user"},{"op":"assign","field":"user","expression":{"kind":"literal","value":1}}]}}},{"value":{"kind":"named-query","name":"B","plan":{"source":{}}}}]}]}}})).unwrap();
    assert_eq!(app::generate(req, None).outcome, Outcome::Invalid);
}

#[test]
fn correlation_collection_requires_a_single_row_bound() {
    for aggregate in [
        "collect([user],multival=false)",
        "stats([collect([user],multival=false)])",
    ] {
        let query = format!("correlate(A:{{true}},aggregation=[N:{{{aggregate}}}])");
        let result = check(&query, false);
        assert_eq!(result.outcome, Outcome::Unsupported, "{query}");
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_UNSUPPORTED_CORRELATION_ROWS")
        );
        for cap in ["head(1)", "tail(1)"] {
            let query = format!("correlate(A:{{true}},aggregation=[N:{{{aggregate} | {cap}}}])");
            assert_eq!(check(&query, false).outcome, Outcome::Ready, "{query}");
        }
    }
    for aggregate in [
        "collect([user])",
        "collect([user],multival=true)",
        "stats([collect([user],multival=true)])",
    ] {
        let query = format!("correlate(A:{{true}},aggregation=[N:{{{aggregate}}}])");
        assert_eq!(check(&query, false).outcome, Outcome::Ready, "{query}");
    }
}

#[test]
fn ambiguous_time_contracts_retain_coverage_and_runtime_uncertainty() {
    for q in [
        "series(user,maxpause=1month)",
        "correlate(A:{true},sequence=true,jitterTolerance=0s)",
    ] {
        let r = check(q, false);
        assert_eq!(r.outcome, Outcome::Unsupported);
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == "CQL_UNSUPPORTED_DURATION")
        );
    }
    for function in ["bucket", "timeChart"] {
        let r = check(&format!("{function}(minSpan=1000)"), false);
        assert_eq!(r.outcome, Outcome::Unsupported);
        assert!(!r.diagnostics.iter().any(|d| d.code == "CQL_ARGUMENT_TYPE"));
    }
    for options in [
        "sequenceBy=ordinal",
        "within=1m,jitterTolerance=10s",
        "sequenceBy=@ingesttimestamp",
        "within=1m",
    ] {
        let q = format!(
            "correlate(A:{{true}},B:{{true}},sequence=true,{options},aggregation=[N:{{count()}} after:\"B\" before:\"A\"])"
        );
        let r = check(&q, false);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.diagnostics);
        assert!(
            r.diagnostics
                .iter()
                .any(|d| d.code == "CQL_TIME_WINDOW_RUNTIME")
        );
    }
    assert_eq!(check("correlate(A:{true},sequence=true,sequenceBy=ordinal,aggregation=[N:{count()} after:\"A +0s\"])",false).outcome,Outcome::Invalid);
}
