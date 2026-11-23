use cqlmatic::{
    app,
    contracts::{CheckRequest, Context, Outcome, Severity},
};

fn check(query: &str) -> cqlmatic::contracts::Analysis {
    let result = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context::default(),
        },
        None,
    );
    assert_eq!(
        result.outcome,
        Outcome::Ready,
        "{query}: {:?}",
        result.diagnostics
    );
    result
}

fn diags_by_code<'a>(
    result: &'a cqlmatic::contracts::Analysis,
    code: &str,
) -> Vec<(Severity, &'a str)> {
    result
        .diagnostics
        .iter()
        .filter(|d| d.code == code)
        .map(|d| (d.severity.clone(), d.message.as_str()))
        .collect()
}

#[test]
fn correlate_exports_labeled_builtin_metadata_not_bare() {
    let base = "correlate(dns: { kind=dns }, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s)";

    // correlate exports Label.@id, Label.@timestamp, Label.@ingesttimestamp,
    // and Label.@timestamp.nanos on mandatory queries without include:.
    let labeled = check(&format!(
        "{base} | groupBy([dns.@id, dns.@timestamp, dns.@ingesttimestamp, dns.@timestamp.nanos, conn.@id, conn.@timestamp], function=count(), limit=max)"
    ));
    assert!(
        diags_by_code(&labeled, "CQL_UNEXPORTED_FIELD").is_empty(),
        "labeled built-in metadata should be exported: {:?}",
        labeled.diagnostics
    );
    assert!(
        diags_by_code(&labeled, "CQL_DROPPED_FIELD").is_empty(),
        "labeled built-in metadata should not be marked dropped: {:?}",
        labeled.diagnostics
    );

    // Bare @id and @timestamp are NOT exported on correlate output rows.
    let bare = check(&format!(
        "{base} | groupBy([@id, @timestamp], function=count(), limit=max)"
    ));
    let unexported = diags_by_code(&bare, "CQL_UNEXPORTED_FIELD");
    assert_eq!(unexported.len(), 1, "{:?}", bare.diagnostics);
    assert_eq!(unexported[0].0, Severity::Warning);
    assert!(unexported[0].1.contains("@id"), "{}", unexported[0].1);
    assert!(
        unexported[0].1.contains("@timestamp"),
        "{}",
        unexported[0].1
    );

    // If a mandatory branch explicitly projects out @id with select, Label.@id is NOT exported,
    // while kept Label.@timestamp remains exported.
    let projected = check(
        "correlate(dns: { kind=dns | select([src, @timestamp]) }, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.@id, dns.@timestamp], function=count(), limit=max)",
    );
    let projected_unexported = diags_by_code(&projected, "CQL_UNEXPORTED_FIELD");
    assert_eq!(projected_unexported.len(), 1, "{:?}", projected.diagnostics);
    assert!(
        projected_unexported[0].1.contains("dns.@id"),
        "{}",
        projected_unexported[0].1
    );
    assert!(
        !projected_unexported[0].1.contains("dns.@timestamp"),
        "{}",
        projected_unexported[0].1
    );

    // If a mandatory branch drops @id explicitly, Label.@id warns CQL_UNEXPORTED_FIELD.
    let dropped_id = check(
        "correlate(dns: { kind=dns | drop([@id]) }, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.@id], function=count(), limit=max)",
    );
    let dropped_unexported = diags_by_code(&dropped_id, "CQL_UNEXPORTED_FIELD");
    assert_eq!(dropped_unexported.len(), 1, "{:?}", dropped_id.diagnostics);
    assert!(
        dropped_unexported[0].1.contains("dns.@id"),
        "{}",
        dropped_unexported[0].1
    );
    assert!(
        dropped_unexported[0].1.contains("inside dns's pipeline"),
        "{}",
        dropped_unexported[0].1
    );

    // Same when the mandatory branch uses include: * or renames @id.
    for query in [
        "correlate(dns: { kind=dns | drop([@id]) } include: *, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.@id], function=count(), limit=max)",
        "correlate(dns: { kind=dns | rename(@id, as=event_id) }, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.@id], function=count(), limit=max)",
    ] {
        let res = check(query);
        let unexp = diags_by_code(&res, "CQL_UNEXPORTED_FIELD");
        assert_eq!(unexp.len(), 1, "{query}: {:?}", res.diagnostics);
        assert!(unexp[0].1.contains("dns.@id"), "{query}: {}", unexp[0].1);
    }

    // Re-assigning @id after drop restores dns.@id export.
    let restored = check(
        "correlate(dns: { kind=dns | drop([@id]) | @id := \"x\" }, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.@id], function=count(), limit=max)",
    );
    assert!(
        diags_by_code(&restored, "CQL_UNEXPORTED_FIELD").is_empty(),
        "{:?}",
        restored.diagnostics
    );

    // includeConstraintValues=false still exports mandatory Label.@id and Label.@timestamp.
    let no_constraints = check(
        "correlate(dns: { kind=dns }, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s, includeConstraintValues=false) | groupBy([dns.@id, conn.@timestamp], function=count(), limit=max)",
    );
    assert!(
        diags_by_code(&no_constraints, "CQL_UNEXPORTED_FIELD").is_empty(),
        "{:?}",
        no_constraints.diagnostics
    );
    assert!(
        diags_by_code(&no_constraints, "CQL_DROPPED_FIELD").is_empty(),
        "{:?}",
        no_constraints.diagnostics
    );

    // An aggregation query missing a field points to that query's pipeline (not include:).
    let agg_missing = check(
        "correlate(query=first: { kind=a }, aggregation=[later: { kind=b | user <=> first.user | count() }], within=5m, root=\"first\") | groupBy([later.missing_col], function=count(), limit=max)",
    );
    let agg_unexp = diags_by_code(&agg_missing, "CQL_UNEXPORTED_FIELD");
    assert_eq!(agg_unexp.len(), 1, "{:?}", agg_missing.diagnostics);
    assert!(
        agg_unexp[0].1.contains("inside later's pipeline") && !agg_unexp[0].1.contains("include:"),
        "{}",
        agg_unexp[0].1
    );
}

