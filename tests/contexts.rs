use cqlmatic::{
    app, catalog,
    contracts::*,
    environment::{self, Pack},
    language::parser,
};

fn historical() -> Pack {
    serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap()
}
fn check(query: &str, strict: bool) -> Analysis {
    let pack = historical();
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
fn historical_pack_admission_preserves_original_identity_and_digest() {
    let old = historical();
    old.validate().unwrap();
    assert_eq!(old.profile_id, "cqlmatic-ngsiem-core-v1");
    assert_eq!(
        old.content_digest,
        "d7b36c2be21df5082a9a49dfbdabf3bcb87de5b0f59acf6aaa18cadd22fb114d"
    );
    assert_eq!(old.digest().unwrap(), old.content_digest);
    let mut current = Pack::new(
        old.environment.clone(),
        "2030-01-01T00:00:00Z".into(),
        old.data.clone(),
    )
    .unwrap();
    let merged = environment::merge(&old, &current).unwrap();
    assert_ne!(merged.content_digest, old.content_digest);
    assert_eq!(merged.data.provenance[0], old.data.provenance[0]);
    let parent = merged
        .data
        .provenance
        .iter()
        .find(|p| p["source_hash"] == old.content_digest)
        .unwrap();
    assert_eq!(parent["source_type"], "manual-curation");
    assert_eq!(parent["coverage_status"], "declared-only");
    let metadata: serde_json::Value =
        serde_json::from_str(parent["notes"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["profile_id"], old.profile_id);
    assert_eq!(metadata["knowledge_revision"], old.knowledge_revision);
    assert_eq!(
        merged.data.provenance[0]["retrieved_at"],
        "2026-10-01T00:00:00Z"
    );
    current.knowledge_revision = "undeclared".into();
    current.content_digest = current.digest().unwrap();
    assert_eq!(current.validate().unwrap_err().code, "PACK_PROFILE");
    assert_eq!(old.digest().unwrap(), old.content_digest);
}

#[test]
fn argument_binding_is_shared_and_never_last_wins() {
    let q = parser::parse("count(user, field=other)").unwrap();
    let c = match &q.steps[0] {
        cqlmatic::language::ast::Step::Call(c) => c,
        _ => panic!(),
    };
    let resolved = catalog::resolve(c).unwrap();
    assert_eq!(resolved.arguments["field"].text(), Some("user"));
    assert_eq!(resolved.diagnostics[0].code, "CQL_DUPLICATE_ARGUMENT");
    let result = check("count(user, field=other)", true);
    assert_eq!(result.outcome, Outcome::Invalid);
    assert!(
        !result
            .grounding
            .basis
            .values()
            .any(|basis| basis.contains("observed"))
    );
}

#[test]
fn nested_expressions_and_leading_source_selectors_ground_their_inputs() {
    let r = check(
        "user=* #repo=example | x := if(true, then=missing, else=user)",
        true,
    );
    assert_eq!(r.outcome, Outcome::NeedsContext);
    assert_eq!(
        r.grounding.basis["example:*:user"],
        "observed in a bounded sample"
    );
    assert!(r.grounding.missing.iter().any(|m| m.contains("missing")));
    let late = check("x := user | count() | #repo=other", false);
    assert!(late.grounding.basis.contains_key("*:*:user"));
    assert!(!late.grounding.basis.contains_key("other:*:user"));
}

#[test]
fn branch_outputs_and_foreign_columns_keep_their_contexts() {
    let r = check(
        "#repo=example | drop([x,y]) | x := user | case { user=a | y := x; * | y := x } | test(y == user)",
        true,
    );
    assert_eq!(r.outcome, Outcome::Ready, "{:?}", r.diagnostics);
    assert_eq!(r.grounding.basis["example:*:x"], "query-produced");
    assert_eq!(r.grounding.basis["example:*:y"], "query-produced");
    let join = check(
        "#repo=example | join({ #repo=example | drop([x]) | x := user }, field=user, key=x, include=[x])",
        true,
    );
    assert_eq!(join.outcome, Outcome::Ready, "{:?}", join.grounding);
    assert_eq!(join.grounding.basis["example:*:x"], "query-produced");
}

#[test]
fn independent_queries_and_replacement_clear_ordering_and_external_evidence() {
    for query in [
        "head(1) | join({ neighbor(user) }, field=user)",
        "head(1) | readFile(\"lookup.csv\") | neighbor(user)",
    ] {
        assert!(
            check(query, false)
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_NEIGHBOR_ORDER")
        );
    }
    let external = check("#repo=example | readFile(\"lookup.csv\") | user=*", true);
    assert_eq!(external.outcome, Outcome::NeedsContext);
    assert_eq!(
        external.grounding.basis["*:*:user"],
        "external-source; unverified"
    );
}

#[test]
fn branch_validation_inherits_stream_and_repeated_reads_still_warn() {
    assert!(
        check("createEvents([\"user=a\"]) | case { * | user=a }", false)
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_SYNTHETIC_FIELD_BEFORE_PARSE")
    );
    assert!(
        check("user=* | select([other]) | user=a", false)
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_DROPPED_FIELD")
    );
    assert!(
        check("case { * | head(1); * | head(2) } | neighbor(user)", false)
            .diagnostics
            .iter()
            .any(|d| d.code == "CQL_NEIGHBOR_UNORDERED")
    );
}

#[test]
fn diagnostic_classification_is_explicit_and_not_a_code_substring() {
    assert!(!Diagnostic::error("UNSUPPORTED_AS_TEXT", "an established rule").is_unsupported());
    assert!(Diagnostic::unsupported("COVERAGE_GAP", "unqualified form").is_unsupported());
    assert_eq!(
        check("unknownFunction()", false).outcome,
        Outcome::Unsupported
    );
    assert_eq!(check("count(unknown=1)", false).outcome, Outcome::Invalid);
}

#[test]
fn later_and_conflicting_selectors_never_borrow_another_repositorys_evidence() {
    let later = check("before := user | #repo=other | user=*", true);
    assert_eq!(later.outcome, Outcome::NeedsContext);
    assert_eq!(
        later.grounding.basis["*:*:user"],
        "observed in a bounded sample"
    );
    assert!(later.grounding.missing.iter().any(|m| m.contains("other")));
    for q in [
        "#repo=example #repo=other user=*",
        "(#repo=example OR #repo=other) user=*",
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::NeedsContext);
        assert!(r.grounding.basis["*:*:user"].contains("unverified"));
    }
}

#[test]
fn branch_origin_and_conditional_production_never_waive_incoming_evidence() {
    for q in [
        "#repo=example | case { * | readFile(\"lookup.csv\") } | user=*",
        "#repo=example | case { user=a | readFile(\"lookup.csv\"); * } | user=*",
        "#repo=example | case { user=a | unmeasured := \"x\"; * } | unmeasured=*",
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::NeedsContext, "{q}: {:?}", r.grounding);
    }
    for name in ["as", "asArray", "var"] {
        let r = check(&format!("#repo=example | eval({name}=unmeasured)"), true);
        assert_eq!(r.outcome, Outcome::NeedsContext);
        assert!(r.grounding.missing.iter().any(|m| m.contains("unmeasured")));
    }
}

