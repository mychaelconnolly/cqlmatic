use cqlmatic::{
    app,
    contracts::*,
    environment::{Pack, PackData},
};
use serde_json::{Value, json};

const ALPHA: &str = "#repo=alpha_repo #type=alpha-json";
const BETA: &str = "#repo=beta_repo #type=beta-syslog";
const WINDOW: &str = "2026-01-01T00:00:00Z/2026-01-01T00:15:00Z";

fn source(id: &str, repo: &str, type_: &str) -> Value {
    json!({
        "id": id,
        "kind": "repo",
        "name": repo,
        "inventory_status": "observed-only",
        "selectors": {"repo": repo, "type": type_},
        "evidence": {"observed": true},
        "provenance_ids": ["prov:synthetic"]
    })
}

fn field(source: &str, path: &str, observed: bool) -> Value {
    json!({
        "id": format!("env:field:{source}:{path}"),
        "source_id": source,
        "path": path.trim_start_matches('#'),
        "data_type": "string",
        "is_tag": path.starts_with('#'),
        "evidence_kind": if observed { "observed" } else { "documented" },
        "coverage": {"observed": if observed { json!(true) } else { Value::Null }},
        "provenance_ids": ["prov:synthetic"]
    })
}

/// A field the map measured in alpha's sample window and did not observe.
fn measured_absent(path: &str, declared: bool) -> Value {
    json!({
        "id": format!("env:field:alpha:{path}"),
        "source_id": "env:repo:alpha",
        "path": path,
        "data_type": "unknown",
        "is_tag": false,
        "evidence_kind": "observed",
        "observation_window": WINDOW,
        "coverage": {"declared": declared, "observed": false, "rate": 0},
        "provenance_ids": ["prov:synthetic"]
    })
}

fn sources() -> Vec<Value> {
    vec![
        source("env:repo:alpha", "alpha_repo", "alpha-json"),
        source("env:repo:beta", "beta_repo", "beta-syslog"),
    ]
}

/// One observed field, a documented field, a tag and an array element in alpha, and
/// documented fields only in beta.
fn fields() -> Vec<Value> {
    vec![
        field("env:repo:alpha", "src.ip", true),
        field("env:repo:alpha", "#Vendor", true),
        field("env:repo:alpha", "user.name", false),
        field("env:repo:alpha", "event.category[0]", false),
        field("env:repo:beta", "host.name", false),
        field("env:repo:beta", "process.name", false),
    ]
}

fn pack() -> Pack {
    pack_from(sources(), fields(), "synthetic-view")
}

fn pack_from(sources: Vec<Value>, fields: Vec<Value>, view: &str) -> Pack {
    Pack::new(
        EnvironmentId {
            cloud: Cloud::Us1,
            tenant: "0".repeat(32),
            view: view.into(),
        },
        "2026-01-02T00:00:00Z".into(),
        PackData {
            sources,
            fields,
            provenance: vec![json!({
                "id": "prov:synthetic",
                "source_type": "mcp-observation",
                "canonical_source": "synthetic unmapped grounding regression",
                "retrieved_at": "2026-01-01T00:00:00Z",
                "redaction_status": "none",
                "coverage_status": "complete"
            })],
            ..PackData::default()
        },
    )
    .unwrap()
}

fn check(query: &str, pack: Option<&Pack>, require_observed: bool) -> Analysis {
    app::check(
        CheckRequest {
            schema_version: 1,
            id: None,
            query: query.into(),
            context: Context {
                require_observed,
                ..Context::default()
            },
        },
        pack,
    )
}

fn warnings<'a>(result: &'a Analysis, code: &str) -> Vec<&'a str> {
    result
        .diagnostics
        .iter()
        .filter(|d| d.code == code && d.severity == Severity::Warning)
        .map(|d| d.message.as_str())
        .collect()
}