#[test]
fn core_warnings_include_actionable_rewrite_hints() {
    // 1. CQL_UNEXPORTED_FIELD (query-scoped): tells caller to add fields to include: [...] after }.
    let unexported_query = check(
        "correlate(dns: { kind=dns } include: [src], conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.qname], function=count(), limit=max)",
    );
    let uq = diags_by_code(&unexported_query, "CQL_UNEXPORTED_FIELD");
    assert_eq!(uq.len(), 1, "{:?}", unexported_query.diagnostics);
    assert!(
        uq[0].1.contains("include:") && uq[0].1.contains("after"),
        "expected concrete include: rewrite hint, got: {}",
        uq[0].1
    );

    // 2. CQL_UNEXPORTED_FIELD (unprefixed): tells caller to prefix QUERY.FIELD and list in include:.
    let unexported_bare = check(
        "correlate(dns: { kind=dns } include: [src], conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([qname], function=count(), limit=max)",
    );
    let ub = diags_by_code(&unexported_bare, "CQL_UNEXPORTED_FIELD");
    assert_eq!(ub.len(), 1, "{:?}", unexported_bare.diagnostics);
    assert!(
        ub[0].1.contains("QUERY.FIELD") && ub[0].1.contains("include:"),
        "expected prefix + include: rewrite hint, got: {}",
        ub[0].1
    );

    // 3. CQL_DROPPED_FIELD (after bucket/select projection AND after drop/rename): names retention rewrite.
    let dropped_proj =
        check("kind=conn | bucket(span=1h, function=count()) | groupBy([src, _bucket], limit=max)");
    let dp = diags_by_code(&dropped_proj, "CQL_DROPPED_FIELD");
    assert_eq!(dp.len(), 1, "{:?}", dropped_proj.diagnostics);
    assert!(
        dp[0].1.contains("bucket") || dp[0].1.contains("select") || dp[0].1.contains("groupBy"),
        "expected retention rewrite hint, got: {}",
        dp[0].1
    );

    let dropped_step = check("kind=conn | drop([src]) | src=*");
    let ds = diags_by_code(&dropped_step, "CQL_DROPPED_FIELD");
    assert_eq!(ds.len(), 1, "{:?}", dropped_step.diagnostics);
    assert!(
        ds[0].1.contains("before") || ds[0].1.contains("keep") || ds[0].1.contains("new name"),
        "expected dropped-field rewrite hint, got: {}",
        ds[0].1
    );

    // 4. CQL_JOIN_MAX: tells caller to set max= or aggregate subquery with groupBy.
    let join_max = check(
        "kind=dns | dns_ts := @timestamp | join({kind=conn | conn_ts := @timestamp}, field=src, include=[conn_ts]) | test(conn_ts >= dns_ts)",
    );
    let jm = diags_by_code(&join_max, "CQL_JOIN_MAX");
    assert_eq!(jm.len(), 1, "{:?}", join_max.diagnostics);
    assert!(
        jm[0].1.contains("max=") && jm[0].1.contains("groupBy"),
        "expected max= / groupBy rewrite hint, got: {}",
        jm[0].1
    );
}