#[test]
fn continuing_query_arguments_inherit_source_and_derived_fields() {
    for q in [
        "#repo=other | x := 1 | series(x, startmatch={user=a})",
        "#repo=other | x := 1 | selfJoinFilter(field=x, where=[{user=a}])",
    ] {
        assert_eq!(check(q, true).outcome, Outcome::NeedsContext, "{q}");
    }
    assert_eq!(
        check("#repo=example | x := 1 | series(x, startmatch={x=1})", true).outcome,
        Outcome::Ready
    );
}

#[test]
fn local_tables_and_join_outputs_keep_query_lineage() {
    for q in [
        "#repo=example | parseJson(include=[user])",
        "defineTable(name=\"t\", query={ #repo=example }, include=[user]) | #repo=example | match(table=\"t\", field=user, column=user)",
        "#repo=example | drop([x]) | join({#repo=example | drop([x]) | x:=user}, field=user, key=x, include=[x]) | test(x == user)",
        "eval(z=1, a=z) | a=1",
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.grounding);
    }
}

#[test]
fn uncertainty_is_not_overwritten_by_an_observation_of_another_origin() {
    let counted = check("readFile(\"lookup.csv\") | count() | _count>0", true);
    assert_eq!(counted.outcome, Outcome::NeedsContext);
    assert_eq!(counted.grounding.basis["*:*:_count"], "query-produced");
    assert!(
        counted
            .grounding
            .basis
            .keys()
            .any(|k| k.contains("<resource:"))
    );
    let r = check("match(file=\"lookup.csv\", field=user, column=user)", true);
    assert_eq!(r.outcome, Outcome::NeedsContext);
    assert!(r.grounding.basis["*:*:user"].contains("external-source; unverified"));
    assert!(r.grounding.basis["*:*:user"].contains("observed in a bounded sample"));
}