fn assert_no_warning(query: &str, pack: &Pack) {
    let result = check(query, Some(pack), false);
    assert_eq!(
        result.outcome,
        Outcome::Ready,
        "{query}: {:?}",
        result.diagnostics
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| d.severity != Severity::Warning),
        "{query}: {:?}",
        result.diagnostics
    );
    assert_eq!(result.exit_code(true), 0, "{query}");
}

#[test]
fn a_field_the_selected_source_lacks_warns_and_names_the_source() {
    let result = check(
        r#"#repo=alpha_repo | dest.made_up="x" | count()"#,
        Some(&pack()),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    let found = warnings(&result, "CQL_UNMAPPED_FIELD");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(found[0].contains("dest.made_up"), "{}", found[0]);
    assert!(found[0].contains(ALPHA), "{}", found[0]);
    assert!(!found[0].contains(BETA), "{}", found[0]);
    assert_eq!(
        result.grounding.basis["alpha_repo:*:dest.made_up"],
        "query-supplied; not measured in selected pack"
    );
    assert_eq!(result.exit_code(true), 1);
}

#[test]
fn a_field_no_mapped_source_has_names_every_source_when_the_query_selects_none() {
    let result = check(r#"audit.made_up_id=4624 | count()"#, Some(&pack()), false);
    let found = warnings(&result, "CQL_UNMAPPED_FIELD");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(found[0].contains("audit.made_up_id"), "{}", found[0]);
    assert!(
        found[0].contains(ALPHA) && found[0].contains(BETA),
        "{}",
        found[0]
    );
    // A field one mapped source documents is quiet when the query selects no source.
    assert_no_warning(r#"process.name="x" | count()"#, &pack());
}

/// Ready, with the expected evidence for the field and neither map warning.
fn assert_quiet(query: &str, pack: &Pack, key: &str, basis: &str) {
    let result = check(query, Some(pack), false);
    assert_eq!(
        result.outcome,
        Outcome::Ready,
        "{query}: {:?}",
        result.diagnostics
    );
    assert_eq!(
        result.grounding.basis.get(key).map(String::as_str),
        Some(basis),
        "{query}: {:?}",
        result.grounding.basis
    );
    assert!(
        result
            .diagnostics
            .iter()
            .all(|d| !d.code.starts_with("CQL_UNMAPPED_") && d.code != "CQL_GROUNDING_LENIENT"),
        "{query}: {:?}",
        result.diagnostics
    );
}

#[test]
fn documented_produced_tag_array_and_metadata_fields_are_quiet() {
    let unmeasured = "query-supplied; not measured in selected pack";
    let listed = "listed field namespace; population not measured";
    for (query, key, basis) in [
        (
            r#"#repo=alpha_repo | user.name="x" | count()"#,
            "alpha_repo:*:user.name",
            listed,
        ),
        (
            r#"#repo=alpha_repo | n := length(src.ip) | n > 3 | count()"#,
            "alpha_repo:*:n",
            "query-produced; conditional; incoming field unverified",
        ),
        (
            r#"#repo=alpha_repo | #Vendor="x" | count()"#,
            "alpha_repo:*:#Vendor",
            "observed in a bounded sample",
        ),
        (
            r#"#repo=alpha_repo | event.category[0]="network" | count()"#,
            "alpha_repo:*:event.category[0]",
            listed,
        ),
        (
            r#"#repo=alpha_repo | array:contains(array="event.category[]", value="network") | count()"#,
            "alpha_repo:*:event.category[]",
            unmeasured,
        ),
        (
            r#"#repo=alpha_repo | @ingesttimestamp > 0 | count()"#,
            "alpha_repo:*:@ingesttimestamp",
            unmeasured,
        ),
    ] {
        assert_quiet(query, &pack(), key, basis);
    }
}

#[test]
fn a_correct_query_with_a_map_gets_no_grounding_warning() {
    assert_no_warning(
        r#"#repo=alpha_repo | src.ip="10.0.0.1" | groupBy([user.name], limit=max)"#,
        &pack(),
    );
    assert_no_warning(
        r#"#repo=beta_repo #type=beta-syslog | host.name="h" | count()"#,
        &pack(),
    );
}

#[test]
fn a_repository_no_mapped_source_has_warns_and_names_the_mapped_selectors() {
    let result = check(
        r#"#repo=gamma_repo | src.ip=* | count()"#,
        Some(&pack()),
        false,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    let found = warnings(&result, "CQL_UNMAPPED_SOURCE");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(found[0].contains("#repo=gamma_repo"), "{}", found[0]);
    assert!(
        found[0].contains(ALPHA) && found[0].contains(BETA),
        "{}",
        found[0]
    );
    assert!(!found[0].contains("view"), "{}", found[0]);
    // The selection is reported once; its fields are not reported again.
    assert!(
        warnings(&result, "CQL_UNMAPPED_FIELD").is_empty(),
        "{:?}",
        result.diagnostics
    );
    assert_eq!(result.exit_code(true), 1);

    let result = check(
        r#"#repo=alpha_repo #type=beta-syslog | src.ip=* | count()"#,
        Some(&pack()),
        false,
    );
    let found = warnings(&result, "CQL_UNMAPPED_SOURCE");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(
        found[0].contains("#repo=alpha_repo #type=beta-syslog"),
        "{}",
        found[0]
    );
}

#[test]
fn the_target_view_name_used_as_a_repository_is_called_the_view() {
    for query in [
        r#"#repo=synthetic-view | src.ip="10.0.0.1" | count()"#,
        r#"#repo="synthetic-view" | dest.made_up="x" | count()"#,
    ] {
        let result = check(query, Some(&pack()), false);
        let found = warnings(&result, "CQL_UNMAPPED_SOURCE");
        assert_eq!(found.len(), 1, "{query}: {:?}", result.diagnostics);
        assert!(found[0].contains("#repo=synthetic-view"), "{}", found[0]);
        assert!(found[0].contains("target view"), "{}", found[0]);
        assert!(
            found[0].contains(ALPHA) && found[0].contains(BETA),
            "{}",
            found[0]
        );
        assert!(
            warnings(&result, "CQL_UNMAPPED_FIELD").is_empty(),
            "{query}: {:?}",
            result.diagnostics
        );
    }
    // With a mapped #type, the warning names the source that has it, not every source.
    let result = check(
        r#"#repo=synthetic-view #type=beta-syslog | host.name=* | count()"#,
        Some(&pack()),
        false,
    );
    let found = warnings(&result, "CQL_UNMAPPED_SOURCE");
    assert_eq!(found.len(), 1, "{:?}", result.diagnostics);
    assert!(found[0].contains("target view"), "{}", found[0]);
    assert!(found[0].contains(BETA), "{}", found[0]);
    assert!(!found[0].contains(ALPHA), "{}", found[0]);
}

#[test]
fn strict_mode_output_is_unchanged() {
    for (query, missing) in [
        (
            r#"#repo=alpha_repo | dest.made_up="x" | count()"#,
            vec!["alpha_repo:*:dest.made_up"],
        ),
        (
            r#"#repo=synthetic-view | src.ip="10.0.0.1" | count()"#,
            vec!["synthetic-view:*:src.ip"],
        ),
        (
            r#"#repo=alpha_repo | user.name="x" | count()"#,
            vec!["alpha_repo:*:user.name"],
        ),
    ] {
        let result = check(query, Some(&pack()), true);
        assert_eq!(result.outcome, Outcome::NeedsContext, "{query}");
        assert_eq!(result.grounding.missing, missing, "{query}");
        let codes: Vec<_> = result.diagnostics.iter().map(|d| d.code.as_str()).collect();
        assert_eq!(codes, ["CQL_CONTEXT_REQUIRED"], "{query}");
    }
    let result = check(
        r#"#repo=alpha_repo | src.ip="10.0.0.1" | count()"#,
        Some(&pack()),
        true,
    );
    assert_eq!(result.outcome, Outcome::Ready);
    assert!(result.diagnostics.is_empty(), "{:?}", result.diagnostics);
}

#[test]
fn without_a_map_check_adds_no_grounding_warning() {
    // An informational grouping ceiling alone leaves --fail-on-warning at exit 0.
    let result = check("groupBy([user], function=count(), limit=max)", None, false);
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
    let codes: Vec<_> = result
        .diagnostics
        .iter()
        .map(|d| (d.code.as_str(), d.severity.clone()))
        .collect();
    assert_eq!(codes, [("CQL_GROUPBY_LIMIT", Severity::Info)]);
    assert_eq!(result.exit_code(true), 0);
    assert_eq!(
        result.grounding.basis["*:*:user"],
        "query-supplied; deployment unverified"
    );
}

fn one_warning(result: &Analysis, code: &str) -> String {
    let found = warnings(result, code);
    assert_eq!(found.len(), 1, "{code}: {:?}", result.diagnostics);
    found[0].to_string()
}

fn no_warning(result: &Analysis, code: &str) {
    assert!(
        warnings(result, code).is_empty(),
        "{:?}",
        result.diagnostics
    );
}

#[test]
fn reading_a_field_an_earlier_step_removed_warns_with_or_without_a_map() {
    let mapped = pack();
    for query in [
        "#repo=alpha_repo | drop(src.ip) | src.ip=*",
        "#repo=alpha_repo | rename(field=src.ip, as=source_ip) | src.ip=*",
    ] {
        for pack in [Some(&mapped), None] {
            let result = check(query, pack, false);
            assert_eq!(result.outcome, Outcome::Ready, "{query}");
            let message = one_warning(&result, "CQL_DROPPED_FIELD");
            assert!(message.starts_with("'src.ip' "), "{message}");
            assert_eq!(result.exit_code(true), 1, "{query}");
        }
    }
    assert_no_warning(
        "#repo=alpha_repo | rename(field=src.ip, as=source_ip) | source_ip=*",
        &mapped,
    );
    // Filters and other removals between the removal and the read create no field.
    for query in [
        "#repo=alpha_repo | drop(src.ip) | user.name=* | src.ip=*",
        "#repo=alpha_repo | drop(src.ip) | drop(user.name) | src.ip=*",
        "#repo=alpha_repo | drop(src.ip) | x := src.ip",
    ] {
        let message = one_warning(&check(query, Some(&mapped), false), "CQL_DROPPED_FIELD");
        assert!(message.starts_with("'src.ip' "), "{query}: {message}");
    }
    // A parser, an assignment or a function that writes fields in between may recreate it.
    for query in [
        r#"#repo=alpha_repo | payload := "{\"ip\":\"192.0.2.1\"}" | drop(src.ip) | parseJson(field=payload,prefix="src.") | src.ip=*"#,
        "#repo=alpha_repo | drop(src.ip) | parseJson(field=user.name) | src.ip=*",
        "#repo=alpha_repo | drop(src.ip) | x := 1 | src.ip=*",
        r#"#repo=alpha_repo | drop(src.ip) | format("%s", field=user.name, as=y) | src.ip=*"#,
        "#repo=alpha_repo | drop(src.ip) | user.name=/(?<k>x)/ | src.ip=*",
    ] {
        let result = check(query, Some(&mapped), false);
        assert_eq!(result.outcome, Outcome::Ready, "{query}");
        no_warning(&result, "CQL_DROPPED_FIELD");
    }
    // A read the projection check already reports is not reported twice.
    let result = check(
        "#repo=alpha_repo | select([src.ip]) | user.name=*",
        Some(&mapped),
        false,
    );
    one_warning(&result, "CQL_DROPPED_FIELD");
    let result = check(
        "#repo=alpha_repo | drop(src.ip) | src.ip=*",
        Some(&mapped),
        true,
    );
    let codes: Vec<_> = result.diagnostics.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["CQL_CONTEXT_REQUIRED"]);
}

#[test]
fn in_selectors_are_not_compared() {
    for query in [
        r##"in(field="#repo", values=["alpha_repo"]) | host.name=*"##,
        r##"in(field="#repo", values=["synthetic-view"]) | count()"##,
        r#"#repo=~in(values=["synthetic-view"]) | count()"#,
        r##"in(field="#repo", values=["gamma_repo"]) | made.up=*"##,
        r##"made.up=* | in(field="#type", values=["alpha-json"])"##,
        r##"in(field="#repo", values=["gamma_repo"], ignoreCase=true) | count()"##,
        r##"in(field="#repo", values=["gamma_repo", "delta_repo"]) | count()"##,
    ] {
        assert_no_warning(query, &pack());
    }
}

#[test]
fn strict_mode_ignores_in_selectors_as_before() {
    // As in 0.1.10, in() does not narrow the sources strict grounding compares.
    for query in [
        r##"src.ip=* | in(field="#repo",values=["beta_repo"]) | count()"##,
        r##"in(field="#repo",values=["beta_repo"]) | src.ip=* | count()"##,
    ] {
        let result = check(query, Some(&pack()), true);
        assert_eq!(result.outcome, Outcome::Ready, "{query}");
        assert!(result.diagnostics.is_empty(), "{query}");
        assert_eq!(
            result.grounding.basis["*:*:src.ip"],
            "observed in a bounded sample"
        );
    }
    let result = check(
        r##"in(field="#repo", values=["alpha_repo"]) | host.name=*"##,
        Some(&pack()),
        true,
    );
    assert_eq!(result.grounding.missing, ["*:*:host.name"]);
}

#[test]
fn a_literal_selector_is_checked_when_the_other_selector_is_not_literal() {
    for query in [
        "#repo=synthetic-view #type=/alpha/ | count()",
        "#repo=synthetic-view #type=alpha* | count()",
        "#repo=synthetic-view #type!=alpha-json | count()",
    ] {
        let message = one_warning(&check(query, Some(&pack()), false), "CQL_UNMAPPED_SOURCE");
        assert!(
            message.starts_with("#repo=synthetic-view matches no mapped source."),
            "{query}: {message}"
        );
        assert!(message.contains("target view"), "{query}: {message}");
    }
    assert_no_warning(
        "#repo=alpha_repo #type=alpha* | src.ip=* | count()",
        &pack(),
    );
}

#[test]
fn tag_and_data_namespaces_and_member_paths_stay_distinct() {
    for (query, field) in [
        ("#repo=alpha_repo | Vendor=* | count()", "Vendor"),
        ("#repo=alpha_repo | #src.ip=* | count()", "#src.ip"),
        (
            "#repo=alpha_repo | src.ip.octet=* | count()",
            "src.ip.octet",
        ),
        // The map lists event.category[0]; this reads element 0 of an `event` array.
        (
            "#repo=alpha_repo | event[0].category=* | count()",
            "event[0].category",
        ),
        // Only a documented object or array parent covers an unlisted member.
        ("#repo=alpha_repo | seen.inner=* | count()", "seen.inner"),
    ] {
        let mut fields = fields();
        fields.push(json!({
            "id": "env:field:alpha:seen",
            "source_id": "env:repo:alpha",
            "path": "seen",
            "data_type": "object",
            "is_tag": false,
            "evidence_kind": "observed",
            "coverage": {"observed": true},
            "provenance_ids": ["prov:synthetic"]
        }));
        let pack = pack_from(sources(), fields, "synthetic-view");
        let message = one_warning(&check(query, Some(&pack), false), "CQL_UNMAPPED_FIELD");
        assert!(message.starts_with(&format!("{field} ")), "{message}");
    }
    let mut fields = fields();
    fields.push(json!({
        "id": "env:field:alpha:obj",
        "source_id": "env:repo:alpha",
        "path": "obj",
        "data_type": "object",
        "is_tag": false,
        "evidence_kind": "documented",
        "provenance_ids": ["prov:synthetic"]
    }));
    fields.push(field("env:repo:alpha", "items[0].name", true));
    let pack = pack_from(sources(), fields, "synthetic-view");
    // A read of a whole array is covered by the records of its elements.
    for query in [
        "#repo=alpha_repo | items[3].name=* | count()",
        r#"#repo=alpha_repo | objectArray:exists("items[]",var=x,condition={x.name=*}) | count()"#,
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(result.outcome, Outcome::Ready, "{query}");
        no_warning(&result, "CQL_UNMAPPED_FIELD");
    }
    let unmeasured = "query-supplied; not measured in selected pack";
    // A member of a listed object, and an array position the sample did not show, stay quiet.
    assert_quiet(
        "#repo=alpha_repo | obj.inner=* | count()",
        &pack,
        "alpha_repo:*:obj.inner",
        unmeasured,
    );
    assert_quiet(
        "#repo=alpha_repo | event.category[9]=* | count()",
        &pack,
        "alpha_repo:*:event.category[9]",
        unmeasured,
    );
}

#[test]
fn recorded_datasets_and_record_kind_fields_count_as_mapped() {
    let mut sources = sources();
    sources[0]["relationship"] = json!({
        "datasets": ["audit", "auth"],
        "record_kinds": [{"field": "#event.kind", "values": ["login", "logout"], "truncated": false}]
    });
    let pack = pack_from(sources, fields(), "synthetic-view");
    for query in [
        "#repo=alpha_repo | #event.dataset=auth | count()",
        "#repo=alpha_repo | #event.kind=login | count()",
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(result.outcome, Outcome::Ready, "{query}");
        no_warning(&result, "CQL_UNMAPPED_FIELD");
    }
    let result = check(
        "#repo=beta_repo | #event.dataset=auth | count()",
        Some(&pack),
        false,
    );
    let message = one_warning(&result, "CQL_UNMAPPED_FIELD");
    assert!(message.starts_with("#event.dataset "), "{message}");
}

#[test]
fn arrays_the_query_creates_are_not_reported() {
    for query in [
        r#"#repo=alpha_repo | splitString(by=".", field=src.ip, as=picked) | concatArray("picked[]")"#,
        r#"#repo=alpha_repo | "picked[0]" := "network" | concatArray("picked[]")"#,
        r#"#repo=alpha_repo | eval("picked[0]"="network") | concatArray("picked[]")"#,
        r#"#repo=alpha_repo | "picked[0]" := "network" | array:contains(array="picked[]", value="network")"#,
        r#"#repo=alpha_repo | "picked[0].k" := "network" | array:contains(array="picked[]", value="network")"#,
    ] {
        let result = check(query, Some(&pack()), false);
        assert_eq!(result.outcome, Outcome::Ready, "{query}");
        no_warning(&result, "CQL_UNMAPPED_FIELD");
        assert!(
            result.grounding.basis["alpha_repo:*:picked[]"].starts_with("query-produced"),
            "{query}: {:?}",
            result.grounding.basis
        );
    }
    // Strict mode needs no deployment evidence for an array written by index.
    let result = check(
        r#"#repo=alpha_repo | "picked[0]" := "network" | array:contains(array="picked[]", value="network")"#,
        Some(&pack()),
        true,
    );
    assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.grounding);
    // An array the query only reads is still compared with the map.
    let result = check(
        r#"#repo=alpha_repo | concatArray("picked[]")"#,
        Some(&pack()),
        false,
    );
    let message = one_warning(&result, "CQL_UNMAPPED_FIELD");
    assert!(message.starts_with("picked[] "), "{message}");
}

#[test]
fn a_source_without_selectors_is_compared_when_the_query_selects_none() {
    let mut sources = sources();
    sources[1].as_object_mut().unwrap().remove("selectors");
    let pack = pack_from(sources, fields(), "synthetic-view");
    assert_no_warning("host.name=* | count()", &pack);
    let message = one_warning(
        &check("made.up=* | count()", Some(&pack), false),
        "CQL_UNMAPPED_FIELD",
    );
    assert!(
        message.contains(ALPHA) && message.contains("beta_repo"),
        "{message}"
    );
}

#[test]
fn the_view_name_as_a_repository_warns_where_a_source_records_no_repository() {
    let mut sources = sources();
    sources[1]["selectors"] = json!({"type": "beta-syslog"});
    let pack = pack_from(sources, fields(), "synthetic-view");
    let message = one_warning(
        &check("#repo=synthetic-view | count()", Some(&pack), false),
        "CQL_UNMAPPED_SOURCE",
    );
    assert!(message.contains("target view"), "{message}");
    // Beta records no repository, so another repository name is not compared.
    assert_no_warning("#repo=gamma_repo | count()", &pack);
}

#[test]
fn the_view_hint_appears_only_when_no_mapped_selector_has_the_value() {
    let view_is_repo = pack_from(sources(), fields(), "alpha_repo");
    let message = one_warning(
        &check(
            "#repo=alpha_repo #type=missing | count()",
            Some(&view_is_repo),
            false,
        ),
        "CQL_UNMAPPED_SOURCE",
    );
    assert!(
        message.starts_with("#type=missing matches no mapped source."),
        "{message}"
    );
    assert!(!message.contains("target view"), "{message}");
    let message = one_warning(
        &check("#type=synthetic-view | count()", Some(&pack()), false),
        "CQL_UNMAPPED_SOURCE",
    );
    assert!(message.contains("target view"), "{message}");
}

#[test]
fn a_field_measured_absent_warns_with_its_window_and_a_declared_field_is_quiet() {
    let mut fields = fields();
    fields.push(measured_absent("zero_only", false));
    fields.push(measured_absent("decl.absent", true));
    fields.push(json!({
        "id": "env:field:alpha:decl.field",
        "source_id": "env:repo:alpha",
        "path": "decl.field",
        "data_type": "unknown",
        "is_tag": false,
        "evidence_kind": "declared",
        "coverage": {"declared": true},
        "provenance_ids": ["prov:synthetic"]
    }));
    // The exact record wins over a documented parent object.
    fields.push(measured_absent("obj.zero_only", false));
    fields.push(json!({
        "id": "env:field:alpha:obj",
        "source_id": "env:repo:alpha",
        "path": "obj",
        "data_type": "object",
        "is_tag": false,
        "evidence_kind": "documented",
        "provenance_ids": ["prov:synthetic"]
    }));
    let pack = pack_from(sources(), fields, "synthetic-view");
    for field in ["zero_only", "obj.zero_only"] {
        let result = check(
            &format!("#repo=alpha_repo | {field}=* | count()"),
            Some(&pack),
            false,
        );
        assert_eq!(result.outcome, Outcome::Ready, "{:?}", result.diagnostics);
        let message = one_warning(&result, "CQL_UNMAPPED_FIELD");
        assert!(message.starts_with(&format!("{field} ")), "{message}");
        assert!(
            message.contains(ALPHA) && message.contains(WINDOW),
            "{message}"
        );
        assert_eq!(result.exit_code(true), 1);
    }
    for query in [
        "#repo=alpha_repo | decl.field=1 | count()",
        "#repo=alpha_repo | decl.absent=1 | count()",
    ] {
        let result = check(query, Some(&pack), false);
        assert_eq!(result.outcome, Outcome::Ready, "{query}");
        no_warning(&result, "CQL_UNMAPPED_FIELD");
    }
    let message = one_warning(
        &check(
            r#"#repo=alpha_repo | dest.made_up="x" | count()"#,
            Some(&pack),
            false,
        ),
        "CQL_UNMAPPED_FIELD",
    );
    assert!(
        message.contains("not observed, documented or declared"),
        "{message}"
    );
}