#[test]
fn match_nrows_warns_only_when_omitted_and_informs_when_explicit() {
    let raw_table = "defineTable(name=\"events_b\", query={kind=b | seen_at := @timestamp}, include=[key, seen_at]) | kind=a";

    // Omitted nrows (default 1) stays a warning and exits 1 under --fail-on-warning.
    let omitted = check(&format!(
        "{raw_table} | match(table=\"events_b\", field=key, column=key, include=[seen_at]) | test(seen_at >= @timestamp)"
    ));
    let omitted_diags = diags_by_code(&omitted, "CQL_MATCH_NROWS");
    assert_eq!(omitted_diags.len(), 1, "{:?}", omitted.diagnostics);
    assert_eq!(omitted_diags[0].0, Severity::Warning);
    assert!(
        omitted_diags[0].1.contains("nrows=max"),
        "{}",
        omitted_diags[0].1
    );
    assert_eq!(omitted.exit_code(true), 1);

    // Explicit nrows=max, numeric nrows=25, nrows=1, and parameterized nrows=?{cap=50} are informational.
    for explicit_arg in ["nrows=max", "nrows=25", "nrows=1", "nrows=?{cap=50}"] {
        let res = check(&format!(
            "{raw_table} | match(table=\"events_b\", field=key, column=key, include=[seen_at], {explicit_arg}) | test(seen_at >= @timestamp)"
        ));
        let diags = diags_by_code(&res, "CQL_MATCH_NROWS");
        assert_eq!(diags.len(), 1, "{explicit_arg}: {:?}", res.diagnostics);
        assert_eq!(
            diags[0].0,
            Severity::Info,
            "{explicit_arg} should be informational, got {:?}",
            diags[0]
        );
        if explicit_arg == "nrows=1" {
            assert!(
                diags[0].1.contains("at most 1 row ") && !diags[0].1.contains("1 rows"),
                "{}",
                diags[0].1
            );
        }
        assert_eq!(
            res.exit_code(true),
            0,
            "{explicit_arg} should pass --fail-on-warning"
        );
    }
}