#[test]
fn replacement_state_survives_branches_and_regex_does_not_restore_dropped_fields() {
    for q in [
        "count() | readFile(\"lookup.csv\") | correlate(A:{user=a}, B:{user=b | user <=> A.user})",
        "case { * | readFile(\"lookup.csv\") } | correlate(A:{user=a}, B:{user=b | user <=> A.user})",
    ] {
        assert!(
            check(q, false)
                .diagnostics
                .iter()
                .any(|d| d.code == "CQL_CORRELATION_PLACEMENT"),
            "{q}"
        );
    }
    assert!(
        check(
            "count() | regex(\"x\", field=user, strict=false) | user=a",
            false
        )
        .diagnostics
        .iter()
        .any(|d| d.code == "CQL_DROPPED_FIELD")
    );
}

#[test]
fn removed_exports_cannot_reacquire_deployment_evidence() {
    for q in [
        "defineTable(name=\"a\", query={#repo=example | x:=user}, include=[x]) | #repo=example | match(table=\"a\", field=user, column=user)",
        "#repo=example | join({#repo=example | x:=user | drop([x])}, field=user, key=x)",
        "#repo=example | join({#repo=example | select([other])}, field=user)",
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::NeedsContext, "{q}: {:?}", r.grounding);
        assert!(
            r.grounding
                .basis
                .values()
                .any(|basis| basis.contains("not exported"))
        );
    }
}

#[test]
fn independent_searches_keep_lexical_table_definitions_and_documented_file_spelling() {
    for q in [
        "defineTable(name=\"a\", query={#repo=example}, include=[user]) | defineTable(name=\"b\", query={#repo=example | match(table=\"a\", field=user, column=user)}, include=[user]) | #repo=example | match(table=\"b\", field=user)",
        "defineTable(name=\"a\", query={#repo=example}, include=[user]) | #repo=example | match(file=\"a\", field=user)",
        "defineTable(name=\"a\", query={#repo=example}, include=[user]) | readFile(\"a\") | user=*",
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.grounding);
    }
}

#[test]
fn definite_overwrites_clear_conditional_state_in_order_and_across_all_branches() {
    for q in [
        "#repo=example | case {user=a | x:=1; *} | eval(x=1) | x=*",
        "#repo=example | case {user=a | x:=1; *} | eval(x=1,a=x) | test(a==1)",
        "#repo=example | case {user=a | x:=1; *} | case {user=a | x:=2; * | x:=3} | x=*",
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.grounding);
    }
    assert_eq!(
        check(
            "#repo=example | case {user=a | x:=1; *} | eval(a=x,x=1) | test(a==1)",
            true
        )
        .outcome,
        Outcome::NeedsContext
    );
}

#[test]
fn renamed_away_exports_never_reacquire_observed_evidence() {
    for q in [
        "#repo=example | join({#repo=example | select([user]) | rename(user,as=x)},field=user,key=user)",
        "defineTable(name=\"a\",query={#repo=example | rename(user,as=x)},include=[user]) | #repo=example | match(table=\"a\",field=user,column=user)",
    ] {
        assert_eq!(check(q, true).outcome, Outcome::NeedsContext, "{q}");
    }
    for q in [
        "#repo=example | rename(user,as=user) | user=*",
        "#repo=example | join({#repo=example | select([user]) | rename(user,as=x)},field=user,key=x)",
    ] {
        assert_eq!(check(q, true).outcome, Outcome::Ready, "{q}");
    }
}

#[test]
fn known_absence_on_a_branch_does_not_introduce_an_incoming_field_requirement() {
    for q in [
        "#repo=example | x:=1 | case {user=a | drop([x]); *} | x=*",
        "#repo=example | case {user=a | x:=1; * | select([user])} | x=*",
    ] {
        let r = check(q, true);
        assert_eq!(r.outcome, Outcome::Ready, "{q}: {:?}", r.grounding);
        assert_eq!(r.grounding.basis["example:*:x"], "query-produced");
    }
}