#[test]
fn correlate_export_hint_edge_cases() {
    // 1. select inside a mandatory branch (with or without include: *) routes to pipeline hint, not include:.
    for query in [
        "correlate(dns: { kind=dns | select([src, @timestamp]) }, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.@id, dns.@timestamp], function=count(), limit=max)",
        "correlate(dns: { kind=dns | select([src, @timestamp]) } include: *, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.qname], function=count(), limit=max)",
    ] {
        let res = check(query);
        let unexp = diags_by_code(&res, "CQL_UNEXPORTED_FIELD");
        assert_eq!(unexp.len(), 1, "{query}: {:?}", res.diagnostics);
        assert!(
            unexp[0].1.contains("inside dns's pipeline") && !unexp[0].1.contains("include:"),
            "{query}: {}",
            unexp[0].1
        );
    }

    // 2. Putting a projected-out built-in in include: [@id] does NOT grant query-produced grounding.
    let proj_inc = check("correlate(A:{select(@timestamp)} include:[@id]) | A.@id=*");
    assert_eq!(
        proj_inc
            .grounding
            .basis
            .get("*:*:A.@id")
            .map(String::as_str),
        Some("not exported by preceding query"),
        "{:?}",
        proj_inc.grounding.basis
    );

    // 3. Strict grounding rejects A.@id when every branch path discarded @id (splitString or mixed case).
    let pack: cqlmatic::environment::Pack =
        serde_json::from_str(include_str!("fixtures/legacy-core-v1-pack.json")).unwrap();
    for body in [
        r#"select([@timestamp,user]) | splitString(user,by=",",as=parts)"#,
        r#"case { user=x | drop(@id); * | select(@timestamp) }"#,
    ] {
        let res = app::check(
            CheckRequest {
                schema_version: 1,
                id: None,
                query: format!("correlate(A:{{{body}}}) | A.@id=*"),
                context: Context {
                    require_observed: true,
                    ..Context::default()
                },
            },
            Some(&pack),
        );
        assert_eq!(
            res.outcome,
            Outcome::NeedsContext,
            "{body}: {:?}",
            res.grounding
        );
        assert_eq!(
            res.grounding.basis.get("*:*:A.@id").map(String::as_str),
            Some("not exported by preceding query"),
            "{body}: {:?}",
            res.grounding.basis
        );
    }

    // 4. Bare field after correlate qualifies include: hint for mandatory queries only.
    let agg_bare = check("correlate(A:{true},aggregation=[N:{count(as=n)}]) | n>0");
    let ab = diags_by_code(&agg_bare, "CQL_UNEXPORTED_FIELD");
    assert_eq!(ab.len(), 1, "{:?}", agg_bare.diagnostics);
    assert!(ab[0].1.contains("for a mandatory query"), "{}", ab[0].1);

    // 5. QueryExport strips label prefix in concrete include: [field] example.
    let qexp = check(
        "correlate(dns: { kind=dns } include: [src], conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.qname], function=count(), limit=max)",
    );
    let qe = diags_by_code(&qexp, "CQL_UNEXPORTED_FIELD");
    assert_eq!(qe.len(), 1);
    assert!(qe[0].1.contains("include: [qname]"), "{}", qe[0].1);

    // 6. CQL_DROPPED_FIELD after correlate names correlate (not select/groupBy/bucket),
    //    and post-correlate rebuild + rename blames the rename (not correlate).
    let cd = diags_by_code(&qexp, "CQL_DROPPED_FIELD");
    assert_eq!(cd.len(), 1);
    assert!(
        cd[0].1.contains("correlate") && !cd[0].1.contains("bucket("),
        "{}",
        cd[0].1
    );

    let rebuilt_rename = check(
        r#"correlate(A:{drop(@id)}) | A.@id:="rebuilt" | rename(A.@id,as=event_id) | A.@id=*"#,
    );
    assert!(
        diags_by_code(&rebuilt_rename, "CQL_UNEXPORTED_FIELD").is_empty(),
        "{:?}",
        rebuilt_rename.diagnostics
    );
    let rr_drop = diags_by_code(&rebuilt_rename, "CQL_DROPPED_FIELD");
    assert_eq!(rr_drop.len(), 1, "{:?}", rebuilt_rename.diagnostics);
    assert!(rr_drop[0].1.contains("after rename"), "{}", rr_drop[0].1);

    // 7. Post-correlate array:drop is not blamed on correlate; inside-branch array:drop points to pipeline.
    let after_arr = check(
        r#"correlate(first: { kind=a } include: *, second: { kind=b | user <=> first.user }, within=5m) | array:drop("first.answers[]") | "first.answers[0]" != *"#,
    );
    assert!(
        diags_by_code(&after_arr, "CQL_UNEXPORTED_FIELD").is_empty(),
        "{:?}",
        after_arr.diagnostics
    );

    let in_arr = check(
        r#"correlate(first: { kind=a | array:drop("answers[]") } include: *, second: { kind=b | user <=> first.user }, within=5m) | "first.answers[0]" != *"#,
    );
    let ia = diags_by_code(&in_arr, "CQL_UNEXPORTED_FIELD");
    assert_eq!(ia.len(), 1, "{:?}", in_arr.diagnostics);
    assert!(ia[0].1.contains("inside first's pipeline"), "{}", ia[0].1);

    // 8. validate.rs and app.rs agree on drop([@id]) inside branch even after an intervening assignment or under --require-observed.
    let after_assign = check(
        "correlate(dns: { kind=dns | drop([@id]) }, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | x := 1 | groupBy([dns.@id, x], function=count(), limit=max)",
    );
    assert_eq!(diags_by_code(&after_assign, "CQL_DROPPED_FIELD").len(), 1);
    assert_eq!(
        diags_by_code(&after_assign, "CQL_UNEXPORTED_FIELD").len(),
        1
    );

    let strict_drop = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: "correlate(dns: { kind=dns | drop([@id]) }, conn: { kind=conn | src <=> dns.src }, sequence=true, within=60s) | groupBy([dns.@id], function=count(), limit=max)".into(),
            context: Context {
                require_observed: true,
                ..Context::default()
            },
        },
        Some(&pack),
    );
    assert_eq!(strict_drop.outcome, Outcome::NeedsContext);
    assert_eq!(diags_by_code(&strict_drop, "CQL_DROPPED_FIELD").len(), 1);

    // 9. CQL_JOIN_MAX and omitted CQL_MATCH_NROWS qualify groupBy hint.
    let jm = check(
        "kind=dns | dns_ts := @timestamp | join({kind=conn | conn_ts := @timestamp}, field=src, include=[conn_ts]) | test(conn_ts >= dns_ts)",
    );
    assert!(
        diags_by_code(&jm, "CQL_JOIN_MAX")[0]
            .1
            .contains("when one summary value per key suffices")
    );

    // 10. Rebuilt array element after array:drop inside a correlate branch stays exported,
    // while an unrebuilt element of the dropped array stays unavailable.
    let rebuilt_query = r#"correlate(A: { #repo=example | splitString(user, by=",", as=answers) | array:drop("answers[]") | "answers[0]" := "x" } include: ["answers[0]"]) | "A.answers[0]" = *"#;
    let rebuilt = check(rebuilt_query);
    assert!(
        diags_by_code(&rebuilt, "CQL_UNEXPORTED_FIELD").is_empty(),
        "rebuilt array element should stay exported: {:?}",
        rebuilt.diagnostics
    );
    assert!(
        diags_by_code(&rebuilt, "CQL_DROPPED_FIELD").is_empty(),
        "rebuilt array element should not be flagged dropped: {:?}",
        rebuilt.diagnostics
    );
    let rebuilt_strict = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: rebuilt_query.into(),
            context: Context {
                require_observed: true,
                ..Context::default()
            },
        },
        Some(&pack),
    );
    assert_eq!(
        rebuilt_strict.outcome,
        Outcome::Ready,
        "{:?}",
        rebuilt_strict.grounding
    );
    assert_eq!(
        rebuilt_strict
            .grounding
            .basis
            .get("*:*:A.answers[0]")
            .map(String::as_str),
        Some("query-produced"),
        "{:?}",
        rebuilt_strict.grounding.basis
    );

    let unrebuilt_query = r#"correlate(A: { #repo=example | splitString(user, by=",", as=answers) | array:drop("answers[]") | "answers[0]" := "x" } include: ["answers[0]", "answers[1]"]) | "A.answers[1]" = *"#;
    let unrebuilt = check(unrebuilt_query);
    let unrebuilt_diags = diags_by_code(&unrebuilt, "CQL_UNEXPORTED_FIELD");
    assert_eq!(unrebuilt_diags.len(), 1, "{:?}", unrebuilt.diagnostics);
    assert!(
        unrebuilt_diags[0].1.contains("inside A's pipeline"),
        "expected pipeline guidance for unrebuilt dropped element, got: {}",
        unrebuilt_diags[0].1
    );
    let unrebuilt_strict = app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: unrebuilt_query.into(),
            context: Context {
                require_observed: true,
                ..Context::default()
            },
        },
        Some(&pack),
    );
    assert_eq!(
        unrebuilt_strict.outcome,
        Outcome::NeedsContext,
        "{:?}",
        unrebuilt_strict.grounding
    );
    assert_eq!(
        unrebuilt_strict
            .grounding
            .basis
            .get("*:*:A.answers[1]")
            .map(String::as_str),
        Some("not exported by preceding query"),
        "{:?}",
        unrebuilt_strict.grounding.basis
    );
}
